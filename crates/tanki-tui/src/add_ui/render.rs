//! Drawing for the add / edit form (cell-buffer snapshot tested).
use super::*;
use crate::services::{card_side, CardRenderer};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Widget},
};
use tanki_render::RenderOptions;

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
fn red() -> Style {
    Style::default().fg(Color::Red)
}
fn yellow() -> Style {
    Style::default().fg(Color::Yellow)
}

/// Writes `text` clipped to `width` columns.
fn put(buf: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    if width > 0 {
        buf.set_stringn(x, y, text, width as usize, style);
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

pub(super) fn draw(ui: &AddUi, area: Rect, buf: &mut Buffer, renderer: &dyn CardRenderer) {
    if area.width < 10 || area.height < 4 {
        return;
    }
    let (form_area, preview_area) = if ui.preview_on {
        if area.width >= 100 {
            let [l, r] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
            (l, Some(r))
        } else {
            let [t, bottom] =
                Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
            (t, Some(bottom))
        }
    } else {
        (area, None)
    };
    draw_form(ui, form_area, buf);
    if let Some(p) = preview_area {
        draw_preview(ui, p, buf, renderer);
    }
    draw_overlay(ui, area, buf);
}

fn draw_form(ui: &AddUi, area: Rect, buf: &mut Buffer) {
    let form = &ui.form;
    let [header, fields_area, tags_area, check_area, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // Header.
    let title = match &form.edit {
        Some(e) => format!("Edit note {} · {}", e.id, form.notetype.name),
        None => {
            let deck = ui
                .decks
                .iter()
                .find(|(id, _)| *id == form.deck_id)
                .map_or("?", |(_, n)| n.as_str());
            format!("Add · Type: {} (Ctrl+N) · Deck: {} (Ctrl+D)", form.notetype.name, deck)
        }
    };
    put(buf, header.x, header.y, header.width, &title, bold());

    draw_fields(ui, fields_area, buf);

    // Tags.
    let tags_focused = form.focus == Focus::Tags;
    let label_style = if tags_focused { bold() } else { dim() };
    put(buf, tags_area.x, tags_area.y, tags_area.width, "Tags", label_style);
    let input_y = tags_area.y + 1;
    put(buf, tags_area.x, input_y, tags_area.width, form.tags.text(), Style::default());
    if tags_focused {
        let col = (form.tags.cursor_column() as u16).min(tags_area.width.saturating_sub(1));
        let cell = &mut buf[(tags_area.x + col, input_y)];
        let style = cell.style().add_modifier(Modifier::REVERSED);
        cell.set_style(style);
        draw_tag_popup(ui, fields_area, buf);
    }

    // Field check state and status.
    let (text, style) = match form.check {
        FieldsCheck::Normal => (String::new(), dim()),
        FieldsCheck::Empty => ("The first field is empty (adding is blocked)".into(), dim()),
        FieldsCheck::Duplicate => (
            "Duplicate: another note has the same first field (adding is allowed)".into(),
            yellow(),
        ),
        FieldsCheck::MissingCloze => (
            "No cloze deletion yet (adding will ask)".into(),
            yellow(),
        ),
        FieldsCheck::NotetypeNotCloze => (
            "Cloze deletion in a note type that is not a cloze type (adding is blocked)".into(),
            red(),
        ),
        FieldsCheck::FieldNotCloze => (
            "Cloze deletion in a field the card does not use for clozes (adding is blocked)".into(),
            red(),
        ),
    };
    put(buf, check_area.x, check_area.y, check_area.width, &text, style);
    if let Some(s) = &ui.status {
        put(
            buf,
            status_area.x,
            status_area.y,
            status_area.width,
            &s.text,
            if s.error { red() } else { dim() },
        );
    }
}

fn draw_fields(ui: &AddUi, area: Rect, buf: &mut Buffer) {
    let form = &ui.form;
    let width = area.width as usize;
    let focus = match form.focus {
        Focus::Field(i) => Some(i),
        Focus::Tags => None,
    };
    let natural: Vec<u16> = form
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if f.info.collapsed && focus != Some(i) {
                0
            } else {
                (f.area.visual_lines(width).len() as u16).clamp(1, 3)
            }
        })
        .collect();
    let mut heights = natural.clone();
    if let Some(i) = focus {
        let others: u16 = natural
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, h)| h + 1)
            .sum();
        heights[i] = area.height.saturating_sub(others + 1).max(3);
    }
    // Scroll so the focused field fits.
    let mut first = 0;
    if let Some(i) = focus {
        while first < i && heights[first..=i].iter().map(|h| h + 1).sum::<u16>() > area.height {
            first += 1;
        }
    }
    let mut y = area.y;
    let bottom = area.y + area.height;
    for (i, f) in form.fields.iter().enumerate().skip(first) {
        if y >= bottom {
            break;
        }
        let focused = focus == Some(i);
        let mut label = f.info.name.clone();
        if f.info.sticky {
            label.push_str(" [sticky]");
        }
        if f.info.rtl {
            label.push_str(" [RTL]");
        }
        if f.info.collapsed && !focused {
            label.push_str(" (collapsed)");
        }
        let style = if focused { bold().add_modifier(Modifier::UNDERLINED) } else { dim() };
        put(buf, area.x, y, area.width, &label, style);
        y += 1;
        let h = heights[i].min(bottom - y);
        if h == 0 {
            continue;
        }
        let rect = Rect::new(area.x, y, area.width, h);
        if f.area.is_empty() && !f.info.description.is_empty() {
            put(buf, rect.x, rect.y, rect.width, &f.info.description, dim());
        }
        f.area.render(rect, buf, focused);
        y += h;
    }
}

