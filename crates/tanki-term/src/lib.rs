//! Terminal capability diagnostics. This crate never opens a collection.
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphicsMode {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Iterm,
    Blocks,
    None,
}
#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    pub graphics: GraphicsMode,
    pub show_image: bool,
}
#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub summary: String,
}
/// Fixed Phase 0 interface; implementations probe before starting an event reader.
pub trait TerminalDoctor {
    fn run(&mut self, options: &DoctorOptions) -> io::Result<DoctorReport>;
}

// ---------------------------------------------------------------------------
// Phase 1 interface (fixed in the "agree first" step; the terminal stream may
// add items, changing these needs the lead).

/// What the startup probe found (PLAN 5.2). Values the terminal did not
/// answer are `None`/`false`; nothing is inferred from environment variables
/// except the transport hints the probe already uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// The mode in use: the `--graphics` override, else the detected mode.
    pub graphics: GraphicsMode,
    /// What auto-detection chose (never `Auto`).
    pub detected: GraphicsMode,
    /// Cell size in pixels `(width, height)`.
    pub cell_px: Option<(u16, u16)>,
    /// Default background and foreground colours (OSC 11 / OSC 10).
    pub background: Option<(u8, u8, u8)>,
    pub foreground: Option<(u8, u8, u8)>,
    /// Kitty keyboard protocol available.
    pub keyboard_enhancement: bool,
    /// DECSET 2026 synchronized output available.
    pub synchronized_output: bool,
    /// XTVERSION reply.
    pub version: Option<String>,
    /// Running inside tmux (DCS passthrough needed).
    pub tmux: bool,
}

impl Capabilities {
    /// Dark background, judged from `background` luminance; `None` if unknown.
    pub fn dark(&self) -> Option<bool> {
        self.background
            .map(|(r, g, b)| (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) < 128.0)
    }
}

mod images;
pub use images::Images;

/// Queries the terminal once (raw mode, DA1 sentinel, one second timeout).
/// Call before any event reader starts. Raw mode is left as it was found.
/// When stdin/stdout are not terminals nothing is written and the result is
/// defaults with `graphics` = the override or `None`.
pub fn probe(requested: GraphicsMode) -> io::Result<Capabilities> {
    Ok(probe_raw(requested)?
        .map(|(p, _raw)| p.caps)
        .unwrap_or_else(|| offline(requested)))
}

fn offline(requested: GraphicsMode) -> Capabilities {
    let graphics = if requested == GraphicsMode::Auto {
        GraphicsMode::None
    } else {
        requested
    };
    Capabilities {
        graphics,
        detected: GraphicsMode::None,
        ..Capabilities::default()
    }
}

use std::io::{IsTerminal, Read, Write};
use std::time::{Duration, Instant};

/// The system terminal capability probe. Call before starting a terminal event reader.
#[derive(Default)]
pub struct SystemTerminalDoctor;

// A query is not an image placement. DA1 terminates the batch; unsupported replies
// are left unknown, rather than inferred from terminal environment variables.
const KITTY_QUERY: &str = "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
const QUERY: &str = "\x1b[>0q\x1b[16t\x1b[14t\x1b[?u\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?2026$p\x1b[?2031$p\x1b[?5522$p\x1b[?1016$p\x1b[c";
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAIUlEQVR4nGP4YGQEQXILPkAQpggD9RThkYOLUFHRsPYdANPx2JGDc6mSAAAAAElFTkSuQmCC";

#[derive(Default)]
struct Hints {
    term: String,
    program: String,
    tmux: String,
    herdr: String,
}

impl Hints {
    fn from_env() -> Self {
        fn value(key: &str) -> String {
            std::env::var(key)
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect()
        }
        Self {
            term: value("TERM"),
            program: value("TERM_PROGRAM"),
            tmux: value("TMUX"),
            herdr: value("HERDR_ENV"),
        }
    }
    fn kitty_query_safe(&self) -> bool {
        self.program != "Apple_Terminal"
            && self.term != "dumb"
            && self.term != "linux"
            && !(self.term.starts_with("screen") && self.tmux.is_empty())
    }
}

fn passthrough(sequence: &str, tmux: bool) -> String {
    if tmux {
        format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
    } else {
        sequence.to_owned()
    }
}

