//! Blitz is confined to this worker: Stylo, font discovery, resource reads,
//! layout and painting never execute on the terminal thread.
use super::*;
use crate::{ImageBlock, ImageSource, Line};
use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::{
    net::{NetHandler, NetProvider, Request, Url},
    shell::{ColorScheme, Viewport},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock,
    },
};

const MAX_CACHE: usize = 8;
const MAX_CACHE_BYTES: usize = 256 << 20;
const MAX_HEIGHT: u32 = 8192;
type Cached = Result<RenderedCard, String>;

#[derive(Default)]
struct State {
    completed: BTreeMap<u64, (Cached, u64)>,
    pending: BTreeSet<u64>,
    tick: u64,
}

impl State {
    fn get(&mut self, key: u64) -> Option<Cached> {
        self.tick += 1;
        let entry = self.completed.get_mut(&key)?;
        entry.1 = self.tick;
        Some(entry.0.clone())
    }
    fn insert(&mut self, key: u64, rendered: Cached) {
        self.tick += 1;
        self.completed.insert(key, (rendered, self.tick));
        loop {
            let bytes: usize = self
                .completed
                .values()
                .filter_map(|(card, _)| card.as_ref().ok())
                .flat_map(|card| &card.images)
                .map(|image| match &image.source {
                    ImageSource::Bitmap { rgba, .. } => rgba.len(),
                    _ => 0,
                })
                .sum();
            if self.completed.len() <= MAX_CACHE && bytes <= MAX_CACHE_BYTES {
                break;
            }
            let Some(oldest) = self
                .completed
                .iter()
                .min_by_key(|(_, (_, tick))| *tick)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.completed.remove(&oldest);
        }
    }
}
struct Worker {
    tx: SyncSender<(u64, CardSide, RenderOptions)>,
    state: Arc<Mutex<State>>,
}
static WORKER: OnceLock<Worker> = OnceLock::new();
static REVISION: AtomicU64 = AtomicU64::new(0);

fn worker() -> &'static Worker {
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<(u64, CardSide, RenderOptions)>(4);
        let state = Arc::new(Mutex::new(State::default()));
        let output = state.clone();
        std::thread::Builder::new()
            .name("tanki-bitmap".into())
            .spawn(move || {
                for (key, side, opts) in rx {
                    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        rasterize(&side, &opts)
                    }))
                    .unwrap_or_else(|_| Err("Blitz could not render this card".into()));
                    let mut state = output.lock().unwrap();
                    state.pending.remove(&key);
                    state.insert(key, rendered);
                    REVISION.fetch_add(1, Ordering::Relaxed);
                }
            })
            .expect("start bitmap worker");
        Worker { tx, state }
    })
}

pub(super) fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

pub(super) fn request(side: &CardSide, opts: &RenderOptions) -> Option<Cached> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        &side.html,
        &side.css,
        &side.body_classes,
        &side.media_dir,
        format!("{opts:?}"),
    )
        .hash(&mut hasher);
    let key = hasher.finish();
    let worker = worker();
    let mut state = worker.state.lock().unwrap();
    if let Some(rendered) = state.get(key) {
        return Some(rendered);
    }
    if !state.pending.contains(&key)
        && worker
            .tx
            .try_send((key, side.clone(), opts.clone()))
            .is_ok()
    {
        state.pending.insert(key);
    }
    None
}

/// Only local resources under collection.media are visible to Blitz. The
/// URL host/scheme and canonical path check also apply to nested CSS imports,
/// fonts and symlinks. Failed requests complete with empty bytes.
struct LocalMedia {
    root: PathBuf,
    generated: BTreeMap<String, Vec<u8>>,
}
impl NetProvider for LocalMedia {
    fn fetch(&self, _: usize, request: Request, handler: Box<dyn NetHandler>) {
        let bytes = if let Some(bytes) = self.generated.get(request.url.as_str()) {
            bytes.clone()
        } else if request.url.scheme() == "file" {
            request
                .url
                .to_file_path()
                .ok()
                .and_then(|p| p.canonicalize().ok())
                .filter(|p| p.starts_with(&self.root))
                .filter(|p| p.metadata().is_ok_and(|m| m.len() <= 32 * 1024 * 1024))
                .and_then(|p| {
                    let bytes = std::fs::read(&p).ok()?;
                    if p.extension().is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz")
                    }) || String::from_utf8_lossy(&bytes[..bytes.len().min(512)])
                        .contains("<svg")
                    {
                        safe_svg_png(&bytes).ok()
                    } else {
                        Some(bytes)
                    }
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        handler.bytes(request.url.to_string(), bytes.into());
    }
}

// Blitz's native SVG loader uses usvg's default file resolver, bypassing the
// NetProvider. Keep that feature disabled and rasterize every SVG ourselves
// with the existing resolver that refuses file paths (data URLs remain safe).
fn safe_svg_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let natural = crate::media::svg_dimensions(bytes).unwrap_or((1024, 1024));
    let image =
        crate::image_cache::render_svg(bytes, (natural.0.clamp(1, 4096), natural.1.clamp(1, 4096)))
            .map_err(|e| format!("{e:?}"))?;
    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png.into_inner())
}

