use super::*;
use crate::app::{App, Profile, Services, Start};
use crate::keymap::Action;
use crate::services::{Display, NoImages};
use crate::testing::{card, t0, FakeAudio, FakeBackend};
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use tanki_render::{Line, Rgb, Span, Style};

#[derive(Clone, Default)]
struct Output(Rc<RefCell<Vec<u8>>>);
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Output {
    fn take(&self) -> String {
        String::from_utf8(std::mem::take(&mut *self.0.borrow_mut())).unwrap()
    }
}

struct FixtureRenderer;
impl CardRenderer for FixtureRenderer {
    fn render(&self, side: &CardSide, _: &RenderOptions) -> RenderedCard {
        rendered(Some(&side.html), "read")
    }
    fn paint(&self, card: &RenderedCard, scroll: usize, area: Rect, buffer: &mut Buffer) {
        crate::services::paint_lines(&card.lines, scroll, area, buffer);
    }
}

fn rendered(target: Option<&str>, text: &str) -> RenderedCard {
    RenderedCard {
        lines: vec![Line {
            spans: vec![Span {
                text: text.into(),
                link: target.map(str::to_owned),
                style: Style {
                    fg: Some(Rgb(1, 2, 3)),
                    bold: true,
                    ..Default::default()
                },
                ..Default::default()
            }],
        }],
        ..Default::default()
    }
}
fn paint(renderer: &impl CardRenderer, card: &RenderedCard, area: Rect, visible: bool) -> Buffer {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 8));
    renderer.begin_frame();
    renderer.paint(card, 0, area, &mut buffer);
    renderer.finish_frame(&buffer, visible);
    buffer
}
fn open(uri: &str) -> String {
    format!("\x1b]8;;{uri}\x1b\\")
}

#[test]
fn only_safe_absolute_web_and_mail_uris_are_terminal_actions() {
    for uri in [
        "https://example.org/a?b=c#d",
        "HTTP://example.org",
        "mailto:study@example.org",
        "https://例え.jp/道",
    ] {
        assert!(safe_uri(uri), "{uri:?}");
    }
    for uri in [
        "",
        "//example.org",
        "#answer",
        "https://",
        "https:///x",
        "http:example.org",
        "mailto:",
        "javascript:alert(1)",
        "data:text/html,a",
        "file:///tmp/a",
        "command:run",
        "ssh://host",
        "https://example.org/a b",
        "https://example.org/\u{2028}x",
    ] {
        assert!(!safe_uri(uri), "{uri:?}");
    }
    for code in (0..=31).chain(127..=159) {
        let uri = format!("https://example.org/{}x", char::from_u32(code).unwrap());
        assert!(!safe_uri(&uri), "control {code}");
    }
    assert!(!safe_uri(&format!(
        "https://example.org/{}",
        "x".repeat(8192)
    )));
}

#[test]
fn osc8_wraps_actual_cells_without_changing_symbols_positions_or_styles() {
    let links = Links::default();
    let renderer = LinkedRenderer::new(FixtureRenderer, links.clone());
    let card = rendered(Some("https://example.org"), "日e\u{301}👩‍💻");
    let area = Rect::new(3, 2, 6, 1);
    let buffer = paint(&renderer, &card, area, true);
    let mut expected = Buffer::empty(buffer.area);
    FixtureRenderer.paint(&card, 0, area, &mut expected);
    assert_eq!(buffer, expected);
    assert_eq!(
        links.0.borrow().targets.keys().copied().collect::<Vec<_>>(),
        [(2, 3), (2, 5), (2, 6)]
    );
    assert_eq!(buffer[(3, 2)].fg, Color::Rgb(1, 2, 3));
    assert!(buffer[(3, 2)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(5, 2)].symbol(), "e\u{301}");
    let output = Output::default();
    let mut backend = HyperlinkBackend::new(output.clone(), links);
    backend
        .draw(Buffer::empty(buffer.area).diff_iter(&buffer))
        .unwrap();
    let bytes = output.take();
    assert!(bytes.contains(&open("https://example.org")));
    assert!(bytes.contains("\x1b[3;4H"), "{bytes:?}");
    // NO_COLOR can suppress color escape bytes. The decorator must produce
    // exactly Crossterm's cursor/style/text bytes after removing OSC 8.
    let baseline = Output::default();
    CrosstermBackend::new(baseline.clone())
        .draw(Buffer::empty(buffer.area).diff_iter(&buffer))
        .unwrap();
    assert_eq!(
        bytes
            .replace(&open("https://example.org"), "")
            .replace(std::str::from_utf8(CLOSE).unwrap(), ""),
        baseline.take()
    );
    assert!(bytes.contains("日") && bytes.contains("e\u{301}") && bytes.contains("👩‍💻"));
    assert!(bytes.ends_with(std::str::from_utf8(CLOSE).unwrap()));
    assert!(buffer
        .content
        .iter()
        .all(|cell| !cell.symbol().contains('\x1b')));
}

