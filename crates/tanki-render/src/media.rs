//! Media references: resolving `src` against the media folder (R3 §4.1),
//! reading image dimensions cheaply, and sizing images in cells (R3 §4.2).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// What an `src` attribute points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRef {
    /// A file inside the media folder (it may not exist).
    Local(PathBuf),
    /// `http(s)://…`; never fetched here.
    Remote(String),
    /// Unsafe or unsupported (absolute paths, `..`, `/`, `data:`, …).
    Rejected(String),
}

fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut changed = false;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                changed = true;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    if !changed {
        return None;
    }
    String::from_utf8(out).ok()
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !Path::new(name).is_absolute()
        // Windows drive letters such as `C:`.
        && !(name.len() >= 2 && name.as_bytes()[1] == b':' && name.as_bytes()[0].is_ascii_alphabetic())
}

/// Resolves an `src` (already entity-decoded by the HTML parser) against
/// `media_dir`, applying Anki's traversal rules: no absolute paths, no `/`
/// and no `..`. The raw name is tried first, then its percent-decoded form.
pub fn resolve(media_dir: &Path, src: &str) -> MediaRef {
    let src = src.trim();
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//") {
        return MediaRef::Remote(src.to_string());
    }
    if lower.starts_with("data:") || lower.starts_with("file:") || lower.starts_with("javascript:") {
        return MediaRef::Rejected(src.chars().take(40).collect());
    }
    let decoded = percent_decode(src);
    let raw_ok = safe_name(src);
    let dec_ok = decoded.as_deref().is_some_and(safe_name);
    if decoded.is_some() && !dec_ok {
        // The decoded form tries to escape (e.g. `%2e%2e%2f`): refuse outright.
        return MediaRef::Rejected(src.to_string());
    }
    if raw_ok {
        let p = media_dir.join(src);
        if p.exists() || !dec_ok {
            return MediaRef::Local(p);
        }
    }
    match decoded {
        Some(d) if dec_ok => MediaRef::Local(media_dir.join(d)),
        _ => MediaRef::Rejected(src.to_string()),
    }
}

/// The last path component of a reference, for labels.
pub fn display_name(src: &str) -> String {
    let s = src.split(['?', '#']).next().unwrap_or(src);
    let s = s.trim_end_matches('/');
    let name = s.rsplit('/').next().unwrap_or(s);
    percent_decode(name).unwrap_or_else(|| name.to_string())
}

pub fn is_svg_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

type DimKey = (PathBuf, u64, Option<std::time::SystemTime>);

fn dim_cache() -> &'static Mutex<HashMap<DimKey, Option<(u32, u32)>>> {
    static CACHE: OnceLock<Mutex<HashMap<DimKey, Option<(u32, u32)>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pixel size of an image file, read from its header (or, for SVG, its
/// root element). `Err(true)` means the file does not exist.
pub fn image_dimensions(path: &Path) -> Result<Option<(u32, u32)>, bool> {
    let meta = std::fs::metadata(path).map_err(|_| true)?;
    if !meta.is_file() {
        return Err(true);
    }
    let key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    if let Some(v) = dim_cache().lock().ok().and_then(|c| c.get(&key).copied()) {
        return Ok(v);
    }
    let dims = if is_svg_path(path) {
        std::fs::read(path).ok().and_then(|d| svg_dimensions(&d))
    } else {
        image::ImageReader::open(path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.into_dimensions().ok())
    };
    if let Ok(mut c) = dim_cache().lock() {
        if c.len() > 4096 {
            c.clear();
        }
        c.insert(key, dims);
    }
    Ok(dims)
}

/// Size of an SVG document in CSS pixels.
pub fn svg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(data, &opt).ok()?;
    let s = tree.size();
    Some((s.width().round().max(1.0) as u32, s.height().round().max(1.0) as u32))
}

/// Inputs to image sizing.
#[derive(Debug, Clone, Copy)]
pub struct SizeRequest {
    /// The image's own pixel size, when known.
    pub natural: Option<(u32, u32)>,
    /// CSS `width`/`height` (attributes or style), in px; percentages
    /// already resolved against the container.
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub max_width: Option<f32>,
    pub max_height: Option<f32>,
    pub cell_px: (u16, u16),
    /// Columns available (`max-width: 100%`).
    pub max_cols: usize,
    pub viewport_rows: u16,
}

