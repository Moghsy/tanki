//! Drawing for the note types screens (cell-buffer snapshot tested).
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
fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

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

fn boxed(buf: &mut Buffer, area: Rect, title: &str, width: u16, height: u16) -> Rect {
    let rect = centered(area, width, height);
    Clear.render(rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(rect);
    block.render(rect, buf);
    inner
}

pub(super) fn draw(ui: &NotetypesUi, area: Rect, buf: &mut Buffer, renderer: &dyn CardRenderer) {
    if area.width < 20 || area.height < 6 {
        return;
    }
    let [body, status] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    match &ui.screen {
        Screen::List => draw_list(ui, body, buf),
        Screen::Fields(s) => draw_fields(s, body, buf),
        Screen::Templates(s) => draw_templates(ui, s, body, buf, renderer),
        Screen::Latex(s) => draw_latex(s, body, buf),
    }
    if let Some(st) = &ui.status {
        put(
            buf,
            status.x,
            status.y,
            status.width,
            &st.text,
            if st.error { red() } else { dim() },
        );
    }
    draw_overlay(ui, area, buf);
}

fn kind_label(nt: &NotetypeSummary) -> &'static str {
    match (nt.kind, nt.stock_kind) {
        (_, Some(StockKind::ImageOcclusion)) => "occlusion",
        (NotetypeKind::Cloze, _) => "cloze",
        _ => "standard",
    }
}

fn draw_list(ui: &NotetypesUi, area: Rect, buf: &mut Buffer) {
    put(buf, area.x, area.y, area.width, "Note types", bold());
    put(
        buf,
        area.x,
        area.y + 1,
        area.width,
        &format!(
            "  {:<32} {:<10} {:>6} {:>7} {:>6}",
            "Name", "Kind", "Notes", "Fields", "Cards"
        ),
        dim(),
    );
    let rows = (area.height as usize).saturating_sub(2);
    let skip = ui.selected.saturating_sub(rows.saturating_sub(1));
    for (row, nt) in ui.notetypes.iter().enumerate().skip(skip).take(rows) {
        let y = area.y + 2 + (row - skip) as u16;
        let marker = if row == ui.selected { ">" } else { " " };
        let line = format!(
            "{marker} {:<32} {:<10} {:>6} {:>7} {:>6}",
            nt.name,
            kind_label(nt),
            nt.use_count,
            nt.field_count,
            nt.template_count
        );
        let style = if row == ui.selected {
            selected()
        } else {
            Style::default()
        };
        put(buf, area.x, y, area.width, &line, style);
    }
}

fn draw_fields(s: &FieldsScreen, area: Rect, buf: &mut Buffer) {
    let title = format!(
        "Fields of {} ({} note{}){}",
        s.draft.name,
        s.draft.use_count,
        if s.draft.use_count == 1 { "" } else { "s" },
        if s.draft != s.original { " *" } else { "" }
    );
    put(buf, area.x, area.y, area.width, &title, bold());
    let rows = (area.height as usize).saturating_sub(7).max(1);
    let skip = s.cursor.saturating_sub(rows.saturating_sub(1));
    for (i, f) in s.draft.fields.iter().enumerate().skip(skip).take(rows) {
        let y = area.y + 1 + (i - skip) as u16;
        let mut flags = Vec::new();
        if i == s.draft.sort_field {
            flags.push("sort");
        }
        if f.rtl {
            flags.push("RTL");
        }
        if f.plain_text {
            flags.push("HTML");
        }
        if f.collapsed {
            flags.push("collapsed");
        }
        if f.exclude_from_search {
            flags.push("no search");
        }
        if f.required {
            flags.push("required");
        }
        if f.ord.is_none() {
            flags.push("new");
        }
        let marker = if i == s.cursor { ">" } else { " " };
        let line = format!("{marker} {:>2}. {:<28} {}", i + 1, f.name, flags.join(", "));
        let style = if i == s.cursor {
            selected()
        } else {
            Style::default()
        };
        put(buf, area.x, y, area.width, &line, style);
    }
    if let Some(f) = s.draft.fields.get(s.cursor) {
        let y = area.bottom().saturating_sub(5);
        put(
            buf,
            area.x,
            y,
            area.width,
            &format!("Field {}", f.name),
            bold(),
        );
        put(
            buf,
            area.x,
            y + 1,
            area.width,
            &format!("  Editor font: {} {}", f.font_name, f.font_size),
            dim(),
        );
        let description = if f.description.is_empty() {
            "(none)"
        } else {
            &f.description
        };
        put(
            buf,
            area.x,
            y + 2,
            area.width,
            &format!("  Description: {description}"),
            dim(),
        );
        put(
            buf,
            area.x,
            y + 3,
            area.width,
            "  Adding, deleting or moving fields and changing the sort field need a full sync.",
            dim(),
        );
    }
}

