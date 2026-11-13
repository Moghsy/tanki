//! Drawing: every screen, overlay and the status line, into a ratatui frame.
use crate::app::{graphics_name, App, ChoiceValue, Overlay, Screen};
use crate::backend::DeckRow;
use crate::keymap::{Action, Scope};
use crate::reviewer::{next_learn_message, Phase};
use crate::services::{AudioStatus, ImageState};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use tanki_core::QueueKind;
use unicode_width::UnicodeWidthStr;

const NEW: Color = Color::LightBlue;
const LEARN: Color = Color::LightRed;
const DUE: Color = Color::LightGreen;
const DIM: Color = Color::DarkGray;

fn dim() -> Style {
    Style::default().fg(DIM)
}

fn count_span(n: u32, color: Color) -> Span<'static> {
    let style = if n == 0 {
        dim()
    } else {
        Style::default().fg(color)
    };
    Span::styled(n.to_string(), style)
}

/// Desktop's `--flag-N` colours.
pub fn flag_color(flag: u8) -> Color {
    match flag {
        1 => Color::Red,
        2 => Color::Rgb(255, 165, 0),
        3 => Color::Green,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Rgb(160, 32, 240),
        _ => Color::Reset,
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn timer_text(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl App<'_> {
    pub fn draw(&mut self, frame: &mut Frame) {
        self.services.renderer.begin_frame();
        let area = frame.area();
        if area.height < 10 || area.width < 50 {
            frame.render_widget(
                Paragraph::new("Enlarge the terminal to at least 50 × 10. Ctrl+Q quits.")
                    .wrap(Wrap { trim: true }),
                area,
            );
            self.services
                .renderer
                .finish_frame(frame.buffer_mut(), false);
            return;
        }
        let main = Rect::new(area.x, area.y, area.width, area.height - 1);
        let status = Rect::new(area.x, area.bottom() - 1, area.width, 1);
        let zoom = matches!(self.overlay, Some(Overlay::Zoom { .. }));
        if !zoom {
            match self.screen.scope() {
                Scope::Decks => self.draw_decks(frame, main),
                Scope::Overview => self.draw_overview(frame, main),
                Scope::Review => self.draw_review(frame, main),
                _ => self.draw_congrats(frame, main),
            }
        }
        self.draw_overlay(frame, main);
        self.draw_status(frame.buffer_mut(), status);
        self.services
            .renderer
            .finish_frame(frame.buffer_mut(), self.overlay.is_none());
    }

    // ---- deck list ---------------------------------------------------------

    fn draw_decks(&mut self, frame: &mut Frame, area: Rect) {
        let Screen::Decks(view) = &mut self.screen else {
            return;
        };
        let buf = frame.buffer_mut();
        let cols = 7u16;
        let name_width = area.width.saturating_sub(cols * 3 + 2);
        let header_y = area.y;
        let header = Style::default().add_modifier(Modifier::BOLD);
        buf.set_stringn(area.x + 1, header_y, "Deck", name_width as usize, header);
        for (i, title) in ["New", "Learn", "Due"].iter().enumerate() {
            let x = area.x + 1 + name_width + cols * i as u16;
            buf.set_string(x + cols - title.len() as u16, header_y, title, header);
        }
        let footer_rows = 3u16;
        let list = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1 + footer_rows),
        );
        view.list_area = list;
        if let Some(error) = &view.error {
            let p = Paragraph::new(error.as_str())
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true });
            frame.render_widget(p, list);
            return;
        }
        if view.rows.is_empty() {
            buf.set_string(list.x + 1, list.y, "No decks.", dim());
        }
        let height = list.height as usize;
        if view.selected < view.offset {
            view.offset = view.selected;
        } else if height > 0 && view.selected >= view.offset + height {
            view.offset = view.selected + 1 - height;
        }
        for (i, row) in view.rows.iter().enumerate().skip(view.offset).take(height) {
            let y = list.y + (i - view.offset) as u16;
            draw_deck_row(buf, row, area.x, y, name_width, cols, i == view.selected);
        }
        let footer_y = list.bottom() + 1;
        if let Some(studied) = &view.studied {
            let line =
                Line::from(Span::styled(studied.clone(), dim())).alignment(Alignment::Center);
            frame.render_widget(line, Rect::new(area.x, footer_y, area.width, 1));
        }
        let hint = "Enter open · s study · Space collapse · : commands · ? help";
        let line = Line::from(Span::styled(hint, dim())).alignment(Alignment::Center);
        frame.render_widget(line, Rect::new(area.x, footer_y + 1, area.width, 1));
    }

    // ---- overview ----------------------------------------------------------

    fn draw_description(&mut self, buf: &mut Buffer, html: &str, area: Rect) -> u16 {
        if html.trim().is_empty() || area.height == 0 {
            return 0;
        }
        let side = tanki_render::CardSide {
            html: html.to_owned(),
            css: String::new(),
            body_classes: if self.services.display.night_mode {
                "nightMode night_mode".into()
            } else {
                String::new()
            },
            media_dir: self.profile.media_dir.clone(),
        };
        let options = self.services.display.render_options(
            tanki_term::GraphicsMode::None,
            area,
            true,
            &Default::default(),
        );
        let rendered = self.services.renderer.render(&side, &options);
        let used = (rendered.lines.len() as u16).min(area.height);
        self.services.renderer.paint(
            &rendered,
            0,
            Rect {
                height: used,
                ..area
            },
            buf,
        );
        used
    }

    fn draw_overview(&mut self, frame: &mut Frame, area: Rect) {
        let Screen::Overview(view) = &self.screen else {
            return;
        };
        let data = view.data.clone();
        let error = view.error.clone();
        let buf = frame.buffer_mut();
        let inner = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height.saturating_sub(1),
        );
        let mut y = inner.y;
        let Some(data) = data else {
            let text = format!(
                "The deck overview is not available: {}\n\nPress Enter to study the current deck, Esc for the deck list.",
                error.unwrap_or_default()
            );
            let p = Paragraph::new(text).wrap(Wrap { trim: true });
            frame.render_widget(p, inner);
            return;
        };
        let title = Line::from(Span::styled(
            data.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center);
        title.render_into(buf, Rect::new(inner.x, y, inner.width, 1));
        y += 2;
        let desc_area = Rect::new(
            inner.x,
            y,
            inner.width,
            inner.height.saturating_sub(10).min(12),
        );
        let used = self.draw_description(frame.buffer_mut(), &data.description, desc_area);
        let buf = frame.buffer_mut();
        y += used + u16::from(used > 0);
        let label_x = inner.x + inner.width.saturating_sub(24) / 2;
        let rows = [
            ("New:", data.new, data.new_delta, NEW),
            ("Learning:", data.learning, data.learning_delta, LEARN),
            ("To Review:", data.review, data.review_delta, DUE),
        ];
        for (label, count, delta, color) in rows {
            if y >= inner.bottom() {
                break;
            }
            buf.set_string(label_x, y, label, Style::default());
            let count_text = count.to_string();
            buf.set_string(
                label_x + 12,
                y,
                &count_text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            );
            if delta != 0 {
                buf.set_string(
                    label_x + 13 + count_text.len() as u16,
                    y,
                    format!("{delta:+}"),
                    dim(),
                );
            }
            y += 1;
        }
        y += 1;
        if y < inner.bottom() {
            let button = Line::from(vec![
                Span::styled(
                    " Study Now ",
                    Style::default().add_modifier(Modifier::REVERSED),
                ),
                Span::styled("  Enter", dim()),
            ])
            .alignment(Alignment::Center);
            button.render_into(buf, Rect::new(inner.x, y, inner.width, 1));
            y += 2;
        }
        let mut links = Vec::new();
        if data.have_user_buried || data.have_sched_buried {
            links.push("u Unbury");
        }
        links.extend(["Esc Decks", "? Help"]);
        if y < inner.bottom() {
            let line =
                Line::from(Span::styled(links.join("   "), dim())).alignment(Alignment::Center);
            line.render_into(buf, Rect::new(inner.x, y, inner.width, 1));
        }
    }

    // ---- congrats ----------------------------------------------------------

    fn draw_congrats(&mut self, frame: &mut Frame, area: Rect) {
        let Screen::Congrats(view) = &self.screen else {
            return;
        };
        let inner = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height.saturating_sub(1),
        );
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                "Congratulations! You have finished this deck for now.",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::default(),
        ];
        let mut description = String::new();
        match &view.data {
            Some(info) => {
                let elapsed = self
                    .now
                    .at
                    .saturating_duration_since(view.fetched_at)
                    .as_secs();
                let secs = if info.secs_until_next_learn >= 86_400 {
                    info.secs_until_next_learn
                } else {
                    info.secs_until_next_learn.saturating_sub(elapsed as u32)
                };
                let learn = next_learn_message(secs, info.learn_remaining);
                if !learn.is_empty() {
                    lines.push(Line::from(learn));
                    lines.push(Line::default());
                }
                if info.review_remaining {
                    lines.push(Line::from("Today's review limit has been reached, but there are still cards waiting to be reviewed. For optimum memory, consider increasing the daily limit in the options."));
                    lines.push(Line::default());
                }
                if info.new_remaining {
                    lines.push(Line::from("There are more new cards available, but the daily limit has been reached. You can increase the limit in the options, but please bear in mind that the more new cards you introduce, the higher your short-term review workload will become."));
                    lines.push(Line::default());
                }
                if info.have_sched_buried || info.have_user_buried {
                    lines.push(Line::from("One or more cards were buried, and will be shown tomorrow. You can unbury them (u) if you wish to see them immediately."));
                    lines.push(Line::default());
                }
                description = info.deck_description.clone();
            }
            None => {
                if let Some(error) = &view.error {
                    lines.push(Line::from(Span::styled(
                        format!("(details unavailable: {error})"),
                        dim(),
                    )));
                    lines.push(Line::default());
                }
            }
        }
        lines.push(Line::from(Span::styled(
            "Enter deck list · u unbury · r refresh · ? help",
            dim(),
        )));
        let width = usize::from(inner.width.max(1));
        let height = lines
            .iter()
            .map(|line| line.width().div_ceil(width).max(1))
            .sum::<usize>()
            .min(u16::MAX as usize) as u16;
        let text = Text::from(lines);
        let p = Paragraph::new(text).wrap(Wrap { trim: true });
        frame.render_widget(p, inner);
        let desc_area = Rect::new(
            inner.x,
            inner.y + height + 1,
            inner.width,
            inner.height.saturating_sub(height + 1),
        );
        self.draw_description(frame.buffer_mut(), &description, desc_area);
    }

    // ---- reviewer ----------------------------------------------------------

    fn draw_review(&mut self, frame: &mut Frame, area: Rect) {
        let show_images = self.overlay.is_none();
        let warning = match &self.screen {
            Screen::Review(view) => view.warning.clone(),
            _ => return,
        };
        let mut card_area = Rect::new(
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            area.height.saturating_sub(3),
        );
        if let Some(text) = &warning {
            let line = Line::from(Span::styled(
                format!("! {text}"),
                Style::default().fg(Color::Yellow),
            ));
            line.render_into(frame.buffer_mut(), Rect::new(area.x, area.y, area.width, 1));
            card_area.y += 1;
            card_area.height = card_area.height.saturating_sub(1);
        }
        let bar = Rect::new(
            area.x,
            area.bottom().saturating_sub(3),
            area.width,
            3.min(area.height),
        );
        if let Screen::Review(view) = &mut self.screen {
            if view.card_area != card_area {
                view.card_area = card_area;
                view.render = None;
            }
        }
        let has_card = matches!(&self.screen, Screen::Review(v) if v.reviewer.card().is_some());
        if !has_card {
            let p = Paragraph::new("No card to show. Press Esc for the overview.").style(dim());
            frame.render_widget(p, card_area);
            self.draw_review_bar(frame.buffer_mut(), bar);
            return;
        }
        self.ensure_rendered();
        let Screen::Review(view) = &mut self.screen else {
            return;
        };
        let Some(render) = &view.render else {
            return;
        };
        let card = &render.card;
        let scroll = view.scroll;
        let buf = frame.buffer_mut();
        self.services.renderer.paint(card, scroll, card_area, buf);
        // Images fully inside the viewport.
        for block in &card.images {
            if !show_images {
                continue;
            }
            if block.row < scroll {
                continue;
            }
            let top = block.row - scroll;
            if top + block.height as usize > card_area.height as usize {
                continue;
            }
            let rect = Rect::new(
                card_area.x + block.col,
                card_area.y + top as u16,
                block.width.min(card_area.width.saturating_sub(block.col)),
                block.height,
            );
            self.services.renderer.obscure_links(rect);
            match self.services.images.draw(&block.source, rect, buf) {
                ImageState::Drawn => {}
                ImageState::Loading => {
                    buf.set_stringn(
                        rect.x,
                        rect.y,
                        "[loading image]",
                        rect.width as usize,
                        dim(),
                    );
                }
                ImageState::Failed(e) => {
                    let text = format!("[image: {} ({e})]", block.alt);
                    buf.set_stringn(rect.x, rect.y, text, rect.width as usize, dim());
                }
            }
        }
        // The type-answer input over the renderer's widget.
        if let Some(input) = view.reviewer.typed() {
            if let Some(widget) = card
                .widgets
                .iter()
                .find(|w| w.kind == tanki_render::WidgetKind::TypeAnswer)
            {
                if widget.row >= scroll && widget.row - scroll < card_area.height as usize {
                    let y = card_area.y + (widget.row - scroll) as u16;
                    let x = card_area.x + widget.col;
                    let width = widget
                        .width
                        .max(20)
                        .min(card_area.right().saturating_sub(x));
                    self.services
                        .renderer
                        .obscure_links(Rect::new(x, y, width, 1));
                    let style = Style::default().add_modifier(Modifier::UNDERLINED);
                    let blank = " ".repeat(width as usize);
                    buf.set_stringn(x, y, &blank, width as usize, style);
                    let text = input.text();
                    let cursor = input.cursor_column() as u16;
                    // Keep the cursor visible in a long answer.
                    let skip = cursor.saturating_sub(width.saturating_sub(1));
                    let visible: String = skip_columns(text, skip as usize);
                    buf.set_stringn(x, y, visible, width as usize, style);
                    if view.input_focused {
                        frame.set_cursor_position((x + cursor - skip, y));
                    }
                }
            }
        }
        // More content below / above.
        let buf = frame.buffer_mut();
        let total = card.lines.len();
        if total > scroll + card_area.height as usize {
            let text = "↓ more (j/PgDn)";
            self.services.renderer.obscure_links(Rect::new(
                card_area.right().saturating_sub(text.width() as u16),
                card_area.bottom().saturating_sub(1),
                text.width() as u16,
                1,
            ));
            buf.set_string(
                card_area.right().saturating_sub(text.width() as u16),
                card_area.bottom().saturating_sub(1),
                text,
                dim(),
            );
        }
        if scroll > 0 {
            self.services.renderer.obscure_links(Rect::new(
                card_area.right().saturating_sub(1),
                card_area.y,
                1,
                1,
            ));
            buf.set_string(card_area.right().saturating_sub(1), card_area.y, "↑", dim());
        }
        self.draw_review_bar(buf, bar);
    }

    fn draw_review_bar(&mut self, buf: &mut Buffer, bar: Rect) {
        let Screen::Review(view) = &self.screen else {
            return;
        };
        let r = &view.reviewer;
        let now = self.now.at;
        let scopes = [Scope::Review, Scope::Global];
        // Row 0: separator, flag/mark, auto-advance, timer.
        let sep = "─".repeat(bar.width as usize);
        buf.set_string(bar.x, bar.y, &sep, dim());
        let mut x = bar.x + 1;
        if let Some(card) = r.card() {
            if card.queued.flag > 0 {
                let name = card
                    .config
                    .flag_names
                    .get(card.queued.flag as usize - 1)
                    .cloned()
                    .unwrap_or_default();
                let text = format!(" ⚑ {name} ");
                buf.set_string(
                    x,
                    bar.y,
                    &text,
                    Style::default().fg(flag_color(card.queued.flag)),
                );
                x += text.width() as u16;
            }
            if card.queued.marked {
                buf.set_string(x, bar.y, " ★ marked ", Style::default().fg(Color::Yellow));
                x += 10;
            }
        }
        if r.auto_advance() {
            buf.set_string(x, bar.y, " auto advance ", Style::default().fg(Color::Cyan));
        }
        if let Some(timer) = r.timer(now) {
            let text = format!(" {} ", timer_text(timer.secs));
            let style = if timer.capped {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            buf.set_string(
                bar.right().saturating_sub(text.width() as u16 + 1),
                bar.y,
                &text,
                style,
            );
        }
        let Some(card) = r.card() else {
            return;
        };
        let more = format!("More {}", self.keymap.hint(&scopes, Action::More));
        buf.set_string(
            bar.right().saturating_sub(more.width() as u16 + 1),
            bar.y + 2,
            &more,
            dim(),
        );
        match r.phase() {
            Phase::Question => {
                if card.config.show_remaining_count {
                    let c = r.counts();
                    let active = card.queued.queue;
                    let span = |n: u32, color: Color, kind: QueueKind| {
                        let mut style = Style::default().fg(color);
                        if kind == active {
                            style = style.add_modifier(Modifier::UNDERLINED);
                        }
                        Span::styled(n.to_string(), style)
                    };
                    let line = Line::from(vec![
                        span(c.new, NEW, QueueKind::New),
                        Span::raw(" + "),
                        span(c.learning, LEARN, QueueKind::Learning),
                        Span::raw(" + "),
                        span(c.review, DUE, QueueKind::Review),
                    ])
                    .alignment(Alignment::Center);
                    line.render_into(buf, Rect::new(bar.x, bar.y + 1, bar.width, 1));
                }
                let hint = self.keymap.hint(&scopes, Action::ShowAnswerOrGood);
                let line = Line::from(vec![
                    Span::styled(
                        " Show Answer ",
                        Style::default().add_modifier(Modifier::REVERSED),
                    ),
                    Span::styled(format!(" {hint}"), dim()),
                ])
                .alignment(Alignment::Center);
                line.render_into(buf, Rect::new(bar.x, bar.y + 2, bar.width, 1));
            }
            Phase::Answer | Phase::Transition => {
                let buttons = [
                    (Action::Again, "Again", Color::Red),
                    (Action::Hard, "Hard", Color::Rgb(255, 165, 0)),
                    (Action::Good, "Good", Color::Green),
                    (Action::Easy, "Easy", Color::Blue),
                ];
                let cell = 12u16;
                let total = cell * buttons.len() as u16;
                let start = bar.x + bar.width.saturating_sub(total) / 2;
                for (i, (action, label, color)) in buttons.iter().enumerate() {
                    let x = start + cell * i as u16;
                    if card.config.show_intervals {
                        if let Some(interval) = card.queued.intervals.get(i) {
                            let w = interval.width() as u16;
                            buf.set_string(
                                x + cell.saturating_sub(w) / 2,
                                bar.y + 1,
                                interval,
                                dim(),
                            );
                        }
                    }
                    let key = self.keymap.hint(&scopes, *action);
                    let text = format!(" {key} {label} ");
                    let mut style = Style::default().fg(*color).add_modifier(Modifier::REVERSED);
                    // Good is desktop's default button (`defease`).
                    if *action == Action::Good {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    let w = text.width() as u16;
                    buf.set_string(x + cell.saturating_sub(w) / 2, bar.y + 2, &text, style);
                }
            }
        }
    }

    // ---- overlays ----------------------------------------------------------

    fn popup(&self, frame: &mut Frame, area: Rect, width: u16, height: u16, title: &str) -> Rect {
        let rect = centered(area, width, height);
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        inner
    }

    fn draw_overlay(&mut self, frame: &mut Frame, area: Rect) {
        let Some(overlay) = self.overlay.take() else {
            return;
        };
        match &overlay {
            Overlay::Help { scroll } => self.draw_help(frame, area, *scroll),
            Overlay::About => {
                let inner = self.popup(frame, area, 64, 10, "About tanki");
                let text = format!(
                    "tanki {}\n\n{}\n\nPress any key to close.",
                    env!("CARGO_PKG_VERSION"),
                    crate::DISCLAIMER
                );
                frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), inner);
            }
            Overlay::Palette { input, selected } => {
                let matches = self.palette_matches(input.text());
                let height =
                    (matches.len() as u16 + 3).clamp(5, area.height.saturating_sub(2).max(5));
                let rect = Rect::new(
                    area.x + area.width.saturating_sub(60) / 2,
                    area.y + 1,
                    60.min(area.width),
                    height.min(area.height),
                );
                frame.render_widget(Clear, rect);
                let block = Block::default().borders(Borders::ALL).title(" Commands ");
                let inner = block.inner(rect);
                frame.render_widget(block, rect);
                let buf = frame.buffer_mut();
                buf.set_string(inner.x, inner.y, "> ", Style::default().fg(Color::Cyan));
                buf.set_stringn(
                    inner.x + 2,
                    inner.y,
                    input.text(),
                    inner.width.saturating_sub(2) as usize,
                    Style::default(),
                );
                let scopes = self.scopes();
                let rows = inner.height.saturating_sub(1) as usize;
                let offset = selected.saturating_sub(rows.saturating_sub(1));
                for (i, action) in matches.iter().enumerate().skip(offset).take(rows) {
                    let y = inner.y + 1 + (i - offset) as u16;
                    let style = if i == *selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    let blank = " ".repeat(inner.width as usize);
                    buf.set_string(inner.x, y, &blank, style);
                    buf.set_stringn(
                        inner.x + 1,
                        y,
                        action.description(),
                        inner.width.saturating_sub(14) as usize,
                        style,
                    );
                    let key = self.keymap.hint(&scopes, *action);
                    buf.set_string(
                        inner.right().saturating_sub(key.width() as u16 + 1),
                        y,
                        &key,
                        style.fg(DIM),
                    );
                }
                frame.set_cursor_position((inner.x + 2 + input.cursor_column() as u16, inner.y));
            }
            Overlay::Menu {
                title,
                items,
                selected,
            } => {
                let height = (items.len() as u16 + 2).min(area.height);
                let inner = self.popup(frame, area, 44, height, title);
                let rows = inner.height as usize;
                let offset = selected.saturating_sub(rows.saturating_sub(1));
                let buf = frame.buffer_mut();
                for (i, item) in items.iter().enumerate().skip(offset).take(rows) {
                    let y = inner.y + (i - offset) as u16;
                    let style = if i == *selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    buf.set_string(inner.x, y, " ".repeat(inner.width as usize), style);
                    let check = if item.checked { "✓ " } else { "  " };
                    buf.set_stringn(
                        inner.x,
                        y,
                        format!("{check}{}", item.label),
                        inner.width as usize,
                        style,
                    );
                    buf.set_string(
                        inner.right().saturating_sub(item.key.width() as u16 + 1),
                        y,
                        &item.key,
                        style.fg(DIM),
                    );
                }
            }
            Overlay::Prompt { title, input, .. } => {
                let inner = self.popup(frame, area, 64, 6, "Set Due Date");
                let p = Paragraph::new(title.as_str()).wrap(Wrap { trim: true });
                frame.render_widget(
                    p,
                    Rect {
                        height: inner.height.saturating_sub(1),
                        ..inner
                    },
                );
                let y = inner.bottom().saturating_sub(1);
                let buf = frame.buffer_mut();
                buf.set_string(inner.x, y, "> ", Style::default().fg(Color::Cyan));
                buf.set_stringn(
                    inner.x + 2,
                    y,
                    input.text(),
                    inner.width.saturating_sub(2) as usize,
                    Style::default(),
                );
                frame.set_cursor_position((inner.x + 2 + input.cursor_column() as u16, y));
            }
            Overlay::Confirm { message, yes, .. } => {
                let inner = self.popup(frame, area, 60, 8, "Confirm");
                let p = Paragraph::new(message.as_str()).wrap(Wrap { trim: true });
                frame.render_widget(
                    p,
                    Rect {
                        height: inner.height.saturating_sub(1),
                        ..inner
                    },
                );
                let on = Style::default().add_modifier(Modifier::REVERSED);
                let (y_style, n_style) = if *yes {
                    (on, Style::default())
                } else {
                    (Style::default(), on)
                };
                let line = Line::from(vec![
                    Span::styled(" Yes (y) ", y_style),
                    Span::raw("   "),
                    Span::styled(" No (n) ", n_style),
                ])
                .alignment(Alignment::Center);
                line.render_into(
                    frame.buffer_mut(),
                    Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                );
            }
            Overlay::Choice {
                title,
                message,
                options,
                selected,
                ..
            } => {
                let height = options.len() as u16 + 5;
                let inner = self.popup(frame, area, 56, height, title);
                let buf = frame.buffer_mut();
                buf.set_stringn(
                    inner.x + 1,
                    inner.y,
                    message,
                    inner.width.saturating_sub(2) as usize,
                    Style::default(),
                );
                for (i, (label, value)) in options.iter().enumerate() {
                    let y = inner.y + 2 + i as u16;
                    let style = if i == *selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else if *value == ChoiceValue::Cancel {
                        dim()
                    } else {
                        Style::default()
                    };
                    buf.set_stringn(
                        inner.x + 2,
                        y,
                        format!(" {label} "),
                        inner.width.saturating_sub(2) as usize,
                        style,
                    );
                }
            }
            Overlay::Forget {
                restore,
                reset,
                row,
            } => {
                let inner = self.popup(frame, area, 56, 7, "Reset Card to New");
                let buf = frame.buffer_mut();
                let rows = [
                    format!(
                        "[{}] Restore original position where possible",
                        if *restore { "x" } else { " " }
                    ),
                    format!(
                        "[{}] Reset repetition and lapse counts",
                        if *reset { "x" } else { " " }
                    ),
                    "    OK (Enter)   Cancel (Esc)".to_owned(),
                ];
                for (i, text) in rows.iter().enumerate() {
                    let style = if i == *row {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    buf.set_stringn(
                        inner.x + 1,
                        inner.y + 1 + i as u16,
                        text,
                        inner.width.saturating_sub(2) as usize,
                        style,
                    );
                }
            }
            Overlay::Zoom { index } => self.draw_zoom(frame, area, *index),
        }
        self.overlay = Some(overlay);
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect, scroll: usize) {
        let width = area.width.saturating_sub(4).min(76);
        let height = area.height.saturating_sub(2);
        let title = format!("Keys: {}", self.screen.scope().title());
        let inner = self.popup(frame, area, width, height, &title);
        let active = self.scopes();
        let mut lines: Vec<Line> = Vec::new();
        for scope in &active {
            lines.push(Line::from(Span::styled(
                scope.title(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for (action, keys) in self.keymap.help_rows(*scope, &active) {
                let keys: Vec<String> = keys.iter().map(ToString::to_string).collect();
                let keys = keys.join(", ");
                lines.push(Line::from(vec![
                    Span::styled(format!("  {keys:<22} "), Style::default().fg(Color::Cyan)),
                    Span::raw(action.description()),
                ]));
            }
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(crate::DISCLAIMER, dim())));
        let p = Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((scroll.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(p, inner);
    }

    fn draw_zoom(&mut self, frame: &mut Frame, area: Rect, index: usize) {
        let Screen::Review(view) = &self.screen else {
            return;
        };
        let Some(block) = view
            .render
            .as_ref()
            .and_then(|r| r.card.images.get(index))
            .cloned()
        else {
            return;
        };
        let count = view.render.as_ref().map_or(0, |r| r.card.images.len());
        let buf = frame.buffer_mut();
        let title = format!(
            " Image {} of {count}{}  ·  n/p next/previous · Esc close ",
            index + 1,
            if block.alt.is_empty() {
                String::new()
            } else {
                format!(": {}", block.alt)
            }
        );
        buf.set_stringn(
            area.x,
            area.y,
            title,
            area.width as usize,
            Style::default().add_modifier(Modifier::REVERSED),
        );
        let rect = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );
        match self.services.images.draw(&block.source, rect, buf) {
            ImageState::Drawn => {}
            ImageState::Loading => {
                buf.set_string(rect.x + 1, rect.y + 1, "Loading image...", dim());
            }
            ImageState::Failed(e) => {
                buf.set_stringn(
                    rect.x + 1,
                    rect.y + 1,
                    format!("Cannot show the image: {e}"),
                    rect.width as usize,
                    dim(),
                );
            }
        }
    }

    // ---- status line -------------------------------------------------------

    fn draw_status(&self, buf: &mut Buffer, area: Rect) {
        let base = Style::default().bg(Color::Rgb(40, 40, 40)).fg(Color::Gray);
        buf.set_string(area.x, area.y, " ".repeat(area.width as usize), base);
        let counts = match &self.screen {
            Screen::Review(view) => {
                let c = view.reviewer.counts();
                Some((c.new, c.learning, c.review))
            }
            _ => self.totals,
        };
        let mut left = vec![Span::styled(
            format!(" {} ", self.profile.name),
            base.add_modifier(Modifier::BOLD),
        )];
        if let Some((n, l, r)) = counts {
            left.push(Span::styled("│ ", base));
            for (i, (value, color)) in [(n, NEW), (l, LEARN), (r, DUE)].into_iter().enumerate() {
                if i > 0 {
                    left.push(Span::styled(" ", base));
                }
                let span = count_span(value, color);
                left.push(Span::styled(span.content, base.patch(span.style)));
            }
            left.push(Span::styled(" ", base));
        }
        let audio = match self.services.audio.status() {
            AudioStatus::Mpv => "♪ mpv".to_owned(),
            AudioStatus::Builtin => "♪ on".to_owned(),
            AudioStatus::NoDevice(_) => "♪ no device".to_owned(),
            AudioStatus::Off => "♪ off".to_owned(),
        };
        let audio = if self.services.audio.is_playing() {
            format!("{audio} ▶")
        } else {
            audio
        };
        let right = format!(
            " {audio} │ ▣ {} │ ? help ",
            graphics_name(self.services.images.mode())
        );
        let right_w = right.width() as u16;
        let left_line = Line::from(left);
        let left_w = left_line.width() as u16;
        left_line.render_into(buf, area);
        let rx = area.right().saturating_sub(right_w);
        buf.set_string(rx, area.y, &right, base);
        if let Some(message) = self.visible_message() {
            let style = if message.error {
                base.fg(Color::LightRed)
            } else {
                base.fg(Color::White)
            };
            let x = area.x + left_w + 1;
            let max = rx.saturating_sub(x + 1) as usize;
            buf.set_stringn(x, area.y, &message.text, max, style);
        }
    }
}

fn draw_deck_row(
    buf: &mut Buffer,
    row: &DeckRow,
    x0: u16,
    y: u16,
    name_width: u16,
    cols: u16,
    selected: bool,
) {
    let total = name_width + cols * 3 + 2;
    let base = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    buf.set_string(x0, y, " ".repeat(total as usize), base);
    let marker = match (row.has_children, row.collapsed) {
        (true, true) => "▸ ",
        (true, false) => "▾ ",
        _ => "  ",
    };
    let name = format!("{}{marker}{}", "  ".repeat(row.depth), row.name);
    let name_style = if row.filtered {
        base.fg(Color::Cyan)
    } else {
        base
    };
    buf.set_stringn(x0 + 1, y, name, name_width as usize, name_style);
    for (i, (n, color)) in [(row.new, NEW), (row.learning, LEARN), (row.review, DUE)]
        .into_iter()
        .enumerate()
    {
        let text = n.to_string();
        let x = x0 + 1 + name_width + cols * i as u16 + cols - text.len() as u16;
        let style = if n == 0 { base.fg(DIM) } else { base.fg(color) };
        buf.set_string(x, y, text, style);
    }
}

fn skip_columns(text: &str, columns: usize) -> String {
    let mut skipped = 0;
    let mut out = String::new();
    for c in text.chars() {
        if skipped < columns {
            skipped += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            continue;
        }
        out.push(c);
    }
    out
}

/// Draws a `Line` into a buffer area (ratatui's `Widget` without a frame).
trait RenderInto {
    fn render_into(self, buf: &mut Buffer, area: Rect);
}

impl RenderInto for Line<'_> {
    fn render_into(self, buf: &mut Buffer, area: Rect) {
        use ratatui::widgets::Widget;
        self.render(area.intersection(buf.area), buf);
    }
}