#[test]
fn link_target_changes_and_removals_repaint_identical_text() {
    let links = Links::default();
    let renderer = LinkedRenderer::new(FixtureRenderer, links.clone());
    let output = Output::default();
    let mut backend = HyperlinkBackend::new(output.clone(), links);
    let area = Rect::new(1, 1, 10, 1);
    let first = paint(
        &renderer,
        &rendered(Some("https://one.example"), "same"),
        area,
        true,
    );
    backend
        .draw(Buffer::empty(first.area).diff_iter(&first))
        .unwrap();
    output.take();
    let second = paint(
        &renderer,
        &rendered(Some("https://two.example"), "same"),
        area,
        true,
    );
    assert!(first.diff(&second).is_empty());
    backend.draw(first.diff_iter(&second)).unwrap();
    let bytes = output.take();
    assert!(bytes.contains(&open("https://two.example")) && bytes.contains("same"));
    assert!(!bytes.contains("one.example"));
    let third = paint(&renderer, &rendered(None, "same"), area, true);
    assert!(second.diff(&third).is_empty());
    backend.draw(second.diff_iter(&third)).unwrap();
    let bytes = output.take();
    assert!(bytes.contains("same") && !bytes.contains("\x1b]8;;https:"));
    assert!(bytes.starts_with(std::str::from_utf8(CLOSE).unwrap()));
    assert!(bytes.ends_with(std::str::from_utf8(CLOSE).unwrap()));
}

#[test]
fn unlinked_text_closes_link_and_control_bytes_never_reach_osc8() {
    let links = Links::default();
    let renderer = LinkedRenderer::new(FixtureRenderer, links.clone());
    let mut card = rendered(Some("https://example.org"), "linked");
    card.lines[0].spans.push(Span {
        text: " plain".into(),
        ..Default::default()
    });
    card.lines[0].spans.push(Span {
        text: " unsafe".into(),
        link: Some("https://bad/\x07\x1b\\INJECT".into()),
        ..Default::default()
    });
    let buffer = paint(&renderer, &card, Rect::new(0, 0, 30, 1), true);
    let output = Output::default();
    let mut backend = HyperlinkBackend::new(output.clone(), links);
    backend
        .draw(Buffer::empty(buffer.area).diff_iter(&buffer))
        .unwrap();
    let bytes = output.take();
    let plain = bytes.find("plain").unwrap();
    let linked = bytes.find("linked").unwrap();
    assert!(bytes[linked..plain].contains(std::str::from_utf8(CLOSE).unwrap()));
    assert!(!bytes.contains("INJECT") && !bytes.contains('\x07'));
}

#[test]
fn scroll_clipping_and_repainting_the_same_area_replace_link_metadata() {
    let links = Links::default();
    let renderer = LinkedRenderer::new(FixtureRenderer, links.clone());
    let mut card = rendered(Some("https://hidden.example"), "above");
    card.lines.push(
        rendered(Some("https://shown.example"), "日本")
            .lines
            .remove(0),
    );
    let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 3));
    renderer.begin_frame();
    renderer.paint(&card, 1, Rect::new(7, 1, 3, 1), &mut buffer);
    renderer.finish_frame(&buffer, true);
    assert_eq!(buffer[(7, 1)].symbol(), "日");
    assert_eq!(
        links.0.borrow().targets.keys().copied().collect::<Vec<_>>(),
        [(1, 7)]
    );
    assert!(links
        .0
        .borrow()
        .targets
        .values()
        .all(|target| target == "https://shown.example"));
    renderer.paint(&rendered(None, "日"), 0, Rect::new(7, 1, 3, 1), &mut buffer);
    renderer.finish_frame(&buffer, true);
    assert!(links.0.borrow().targets.is_empty());
}

#[test]
fn nonlink_widgets_can_occlude_identical_cells_without_leaking_the_anchor() {
    let links = Links::default();
    let renderer = LinkedRenderer::new(FixtureRenderer, links.clone());
    let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 3));
    renderer.begin_frame();
    renderer.paint(
        &rendered(Some("https://example.org"), "same"),
        0,
        Rect::new(0, 0, 10, 1),
        &mut buffer,
    );
    // A UI widget can contain exactly the same text/style as the old anchor.
    // Its ownership, rather than cell equality alone, removes the link.
    renderer.obscure_links(Rect::new(0, 0, 4, 1));
    renderer.finish_frame(&buffer, true);
    assert!(links.0.borrow().targets.is_empty());
}