fn draw_latex(s: &LatexScreen, area: Rect, buf: &mut Buffer) {
    let title = format!(
        "LaTeX options of {}{}",
        s.draft.name,
        if s.dirty() { " *" } else { "" }
    );
    put(buf, area.x, area.y, area.width, &title, bold());
    let [pre, post, svg] = Layout::vertical([
        Constraint::Percentage(45),
        Constraint::Percentage(45),
        Constraint::Length(1),
    ])
    .areas(Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    ));
    for (i, (name, text, rect)) in [("Header", &s.pre, pre), ("Footer", &s.post, post)]
        .into_iter()
        .enumerate()
    {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {name} "))
            .border_style(if s.focus == i { bold() } else { dim() });
        let inner = block.inner(rect);
        block.render(rect, buf);
        text.render(inner, buf, s.focus == i);
    }
    let check = if s.draft.latex_svg { "[x]" } else { "[ ]" };
    put(
        buf,
        svg.x,
        svg.y,
        svg.width,
        &format!("{check} Generate SVG images instead of PNG"),
        if s.focus == 2 {
            selected()
        } else {
            Style::default()
        },
    );
}

fn draw_templates(
    ui: &NotetypesUi,
    s: &TemplatesScreen,
    area: Rect,
    buf: &mut Buffer,
    renderer: &dyn CardRenderer,
) {
    // Header: the card types, then the parts.
    let mut x = area.x;
    let title = format!(
        "Cards of {}{}: ",
        s.draft.name,
        if s.dirty() { " *" } else { "" }
    );
    put(buf, x, area.y, area.width, &title, bold());
    x += unicode_width::UnicodeWidthStr::width(title.as_str()) as u16;
    for (i, t) in s.draft.templates.iter().enumerate() {
        let label = format!("[{}]", t.name);
        let w = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
        if x + w >= area.right() {
            break;
        }
        put(
            buf,
            x,
            area.y,
            w,
            &label,
            if i == s.index {
                selected()
            } else {
                Style::default()
            },
        );
        x += w + 1;
    }
    let mut x = area.x;
    for tab in Tab::ALL {
        let label = format!(" {} ", tab.title());
        let w = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
        if x + w > area.right() {
            break;
        }
        put(
            buf,
            x,
            area.y + 1,
            w,
            &label,
            if tab == s.tab { selected() } else { dim() },
        );
        x += w + 1;
    }
    let deck = s
        .template()
        .target_deck_id
        .map_or(String::new(), |id| format!("  deck override: #{id}"));
    if !deck.is_empty() {
        put(
            buf,
            x,
            area.y + 1,
            area.right().saturating_sub(x),
            &deck,
            dim(),
        );
    }
    let main = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(3),
    );
    let (editor, preview) = if area.width >= 100 {
        let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(main);
        (l, r)
    } else {
        let [t, b] =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(main);
        (t, b)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", s.tab.title()));
    let inner = block.inner(editor);
    block.render(editor, buf);
    s.area.render(inner, buf, true);
    draw_preview(ui, s, preview, buf, renderer);
    // Inline errors.
    let error = s
        .problem
        .as_ref()
        .map(|p| format!("Card {}: {}", p.template + 1, p.message))
        .or_else(|| match &s.preview {
            Some(Ok(p)) => p.error.clone(),
            Some(Err(e)) => Some(e.clone()),
            None => None,
        });
    if let Some(error) = error {
        put(buf, area.x, area.bottom() - 1, area.width, &error, red());
    }
}