fn draw_tag_popup(ui: &AddUi, fields_area: Rect, buf: &mut Buffer) {
    let items = &ui.tag_popup.items;
    if items.is_empty() {
        return;
    }
    let h = items.len().min(6) as u16;
    let w = (items.iter().map(|i| i.chars().count()).max().unwrap_or(4) as u16 + 4)
        .min(fields_area.width);
    let rect = Rect::new(
        fields_area.x,
        (fields_area.y + fields_area.height).saturating_sub(h + 1).max(fields_area.y),
        w,
        h.min(fields_area.height),
    );
    Clear.render(rect, buf);
    for (i, item) in items.iter().take(rect.height as usize).enumerate() {
        let style = if i == ui.tag_popup.selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        put(buf, rect.x, rect.y + i as u16, rect.width, &format!(" {item} "), style);
    }
}

fn draw_preview(ui: &AddUi, area: Rect, buf: &mut Buffer, renderer: &dyn CardRenderer) {
    let block = Block::default().borders(Borders::ALL).title(" Preview ");
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 3 {
        return;
    }
    match &ui.preview {
        None => put(buf, inner.x, inner.y, inner.width, "Rendering…", dim()),
        Some(Err(e)) => put(buf, inner.x, inner.y, inner.width, e, red()),
        Some(Ok(card)) => {
            let [q, a] =
                Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
            for (title, html, rect) in
                [("Question", &card.question, q), ("Answer", &card.answer, a)]
            {
                if rect.height < 2 {
                    continue;
                }
                put(buf, rect.x, rect.y, rect.width, &format!("── {title} ──"), dim());
                let body = Rect::new(rect.x, rect.y + 1, rect.width, rect.height - 1);
                let side = card_side(card, html.clone(), &ui.media_dir, ui.night_mode);
                let options = RenderOptions {
                    width: body.width,
                    viewport_rows: body.height,
                    night_mode: ui.night_mode,
                    ..RenderOptions::default()
                };
                let rendered = renderer.render(&side, &options);
                renderer.paint(&rendered, 0, body, buf);
            }
        }
    }
}

fn boxed(buf: &mut Buffer, area: Rect, title: &str, height: u16, width: u16) -> Rect {
    let rect = centered(area, width, height);
    Clear.render(rect, buf);
    let block = Block::default().borders(Borders::ALL).title(format!(" {title} "));
    let inner = block.inner(rect);
    block.render(rect, buf);
    inner
}

