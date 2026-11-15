//! Terminal ownership and the event loop. The probe runs before the sole
//! input reader. Both the reader and raw terminal are joined/restored on error.
//! `$EDITOR` runs with the reader paused and the terminal restored, then the
//! screen is taken back and repainted.
use crate::app::{App, AppEvent, Profile, Services, Start};
use crate::keymap::Keymap;
use crate::links::{HyperlinkBackend, LinkedRenderer, Links};
use crate::reviewer::Now;
use crate::services::{AnkiRenderer, Display, MediaAudio, TermImages};
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use ratatui::Terminal;
use std::io::{self, IsTerminal, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};
use tanki_core::CollectionActor;
use tanki_term::{Capabilities, GraphicsMode};

pub struct Options {
    pub playback: tanki_media::PlaybackOptions,
    pub graphics: GraphicsMode,
    pub start: Start,
    pub keymap: Keymap,
    /// tanki's config.toml; `None` means sync is unavailable.
    pub config: Option<std::path::PathBuf>,
    /// The Anki base folder (profiles on the Tools screen).
    pub base: Option<std::path::PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            playback: Default::default(),
            graphics: GraphicsMode::Auto,
            start: Start::Decks,
            keymap: Keymap::default(),
            config: None,
            base: None,
        }
    }
}

/// The caller keeps ownership of the actor and closes it after this returns.
/// `Some(name)`: the user chose to switch to that profile of the same base
/// folder; the caller closes this collection and runs again on that one.
pub fn run(actor: &CollectionActor, options: Options) -> io::Result<Option<String>> {
    let config = options
        .config
        .as_ref()
        .map(|p| tanki_core::config::Config::load(p))
        .transpose()
        .map_err(io::Error::other)?
        .unwrap_or_default();
    if !config.connect.in_tui {
        return run_inner(actor, options);
    }
    let api =
        tanki_connect::ServeOptions::from_settings(&config.connect).map_err(io::Error::other)?;
    let listener = std::net::TcpListener::bind(api.bind)?;
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            tanki_connect::serve_listener(actor, &api, listener, &stop).map_err(|e| e.to_string())
        });
        struct StopOnExit<'a>(&'a AtomicBool);
        impl Drop for StopOnExit<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let guard = StopOnExit(&stop);
        let result = run_inner(actor, options);
        drop(guard);
        let server = worker
            .join()
            .map_err(|_| io::Error::other("AnkiConnect worker panicked"))?;
        server.map_err(io::Error::other)?;
        result
    })
}
fn run_inner(actor: &CollectionActor, options: Options) -> io::Result<Option<String>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "interactive study requires a terminal on stdin and stdout",
        ));
    }
    let capabilities = tanki_term::probe(options.graphics)?;
    let audio = tanki_media::Audio::start_with_options(
        tanki_media::AudioConfig {
            media_dir: actor.media_dir(),
            ..Default::default()
        },
        options.playback,
    );
    let audio_events = audio.subscribe();
    let terminal_output = audio.terminal_output_lock();
    let images = TermImages::new(
        tanki_term::Images::new(&capabilities),
        tanki_render::ImageCache::new(),
        capabilities.cell_px,
    );
    let image_events = images.subscribe();
    let changes = actor.subscribe();
    let profile = Profile {
        name: actor
            .profile_dir()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        dir: actor.profile_dir().to_path_buf(),
        media_dir: actor.media_dir(),
    };
    let links = Links::with_text_sizing(capabilities.text_sizing);
    let services = Services {
        renderer: Box::new(LinkedRenderer::new(AnkiRenderer, links.clone())),
        images: Box::new(images),
        audio: Box::new(MediaAudio { audio }),
        display: Display::from_capabilities(&capabilities),
        opener: Box::new(crate::services::system_open),
    };
    // Enter and clear before App::new can queue autoplay video.
    let _session = Session::enter(&capabilities)?;
    let mut terminal = Terminal::new(HyperlinkBackend::new(io::stdout(), links))?;
    terminal.clear()?;
    let mut app = App::new(actor, services, profile, options.start, Now::current());
    app.set_keymap(options.keymap);
    app.set_config(options.config);
    app.set_base_dir(options.base);
    let gate = Arc::new(Gate::default());
    let keyboard = capabilities.keyboard_enhancement;
    let editor_gate = gate.clone();
    app.set_editor(Box::new(move |command, path| {
        suspended(&editor_gate, keyboard, || {
            crate::editor_flow::spawn_editor(command, path)
        })
    }));
    let reader = Reader::start(gate)?;
    let mut video_was_visible = false;
    let result = (|| {
        while !app.should_quit() {
            while let Ok(change) = changes.try_recv() {
                app.handle(AppEvent::Changes(change), Now::current());
            }
            while let Ok(audio) = audio_events.try_recv() {
                app.handle(AppEvent::Audio(audio), Now::current());
            }
            if image_events.try_iter().count() > 0 {
                app.handle(AppEvent::ImageReady, Now::current());
            }
            app.handle(AppEvent::Tick, Now::current());
            // The audio worker takes this same short lock before mpv is
            // allowed to write its first in-terminal video frame.
            let output_guard = terminal_output.lock().unwrap();
            if app.services.audio.owns_terminal() {
                video_was_visible = true;
            } else {
                if video_was_visible {
                    app.handle(AppEvent::Resize, Now::current());
                    terminal.clear()?;
                    video_was_visible = false;
                }
                let (cleanup, clear) = app.take_terminal_requests();
                terminal.backend_mut().write_all(cleanup.as_bytes())?;
                if clear {
                    terminal.clear()?;
                }
                if capabilities.synchronized_output {
                    queue!(
                        terminal.backend_mut(),
                        crossterm::terminal::BeginSynchronizedUpdate
                    )?;
                }
                let drawn = terminal.draw(|frame| app.draw(frame)).map(|_| ());
                if capabilities.synchronized_output {
                    execute!(
                        terminal.backend_mut(),
                        crossterm::terminal::EndSynchronizedUpdate
                    )?;
                }
                drawn?;
            }
            drop(output_guard);
            match reader.events.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(event)) => app.handle(event, Now::current()),
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("terminal input reader stopped"))
                }
            }
        }
        Ok(())
    })();
    app.shutdown();
    let (cleanup, _) = app.take_terminal_requests();
    let cleaned = terminal
        .backend_mut()
        .write_all(cleanup.as_bytes())
        .and_then(|()| terminal.backend_mut().flush());
    // Stop the reader before disabling raw mode, including on a drawing failure.
    drop(reader);
    let switch = app.take_profile_switch();
    result.and(cleaned).map(|()| switch)
}