fn draw_preview(
    ui: &NotetypesUi,
    s: &TemplatesScreen,
    area: Rect,
    buf: &mut Buffer,
    renderer: &dyn CardRenderer,
) {
    let who = match s.samples.get(s.sample) {
        Some(_) => format!(" Preview (note {}/{}) ", s.sample + 1, s.samples.len()),
        None => " Preview (empty note) ".to_owned(),
    };
    let block = Block::default().borders(Borders::ALL).title(who);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 3 {
        return;
    }
    match &s.preview {
        None => put(buf, inner.x, inner.y, inner.width, "Rendering…", dim()),
        Some(Err(e)) => put(buf, inner.x, inner.y, inner.width, e, red()),
        Some(Ok(p)) => {
            let [q, a] = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(inner);
            for (title, html, rect) in [
                ("Question", &p.card.question, q),
                ("Answer", &p.card.answer, a),
            ] {
                if rect.height < 2 {
                    continue;
                }
                put(
                    buf,
                    rect.x,
                    rect.y,
                    rect.width,
                    &format!("── {title} ──"),
                    dim(),
                );
                let body = Rect::new(rect.x, rect.y + 1, rect.width, rect.height - 1);
                let side = card_side(&p.card, html.clone(), &ui.media_dir, ui.night_mode);
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

fn draw_overlay(ui: &NotetypesUi, area: Rect, buf: &mut Buffer) {
    let text = |buf: &mut Buffer, inner: Rect, row: u16, text: &str, style: Style| {
        if row < inner.height {
            put(buf, inner.x, inner.y + row, inner.width, text, style);
        }
    };
    match &ui.overlay {
        Overlay::None => {}
        Overlay::Prompt(p) => {
            let inner = boxed(buf, area, &p.title, 60, 4);
            text(
                buf,
                inner,
                0,
                &format!("> {}", p.input.text()),
                Style::default(),
            );
            let col = (p.input.cursor_column() as u16 + 2).min(inner.width.saturating_sub(1));
            let cell = &mut buf[(inner.x + col, inner.y)];
            let style = cell.style().add_modifier(Modifier::REVERSED);
            cell.set_style(style);
        }
        Overlay::Picker(p) => {
            let matches = p.matches();
            let height = (matches.len().min(12) as u16) + 3;
            let inner = boxed(buf, area, &p.title, 56, height);
            text(
                buf,
                inner,
                0,
                &format!("> {}", p.query.text()),
                Style::default(),
            );
            let sel = p.selected.min(matches.len().saturating_sub(1));
            let skip = sel.saturating_sub(11);
            for (row, &i) in matches.iter().skip(skip).take(12).enumerate() {
                let style = if row + skip == sel {
                    selected()
                } else {
                    Style::default()
                };
                text(buf, inner, row as u16 + 1, &p.items[i].1, style);
            }
        }
        Overlay::ConfirmDelete(id) => {
            let nt = ui.notetypes.iter().find(|n| n.id == *id);
            let inner = boxed(buf, area, "Delete note type", 64, 6);
            let name = nt.map_or("this note type", |n| n.name.as_str());
            text(buf, inner, 0, &format!("Delete {name}?"), bold());
            let body = match nt.map_or(0, |n| n.use_count) {
                0 => "It is not used by any note.".to_owned(),
                n => format!(
                    "Its {n} note{} and their cards are deleted too.",
                    if n == 1 { "" } else { "s" }
                ),
            };
            text(buf, inner, 1, &body, yellow());
            text(buf, inner, 3, "y delete · n keep", dim());
        }
        Overlay::ConfirmDiscard => {
            let inner = boxed(buf, area, "Unsaved changes", 50, 5);
            text(
                buf,
                inner,
                0,
                "Discard the changes you made?",
                Style::default(),
            );
            text(buf, inner, 2, "y discard · n keep editing", dim());
        }
        Overlay::ConfirmDeleteField(index) => {
            let inner = boxed(buf, area, "Delete field", 60, 6);
            let (name, count) = match &ui.screen {
                Screen::Fields(s) => (
                    s.draft.fields.get(*index).map_or("", |f| f.name.as_str()),
                    s.draft.use_count,
                ),
                _ => ("", 0),
            };
            text(buf, inner, 0, &format!("Delete the field {name}?"), bold());
            text(
                buf,
                inner,
                1,
                &format!("Its content is removed from {count} note(s) when you save."),
                yellow(),
            );
            text(buf, inner, 3, "y delete · n keep", dim());
        }
        Overlay::ConfirmDeleteTemplate(index) => {
            let inner = boxed(buf, area, "Delete card type", 60, 6);
            let name = match &ui.screen {
                Screen::Templates(s) => s
                    .draft
                    .templates
                    .get(*index)
                    .map_or("", |t| t.name.as_str()),
                _ => "",
            };
            text(
                buf,
                inner,
                0,
                &format!("Delete the card type {name}?"),
                bold(),
            );
            text(
                buf,
                inner,
                1,
                "Its cards are deleted when you save.",
                yellow(),
            );
            text(buf, inner, 3, "y delete · n keep", dim());
        }
        Overlay::FullSync(_) => {
            let inner = boxed(buf, area, "Full sync required", 64, 7);
            text(
                buf,
                inner,
                0,
                "This change requires a one-way full sync:",
                bold(),
            );
            text(
                buf,
                inner,
                1,
                "the next sync uploads the whole collection and",
                Style::default(),
            );
            text(
                buf,
                inner,
                2,
                "other devices must download it again.",
                Style::default(),
            );
            text(buf, inner, 4, "y continue · n cancel", dim());
        }
        Overlay::Restore(form) => draw_restore(ui, form, area, buf),
    }
}

fn draw_restore(ui: &NotetypesUi, form: &RestoreForm, area: Rect, buf: &mut Buffer) {
    if let Some(choice) = form.choosing {
        let inner = boxed(
            buf,
            area,
            "Restore to which stock note type?",
            56,
            ui.stock.len() as u16 + 4,
        );
        put(
            buf,
            inner.x,
            inner.y,
            inner.width,
            "This note type has no recorded original kind.",
            dim(),
        );
        for (i, k) in ui.stock.iter().enumerate() {
            let style = if i == choice {
                selected()
            } else {
                Style::default()
            };
            put(
                buf,
                inner.x,
                inner.y + 1 + i as u16,
                inner.width,
                &k.name,
                style,
            );
        }
        return;
    }
    let plan = &form.plan;
    let inner = boxed(buf, area, "Restore to the stock note type", 72, 13);
    let line = |buf: &mut Buffer, row: u16, text: &str, style: Style| {
        if row < inner.height {
            put(buf, inner.x, inner.y + row, inner.width, text, style);
        }
    };
    line(
        buf,
        0,
        &format!("{} ({} notes)", plan.notetype_name, plan.note_count),
        bold(),
    );
    line(
        buf,
        1,
        "Fields, card types and styling are replaced by the stock ones.",
        Style::default(),
    );
    line(
        buf,
        2,
        &format!(
            "Fields: {}  →  {}",
            plan.old_field_names.join(", "),
            plan.stock_field_names.join(", ")
        ),
        dim(),
    );
    if plan.dropped_fields.is_empty() {
        line(buf, 3, "No field content is lost.", Style::default());
    } else {
        line(
            buf,
            3,
            &format!(
                "Content of these fields is lost: {}",
                plan.dropped_fields.join(", ")
            ),
            red(),
        );
    }
    line(
        buf,
        4,
        &format!(
            "Card types: {}  →  {}",
            plan.old_template_names.join(", "),
            plan.stock_template_names.join(", ")
        ),
        dim(),
    );
    if plan.structure_changes {
        line(
            buf,
            5,
            "The structure changes: a full sync is required.",
            yellow(),
        );
    }
    line(
        buf,
        7,
        "Type the note type's name to confirm:",
        Style::default(),
    );
    line(
        buf,
        8,
        &format!("> {}", form.typed.text()),
        Style::default(),
    );
    line(buf, 10, "Enter restore · Esc cancel", dim());
}