fn query(hints: &Hints) -> String {
    let kitty = if hints.kitty_query_safe() {
        passthrough(KITTY_QUERY, !hints.tmux.is_empty())
    } else {
        String::new()
    };
    format!("{kitty}{QUERY}")
}

#[derive(Default)]
struct Replies {
    kitty: bool,
    sixel: bool,
    version: Option<String>,
    cell: Option<String>,
    area: Option<String>,
    keyboard: Option<String>,
    background: Option<String>,
    foreground: Option<String>,
    modes: [Option<String>; 4],
    sentinel: bool,
}

fn parse(data: &[u8]) -> Replies {
    let s = String::from_utf8_lossy(data);
    let mut c = Replies {
        kitty: s.contains("\x1b_Gi=31;OK\x1b\\"),
        ..Replies::default()
    };
    if let Some(rest) = s.split("\x1bP>|").nth(1) {
        c.version = rest
            .split("\x1b\\")
            .next()
            .map(|v| v.chars().filter(|c| !c.is_control()).take(256).collect());
    }
    for part in s.split("\x1b[").skip(1) {
        if let Some(end) = part.find('c') {
            let body = &part[..end];
            if let Some(params) = body.strip_prefix('?') {
                if params.split(';').all(|v| v.parse::<u16>().is_ok()) {
                    c.sentinel = true;
                    c.sixel = params.split(';').any(|v| v == "4");
                }
            }
        }
        for (prefix, slot) in [("6;", &mut c.cell), ("4;", &mut c.area)] {
            if let Some(rest) = part.strip_prefix(prefix) {
                if let Some(end) = rest.find('t') {
                    let value = &rest[..end];
                    if value.split(';').count() == 2
                        && value.split(';').all(|v| v.parse::<u16>().is_ok())
                    {
                        *slot = Some(value.to_owned());
                    }
                }
            }
        }
        if let Some(rest) = part.strip_prefix('?') {
            if let Some(end) = rest.find('u') {
                if rest[..end].parse::<u16>().is_ok() {
                    c.keyboard = Some(rest[..end].to_owned());
                }
            }
            for (i, mode) in [2026, 2031, 5522, 1016].iter().enumerate() {
                if let Some(rest) = rest.strip_prefix(&format!("{mode};")) {
                    if let Some(value) = rest
                        .strip_suffix("$y")
                        .or_else(|| rest.split_once("$y").map(|v| v.0))
                    {
                        if value.parse::<u8>().is_ok() {
                            c.modes[i] = Some(value.to_owned());
                        }
                    }
                }
            }
        }
    }
    c.background = osc_reply(&s, "11");
    c.foreground = osc_reply(&s, "10");
    c
}

/// Body of an OSC `code` reply (`ESC ] code ; body BEL|ST`); `None` when the
/// terminator has not arrived.
fn osc_reply(s: &str, code: &str) -> Option<String> {
    let rest = s.split(&format!("\x1b]{code};")).nth(1)?;
    let end = match (rest.find('\x07'), rest.find("\x1b\\")) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(
        rest[..end]
            .chars()
            .filter(|c| !c.is_control())
            .take(256)
            .collect(),
    )
}

/// Parses `rgb:r/g/b` with 1 to 4 hex digits per channel into 8-bit values.
fn parse_color(s: &str) -> Option<(u8, u8, u8)> {
    let mut parts = s.strip_prefix("rgb:")?.split('/');
    let mut channel = || {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(p, 16).ok()?;
        let max = (1u32 << (4 * p.len())) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    };
    let (r, g, b) = (channel()?, channel()?, channel()?);
    parts.next().is_none().then_some((r, g, b))
}

fn capabilities(requested: GraphicsMode, c: &Replies, hints: &Hints) -> Capabilities {
    let detected = select(GraphicsMode::Auto, c);
    let cell_px = c.cell.as_deref().and_then(|v| {
        let (h, w) = v.split_once(';')?;
        let (h, w) = (h.parse::<u16>().ok()?, w.parse::<u16>().ok()?);
        (h > 0 && w > 0).then_some((w, h))
    });
    Capabilities {
        graphics: select(requested, c),
        detected,
        cell_px,
        background: c.background.as_deref().and_then(parse_color),
        foreground: c.foreground.as_deref().and_then(parse_color),
        keyboard_enhancement: c.keyboard.is_some(),
        synchronized_output: matches!(c.modes[0].as_deref(), Some("1" | "2" | "3")),
        version: c.version.clone().filter(|v| !v.is_empty()),
        tmux: !hints.tmux.is_empty(),
    }
}

