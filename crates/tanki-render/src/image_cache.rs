//! Off-thread image decoding and scaling with a small cache.

use crate::{ImageError, ImageSource};
use image::DynamicImage;
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) type Key = (ImageSource, (u32, u32));
type Ready = Result<Arc<DynamicImage>, ImageError>;

/// Entries kept before the oldest finished ones are evicted.
const CAPACITY: usize = 64;
/// Files larger than this are refused (decompression bombs, huge photos).
const MAX_FILE_BYTES: u64 = 64 << 20;

enum Entry {
    Pending,
    Done(Ready, u64),
}

#[derive(Default)]
pub(crate) struct Shared {
    map: Mutex<HashMap<Key, Entry>>,
    subscribers: Mutex<Vec<Sender<()>>>,
    tick: std::sync::atomic::AtomicU64,
}

pub(crate) struct Inner {
    shared: Arc<Shared>,
    jobs: Mutex<Option<Sender<Key>>>,
}

impl Inner {
    pub(crate) fn new() -> Self {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = channel::<Key>();
        let rx = Arc::new(Mutex::new(rx));
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4);
        for i in 0..n {
            let rx = rx.clone();
            let shared = shared.clone();
            let _ = std::thread::Builder::new()
                .name(format!("tanki-image-{i}"))
                .spawn(move || loop {
                    let job = match rx.lock() {
                        Ok(r) => r.recv(),
                        Err(_) => return,
                    };
                    let Ok(key) = job else { return };
                    let result = decode(&key.0, key.1);
                    shared.finish(key, result);
                });
        }
        Inner {
            shared,
            jobs: Mutex::new(Some(tx)),
        }
    }

    pub(crate) fn request(&self, source: &ImageSource, max_px: (u32, u32)) {
        let key = (source.clone(), (max_px.0.max(1), max_px.1.max(1)));
        {
            let Ok(mut map) = self.shared.map.lock() else { return };
            if map.contains_key(&key) {
                return;
            }
            map.insert(key.clone(), Entry::Pending);
        }
        let sent = self
            .jobs
            .lock()
            .ok()
            .and_then(|j| j.as_ref().map(|tx| tx.send(key.clone()).is_ok()))
            .unwrap_or(false);
        if !sent {
            // No worker thread: decode here rather than never.
            let result = decode(&key.0, key.1);
            self.shared.finish(key, result);
        }
    }

    pub(crate) fn get(&self, source: &ImageSource, max_px: (u32, u32)) -> Option<Ready> {
        let key = (source.clone(), (max_px.0.max(1), max_px.1.max(1)));
        let map = self.shared.map.lock().ok()?;
        match map.get(&key)? {
            Entry::Pending => None,
            Entry::Done(r, _) => Some(r.clone()),
        }
    }

    pub(crate) fn subscribe(&self) -> Receiver<()> {
        let (tx, rx) = channel();
        if let Ok(mut s) = self.shared.subscribers.lock() {
            s.push(tx);
        }
        rx
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Closing the job channel stops the workers.
        if let Ok(mut j) = self.jobs.lock() {
            j.take();
        }
    }
}

impl Shared {
    fn finish(&self, key: Key, result: Ready) {
        let tick = self.tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut map) = self.map.lock() {
            map.insert(key, Entry::Done(result, tick));
            let done = map.values().filter(|e| matches!(e, Entry::Done(..))).count();
            if done > CAPACITY {
                let oldest = map
                    .iter()
                    .filter_map(|(k, e)| match e {
                        Entry::Done(_, t) => Some((*t, k.clone())),
                        Entry::Pending => None,
                    })
                    .min_by_key(|(t, _)| *t)
                    .map(|(_, k)| k);
                if let Some(k) = oldest {
                    map.remove(&k);
                }
            }
        }
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.retain(|tx| tx.send(()).is_ok());
        }
    }
}

fn looks_like_svg(data: &[u8]) -> bool {
    let head = &data[..data.len().min(512)];
    let s = String::from_utf8_lossy(head);
    let t = s.trim_start_matches('\u{feff}').trim_start();
    t.starts_with("<svg") || (t.starts_with("<?xml") || t.starts_with("<!--") || t.starts_with("<!DOCTYPE")) && s.contains("<svg")
}

/// Decodes `source` and scales it to fit `max_px`, keeping the aspect ratio.
pub(crate) fn decode(source: &ImageSource, max_px: (u32, u32)) -> Ready {
    match source {
        ImageSource::Svg(s) => render_svg(s.as_bytes(), max_px),
        ImageSource::File(path) => {
            let meta = std::fs::metadata(path).map_err(|_| ImageError::NotFound(path.clone()))?;
            if !meta.is_file() {
                return Err(ImageError::NotFound(path.clone()));
            }
            if meta.len() > MAX_FILE_BYTES {
                return Err(ImageError::Unsupported(format!("file too large: {} bytes", meta.len())));
            }
            let data = std::fs::read(path).map_err(|e| ImageError::Decode(e.to_string()))?;
            if crate::media::is_svg_path(path) || looks_like_svg(&data) {
                return render_svg(&data, max_px);
            }
            decode_raster(&data, path, max_px)
        }
    }
}

fn decode_raster(data: &[u8], path: &Path, max_px: (u32, u32)) -> Ready {
    let format = image::guess_format(data).map_err(|_| {
        ImageError::Unsupported(path.extension().and_then(|e| e.to_str()).unwrap_or("unknown").to_string())
    })?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(data), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| match e {
        image::ImageError::Unsupported(u) => ImageError::Unsupported(u.to_string()),
        other => ImageError::Decode(other.to_string()),
    })?;
    let (w, h) = (img.width(), img.height());
    let (mw, mh) = (max_px.0.max(1), max_px.1.max(1));
    let img = if w == 0 || h == 0 || (w <= mw && h <= mh && (w == mw || h == mh)) {
        img
    } else {
        img.resize(mw, mh, image::imageops::FilterType::Triangle)
    };
    Ok(Arc::new(img))
}

fn fontdb() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

fn render_svg(data: &[u8], max_px: (u32, u32)) -> Ready {
    use resvg::usvg;
    let mut opt = usvg::Options {
        fontdb: fontdb(),
        ..usvg::Options::default()
    };
    // Never let an SVG read files by path; data: URLs only.
    opt.image_href_resolver.resolve_string = Box::new(|_, _| None);
    let tree = usvg::Tree::from_data(data, &opt).map_err(|e| ImageError::Decode(e.to_string()))?;
    let size = tree.size();
    let (mw, mh) = (max_px.0.max(1) as f32, max_px.1.max(1) as f32);
    let scale = (mw / size.width()).min(mh / size.height());
    if !scale.is_finite() || scale <= 0.0 {
        return Err(ImageError::Decode("empty SVG".into()));
    }
    let pw = ((size.width() * scale).round() as u32).clamp(1, 8192);
    let ph = ((size.height() * scale).round() as u32).clamp(1, 8192);
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(pw, ph).ok_or_else(|| ImageError::Decode("SVG too large".into()))?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia stores premultiplied RGBA; convert to straight alpha.
    let mut rgba = Vec::with_capacity((pw * ph * 4) as usize);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    let img = image::RgbaImage::from_raw(pw, ph, rgba).ok_or_else(|| ImageError::Decode("bad pixmap".into()))?;
    Ok(Arc::new(DynamicImage::ImageRgba8(img)))
}
