//! Terminal ownership and the event loop. The probe runs before the sole
//! input reader. Both the reader and raw terminal are joined/restored on error.
use crate::app::{App, AppEvent, Profile, Services, Start};
use crate::keymap::Keymap;
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
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, IsTerminal, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::{Duration, Instant};
use tanki_core::CollectionActor;
use tanki_term::{Capabilities, GraphicsMode};

pub struct Options {
    pub graphics: GraphicsMode,
    pub start: Start,
    pub keymap: Keymap,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            graphics: GraphicsMode::Auto,
            start: Start::Decks,
            keymap: Keymap::default(),
        }
    }
}

/// The caller keeps ownership of the actor and closes it after this returns.
pub fn run(actor: &CollectionActor, options: Options) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "interactive study requires a terminal on stdin and stdout",
        ));
    }
    let capabilities = tanki_term::probe(options.graphics)?;
    let audio = tanki_media::Audio::start(tanki_media::AudioConfig {
        media_dir: actor.media_dir(),
        ..Default::default()
    });
    let audio_events = audio.subscribe();
    let images = TermImages::new(
        tanki_term::Images::new(&capabilities),
        Some(tanki_render::ImageCache::new()),
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
    let services = Services {
        renderer: Box::new(AnkiRenderer),
        images: Box::new(images),
        audio: Box::new(MediaAudio { audio }),
        display: Display::from_capabilities(&capabilities),
        opener: Box::new(crate::services::system_open),
    };
    let mut app = App::new(actor, services, profile, options.start, Now::current());
    app.set_keymap(options.keymap);
    let _session = Session::enter(&capabilities)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let reader = Reader::start()?;
    let result = (|| {
        while !app.should_quit() {
            while let Ok(change) = changes.try_recv() {
                app.handle(AppEvent::Changes(change), Now::current());
            }
            while let Ok(audio) = audio_events.try_recv() {
                app.handle(AppEvent::Audio(audio), Now::current());
            }
            if let Some(events) = &image_events {
                if events.try_iter().next().is_some() {
                    app.handle(AppEvent::ImageReady, Now::current());
                }
            }
            app.handle(AppEvent::Tick, Now::current());
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
    result.and(cleaned)
}

struct Session {
    keyboard: bool,
}
impl Session {
    fn enter(caps: &Capabilities) -> io::Result<Self> {
        enable_raw_mode()?;
        // Install the guard before any fallible setup, so partial setup restores.
        let guard = Self {
            keyboard: caps.keyboard_enhancement,
        };
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableFocusChange,
            EnableMouseCapture
        )?;
        if guard.keyboard {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                )
            )?;
        }
        Ok(guard)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let mut out = io::stdout();
        if self.keyboard {
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
    fn start() -> io::Result<Self> {
        let (sender, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let requested = stop.clone();
        let worker = thread::Builder::new()
            .name("tanki-input".into())
            .spawn(move || {
                while !requested.load(Ordering::Relaxed) {
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