fn inline_svg_resources(html: &str) -> Result<(String, BTreeMap<String, Vec<u8>>), String> {
    let dom = Html::parse_fragment(html);
    let mut html = dom.root_element().inner_html();
    let selector = Selector::parse("svg").unwrap();
    let mut generated = BTreeMap::new();
    let mut generated_bytes = 0usize;
    for (i, el) in dom
        .select(&selector)
        .filter(|el| {
            !el.ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .any(|parent| parent.value().name() == "svg")
        })
        .enumerate()
    {
        if i >= 256 {
            return Err("too many inline SVG blocks".into());
        }
        let source = el.html();
        let svg = if !source.contains("xmlns=") {
            source.replacen("<svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"", 1)
        } else {
            source.clone()
        };
        let bytes = safe_svg_png(svg.as_bytes())?;
        generated_bytes += bytes.len();
        if generated_bytes > 64 << 20 {
            return Err("inline SVG resources exceed memory limit".into());
        }
        let url = format!("tanki-generated:///svg-{i}.png");
        let mut replacement = format!("<img src=\"{url}\"");
        for (key, value) in el.value().attrs().filter(|(key, _)| {
            !matches!(
                *key,
                "xmlns" | "xmlns:xlink" | "src" | "viewBox" | "viewbox"
            )
        }) {
            replacement.push_str(&format!(" {key}=\"{}\"", attr(value)));
        }
        replacement.push('>');
        html = html.replace(&source, &replacement);
        generated.insert(url, bytes);
    }
    Ok((html, generated))
}

fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(super) fn rasterize(side: &CardSide, opts: &RenderOptions) -> Cached {
    let (cw, ch) = opts
        .cell_px
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((10, 20));
    // Whole-cell dimensions keep image decoding and vertical slicing at the
    // exact layout scale, including the last partially occupied content row.
    let (cw, ch) = (cw.min(4096), ch.min(MAX_HEIGHT as u16));
    let columns = u32::from(opts.width.max(1)).min((4096 / u32::from(cw)).max(1));
    let width = columns * u32::from(cw);
    let viewport_height = (u32::from(opts.viewport_rows.max(1)) * u32::from(ch)).min(MAX_HEIGHT);
    let root = side
        .media_dir
        .canonicalize()
        .map_err(|e| format!("media folder: {e}"))?;
    let base_url = Url::from_directory_path(&root)
        .map_err(|_| "invalid media folder")?
        .to_string();
    let bg = opts.bg.unwrap_or(if opts.night_mode {
        crate::Rgb(24, 24, 24)
    } else {
        crate::Rgb(255, 255, 255)
    });
    let fg = opts.fg.unwrap_or(if opts.night_mode {
        crate::Rgb(221, 221, 221)
    } else {
        crate::Rgb(0, 0, 0)
    });
    let night = if opts.night_mode {
        "nightMode night_mode"
    } else {
        ""
    };
    let colors = format!(
        "background:rgb({},{},{});color:rgb({},{},{})",
        bg.0, bg.1, bg.2, fg.0, fg.1, fg.2
    );
    let night_css = if opts.night_mode {
        format!("body.nightMode {{ {colors}; }}")
    } else {
        String::new()
    };
    let math_html = crate::math::prepare_html(&side.html, ch, fg);
    let (card_html, generated) = inline_svg_resources(&math_html)?;
    let html = format!("<!doctype html><html><head><style>html,body{{margin:0;{colors}}} body{{padding:8px;box-sizing:border-box}} img{{max-width:100%}} {} {night_css}</style></head><body class=\"{} {night}\"><div id=qa>{card_html}</div></body></html>", side.css, attr(&side.body_classes));
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(
                width,
                viewport_height,
                1.0,
                if opts.night_mode {
                    ColorScheme::Dark
                } else {
                    ColorScheme::Light
                },
            )),
            base_url: Some(base_url),
            net_provider: Some(Arc::new(LocalMedia { root, generated })),
            ..Default::default()
        },
    );
    // LocalMedia completes synchronously. Repeated passes deliver resources
    // discovered while resolving CSS (including @imports and web fonts).
    for _ in 0..8 {
        doc.handle_messages();
        doc.resolve(0.0);
    }
    let layout = doc.root_element().final_layout();
    let content_height = layout
        .size
        .height
        .max(layout.scrollable_overflow_rect.bottom)
        .ceil()
        .max(1.0) as u32;
    let max_height = (MAX_HEIGHT / u32::from(ch)).max(1) * u32::from(ch);
    let height = content_height
        .min(max_height)
        .max(1)
        .div_ceil(u32::from(ch))
        * u32::from(ch);
    let answer_row = doc
        .get_element_by_id("answer")
        .and_then(|id| doc.get_client_bounding_rect(id))
        .map(|rect| (rect.y.max(0.0) / f64::from(ch)) as usize);
    let mut renderer = VelloCpuImageRenderer::new(width, height);
    let mut rgba = Vec::new();
    renderer.render_to_vec(
        |scene| blitz_paint::paint_scene(scene, &mut doc, 1.0, width, height, 0, 0),
        &mut rgba,
    );
    let rows = height.div_ceil(u32::from(ch)).min(u32::from(u16::MAX)) as u16;
    let mut notices = vec!["Bitmap tier (Blitz, no JavaScript)".into()];
    if content_height > MAX_HEIGHT {
        notices
            .push("Bitmap height limited to 8192 pixels; Open in Browser for the full card".into());
    }
    if classify(&side.html, "", &side.css).needs_browser {
        notices.push("Card needs a browser for JavaScript or embedded content".into());
    }
    Ok(RenderedCard {
        lines: vec![Line::default(); rows as usize],
        images: vec![ImageBlock {
            source: ImageSource::Bitmap {
                id: NEXT_BITMAP_ID.fetch_add(1, Ordering::Relaxed),
                width,
                height,
                rgba: Arc::new(rgba),
            },
            row: 0,
            col: 0,
            width: width.div_ceil(u32::from(cw)) as u16,
            height: rows,
            alt: "Full card".into(),
        }],
        answer_row,
        notices,
        ..Default::default()
    })
}

