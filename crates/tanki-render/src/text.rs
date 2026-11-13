//! Text helpers: display widths, Unicode sub/superscripts, roman numerals
//! and colour contrast.

use crate::Rgb;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display width of one grapheme cluster, as terminals (and ratatui) count it.
pub fn grapheme_width(g: &str) -> usize {
    if g.chars().next().is_some_and(|c| c.is_control()) {
        return 0;
    }
    g.width().min(2)
}

pub fn str_width(s: &str) -> usize {
    s.graphemes(true).map(grapheme_width).sum()
}

pub fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        'A' => 'ᴬ',
        'B' => 'ᴮ',
        'D' => 'ᴰ',
        'E' => 'ᴱ',
        'G' => 'ᴳ',
        'H' => 'ᴴ',
        'I' => 'ᴵ',
        'J' => 'ᴶ',
        'K' => 'ᴷ',
        'L' => 'ᴸ',
        'M' => 'ᴹ',
        'N' => 'ᴺ',
        'O' => 'ᴼ',
        'P' => 'ᴾ',
        'R' => 'ᴿ',
        'T' => 'ᵀ',
        'U' => 'ᵁ',
        'V' => 'ⱽ',
        'W' => 'ᵂ',
        ' ' => ' ',
        _ => return None,
    })
}

pub fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        ' ' => ' ',
        _ => return None,
    })
}

/// Maps all of `s` to sub/superscript characters, or `None` if any has no form.
pub fn map_script(s: &str, sup: bool) -> Option<String> {
    let f = if sup { superscript } else { subscript };
    s.chars().map(f).collect()
}

pub fn roman(mut n: usize, upper: bool) -> String {
    if n == 0 || n >= 4000 {
        return n.to_string();
    }
    const T: [(usize, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut s = String::new();
    for (v, r) in T {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if upper {
        s.to_ascii_uppercase()
    } else {
        s
    }
}

pub fn alpha(n: usize, upper: bool) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut n = n;
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    let s: String = s.into_iter().rev().collect();
    if upper {
        s.to_ascii_uppercase()
    } else {
        s
    }
}

pub fn greek(n: usize) -> String {
    const G: &[char] = &[
        'α', 'β', 'γ', 'δ', 'ε', 'ζ', 'η', 'θ', 'ι', 'κ', 'λ', 'μ', 'ν', 'ξ', 'ο', 'π', 'ρ', 'σ', 'τ', 'υ', 'φ', 'χ', 'ψ', 'ω',
    ];
    if n == 0 || n > G.len() {
        return n.to_string();
    }
    G[n - 1].to_string()
}

fn channel(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn luminance(c: Rgb) -> f64 {
    0.2126 * channel(c.0) + 0.7152 * channel(c.1) + 0.0722 * channel(c.2)
}

pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Minimum contrast kept between author text colours and the background.
pub const MIN_CONTRAST: f64 = 3.0;

/// Moves `fg` towards white or black until it reaches [`MIN_CONTRAST`]
/// against `bg`, keeping its hue as far as possible.
pub fn ensure_contrast(fg: Rgb, bg: Rgb) -> Rgb {
    if contrast(fg, bg) >= MIN_CONTRAST {
        return fg;
    }
    let target = if luminance(bg) < 0.18 {
        Rgb(255, 255, 255)
    } else {
        Rgb(0, 0, 0)
    };
    let mix = |t: f64| {
        let m = |f: u8, g: u8| (f as f64 + (g as f64 - f as f64) * t).round() as u8;
        Rgb(m(fg.0, target.0), m(fg.1, target.1), m(fg.2, target.2))
    };
    for step in 1..=20 {
        let c = mix(step as f64 / 20.0);
        if contrast(c, bg) >= MIN_CONTRAST {
            return c;
        }
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("日本語"), 6);
        assert_eq!(str_width("e\u{301}"), 1);
        assert_eq!(str_width("\u{7}"), 0);
    }

    #[test]
    fn scripts() {
        assert_eq!(map_script("2", true).as_deref(), Some("²"));
        assert_eq!(map_script("n+1", false).as_deref(), Some("ₙ₊₁"));
        assert_eq!(map_script("q", true), None);
        assert_eq!(map_script("b", false), None);
    }

    #[test]
    fn numbering() {
        assert_eq!(roman(1994, false), "mcmxciv");
        assert_eq!(roman(4, true), "IV");
        assert_eq!(alpha(1, false), "a");
        assert_eq!(alpha(27, true), "AA");
        assert_eq!(greek(2), "β");
    }

    #[test]
    fn contrast_adjustment() {
        let dark = Rgb(0x1e, 0x1e, 0x1e);
        let adj = ensure_contrast(Rgb(0, 0, 0), dark);
        assert!(contrast(adj, dark) >= MIN_CONTRAST);
        let blue = ensure_contrast(Rgb(0, 0, 255), dark);
        assert!(contrast(blue, dark) >= MIN_CONTRAST);
        assert!(blue.2 == 255, "keeps the hue: {blue:?}");
        // Already readable colours are untouched.
        assert_eq!(ensure_contrast(Rgb(0, 0, 255), Rgb(255, 255, 255)), Rgb(0, 0, 255));
    }
}