fn draw_overlay(ui: &AddUi, area: Rect, buf: &mut Buffer) {
    let sel = Style::default().add_modifier(Modifier::REVERSED);
    match &ui.overlay {
        Overlay::None => {}
        Overlay::Picker(_, p) => {
            let matches = p.matches();
            let h = (matches.len().min(10) as u16) + 3;
            let inner = boxed(buf, area, p.title, h, 50);
            put(buf, inner.x, inner.y, inner.width, &format!("> {}", p.query.text()), Style::default());
            let selected = p.selected.min(matches.len().saturating_sub(1));
            let skip = selected.saturating_sub(9);
            for (row, &idx) in matches.iter().skip(skip).take(10).enumerate() {
                let style = if skip + row == selected { sel } else { Style::default() };
                put(buf, inner.x, inner.y + 1 + row as u16, inner.width, &p.items[idx].1, style);
            }
        }
        Overlay::History(selected) => {
            let h = ui.history.len().min(15) as u16 + 2;
            let inner = boxed(buf, area, "History of added notes", h, 56);
            for (row, e) in ui.history.iter().enumerate().take(inner.height as usize) {
                let style = if row == *selected { sel } else { Style::default() };
                put(buf, inner.x, inner.y + row as u16, inner.width, &e.label, style);
            }
        }
        Overlay::Colour(selected) => {
            let inner = boxed(buf, area, "Text colour", PALETTE.len() as u16 + 2, 24);
            for (row, (name, (r, g, b))) in PALETTE.iter().enumerate() {
                let style = if row == *selected { sel } else { Style::default() };
                let line = format!("{} {name}", row + 1);
                put(buf, inner.x, inner.y + row as u16, inner.width, &line, style);
                let swatch = Rect::new(inner.x + inner.width.saturating_sub(2), inner.y + row as u16, 2, 1);
                for x in swatch.left()..swatch.right() {
                    buf[(x, swatch.y)].set_bg(Color::Rgb(*r, *g, *b));
                }
            }
        }
        Overlay::MediaPath { input, completions, selected } => {
            let h = completions.len().min(8) as u16 + 4;
            let inner = boxed(buf, area, "Attach file or URL", h, 64);
            put(buf, inner.x, inner.y, inner.width, &format!("> {}", input.text()), Style::default());
            for (row, c) in completions.iter().take(8).enumerate() {
                let style = if row == *selected { sel } else { dim() };
                put(buf, inner.x, inner.y + 1 + row as u16, inner.width, c, style);
            }
        }
        Overlay::PasteOffer { paths, .. } => {
            let inner = boxed(buf, area, "Attach file?", 6, 64);
            put(buf, inner.x, inner.y, inner.width, "The pasted text is a file path:", Style::default());
            let first = paths.first().map(|p| p.display().to_string()).unwrap_or_default();
            put(buf, inner.x, inner.y + 1, inner.width, &first, bold());
            if paths.len() > 1 {
                put(buf, inner.x, inner.y + 2, inner.width, &format!("and {} more", paths.len() - 1), dim());
            }
            put(buf, inner.x, inner.y + 3, inner.width, "Enter: attach   n: paste as text", dim());
        }
        Overlay::ConfirmDiscard => {
            let inner = boxed(buf, area, "Discard?", 4, 40);
            put(buf, inner.x, inner.y, inner.width, "Discard current input?", Style::default());
            put(buf, inner.x, inner.y + 1, inner.width, "y: discard   n: keep editing", dim());
        }
        Overlay::ConfirmCloze => {
            let inner = boxed(buf, area, "No cloze deletions", 5, 56);
            put(buf, inner.x, inner.y, inner.width, "You have a cloze note type but have not", Style::default());
            put(buf, inner.x, inner.y + 1, inner.width, "made any cloze deletions. Proceed?", Style::default());
            put(buf, inner.x, inner.y + 2, inner.width, "y: add anyway   n: keep editing", dim());
        }
        Overlay::Conflict { missing, .. } => {
            let inner = boxed(buf, area, "Note changed", 6, 60);
            let msg = if missing.is_empty() {
                "This note was changed elsewhere since you opened it."
            } else {
                "This note was deleted elsewhere."
            };
            put(buf, inner.x, inner.y, inner.width, msg, red());
            put(buf, inner.x, inner.y + 1, inner.width, "Nothing was saved.", Style::default());
            put(buf, inner.x, inner.y + 3, inner.width, "r: reload the note   Esc: keep editing", dim());
        }
    }
}