static NEXT_BITMAP_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_images_and_stylesheets_are_loaded_in_the_bitmap() {
        let tmp = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(20, 20, image::Rgba([255, 0, 0, 255]))
            .save(tmp.path().join("picture.png"))
            .unwrap();
        std::fs::write(
            tmp.path().join("local.css"),
            ".local {background:#00ff00;height:30px}",
        )
        .unwrap();
        let side=CardSide {html:"<link rel=stylesheet href=local.css><div class=local></div><img src=picture.png width=20 height=20>".into(),media_dir:tmp.path().into(),..Default::default()};
        let opts = RenderOptions {
            width: 20,
            viewport_rows: 10,
            cell_px: Some((10, 20)),
            images: true,
            ..Default::default()
        };
        let card = rasterize(&side, &opts).unwrap();
        let ImageSource::Bitmap { height, rgba, .. } = &card.images[0].source else {
            panic!()
        };
        assert_eq!(
            height % 20,
            0,
            "bitmap canvas must end on a whole terminal row"
        );
        assert!(
            rgba.chunks_exact(4)
                .filter(|p| *p == [0, 255, 0, 255])
                .count()
                > 100,
            "external CSS should paint green"
        );
        assert!(
            rgba.chunks_exact(4)
                .filter(|p| *p == [255, 0, 0, 255])
                .count()
                >= 400,
            "local image should paint red"
        );
    }

    #[test]
    fn inline_svg_and_svg_resources_cannot_read_external_files() {
        let media = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.png");
        image::RgbaImage::from_pixel(20, 20, image::Rgba([255, 0, 0, 255]))
            .save(&secret)
            .unwrap();
        let svg=format!("<svg xmlns='http://www.w3.org/2000/svg' width='20' height='20'><rect width='20' height='20' fill='blue'/><image href='{}' width='20' height='20'/></svg>",secret.display());
        std::fs::write(media.path().join("picture.svg"), &svg).unwrap();
        let provider = LocalMedia {
            root: media.path().canonicalize().unwrap(),
            generated: BTreeMap::new(),
        };
        let png = fetched(
            &provider,
            Url::from_file_path(media.path().join("picture.svg")).unwrap(),
        );
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(5, 5).0, [0, 0, 255, 255]);
        let (html, generated) = inline_svg_resources(&svg).unwrap();
        assert!(!html.contains("<svg"));
        assert!(html.contains("tanki-generated:///svg-0.png"));
        let inline = image::load_from_memory(generated.values().next().unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(inline.get_pixel(5, 5).0, [0, 0, 255, 255]);
        let no_namespace =
            "<svg width='20' height='20'><rect width='20' height='20' fill='green'/></svg>";
        assert!(inline_svg_resources(no_namespace).is_ok());
    }

    fn fetched(provider: &LocalMedia, url: Url) -> Vec<u8> {
        struct Capture(std::sync::mpsc::Sender<Vec<u8>>);
        impl NetHandler for Capture {
            fn bytes(self: Box<Self>, _: String, bytes: blitz_traits::net::Bytes) {
                self.0.send(bytes.to_vec()).unwrap();
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        provider.fetch(0, Request::get(url), Box::new(Capture(tx)));
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap()
    }

    #[test]
    fn resource_provider_denies_network_outside_paths_and_symlink_escapes() {
        let media = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = media.path().join("valid.css");
        let secret = outside.path().join("secret.css");
        std::fs::write(&inside, "inside").unwrap();
        std::fs::write(&secret, "outside").unwrap();
        let provider = LocalMedia {
            root: media.path().canonicalize().unwrap(),
            generated: BTreeMap::new(),
        };
        assert_eq!(
            fetched(&provider, Url::from_file_path(&inside).unwrap()),
            b"inside"
        );
        assert!(fetched(&provider, Url::from_file_path(&secret).unwrap()).is_empty());
        assert!(fetched(
            &provider,
            Url::parse("https://example.invalid/never-connect").unwrap()
        )
        .is_empty());
        assert!(fetched(
            &provider,
            Url::parse("http://127.0.0.1:9/never-connect").unwrap()
        )
        .is_empty());
        assert!(fetched(&provider, Url::parse("file://remote-host/secret").unwrap()).is_empty());
        #[cfg(unix)]
        {
            let link = media.path().join("escape.css");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            assert!(fetched(&provider, Url::from_file_path(link).unwrap()).is_empty());
        }
    }

    #[test]
    fn completed_cache_keeps_recent_entries_and_shares_pixels() {
        let mut state = State::default();
        let image = ImageSource::Bitmap {
            id: 1,
            width: 1,
            height: 1,
            rgba: Arc::new(vec![0, 0, 0, 255]),
        };
        let card = RenderedCard {
            images: vec![ImageBlock {
                source: image,
                row: 0,
                col: 0,
                width: 1,
                height: 1,
                alt: String::new(),
            }],
            ..Default::default()
        };
        state.insert(0, Ok(card));
        for key in 1..MAX_CACHE as u64 {
            state.insert(key, Err("fixture".into()));
        }
        let first = state.get(0).unwrap().unwrap();
        state.insert(MAX_CACHE as u64, Err("new".into()));
        assert!(state.completed.contains_key(&0));
        assert!(!state.completed.contains_key(&1));
        let cloned = state.get(0).unwrap().unwrap();
        let (ImageSource::Bitmap { rgba: a, .. }, ImageSource::Bitmap { rgba: b, .. }) =
            (&first.images[0].source, &cloned.images[0].source)
        else {
            panic!()
        };
        assert!(
            Arc::ptr_eq(a, b),
            "cached pixels must not copy on every draw"
        );
    }
    #[test]
    fn flex_layout_paints_coloured_columns_and_full_height() {
        let tmp = tempfile::tempdir().unwrap();
        let side = CardSide { html: "<div style='display:flex;height:100px'><div style='width:50%;background:red'></div><div style='width:50%;background:blue'></div></div><hr id=answer><div style='height:400px'></div>".into(), media_dir: tmp.path().into(), ..Default::default() };
        let card = rasterize(
            &side,
            &RenderOptions {
                width: 20,
                viewport_rows: 10,
                cell_px: Some((10, 20)),
                images: true,
                ..Default::default()
            },
        )
        .unwrap();
        let ImageSource::Bitmap {
            width,
            height,
            rgba,
            ..
        } = &card.images[0].source
        else {
            panic!()
        };
        assert!(*height > 400);
        let pixel = |x: usize, y: usize| &rgba[(y * *width as usize + x) * 4..][..4];
        assert_eq!(pixel(20, 20), &[255, 0, 0, 255]);
        assert_eq!(pixel(160, 20), &[0, 0, 255, 255]);
        assert!(card.answer_row.is_some_and(|r| r >= 5));
    }
    #[test]
    fn background_request_completes_and_is_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let side = CardSide {
            html: "worker test".into(),
            media_dir: tmp.path().into(),
            ..Default::default()
        };
        let opts = RenderOptions::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(result) = request(&side, &opts) {
                assert!(result.is_ok());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(request(&side, &opts).unwrap().is_ok());
    }
}
