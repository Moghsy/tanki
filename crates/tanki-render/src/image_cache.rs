//! Off-thread image decoding and scaling with a small cache.

use crate::{ImageError, ImageSource};
use image::DynamicImage;
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

struct Animation {
    frames: Vec<Arc<DynamicImage>>,
    ends: Vec<Duration>,
    loops: Option<u32>,
}

impl Animation {
    fn index(&self, elapsed: Duration) -> usize {
        let total = self
            .ends
            .last()
            .copied()
            .unwrap_or(Duration::from_millis(1));
        if self
            .loops
            .is_some_and(|loops| elapsed.as_millis() >= total.as_millis() * u128::from(loops))
        {
            return self.frames.len().saturating_sub(1);
        }
        let at = elapsed.as_millis() % total.as_millis().max(1);
        self.ends
            .iter()
            .position(|end| at < end.as_millis())
            .unwrap_or(0)
    }
}

pub(crate) type Key = (ImageSource, (u32, u32), Option<FileStamp>);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn key(source: &ImageSource, max_px: (u32, u32)) -> Key {
    let path = match source {
        ImageSource::File(path)
        | ImageSource::InvertedFile(path)
        | ImageSource::Occlusion { path, .. } => Some(path),
        _ => None,
    };
    let stamp = path
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| FileStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        });
    (source.clone(), (max_px.0.max(1), max_px.1.max(1)), stamp)
}
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
    animations: Mutex<HashMap<Key, Animation>>,
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
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(1, 4);
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
                    let (result, animation) = decode_animation(&key.0, key.1);
                    if let Some(animation) = animation {
                        if let Ok(mut animations) = shared.animations.lock() {
                            animations.insert(key.clone(), animation);
                        }
                    }
                    shared.finish(key, result);
                });
        }
        Inner {
            shared,
            jobs: Mutex::new(Some(tx)),
        }
    }

    pub(crate) fn request(&self, source: &ImageSource, max_px: (u32, u32)) {
        let key = key(source, max_px);
        {
            let Ok(mut map) = self.shared.map.lock() else {
                return;
            };
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
            // The UI thread must never decode, even if workers are unavailable.
            self.shared.finish(
                key,
                Err(ImageError::Decode("image worker unavailable".into())),
            );
        }
    }

    pub(crate) fn get(&self, source: &ImageSource, max_px: (u32, u32)) -> Option<Ready> {
        let key = key(source, max_px);
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

    pub(crate) fn get_at(
        &self,
        source: &ImageSource,
        max_px: (u32, u32),
        elapsed: Duration,
    ) -> Option<Ready> {
        let key = key(source, max_px);
        if let Ok(animations) = self.shared.animations.lock() {
            if let Some(animation) = animations.get(&key) {
                return Some(Ok(animation.frames[animation.index(elapsed)].clone()));
            }
        }
        self.get(source, max_px)
    }

    pub(crate) fn frame_index_at(
        &self,
        source: &ImageSource,
        max_px: (u32, u32),
        elapsed: Duration,
    ) -> usize {
        let key = key(source, max_px);
        self.shared
            .animations
            .lock()
            .ok()
            .and_then(|animations| animations.get(&key).map(|a| a.index(elapsed)))
            .unwrap_or(0)
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
            let done = map
                .values()
                .filter(|e| matches!(e, Entry::Done(..)))
                .count();
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
                    if let Ok(mut animations) = self.animations.lock() {
                        animations.remove(&k);
                    }
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
    t.starts_with("<svg")
        || (t.starts_with("<?xml") || t.starts_with("<!--") || t.starts_with("<!DOCTYPE"))
            && s.contains("<svg")
}

/// Decodes `source` and scales it to fit `max_px`, keeping the aspect ratio.
pub(crate) fn decode(source: &ImageSource, max_px: (u32, u32)) -> Ready {
    match source {
        ImageSource::Bitmap {
            width,
            height,
            rgba,
            ..
        } => {
            let image = image::RgbaImage::from_raw(*width, *height, rgba.as_ref().clone())
                .ok_or_else(|| ImageError::Decode("invalid bitmap dimensions".into()))?;
            Ok(Arc::new(DynamicImage::ImageRgba8(image).resize(
                max_px.0.max(1),
                max_px.1.max(1),
                image::imageops::FilterType::Triangle,
            )))
        }
        ImageSource::InvertedFile(path) => {
            let base = decode(&ImageSource::File(path.clone()), max_px)?;
            let mut image = base.to_rgba8();
            for pixel in image.pixels_mut() {
                for channel in &mut pixel.0[..3] {
                    *channel = 255 - *channel;
                }
            }
            Ok(Arc::new(DynamicImage::ImageRgba8(image)))
        }
        ImageSource::Occlusion { path, overlay } => {
            let base = decode(&ImageSource::File(path.clone()), max_px)?;
            let masks = render_svg(overlay.as_bytes(), (base.width(), base.height()))?;
            let mut image = base.to_rgba8();
            image::imageops::overlay(&mut image, &masks.to_rgba8(), 0, 0);
            Ok(Arc::new(DynamicImage::ImageRgba8(image)))
        }
        ImageSource::Svg(s) => render_svg(s.as_bytes(), max_px),
        ImageSource::File(path) => {
            let meta = std::fs::metadata(path).map_err(|_| ImageError::NotFound(path.clone()))?;
            if !meta.is_file() {
                return Err(ImageError::NotFound(path.clone()));
            }
            if meta.len() > MAX_FILE_BYTES {
                return Err(ImageError::Unsupported(format!(
                    "file too large: {} bytes",
                    meta.len()
                )));
            }
            let data = std::fs::read(path).map_err(|e| ImageError::Decode(e.to_string()))?;
            if crate::media::is_svg_path(path) || looks_like_svg(&data) {
                return render_svg(&data, max_px);
            }
            decode_raster(&data, path, max_px)
        }
    }
}

/// GIF frame disposal is handled by image's compositing decoder. Bound both
/// decoded memory and frame count so a small compressed file cannot exhaust RAM.
fn decode_animation(source: &ImageSource, max_px: (u32, u32)) -> (Ready, Option<Animation>) {
    use image::{AnimationDecoder, ImageDecoder};
    let ImageSource::File(path) = source else {
        return (decode(source, max_px), None);
    };
    let is_gif = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"));
    if !is_gif {
        return (decode(source, max_px), None);
    }
    let run = || -> Result<Animation, ImageError> {
        let meta = std::fs::metadata(path).map_err(|_| ImageError::NotFound(path.clone()))?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(ImageError::Unsupported("GIF file too large".into()));
        }
        let file = std::fs::File::open(path).map_err(|e| ImageError::Decode(e.to_string()))?;
        let mut decoder = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file))
            .map_err(|e| ImageError::Decode(e.to_string()))?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(128 << 20);
        decoder
            .set_limits(limits)
            .map_err(|e| ImageError::Decode(e.to_string()))?;
        let loops = match decoder.loop_count() {
            image::metadata::LoopCount::Infinite => None,
            image::metadata::LoopCount::Finite(n) => Some(n.get()),
        };
        let mut frames = Vec::new();
        let mut ends = Vec::new();
        let mut total = Duration::ZERO;
        let mut memory = 0u64;
        for frame in decoder.into_frames().take(256) {
            let frame = frame.map_err(|e| ImageError::Decode(e.to_string()))?;
            let (numer, denom) = frame.delay().numer_denom_ms();
            total += Duration::from_micros(
                (u64::from(numer) * 1000 / u64::from(denom.max(1))).max(10_000),
            );
            let image = DynamicImage::ImageRgba8(frame.into_buffer()).resize(
                max_px.0.max(1),
                max_px.1.max(1),
                image::imageops::FilterType::Triangle,
            );
            memory += u64::from(image.width()) * u64::from(image.height()) * 4;
            if memory > 128 << 20 {
                return Err(ImageError::Unsupported(
                    "decoded GIF exceeds memory limit".into(),
                ));
            }
            frames.push(Arc::new(image));
            ends.push(total);
        }
        if frames.is_empty() {
            return Err(ImageError::Decode("empty GIF".into()));
        }
        Ok(Animation {
            frames,
            ends,
            loops,
        })
    };
    match run() {
        Ok(animation) => (Ok(animation.frames[0].clone()), Some(animation)),
        Err(error) => (Err(error), None),
    }
}