/// Cells `(cols, rows)` for an image. Follows the browser: explicit sizes win,
/// a single explicit size keeps the aspect ratio, then `max-width: 100%` and
/// `max-height: 95vh` scale it down.
pub fn image_cells(r: &SizeRequest) -> (usize, usize) {
    let cw = r.cell_px.0.max(1) as f32;
    let ch = r.cell_px.1.max(1) as f32;
    let (nw, nh) = r
        .natural
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map(|(w, h)| (w as f32, h as f32))
        .unwrap_or((300.0, 150.0));
    let aspect = nh / nw;
    let (mut w, mut h) = match (r.width, r.height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w * aspect),
        (None, Some(h)) => (h / aspect, h),
        (None, None) => (nw, nh),
    };
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        w = nw;
        h = nh;
    }
    let max_w_px = (r.max_cols as f32 * cw).min(r.max_width.unwrap_or(f32::INFINITY));
    let vp_rows = (r.viewport_rows.max(1) as f32 * 0.95).floor().max(1.0);
    let max_h_px = (vp_rows * ch).min(r.max_height.unwrap_or(f32::INFINITY));
    if w > max_w_px && max_w_px > 0.0 {
        h *= max_w_px / w;
        w = max_w_px;
    }
    if h > max_h_px && max_h_px > 0.0 {
        w *= max_h_px / h;
        h = max_h_px;
    }
    let cols = ((w / cw).ceil() as usize).clamp(1, r.max_cols.max(1));
    let rows = ((h / ch).ceil() as usize).clamp(1, vp_rows as usize);
    (cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_rejected() {
        let dir = Path::new("/media");
        for bad in [
            "../secret.png",
            "/etc/passwd",
            "sub/x.png",
            "..",
            "a\\b.png",
            "%2e%2e%2fsecret.png",
            "..%2Fx.png",
            "C:x.png",
            "data:image/png;base64,AAAA",
            "file:///etc/passwd",
            "",
        ] {
            assert!(matches!(resolve(dir, bad), MediaRef::Rejected(_)), "{bad} must be rejected");
        }
    }

    #[test]
    fn local_and_remote() {
        let dir = Path::new("/media");
        assert_eq!(resolve(dir, "a b.jpg"), MediaRef::Local(dir.join("a b.jpg")));
        assert_eq!(resolve(dir, "a%20b.jpg"), MediaRef::Local(dir.join("a b.jpg")));
        assert_eq!(resolve(dir, "100%.png"), MediaRef::Local(dir.join("100%.png")));
        assert_eq!(resolve(dir, "日本.png"), MediaRef::Local(dir.join("日本.png")));
        assert!(matches!(resolve(dir, "https://x.org/a.png"), MediaRef::Remote(_)));
        assert_eq!(display_name("https://x.org/p/a%20b.png?x=1"), "a b.png");
    }

    #[test]
    fn raw_name_preferred_when_it_exists() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a%20b.png"), b"x").unwrap();
        assert_eq!(resolve(tmp.path(), "a%20b.png"), MediaRef::Local(tmp.path().join("a%20b.png")));
    }

    fn req(natural: Option<(u32, u32)>) -> SizeRequest {
        SizeRequest {
            natural,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
            cell_px: (10, 20),
            max_cols: 80,
            viewport_rows: 24,
        }
    }

    #[test]
    fn sizing() {
        // Natural size.
        assert_eq!(image_cells(&req(Some((100, 40)))), (10, 2));
        // Clamped to the width, keeping the aspect ratio.
        assert_eq!(image_cells(&req(Some((1600, 200)))), (80, 1));
        // Clamped to 95% of the viewport height (22 rows).
        assert_eq!(image_cells(&req(Some((100, 2000)))), (2, 22));
        // One explicit size keeps the aspect.
        let mut r = req(Some((200, 100)));
        r.width = Some(100.0);
        assert_eq!(image_cells(&r), (10, 3));
        // Both explicit.
        r.height = Some(200.0);
        assert_eq!(image_cells(&r), (10, 10));
        // Cell size matters.
        let mut r = req(Some((100, 100)));
        r.cell_px = (5, 10);
        assert_eq!(image_cells(&r), (20, 10));
        // Unknown size falls back to 300x150.
        assert_eq!(image_cells(&req(None)), (30, 8));
        // Tiny widths never produce zero cells.
        let mut r = req(Some((1, 1)));
        r.max_cols = 0;
        assert_eq!(image_cells(&r), (1, 1));
    }
}
