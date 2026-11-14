//! Media attach helpers: dropped/pasted path detection, filesystem
//! completion and the clipboard image command. Nothing here touches the
//! collection; the form sends the bytes or the path to the actor.
use std::path::{Path, PathBuf};
use std::process::Command;

/// Reads the clipboard as a PNG; injectable so tests need no clipboard.
pub type ClipboardFn = Box<dyn Fn() -> Result<Vec<u8>, String>>;

pub const NO_CLIPBOARD_TOOL: &str =
    "No clipboard tool found: install wl-clipboard (wl-paste) or xclip to paste images";

/// `wl-paste --type image/png`, else `xclip -selection clipboard -t image/png -o`.
/// Only already-installed tools are run; nothing is downloaded.
pub fn clipboard_image() -> Result<Vec<u8>, String> {
    let attempts: [(&str, &[&str]); 2] = [
        ("wl-paste", &["--type", "image/png"]),
        ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"]),
    ];
    let mut found = false;
    for (program, args) in attempts {
        match Command::new(program).args(args).output() {
            Ok(out) => {
                found = true;
                if out.status.success() && !out.stdout.is_empty() {
                    return Ok(out.stdout);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Could not run {program}: {e}")),
        }
    }
    if found {
        Err("The clipboard has no image".into())
    } else {
        Err(NO_CLIPBOARD_TOOL.into())
    }
}

/// A name for clipboard images (Anki would pick a unique one anyway).
pub fn clipboard_name() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("paste-{millis:x}.png")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits what a terminal drop produces: shell-style tokens separated by
/// spaces, with `'quoted'`, `"quoted"` and backslash-escaped spaces.
fn shell_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// One path candidate from a token: `file://` URI, `~/` or plain.
fn token_path(token: &str) -> Option<PathBuf> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        if !rest.starts_with('/') {
            return None;
        }
        return Some(PathBuf::from(percent_decode(rest)));
    }
    Some(expand_home(t))
}

pub fn expand_home(s: &str) -> PathBuf {
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(s.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    PathBuf::from(s)
}

/// The existing files a pasted text names, if it is nothing but file paths
/// (one per line, or several shell-quoted tokens). Plain prose, URLs and
/// paths that do not exist give an empty list, so ordinary pastes stay text.
pub fn detect_paths(text: &str) -> Vec<PathBuf> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut tokens: Vec<String> = Vec::new();
    // The whole text as one path first (paths may contain spaces unescaped
    // in a single-line paste).
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let whole = token_path(line).filter(|p| p.is_file());
        if whole.is_some() {
            tokens.push(line.to_owned());
        } else {
            tokens.extend(shell_tokens(line));
        }
    }
    let paths: Vec<PathBuf> = tokens.iter().filter_map(|t| token_path(t)).collect();
    if !paths.is_empty() && paths.iter().all(|p| p.is_file()) {
        paths
    } else {
        Vec::new()
    }
}

/// Filesystem completion for the attach prompt: entries of the typed
/// directory starting with the typed prefix; directories end in `/`.
pub fn complete_path(input: &str) -> Vec<String> {
    let (dir_text, prefix) = match input.rfind('/') {
        Some(i) => (&input[..=i], &input[i + 1..]),
        None => ("", input),
    };
    let dir = if dir_text.is_empty() {
        PathBuf::from(".")
    } else {
        expand_home(dir_text)
    };
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = read
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
                return None;
            }
            let is_dir = entry.path().is_dir();
            Some(format!("{dir_text}{name}{}", if is_dir { "/" } else { "" }))
        })
        .collect();
    out.sort();
    out.truncate(50);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "tanki-add-media-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn detects_plain_quoted_escaped_and_uri_paths() {
        let dir = tmp();
        let a = dir.join("my pic.png");
        let b = dir.join("b.mp3");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let a_s = a.to_str().unwrap();
        assert_eq!(detect_paths(a_s), vec![a.clone()]);
        assert_eq!(detect_paths(&format!("'{a_s}'")), vec![a.clone()]);
        assert_eq!(detect_paths(&format!("\"{a_s}\"\n")), vec![a.clone()]);
        assert_eq!(detect_paths(&a_s.replace(' ', "\\ ")), vec![a.clone()]);
        assert_eq!(
            detect_paths(&format!("file://{}", a_s.replace(' ', "%20"))),
            vec![a.clone()]
        );
        let two = format!("{} {}", a_s.replace(' ', "\\ "), b.to_str().unwrap());
        assert_eq!(detect_paths(&two), vec![a.clone(), b.clone()]);
        assert!(detect_paths("just some text").is_empty());
        assert!(detect_paths("https://example.com/x.png").is_empty());
        assert!(detect_paths(dir.to_str().unwrap()).is_empty(), "a directory is not a file");
        assert!(detect_paths(&format!("{a_s} /no/such/file")).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn completes_paths() {
        let dir = tmp().join("c");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("alpha.png"), b"x").unwrap();
        std::fs::write(dir.join(".hidden"), b"x").unwrap();
        let base = format!("{}/", dir.display());
        assert_eq!(
            complete_path(&base),
            vec![format!("{base}alpha.png"), format!("{base}sub/")]
        );
        assert_eq!(complete_path(&format!("{base}al")), vec![format!("{base}alpha.png")]);
        assert_eq!(complete_path(&format!("{base}.")), vec![format!("{base}.hidden")]);
        assert!(complete_path("/no/such/dir/x").is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