fn decode_raster(data: &[u8], path: &Path, max_px: (u32, u32)) -> Ready {
    let format = image::guess_format(data).map_err(|_| {
        ImageError::Unsupported(
            path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_string(),
        )
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

pub(crate) fn render_svg(data: &[u8], max_px: (u32, u32)) -> Ready {
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
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph)
        .ok_or_else(|| ImageError::Decode("SVG too large".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied RGBA; convert to straight alpha.
    let mut rgba = Vec::with_capacity((pw * ph * 4) as usize);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    let img = image::RgbaImage::from_raw(pw, ph, rgba)
        .ok_or_else(|| ImageError::Decode("bad pixmap".into()))?;
    Ok(Arc::new(DynamicImage::ImageRgba8(img)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animated_gif_honours_delays_looping_and_disposal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("animated.gif");
        let mut encoder =
            image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        for (color, ms) in [([255, 0, 0, 255], 30), ([0, 0, 255, 255], 70)] {
            encoder
                .encode_frame(image::Frame::from_parts(
                    image::RgbaImage::from_pixel(4, 4, image::Rgba(color)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(ms, 1),
                ))
                .unwrap();
        }
        drop(encoder);
        let source = ImageSource::File(path);
        let (ready, animation) = decode_animation(&source, (4, 4));
        assert_eq!(
            ready.unwrap().to_rgba8().get_pixel(0, 0).0,
            [255, 0, 0, 255]
        );
        let animation = animation.unwrap();
        for (ms, index) in [(0, 0), (29, 0), (30, 1), (99, 1), (100, 0), (130, 1)] {
            assert_eq!(animation.index(Duration::from_millis(ms)), index);
        }
        assert_eq!(
            animation.frames[1].to_rgba8().get_pixel(0, 0).0,
            [0, 0, 255, 255]
        );
        let finite = Animation {
            loops: Some(1),
            ..animation
        };
        assert_eq!(finite.index(Duration::from_secs(5)), 1);
    }

    #[test]
    fn occlusion_bitmap_pixels_and_latex_inversion_preserve_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagram.png");
        image::RgbaImage::from_pixel(100, 100, image::Rgba([0, 0, 255, 255]))
            .save(&path)
            .unwrap();
        let overlay="<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100'><rect x='10' y='10' width='20' height='20' fill='#ff8e8e'/></svg>";
        let result = decode(
            &ImageSource::Occlusion {
                path: path.clone(),
                overlay: overlay.into(),
            },
            (100, 100),
        )
        .unwrap()
        .to_rgba8();
        assert_eq!(result.get_pixel(15, 15).0, [255, 142, 142, 255]);
        assert_eq!(result.get_pixel(50, 50).0, [0, 0, 255, 255]);
        image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 128]))
            .save(&path)
            .unwrap();
        let result = decode(&ImageSource::InvertedFile(path), (1, 1))
            .unwrap()
            .to_rgba8();
        assert_eq!(result.get_pixel(0, 0).0, [245, 235, 225, 128]);
    }

    #[test]
    fn raster_formats_decode_and_scale() {
        let dir = tempfile::tempdir().unwrap();
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            20,
            10,
            image::Rgb([200, 20, 10]),
        ));
        for extension in ["png", "jpg", "gif", "webp"] {
            let path = dir.path().join(format!("fixture.{extension}"));
            img.save(&path).unwrap();
            let decoded = decode(&ImageSource::File(path), (10, 10)).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (10, 5), "{extension}");
        }
    }

    #[test]
    fn gif_uses_first_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("animated.gif");
        {
            let mut writer =
                image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
            for rgba in [[255, 0, 0, 255], [0, 0, 255, 255]] {
                writer
                    .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                        2,
                        2,
                        image::Rgba(rgba),
                    )))
                    .unwrap();
            }
        }
        let decoded = decode(&ImageSource::File(path), (2, 2)).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
    }

    #[test]
    fn svg_scales_and_never_loads_external_images() {
        let dir = tempfile::tempdir().unwrap();
        let external = dir.path().join("outside.png");
        image::RgbaImage::from_pixel(20, 10, image::Rgba([255, 0, 0, 255]))
            .save(&external)
            .unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="10"><rect width="20" height="10" fill="blue"/><image xlink:href="{}" width="20" height="10"/></svg>"#,
            external.display()
        );
        let decoded = decode(&ImageSource::Svg(svg), (10, 10)).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (10, 5));
        assert_eq!(decoded.to_rgba8().get_pixel(5, 2).0, [0, 0, 255, 255]);
    }

    #[test]
    fn cache_notifies_subscribers_and_deduplicates_requests() {
        let cache = crate::ImageCache::new();
        let first = cache.subscribe();
        let second = cache.subscribe();
        let source = ImageSource::Svg(r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#.into());
        cache.request(&source, (10, 10));
        first
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        second
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let image = cache.get(&source, (10, 10)).unwrap().unwrap();
        cache.request(&source, (10, 10));
        let cached = cache.get(&source, (10, 10)).unwrap().unwrap();
        assert!(Arc::ptr_eq(&image, &cached));
        assert!(first.try_recv().is_err());
        assert_eq!((image.width(), image.height()), (10, 5));
    }

    #[test]
    fn missing_and_invalid_images_are_cached_errors() {
        let dir = tempfile::tempdir().unwrap();
        let source = ImageSource::File(dir.path().join("missing.png"));
        let cache = crate::ImageCache::new();
        let events = cache.subscribe();
        cache.request(&source, (20, 20));
        events
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(matches!(
            cache.get(&source, (20, 20)),
            Some(Err(ImageError::NotFound(_)))
        ));
        assert!(matches!(
            decode(&ImageSource::Svg("<broken>".into()), (20, 20)),
            Err(ImageError::Decode(_))
        ));
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(matches!(
            decode(&ImageSource::File(bad), (20, 20)),
            Err(ImageError::Unsupported(_))
        ));
    }

    #[test]
    fn missing_images_refresh_when_generated_and_changed_media_is_redecoded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latex-generated.png");
        let source = ImageSource::File(path.clone());
        let cache = crate::ImageCache::new();
        let events = cache.subscribe();
        cache.request(&source, (10, 10));
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            cache.get(&source, (10, 10)),
            Some(Err(ImageError::NotFound(_)))
        ));
        image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        assert!(
            cache.get(&source, (10, 10)).is_none(),
            "missing-file result must not hide a generated file"
        );
        cache.request(&source, (10, 10));
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            cache
                .get(&source, (10, 10))
                .unwrap()
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [255, 0, 0, 255]
        );
        let inverted = ImageSource::InvertedFile(path.clone());
        let composite = ImageSource::Occlusion {
            path: path.clone(),
            overlay: "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'/>".into(),
        };
        let before = [
            key(&source, (10, 10)),
            key(&inverted, (10, 10)),
            key(&composite, (10, 10)),
        ];
        // A different size guarantees a new stamp even on coarse-mtime filesystems.
        image::RgbaImage::from_pixel(20, 20, image::Rgba([0, 0, 255, 255]))
            .save(&path)
            .unwrap();
        for (i, image) in [&source, &inverted, &composite].iter().enumerate() {
            assert_ne!(before[i], key(image, (10, 10)));
        }
        assert!(cache.get_at(&source, (10, 10), Duration::ZERO).is_none());
        cache.request(&source, (10, 10));
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            cache
                .get_at(&source, (10, 10), Duration::ZERO)
                .unwrap()
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [0, 0, 255, 255]
        );
    }

    #[test]
    fn completed_cache_is_bounded() {
        let shared = Shared::default();
        for i in 0..CAPACITY + 4 {
            shared.finish(
                (ImageSource::Svg(i.to_string()), (1, 1), None),
                Err(ImageError::Unsupported("fixture".into())),
            );
        }
        let map = shared.map.lock().unwrap();
        assert_eq!(map.len(), CAPACITY);
        assert!(!map.contains_key(&(ImageSource::Svg("0".into()), (1, 1), None)));
    }
}