struct Session {
    keyboard: bool,
}
impl Session {
    fn enter(caps: &Capabilities) -> io::Result<Self> {
        // Install the guard before any fallible setup, so partial setup restores.
        let guard = Self {
            keyboard: caps.keyboard_enhancement,
        };
        enter_modes(guard.keyboard)?;
        Ok(guard)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        leave_modes(self.keyboard);
    }
}

fn enter_modes(keyboard: bool) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableFocusChange,
        EnableMouseCapture
    )?;
    if keyboard {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        )?;
    }
    Ok(())
}

fn leave_modes(keyboard: bool) {
    let mut out = io::stdout();
    if keyboard {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        crossterm::terminal::EndSynchronizedUpdate,
        DisableMouseCapture,
        DisableFocusChange,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

/// Lets the editor have the terminal: while `paused`, the reader stops
/// polling, and holding `polling` waits out a poll in progress.
#[derive(Default)]
struct Gate {
    paused: AtomicBool,
    polling: Mutex<()>,
}

/// Runs `f` with the input reader paused and the terminal in its normal
/// state (main screen, cooked mode), then takes the screen back.
fn suspended<T>(gate: &Gate, keyboard: bool, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    gate.paused.store(true, Ordering::SeqCst);
    let _polling = gate.polling.lock().unwrap_or_else(|e| e.into_inner());
    leave_modes(keyboard);
    let result = f();
    let resumed = enter_modes(keyboard);
    gate.paused.store(false, Ordering::SeqCst);
    let value = result?;
    resumed?;
    Ok(value)
}

fn input_event(event: Event, at: Instant) -> AppEvent {
    match event {
        Event::Key(key) => AppEvent::Key(key, at),
        Event::Paste(text) => AppEvent::Paste(text),
        Event::Mouse(mouse) => AppEvent::Mouse(mouse),
        Event::Resize(..) => AppEvent::Resize,
        Event::FocusGained => AppEvent::FocusGained,
        Event::FocusLost => AppEvent::FocusLost,
    }
}

struct Reader {
    events: mpsc::Receiver<io::Result<AppEvent>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Reader {
    fn start(gate: Arc<Gate>) -> io::Result<Self> {
        let (sender, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let requested = stop.clone();
        let worker = thread::Builder::new()
            .name("tanki-input".into())
            .spawn(move || {
                while !requested.load(Ordering::Relaxed) {
                    if gate.paused.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    let _polling = gate.polling.lock().unwrap_or_else(|e| e.into_inner());
                    if gate.paused.load(Ordering::SeqCst) {
                        continue;
                    }
                    let result = event::poll(Duration::from_millis(50)).and_then(|ready| {
                        if ready {
                            event::read().map(|event| Some(input_event(event, Instant::now())))
                        } else {
                            Ok(None)
                        }
                    });
                    match result {
                        Ok(Some(event)) => {
                            if sender.send(Ok(event)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            events,
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_mapping_preserves_receipt_time_and_paste() {
        let at = Instant::now();
        let key = event::KeyEvent::new(event::KeyCode::Enter, event::KeyModifiers::NONE);
        assert!(
            matches!(input_event(Event::Key(key), at), AppEvent::Key(k, time) if k == key && time == at)
        );
        assert!(
            matches!(input_event(Event::Paste("日本\n\x1b".into()), at), AppEvent::Paste(s) if s == "日本\n\x1b")
        );
        assert!(matches!(
            input_event(Event::Resize(2, 3), at),
            AppEvent::Resize
        ));
        assert!(matches!(
            input_event(Event::FocusLost, at),
            AppEvent::FocusLost
        ));
    }
}