#[test]
fn drawing_error_still_attempts_to_close_the_hyperlink() {
    struct FailOnce {
        output: Output,
        failed: bool,
    }
    impl Write for FailOnce {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes == b"r" && !self.failed {
                self.failed = true;
                return Err(io::Error::other("simulated output failure"));
            }
            self.output.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let links = Links::default();
    let renderer = LinkedRenderer::new(FixtureRenderer, links.clone());
    let buffer = paint(
        &renderer,
        &rendered(Some("https://example.org"), "read"),
        Rect::new(0, 0, 10, 1),
        true,
    );
    let output = Output::default();
    let mut backend = HyperlinkBackend::new(
        FailOnce {
            output: output.clone(),
            failed: false,
        },
        links,
    );
    // Crossterm prints one cell at a time; fail when it writes the first cell.
    let error = backend
        .draw(Buffer::empty(buffer.area).diff_iter(&buffer))
        .unwrap_err();
    assert!(error.to_string().contains("simulated output failure"));
    let bytes = output.take();
    assert!(bytes.contains(&open("https://example.org")));
    assert!(bytes.ends_with(std::str::from_utf8(CLOSE).unwrap()));
}

#[test]
fn href_only_changes_never_replay_wide_continuations_or_image_cells() {
    let links = Links::default();
    let renderer = LinkedRenderer::new(FixtureRenderer, links.clone());
    let output = Output::default();
    let mut backend = HyperlinkBackend::new(output.clone(), links);
    let first = paint(
        &renderer,
        &rendered(Some("https://old.example"), "a"),
        Rect::new(2, 0, 4, 1),
        true,
    );
    backend
        .draw(Buffer::empty(first.area).diff_iter(&first))
        .unwrap();
    output.take();
    let second = paint(
        &renderer,
        &rendered(None, "日"),
        Rect::new(1, 0, 4, 1),
        true,
    );
    backend.draw(first.diff_iter(&second)).unwrap();
    let bytes = output.take();
    assert_eq!(bytes.matches("日").count(), 1);
    assert!(!bytes.contains('a'));
    let linked = paint(
        &renderer,
        &rendered(Some("https://old.example"), "a"),
        Rect::new(0, 0, 4, 1),
        true,
    );
    backend.draw(second.diff_iter(&linked)).unwrap();
    output.take();
    renderer.begin_frame();
    let mut image = Buffer::empty(linked.area);
    let symbol = "\x1b_Gf=100;IMAGE\x1b\\";
    image[(0, 0)]
        .set_symbol(symbol)
        .set_diff_option(CellDiffOption::ForcedWidth(
            std::num::NonZeroU16::new(1).unwrap(),
        ));
    image[(1, 0)].set_diff_option(CellDiffOption::Skip);
    renderer.finish_frame(&image, true);
    backend.draw(linked.diff_iter(&image)).unwrap();
    let bytes = output.take();
    assert!(bytes.contains(symbol));
    assert!(!bytes.contains("old.example") && !bytes.contains('a'));
}

#[test]
fn app_overlays_small_viewports_and_screen_changes_remove_all_links() {
    let links = Links::default();
    let backend = FakeBackend::new(vec![card(
        1,
        "https://question.example",
        "https://answer.example",
    )]);
    let mut app = App::new(
        &backend,
        Services {
            renderer: Box::new(LinkedRenderer::new(FixtureRenderer, links.clone())),
            images: Box::new(NoImages),
            audio: Box::new(FakeAudio::default()),
            display: Display {
                cell_px: None,
                fg: None,
                bg: None,
                night_mode: true,
            },
            opener: Box::new(|_| Ok(())),
        },
        Profile {
            name: "fixture".into(),
            dir: "/tmp/tanki-link-test".into(),
            media_dir: "/tmp/tanki-link-test/collection.media".into(),
        },
        Start::Review,
        t0(),
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert!(!links.0.borrow().targets.is_empty());
    let output = Output::default();
    let mut emitter = HyperlinkBackend::new(output.clone(), links.clone());
    let before = terminal.backend().buffer().clone();
    emitter
        .draw(Buffer::empty(before.area).diff_iter(&before))
        .unwrap();
    assert!(output.take().contains(&open("https://question.example")));
    app.dispatch(Action::Help);
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert!(links.0.borrow().targets.is_empty());
    emitter
        .draw(before.diff_iter(terminal.backend().buffer()))
        .unwrap();
    let overlay = output.take();
    assert!(!overlay.contains("https://question.example"));
    assert!(overlay.ends_with(std::str::from_utf8(CLOSE).unwrap()));
    app.overlay = None;
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert!(!links.0.borrow().targets.is_empty());
    app.go_decks();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert!(links.0.borrow().targets.is_empty());
    app.start_review();
    let mut small = Terminal::new(TestBackend::new(20, 4)).unwrap();
    small.draw(|frame| app.draw(frame)).unwrap();
    assert!(links.0.borrow().targets.is_empty());
}
