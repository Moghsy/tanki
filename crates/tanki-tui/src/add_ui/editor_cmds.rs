//! Editor commands (PLAN 8.5) applied to the raw HTML in a `TextArea`:
//! they wrap the selection or insert at the cursor. Output shapes follow
//! what desktop's editor stores (R7 §2.2, §2.8).
use crate::textarea::TextArea;

/// Palette for text colour: name and RGB. Stored as desktop's editor does,
/// `<span style="color: rgb(r, g, b);">`.
pub const PALETTE: [(&str, (u8, u8, u8)); 6] = [
    ("Red", (255, 0, 0)),
    ("Orange", (255, 127, 0)),
    ("Green", (0, 170, 0)),
    ("Blue", (0, 85, 255)),
    ("Purple", (170, 0, 255)),
    ("Gray", (128, 128, 128)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorCommand {
    Bold,
    Italic,
    Underline,
    /// Index into [`PALETTE`].
    Colour(usize),
    UnorderedList,
    OrderedList,
    /// `{{cN::…}}` with this number.
    Cloze(u32),
    MathInline,
    MathBlock,
    Chemistry,
}

/// Next cloze number for a new cloze: max + 1 over the numbers
/// `ClozeNumbers` reports for the whole note (never a regex).
pub fn next_cloze_number(numbers: &[u32]) -> u32 {
    numbers.iter().copied().max().unwrap_or(0) + 1
}

/// The number for "same number": the max in use, at least 1.
pub fn same_cloze_number(numbers: &[u32]) -> u32 {
    numbers.iter().copied().max().unwrap_or(1).max(1)
}

pub fn apply(area: &mut TextArea, command: EditorCommand) {
    match command {
        EditorCommand::Bold => area.wrap_selection("<b>", "</b>", false),
        EditorCommand::Italic => area.wrap_selection("<i>", "</i>", false),
        EditorCommand::Underline => area.wrap_selection("<u>", "</u>", false),
        EditorCommand::Colour(i) => {
            let (_, (r, g, b)) = PALETTE[i.min(PALETTE.len() - 1)];
            area.wrap_selection(
                &format!("<span style=\"color: rgb({r}, {g}, {b});\">"),
                "</span>",
                false,
            );
        }
        EditorCommand::UnorderedList => list(area, "ul"),
        EditorCommand::OrderedList => list(area, "ol"),
        EditorCommand::Cloze(n) => area.wrap_selection(&format!("{{{{c{n}::"), "}}", true),
        EditorCommand::MathInline => area.wrap_selection("\\(", "\\)", false),
        EditorCommand::MathBlock => area.wrap_selection("\\[", "\\]", false),
        EditorCommand::Chemistry => area.wrap_selection("\\(\\ce{", "}\\)", false),
    }
}

/// `<ul><li>a</li><li>b</li></ul>`: one item per selected line.
fn list(area: &mut TextArea, tag: &str) {
    let selected = area.selected_text().unwrap_or("").to_owned();
    let items: Vec<&str> = selected
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect();
    if items.is_empty() {
        let open = format!("<{tag}><li>");
        let out = format!("{open}</li></{tag}>");
        area.replace_selection(&out, open.len());
        return;
    }
    let mut out = format!("<{tag}>");
    for item in items {
        out.push_str(&format!("<li>{item}</li>"));
    }
    out.push_str(&format!("</{tag}>"));
    let end = out.len();
    area.replace_selection(&out, end);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, select: bool, cmd: EditorCommand) -> (String, usize) {
        let mut a = TextArea::new(text);
        if select {
            a.select_all();
        }
        apply(&mut a, cmd);
        (a.value().to_owned(), a.cursor())
    }

    #[test]
    fn inline_formats() {
        assert_eq!(run("x", true, EditorCommand::Bold).0, "<b>x</b>");
        assert_eq!(run("x", true, EditorCommand::Italic).0, "<i>x</i>");
        assert_eq!(run("x", true, EditorCommand::Underline).0, "<u>x</u>");
        assert_eq!(
            run("x", true, EditorCommand::Colour(0)).0,
            "<span style=\"color: rgb(255, 0, 0);\">x</span>"
        );
        assert_eq!(run("a", false, EditorCommand::Bold), ("a<b></b>".into(), 4));
    }

    #[test]
    fn lists() {
        assert_eq!(
            run("a\nb", true, EditorCommand::UnorderedList).0,
            "<ul><li>a</li><li>b</li></ul>"
        );
        assert_eq!(run("a", true, EditorCommand::OrderedList).0, "<ol><li>a</li></ol>");
        assert_eq!(
            run("", false, EditorCommand::UnorderedList),
            ("<ul><li></li></ul>".into(), 8)
        );
    }

    #[test]
    fn math_and_chemistry() {
        assert_eq!(run("x", true, EditorCommand::MathInline).0, "\\(x\\)");
        assert_eq!(run("x", true, EditorCommand::MathBlock).0, "\\[x\\]");
        assert_eq!(run("H2O", true, EditorCommand::Chemistry).0, "\\(\\ce{H2O}\\)");
        assert_eq!(run("", false, EditorCommand::Chemistry), ("\\(\\ce{}\\)".into(), 6));
    }

    #[test]
    fn cloze_numbers_and_wrapping() {
        assert_eq!(next_cloze_number(&[]), 1);
        assert_eq!(next_cloze_number(&[1, 3]), 4);
        assert_eq!(same_cloze_number(&[]), 1);
        assert_eq!(same_cloze_number(&[2, 5]), 5);
        let mut a = TextArea::new("Paris is in France");
        a.set_cursor(0);
        for _ in 0..5 {
            a.handle_key(&crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Right,
                crossterm::event::KeyModifiers::SHIFT,
            ));
        }
        apply(&mut a, EditorCommand::Cloze(next_cloze_number(&[1, 2])));
        assert_eq!(a.value(), "{{c3::Paris}} is in France");
        // Same number, whitespace stays outside the braces.
        let mut b = TextArea::new("a {{c1::b}}  c  ");
        b.set_cursor(11);
        b.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::End,
            crossterm::event::KeyModifiers::SHIFT,
        ));
        apply(&mut b, EditorCommand::Cloze(same_cloze_number(&[1])));
        assert_eq!(b.value(), "a {{c1::b}}  {{c1::c}}  ");
        assert_eq!(run("", false, EditorCommand::Cloze(2)), ("{{c2::}}".into(), 6));
    }
}