struct Probed {
    replies: Replies,
    caps: Capabilities,
    hints: Hints,
}

/// Runs the query in raw mode. Returns `None` when not on a terminal. The
/// returned guard restores the raw-mode state it found when dropped.
fn probe_raw(requested: GraphicsMode) -> io::Result<Option<(Probed, RawMode)>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(None);
    }
    let hints = Hints::from_env();
    let raw = RawMode::enter()?;
    let mut out = io::stdout().lock();
    out.write_all(query(&hints).as_bytes())?;
    out.flush()?;
    drop(out);
    let replies = parse(&read_replies()?);
    let caps = capabilities(requested, &replies, &hints);
    Ok(Some((
        Probed {
            replies,
            caps,
            hints,
        },
        raw,
    )))
}

fn select(requested: GraphicsMode, c: &Replies) -> GraphicsMode {
    if requested != GraphicsMode::Auto {
        requested
    } else if c.kitty {
        GraphicsMode::Kitty
    } else if c.sixel {
        GraphicsMode::Sixel
    } else {
        GraphicsMode::Blocks
    }
}

fn display_value(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("unanswered")
}

fn summary(c: &Replies, mode: GraphicsMode, hints: &Hints) -> String {
    format!("Graphics: {mode:?} (kitty query: {}, sixel DA1: {})\nXTVERSION: {}\nCell pixels (height;width): {}\nText area pixels (height;width): {}\nKitty keyboard flags: {}\nBackground: {}\nMode replies (0=unrecognized, 1=set, 2=reset, 3/4=permanent): sync={}, theme={}, clipboard={}, pixel mouse={}\nDA1 sentinel: {}\nEnvironment hints: TERM={:?}, TERM_PROGRAM={:?}, TMUX={:?}, HERDR_ENV={:?}\nKitty uses direct placement; iTerm2 is not auto-detected. Image display requires visual confirmation.",
        if c.kitty { "supported" } else { "unconfirmed" }, if c.sixel { "supported" } else { "unconfirmed" },
        display_value(&c.version), display_value(&c.cell), display_value(&c.area), display_value(&c.keyboard), display_value(&c.background),
        display_value(&c.modes[0]), display_value(&c.modes[1]), display_value(&c.modes[2]), display_value(&c.modes[3]), if c.sentinel { "received" } else { "timed out" }, hints.term, hints.program, hints.tmux, hints.herdr)
}

/// Enables raw mode and restores the state it found (already raw: stays raw).
struct RawMode {
    was_raw: bool,
}
impl RawMode {
    fn enter() -> io::Result<Self> {
        let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if !was_raw {
            crossterm::terminal::enable_raw_mode()?;
        }
        Ok(Self { was_raw })
    }
}
impl Drop for RawMode {
    fn drop(&mut self) {
        if !self.was_raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

#[cfg(unix)]
fn read_replies() -> io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let deadline = Instant::now() + Duration::from_millis(1000);
    let mut bytes = Vec::new();
    while Instant::now() < deadline && bytes.len() < 16384 {
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let mut fd = libc::pollfd {
            fd: stdin.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fd, 1, timeout) };
        if ready < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if ready == 0 {
            break;
        }
        if fd.revents & libc::POLLIN == 0 {
            break;
        }
        let mut chunk = [0; 1024];
        let n = input.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if parse(&bytes).sentinel {
            break;
        }
    }
    Ok(bytes)
}
#[cfg(not(unix))]
fn read_replies() -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "terminal probe is currently supported on Unix only",
    ))
}

