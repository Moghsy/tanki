//! Sync progress and conflict widget (Sync stream, PLAN 8.9).
//!
//! Self-contained state plus a ratatui widget. The app drives it with
//! `on_progress` (from `CollectionActor::progress`), `on_response` (from
//! `PendingOperation::try_recv`) and `on_key`, and executes the returned
//! [`SyncIntent`]: start a sync command, cancel, persist a redirected
//! endpoint, forget a rejected login, refresh after changes, or close.
//! Uses only `tanki_core` types.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Gauge, Paragraph, Widget, Wrap},
};
use tanki_core::{CoreError, DataResponse, OperationProgress, SyncReport, SyncRequired};

/// What the app should send to the actor, wrapped in
/// `DataCommand::Sync(..)` with the profile's stored credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncRequest {
    /// `SyncCommand::Run { direction: Normal, media }`.
    Normal { media: bool },
    /// `SyncCommand::Run { direction: Upload, .. }`.
    Upload,
    /// `SyncCommand::Run { direction: Download, .. }`.
    Download,
    /// `SyncCommand::Media`.
    Media,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncIntent {
    None,
    /// Queue this command (`actor.start_data`) and poll it.
    Start(SyncRequest),
    /// `actor.cancel_operation()`.
    Cancel,
    /// Save this validated endpoint to the config and credentials.
    SaveEndpoint(String),
    /// The server rejected the stored login: remove it from credentials.
    ForgetLogin,
    /// The sync finished; reload decks and queues. Carries a redirect, if any.
    Finished {
        new_endpoint: Option<String>,
    },
    /// Hide the widget.
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Upload,
    Download,
    Cancel,
}

