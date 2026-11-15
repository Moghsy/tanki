//! Kitty OSC 66. Keep escape sequences outside the width-counted cell buffer.
//! https://sw.kovidgoyal.net/kitty/text-sizing-protocol/ (introduced in 0.40).

pub(crate) fn supported(version: Option<&str>, tmux: bool) -> bool {
    if tmux {
        return false;
    }
    let Some(version) = version
        .and_then(|v| v.strip_prefix("kitty("))
        .and_then(|v| v.strip_suffix(')'))
    else {
        return false;
    };
    let mut parts = version.split('.');
    let (Some(major), Some(minor)) = (
        parts.next().and_then(|n| n.parse::<u32>().ok()),
        parts.next().and_then(|n| n.parse::<u32>().ok()),
    ) else {
        return false;
    };
    major > 0 || minor >= 40
}

/// Encode a cell-preserving fractional font size (10..90 percent, in steps of
/// ten). `cells` is the already measured width of one grapheme, at most seven.
/// Call only when [`crate::Capabilities::text_sizing`] is true. Unknown sizes
/// and unsafe text produce no escape sequence.
pub fn sized_text(text: &str, percentage: u8, cells: u16) -> Option<String> {
    if text.is_empty()
        || text.len() > 4096
        || text.chars().any(char::is_control)
        || !(1..=7).contains(&cells)
        || !(1..100).contains(&percentage)
    {
        return None;
    }
    let numerator = ((u16::from(percentage) + 5) / 10).clamp(1, 9);
    Some(format!(
        "\x1b]66;n={numerator}:d=10:w={cells}:v=2:h=2;{text}\x1b\\"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_query_gates_known_support() {
        for version in ["kitty(0.40.0)", "kitty(0.50.1)", "kitty(1.0.0)"] {
            assert!(supported(Some(version), false));
            assert!(!supported(Some(version), true));
        }
        for version in [
            "kitty(0.39.1)",
            "kitty(1)",
            "ghostty 1.4",
            "kitty(abc.def)",
            "kitty(0.50.0) other",
        ] {
            assert!(!supported(Some(version), false));
        }
        assert!(!supported(None, false));
    }
    #[test]
    fn sizing_sequences_have_bounded_metadata_and_safe_text() {
        assert_eq!(
            sized_text("界", 50, 2).unwrap(),
            "\x1b]66;n=5:d=10:w=2:v=2:h=2;界\x1b\\"
        );
        for text in ["\x1b]8;;evil\x07", "line\nbreak", "\u{9b}x", ""] {
            assert!(sized_text(text, 50, 1).is_none());
        }
        assert!(sized_text("x", 100, 1).is_none());
        assert!(sized_text("x", 50, 8).is_none());
        assert!(sized_text(&"x".repeat(4097), 50, 1).is_none());
    }
}