fn image(mode: GraphicsMode, tmux: bool) -> String {
    match mode {
        GraphicsMode::Kitty => format!("{}\r\n", passthrough(&format!("\x1b_Ga=T,f=100,t=d,c=12,r=6,q=2;{PNG}\x1b\\"), tmux)),
        GraphicsMode::Iterm => format!("\x1b]1337;File=inline=1;width=12;height=6;preserveAspectRatio=0:{PNG}\x07\r\n"),
        GraphicsMode::Sixel => "\x1bPq\"1;1;12;6#0;2;100;20;20#0!12~\x1b\\\r\n".into(),
        GraphicsMode::Blocks => "\x1b[48;2;255;50;50m            \x1b[0m\r\n\x1b[48;2;50;150;255m            \x1b[0m\r\n".into(),
        GraphicsMode::None | GraphicsMode::Auto => String::new(),
    }
}

impl TerminalDoctor for SystemTerminalDoctor {
    fn run(&mut self, options: &DoctorOptions) -> io::Result<DoctorReport> {
        let Some((p, raw)) = probe_raw(options.graphics)? else {
            return Ok(DoctorReport { summary: "Terminal probe skipped: stdin and stdout must both be terminals; no image displayed.".into() });
        };
        let mode = p.caps.graphics;
        if options.show_image {
            let mut out = io::stdout().lock();
            out.write_all(format!("Test image ({mode:?}):\r\n").as_bytes())?;
            out.write_all(image(mode, p.caps.tmux).as_bytes())?;
            out.flush()?;
        }
        drop(raw);
        Ok(DoctorReport {
            summary: summary(&p.replies, mode, &p.hints),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_replies() {
        let c = parse(b"\x1b_Gi=31;OK\x1b\\\x1bP>|kitty(1)\x1b\\\x1b[6;20;10t\x1b[4;800;1000t\x1b[?3u\x1b]11;rgb:ffff/0000/0000\x1b\\\x1b[?2026;2$y\x1b[?1;4c");
        assert!(c.kitty && c.sixel && c.sentinel);
        assert_eq!(c.cell.as_deref(), Some("20;10"));
        assert_eq!(c.version.as_deref(), Some("kitty(1)"));
        assert_eq!(c.modes[0].as_deref(), Some("2"));
        assert_eq!(c.background.as_deref(), Some("rgb:ffff/0000/0000"));
    }
    #[test]
    fn chooses_and_overrides() {
        let mut c = Replies::default();
        assert_eq!(select(GraphicsMode::Auto, &c), GraphicsMode::Blocks);
        c.sixel = true;
        assert_eq!(select(GraphicsMode::Auto, &c), GraphicsMode::Sixel);
        c.kitty = true;
        assert_eq!(select(GraphicsMode::Auto, &c), GraphicsMode::Kitty);
        for mode in [
            GraphicsMode::Kitty,
            GraphicsMode::Iterm,
            GraphicsMode::Sixel,
            GraphicsMode::Blocks,
            GraphicsMode::None,
        ] {
            assert_eq!(select(mode, &c), mode);
        }
    }
    #[test]
    fn rejects_incomplete_and_false_sentinel() {
        let c = parse(b"\x1b[?2026;4$y\x1b[?1;40c\x1b_Gi=31;ERROR\x1b\\");
        assert!(c.sentinel && !c.sixel && !c.kitty);
        assert!(!parse(b"\x1b[?1;4").sentinel);
    }
    #[test]
    fn query_honors_transport_hints() {
        let apple = Hints {
            program: "Apple_Terminal".into(),
            ..Hints::default()
        };
        assert!(!query(&apple).contains(KITTY_QUERY));
        let screen = Hints {
            term: "screen-256color".into(),
            ..Hints::default()
        };
        assert!(!query(&screen).contains(KITTY_QUERY));
        let tmux = Hints {
            term: "screen-256color".into(),
            tmux: "present".into(),
            ..Hints::default()
        };
        assert!(query(&tmux).starts_with("\x1bPtmux;\x1b\x1b_G"));
        assert!(query(&Hints::default()).starts_with(KITTY_QUERY));
        assert!(image(GraphicsMode::Kitty, true).starts_with("\x1bPtmux;\x1b\x1b_G"));
        assert!(image(GraphicsMode::Kitty, true).contains("q=2;"));
    }
    #[test]
    fn parses_osc_colours() {
        assert_eq!(parse_color("rgb:ffff/0000/8000"), Some((255, 0, 128)));
        assert_eq!(parse_color("rgb:ff/00/80"), Some((255, 0, 128)));
        assert_eq!(parse_color("rgb:f/0/8"), Some((255, 0, 136)));
        assert_eq!(parse_color("rgb:fff/000/800"), Some((255, 0, 128)));
        for bad in [
            "",
            "rgb:",
            "rgb:ff/00",
            "rgb:ff/00/80/11",
            "rgb:gg/00/00",
            "rgb:fffff/0/0",
            "#ff0000",
            "rgb:/0/0",
        ] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }
    #[test]
    fn parses_osc_10_and_11_with_both_terminators() {
        let c = parse(b"\x1b]10;rgb:cccc/cccc/cccc\x07\x1b]11;rgb:1111/2222/3333\x1b\\");
        assert_eq!(c.foreground.as_deref(), Some("rgb:cccc/cccc/cccc"));
        assert_eq!(c.background.as_deref(), Some("rgb:1111/2222/3333"));
        // partial (no terminator yet) and garbage
        assert_eq!(parse(b"\x1b]11;rgb:1111/22").background, None);
        assert_eq!(parse(b"\x1b]10;").foreground, None);
        let g = parse(b"\xff\xfe\x1b]\x1b[;;c garbage \x1b]11;\x07");
        assert_eq!(g.background.as_deref(), Some(""));
        assert!(parse_color(g.background.as_deref().unwrap()).is_none());
        assert!(query(&Hints::default()).contains("\x1b]10;?"));
    }
    #[test]
    fn builds_capabilities() {
        let r = parse(b"\x1b_Gi=31;OK\x1b\\\x1bP>|ghostty 1.2\x1b\\\x1b[6;20;10t\x1b[?0u\x1b]10;rgb:ffff/ffff/ffff\x07\x1b]11;rgb:0000/0000/0000\x07\x1b[?2026;2$y\x1b[?1;4c");
        let hints = Hints {
            tmux: "/tmp/tmux-1/default,1,0".into(),
            ..Hints::default()
        };
        let c = capabilities(GraphicsMode::Auto, &r, &hints);
        assert_eq!(
            (c.graphics, c.detected),
            (GraphicsMode::Kitty, GraphicsMode::Kitty)
        );
        assert_eq!(c.cell_px, Some((10, 20)));
        assert_eq!(c.background, Some((0, 0, 0)));
        assert_eq!(c.foreground, Some((255, 255, 255)));
        assert_eq!(c.dark(), Some(true));
        assert!(c.keyboard_enhancement && c.synchronized_output && c.tmux);
        assert_eq!(c.version.as_deref(), Some("ghostty 1.2"));
        let c = capabilities(GraphicsMode::Iterm, &r, &Hints::default());
        assert_eq!(
            (c.graphics, c.detected),
            (GraphicsMode::Iterm, GraphicsMode::Kitty)
        );
        // sixel via DA1 only; nothing answered at all means blocks and unknowns
        let c = capabilities(GraphicsMode::Auto, &parse(b"\x1b[?1;4c"), &Hints::default());
        assert_eq!(c.detected, GraphicsMode::Sixel);
        let c = capabilities(GraphicsMode::Auto, &parse(b""), &Hints::default());
        assert_eq!(
            (c.detected, c.cell_px, c.dark(), c.synchronized_output),
            (GraphicsMode::Blocks, None, None, false)
        );
        let light = Capabilities {
            background: Some((250, 250, 250)),
            ..Capabilities::default()
        };
        assert_eq!(light.dark(), Some(false));
    }
    #[test]
    fn offline_defaults() {
        assert_eq!(offline(GraphicsMode::Auto).graphics, GraphicsMode::None);
        assert_eq!(offline(GraphicsMode::Sixel).graphics, GraphicsMode::Sixel);
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            assert_eq!(
                probe(GraphicsMode::Auto).unwrap(),
                offline(GraphicsMode::Auto)
            );
        }
    }
    #[test]
    fn non_tty_skips() {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            let report = SystemTerminalDoctor
                .run(&DoctorOptions {
                    graphics: GraphicsMode::Kitty,
                    show_image: true,
                })
                .unwrap();
            assert!(report.summary.contains("skipped"));
        }
    }
}