#[derive(Debug, Clone)]
pub enum SyncStage {
    Hidden,
    /// Shown before the first sync with a newly configured server.
    FirstTime {
        endpoint: String,
    },
    Running {
        request: SyncRequest,
        progress: OperationProgress,
        cancelling: bool,
    },
    Conflict {
        upload: bool,
        download: bool,
        selected: ConflictChoice,
    },
    Done {
        message: String,
        server_message: String,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct SyncUi {
    stage: SyncStage,
    /// Sync media after the collection.
    media: bool,
}

impl Default for SyncUi {
    fn default() -> Self {
        Self::new(true)
    }
}

const FIRST_TIME: &str = "If this collection has synced with another server before (for example \
     AnkiWeb), the first sync with this server is a one-way full upload or download that replaces \
     one side. Every other device (desktop, phone) must then be pointed at this server too.";
const CONFLICT: &str = "This collection and the server's copy have both changed in ways that \
     cannot be merged. Choose which side to keep; this replaces the other side.";
const UPLOAD: &str =
    "Upload: replace the server's copy with this collection (other devices then download it)";
const DOWNLOAD: &str =
    "Download: replace this collection with the server's copy (changes made here since the last sync are lost; a backup is taken first)";

impl SyncUi {
    pub fn new(media: bool) -> Self {
        Self {
            stage: SyncStage::Hidden,
            media,
        }
    }

    pub fn stage(&self) -> &SyncStage {
        &self.stage
    }

    pub fn is_visible(&self) -> bool {
        !matches!(self.stage, SyncStage::Hidden)
    }

    /// True while a command is with the actor (keep polling).
    pub fn is_running(&self) -> bool {
        matches!(self.stage, SyncStage::Running { .. })
    }

    /// Asks before the first sync with a new server; Enter starts it.
    pub fn first_time(&mut self, endpoint: impl Into<String>) {
        self.stage = SyncStage::FirstTime {
            endpoint: endpoint.into(),
        };
    }

    /// Shows the running state; the app sends the returned request.
    pub fn start(&mut self, request: SyncRequest) -> SyncIntent {
        self.stage = SyncStage::Running {
            request,
            progress: OperationProgress::default(),
            cancelling: false,
        };
        SyncIntent::Start(request)
    }

    /// A normal sync, with media if configured.
    pub fn start_normal(&mut self) -> SyncIntent {
        self.start(SyncRequest::Normal { media: self.media })
    }

    pub fn on_progress(&mut self, latest: OperationProgress) {
        if let SyncStage::Running { progress, .. } = &mut self.stage {
            *progress = latest;
        }
    }

    pub fn on_response(&mut self, result: Result<DataResponse, CoreError>) -> SyncIntent {
        let SyncStage::Running { request, .. } = self.stage else {
            return SyncIntent::None;
        };
        match result {
            Ok(DataResponse::Synced(SyncReport {
                required: SyncRequired::Full { upload, download },
                new_endpoint,
                ..
            })) => {
                self.stage = SyncStage::Conflict {
                    upload,
                    download,
                    selected: ConflictChoice::Cancel,
                };
                new_endpoint.map_or(SyncIntent::None, SyncIntent::SaveEndpoint)
            }
            Ok(DataResponse::Synced(report)) => {
                self.done("Sync complete.", report.server_message);
                SyncIntent::Finished {
                    new_endpoint: report.new_endpoint,
                }
            }
            Ok(_) => match request {
                // The actor syncs media after a full upload or download.
                SyncRequest::Upload => {
                    self.done("Full upload complete.", String::new());
                    SyncIntent::Finished { new_endpoint: None }
                }
                SyncRequest::Download => {
                    self.done("Full download complete.", String::new());
                    SyncIntent::Finished { new_endpoint: None }
                }
                SyncRequest::Normal { .. } | SyncRequest::Media => {
                    self.done("Sync complete.", String::new());
                    SyncIntent::Finished { new_endpoint: None }
                }
            },
            Err(CoreError::Interrupted) => {
                self.stage = SyncStage::Failed {
                    message: "Sync cancelled. Nothing was lost; sync again when ready.".into(),
                };
                SyncIntent::None
            }
            Err(error) if tanki_core::sync_ops::is_auth_error(&error) => {
                self.stage = SyncStage::Failed {
                    message: error.to_string(),
                };
                SyncIntent::ForgetLogin
            }
            Err(error) => {
                self.stage = SyncStage::Failed {
                    message: error.to_string(),
                };
                SyncIntent::None
            }
        }
    }

    fn done(&mut self, message: &str, server_message: String) {
        self.stage = SyncStage::Done {
            message: message.into(),
            server_message,
        };
    }

    pub fn on_key(&mut self, key: KeyEvent) -> SyncIntent {
        let ctrl_c =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        match &mut self.stage {
            SyncStage::Hidden => SyncIntent::None,
            SyncStage::FirstTime { .. } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => self.start_normal(),
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => self.close(),
                _ => SyncIntent::None,
            },
            SyncStage::Running { cancelling, .. } => {
                if (ctrl_c || matches!(key.code, KeyCode::Esc | KeyCode::Char('c'))) && !*cancelling
                {
                    *cancelling = true;
                    SyncIntent::Cancel
                } else {
                    SyncIntent::None
                }
            }
            SyncStage::Conflict {
                upload,
                download,
                selected,
            } => {
                let choices: Vec<ConflictChoice> = [
                    (*upload, ConflictChoice::Upload),
                    (*download, ConflictChoice::Download),
                    (true, ConflictChoice::Cancel),
                ]
                .into_iter()
                .filter_map(|(ok, choice)| ok.then_some(choice))
                .collect();
                let index = choices.iter().position(|c| c == selected).unwrap_or(0);
                let chosen = match key.code {
                    KeyCode::Char('u') if *upload => Some(ConflictChoice::Upload),
                    KeyCode::Char('d') if *download => Some(ConflictChoice::Download),
                    KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('q') => {
                        Some(ConflictChoice::Cancel)
                    }
                    KeyCode::Enter => Some(*selected),
                    KeyCode::Down | KeyCode::Right | KeyCode::Tab | KeyCode::Char('j') => {
                        *selected = choices[(index + 1) % choices.len()];
                        None
                    }
                    KeyCode::Up | KeyCode::Left | KeyCode::BackTab | KeyCode::Char('k') => {
                        *selected = choices[(index + choices.len() - 1) % choices.len()];
                        None
                    }
                    _ => None,
                };
                match chosen {
                    Some(ConflictChoice::Upload) => self.start(SyncRequest::Upload),
                    Some(ConflictChoice::Download) => self.start(SyncRequest::Download),
                    Some(ConflictChoice::Cancel) => self.close(),
                    None => SyncIntent::None,
                }
            }
            SyncStage::Done { .. } | SyncStage::Failed { .. } => match key.code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') | KeyCode::Char('q') => {
                    self.close()
                }
                _ => SyncIntent::None,
            },
        }
    }

    fn close(&mut self) -> SyncIntent {
        self.stage = SyncStage::Hidden;
        SyncIntent::Close
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn running_title(request: SyncRequest) -> &'static str {
    match request {
        SyncRequest::Normal { .. } => "Syncing",
        SyncRequest::Upload => "Uploading collection",
        SyncRequest::Download => "Downloading collection",
        SyncRequest::Media => "Syncing media",
    }
}

impl Widget for &SyncUi {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let hint = |text: &'static str| Line::from(Span::styled(text, dim));
        let (title, lines, gauge): (&str, Vec<Line>, Option<(f64, String)>) = match &self.stage {
            SyncStage::Hidden => return,
            SyncStage::FirstTime { endpoint } => (
                " Sync: new server ",
                vec![
                    Line::from(vec![
                        Span::raw("Server: "),
                        Span::styled(endpoint.clone(), bold),
                    ]),
                    Line::raw(""),
                    Line::raw(FIRST_TIME),
                    Line::raw(""),
                    hint("Enter sync now   Esc not now"),
                ],
                None,
            ),
            SyncStage::Running {
                request,
                progress,
                cancelling,
            } => {
                let stage = if progress.stage.is_empty() {
                    "Connecting…".to_owned()
                } else {
                    progress.stage.clone()
                };
                let gauge = match (progress.current, progress.total) {
                    (Some(current), Some(total)) if total > 0 => Some((
                        (current as f64 / total as f64).clamp(0.0, 1.0),
                        format!("{current} / {total}"),
                    )),
                    _ => None,
                };
                (
                    " Sync ",
                    vec![
                        Line::styled(running_title(*request), bold),
                        Line::raw(stage),
                        Line::raw(""),
                        if *cancelling {
                            hint("Cancelling…")
                        } else {
                            hint("Esc cancel")
                        },
                    ],
                    gauge,
                )
            }
            SyncStage::Conflict {
                upload,
                download,
                selected,
            } => {
                let option = |ok: bool, choice: ConflictChoice, key: &str, text: &str| {
                    let marker = if *selected == choice { "> " } else { "  " };
                    let style = if !ok {
                        dim
                    } else if *selected == choice {
                        bold
                    } else {
                        Style::default()
                    };
                    let suffix = if ok { "" } else { " (not available)" };
                    Line::styled(format!("{marker}[{key}] {text}{suffix}"), style)
                };
                (
                    " Sync: choose one side ",
                    vec![
                        Line::raw(CONFLICT),
                        Line::raw(""),
                        option(*upload, ConflictChoice::Upload, "u", UPLOAD),
                        option(*download, ConflictChoice::Download, "d", DOWNLOAD),
                        option(
                            true,
                            ConflictChoice::Cancel,
                            "Esc",
                            "Cancel: change nothing",
                        ),
                    ],
                    None,
                )
            }
            SyncStage::Done {
                message,
                server_message,
            } => {
                let mut lines = vec![Line::styled(message.clone(), bold)];
                if !server_message.is_empty() {
                    lines.push(Line::raw(format!("Server: {server_message}")));
                }
                lines.push(Line::raw(""));
                lines.push(hint("Enter close"));
                (" Sync ", lines, None)
            }
            SyncStage::Failed { message } => (
                " Sync failed ",
                vec![
                    Line::raw(message.clone()),
                    Line::raw(""),
                    hint("Enter close"),
                ],
                None,
            ),
        };
        let width = area.width.saturating_sub(4).clamp(20, 72);
        let inner_width = width.saturating_sub(2).max(1);
        // Measure the wrapped text by rendering it once off screen.
        let probe_area = Rect::new(0, 0, inner_width, area.height.max(1));
        let mut probe = Buffer::empty(probe_area);
        Paragraph::new(lines.clone())
            .wrap(Wrap { trim: false })
            .render(probe_area, &mut probe);
        let text_rows = (0..probe_area.height)
            .rev()
            .find(|&y| (0..inner_width).any(|x| probe[(x, y)].symbol() != " "))
            .map_or(1, |y| y + 1);
        let height = text_rows + 2 + u16::from(gauge.is_some()) * 2;
        let rect = centered(area, width, height);
        Clear.render(rect, buf);
        let block = Block::bordered().title(title);
        let inner = block.inner(rect);
        block.render(rect, buf);
        let [text_area, gauge_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(if gauge.is_some() { 2 } else { 0 }),
        ])
        .areas(inner);
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(text_area, buf);
        if let Some((ratio, label)) = gauge {
            let bar = Rect {
                y: gauge_area.y + 1,
                height: 1,
                ..gauge_area
            };
            Gauge::default().ratio(ratio).label(label).render(bar, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::buffer_text;
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn draw(ui: &SyncUi, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(ui, frame.area()))
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn report(required: SyncRequired) -> Result<DataResponse, CoreError> {
        Ok(DataResponse::Synced(SyncReport {
            required,
            server_message: String::new(),
            new_endpoint: None,
            synced_before: false,
        }))
    }

    #[test]
    fn hidden_draws_nothing_and_ignores_input() {
        let mut ui = SyncUi::default();
        assert!(!ui.is_visible());
        assert!(draw(&ui, 30, 5).iter().all(String::is_empty));
        assert_eq!(ui.on_key(key(KeyCode::Enter)), SyncIntent::None);
        assert_eq!(
            ui.on_response(report(SyncRequired::NoChanges)),
            SyncIntent::None
        );
    }

    #[test]
    fn first_time_warning_then_normal_sync() {
        let mut ui = SyncUi::new(true);
        ui.first_time("https://sync.example.org/");
        assert_eq!(
            draw(&ui, 60, 16),
            [
                "",
                "",
                "  ┌ Sync: new server ────────────────────────────────────┐",
                "  │Server: https://sync.example.org/                     │",
                "  │                                                      │",
                "  │If this collection has synced with another server     │",
                "  │before (for example AnkiWeb), the first sync with this│",
                "  │server is a one-way full upload or download that      │",
                "  │replaces one side. Every other device (desktop, phone)│",
                "  │must then be pointed at this server too.              │",
                "  │                                                      │",
                "  │Enter sync now   Esc not now                          │",
                "  └──────────────────────────────────────────────────────┘",
                "",
                "",
                "",
            ]
        );
        assert_eq!(
            ui.on_key(key(KeyCode::Enter)),
            SyncIntent::Start(SyncRequest::Normal { media: true })
        );
        assert!(ui.is_running());
        let mut ui = SyncUi::new(false);
        ui.first_time("x");
        assert_eq!(ui.on_key(key(KeyCode::Esc)), SyncIntent::Close);
        assert!(!ui.is_visible());
    }

    #[test]
    fn running_shows_progress_and_cancels_once() {
        let mut ui = SyncUi::new(true);
        ui.start(SyncRequest::Upload);
        ui.on_progress(OperationProgress {
            stage: "Transferring collection".into(),
            current: Some(512),
            total: Some(2048),
        });
        let screen = draw(&ui, 50, 10);
        assert_eq!(
            screen,
            [
                "",
                "  ┌ Sync ──────────────────────────────────────┐",
                "  │Uploading collection                        │",
                "  │Transferring collection                     │",
                "  │                                            │",
                "  │Esc cancel                                  │",
                "  │                                            │",
                "  │███████████      512 / 2048                 │",
                "  └────────────────────────────────────────────┘",
                "",
            ]
        );
        assert_eq!(ui.on_key(key(KeyCode::Esc)), SyncIntent::Cancel);
        assert_eq!(ui.on_key(key(KeyCode::Esc)), SyncIntent::None);
        assert!(draw(&ui, 50, 10)[5].contains("Cancelling…"));
        assert_eq!(
            ui.on_response(Err(CoreError::Interrupted)),
            SyncIntent::None
        );
        assert!(
            matches!(ui.stage(), SyncStage::Failed { message } if message.contains("cancelled"))
        );
        assert_eq!(ui.on_key(key(KeyCode::Enter)), SyncIntent::Close);
    }

    #[test]
    fn conflict_choice_replaces_the_other_side() {
        let mut ui = SyncUi::new(true);
        ui.start_normal();
        assert_eq!(
            ui.on_response(Ok(DataResponse::Synced(SyncReport {
                required: SyncRequired::Full {
                    upload: true,
                    download: true
                },
                server_message: String::new(),
                new_endpoint: Some("http://127.0.0.1:9/".into()),
                synced_before: false,
            }))),
            SyncIntent::SaveEndpoint("http://127.0.0.1:9/".into())
        );
        assert_eq!(
            draw(&ui, 76, 18),
            [
                "",
                "",
                "",
                "  ┌ Sync: choose one side ───────────────────────────────────────────────┐",
                "  │This collection and the server's copy have both changed in ways that  │",
                "  │cannot be merged. Choose which side to keep; this replaces the other  │",
                "  │side.                                                                 │",
                "  │                                                                      │",
                "  │  [u] Upload: replace the server's copy with this collection (other   │",
                "  │devices then download it)                                             │",
                "  │  [d] Download: replace this collection with the server's copy        │",
                "  │(changes made here since the last sync are lost; a backup is taken    │",
                "  │first)                                                                │",
                "  │> [Esc] Cancel: change nothing                                        │",
                "  └──────────────────────────────────────────────────────────────────────┘",
                "",
                "",
                "",
            ]
        );
        // Cancel is the default; moving selects upload, then download.
        assert_eq!(ui.on_key(key(KeyCode::Down)), SyncIntent::None);
        assert!(matches!(
            ui.stage(),
            SyncStage::Conflict {
                selected: ConflictChoice::Upload,
                ..
            }
        ));
        let mut download = ui.clone();
        // The actor syncs media after a full upload itself; the app refreshes.
        assert_eq!(
            ui.on_key(key(KeyCode::Enter)),
            SyncIntent::Start(SyncRequest::Upload)
        );
        assert_eq!(
            ui.on_response(Ok(DataResponse::Done)),
            SyncIntent::Finished { new_endpoint: None }
        );
        assert!(matches!(ui.stage(), SyncStage::Done { .. }));
        // The actor syncs media after a full download itself.
        download.on_key(key(KeyCode::Down));
        assert_eq!(
            download.on_key(key(KeyCode::Enter)),
            SyncIntent::Start(SyncRequest::Download)
        );
        assert_eq!(
            download.on_response(Ok(DataResponse::Done)),
            SyncIntent::Finished { new_endpoint: None }
        );
        assert!(matches!(ui.stage(), SyncStage::Done { .. }));
    }

    #[test]
    fn unavailable_side_cannot_be_chosen() {
        let mut ui = SyncUi::new(false);
        ui.start_normal();
        ui.on_response(report(SyncRequired::Full {
            upload: true,
            download: false,
        }));
        let screen = draw(&ui, 76, 18).join("\n");
        assert!(screen.contains("(not available)"), "{screen}");
        assert_eq!(ui.on_key(key(KeyCode::Char('d'))), SyncIntent::None);
        // Selection skips the unavailable choice.
        ui.on_key(key(KeyCode::Up));
        assert!(matches!(
            ui.stage(),
            SyncStage::Conflict {
                selected: ConflictChoice::Upload,
                ..
            }
        ));
        assert_eq!(
            ui.on_key(key(KeyCode::Char('u'))),
            SyncIntent::Start(SyncRequest::Upload)
        );
        assert_eq!(
            ui.on_response(Ok(DataResponse::Done)),
            SyncIntent::Finished { new_endpoint: None }
        );
        let mut ui = SyncUi::new(true);
        ui.start_normal();
        ui.on_response(report(SyncRequired::Full {
            upload: false,
            download: true,
        }));
        assert_eq!(ui.on_key(key(KeyCode::Esc)), SyncIntent::Close);
    }

    #[test]
    fn results_and_errors() {
        let mut ui = SyncUi::new(true);
        ui.start_normal();
        assert_eq!(
            ui.on_response(Ok(DataResponse::Synced(SyncReport {
                required: SyncRequired::NoChanges,
                server_message: "Welcome".into(),
                new_endpoint: Some("http://h/".into()),
                synced_before: false,
            }))),
            SyncIntent::Finished {
                new_endpoint: Some("http://h/".into())
            }
        );
        assert_eq!(
            draw(&ui, 40, 8),
            [
                "",
                "  ┌ Sync ────────────────────────────┐",
                "  │Sync complete.                    │",
                "  │Server: Welcome                   │",
                "  │                                  │",
                "  │Enter close                       │",
                "  └──────────────────────────────────┘",
                "",
            ]
        );
        ui.start_normal();
        assert_eq!(
            ui.on_response(Err(CoreError::Invalid(
                tanki_core::sync_ops::AUTH_REQUIRED.into()
            ))),
            SyncIntent::ForgetLogin
        );
        ui.start_normal();
        assert_eq!(
            ui.on_response(Err(CoreError::Other("Network down".into()))),
            SyncIntent::None
        );
        assert_eq!(
            draw(&ui, 40, 7),
            [
                "",
                "  ┌ Sync failed ─────────────────────┐",
                "  │Network down                      │",
                "  │                                  │",
                "  │Enter close                       │",
                "  └──────────────────────────────────┘",
                "",
            ]
        );
        assert_eq!(ui.on_key(key(KeyCode::Char('x'))), SyncIntent::None);
        assert_eq!(ui.on_key(key(KeyCode::Esc)), SyncIntent::Close);
    }
}
