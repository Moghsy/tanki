//! The queue and its background thread, generic over a [`Backend`] so the
//! behaviour can be tested with a fake that needs no sound device.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{AudioEvent, BackendStatus, MediaTag};

/// How often the worker asks the backend whether the current file ended.
const POLL: Duration = Duration::from_millis(40);

/// One playback engine (mpv process or rodio sink). Lives on the worker thread.
pub(crate) trait Backend {
    /// Start `path`, replacing whatever plays now.
    fn play(&mut self, path: &Path) -> Result<(), String>;
    fn stop(&mut self);
    fn toggle_pause(&mut self);
    /// Relative seek; `Err` carries a status-line message.
    fn seek(&mut self, seconds: f64) -> Result<(), String>;
    /// The file started with `play` has ended on its own.
    fn finished(&mut self) -> bool;
}

/// What `select` produces: a backend, or a status with no backend.
pub(crate) enum Selected {
    Active(Box<dyn Backend>, BackendStatus),
    Inactive(BackendStatus),
}

enum Cmd {
    Play(Vec<MediaTag>, bool),
    Stop,
    Pause,
    Seek(f64),
    Shutdown,
}

type Subscribers = Arc<Mutex<Vec<Sender<AudioEvent>>>>;

pub(crate) struct Handle {
    tx: Sender<Cmd>,
    busy: Arc<Mutex<bool>>,
    subs: Subscribers,
    status: BackendStatus,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// `make` runs on the worker thread (audio streams are often `!Send`).
    pub(crate) fn spawn<F>(media_dir: PathBuf, make: F) -> Self
    where
        F: FnOnce() -> Selected + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();
        let busy = Arc::new(Mutex::new(false));
        let subs: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let worker_busy = busy.clone();
        let worker_subs = subs.clone();
        let thread = std::thread::Builder::new()
            .name("tanki-audio".into())
            .spawn(move || {
                let (backend, status) = match make() {
                    Selected::Active(b, s) => (Some(b), s),
                    Selected::Inactive(s) => (None, s),
                };
                let _ = status_tx.send(status.clone());
                Worker {
                    media_dir,
                    backend,
                    status,
                    rx,
                    busy: worker_busy,
                    subs: worker_subs,
                    queue: VecDeque::new(),
                    current: None,
                    worked: false,
                }
                .run();
            })
            .ok();
        let status = status_rx
            .recv()
            .unwrap_or_else(|_| BackendStatus::NoDevice("audio thread failed to start".into()));
        Self {
            tx,
            busy,
            subs,
            status,
            thread,
        }
    }

    pub(crate) fn status(&self) -> BackendStatus {
        self.status.clone()
    }

    pub(crate) fn play(&self, tags: Vec<MediaTag>, interrupt: bool) {
        if matches!(self.status, BackendStatus::Off) {
            return;
        }
        if !matches!(self.status, BackendStatus::NoDevice(_)) {
            let mut busy = self.busy.lock().unwrap();
            *busy = *busy || interrupt || !tags.is_empty();
            // Sending under the lock keeps `busy` and the channel consistent
            // with the worker's idle check.
            let _ = self.tx.send(Cmd::Play(tags, interrupt));
        } else {
            let _ = self.tx.send(Cmd::Play(tags, interrupt));
        }
    }

    pub(crate) fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }
    pub(crate) fn toggle_pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }
    pub(crate) fn seek(&self, seconds: f64) {
        let _ = self.tx.send(Cmd::Seek(seconds));
    }
    pub(crate) fn is_playing(&self) -> bool {
        *self.busy.lock().unwrap()
    }
    pub(crate) fn subscribe(&self) -> Receiver<AudioEvent> {
        let (tx, rx) = mpsc::channel();
        self.subs.lock().unwrap().push(tx);
        rx
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Worker {
    media_dir: PathBuf,
    backend: Option<Box<dyn Backend>>,
    status: BackendStatus,
    rx: Receiver<Cmd>,
    busy: Arc<Mutex<bool>>,
    subs: Subscribers,
    queue: VecDeque<MediaTag>,
    current: Option<MediaTag>,
    /// Something started or failed since the last `Idle`.
    worked: bool,
}

impl Worker {
    fn emit(&self, event: AudioEvent) {
        self.subs
            .lock()
            .unwrap()
            .retain(|s| s.send(event.clone()).is_ok());
    }

    fn run(mut self) {
        loop {
            if self.current.is_none() {
                if let Some(tag) = self.queue.pop_front() {
                    self.start(tag);
                    continue;
                }
                // Idle: decide under the lock so `is_playing` never reports
                // false while a command is still in flight.
                let cmd = {
                    let mut busy = self.busy.lock().unwrap();
                    match self.rx.try_recv() {
                        Ok(c) => Some(c),
                        Err(TryRecvError::Empty) => {
                            if self.worked {
                                self.worked = false;
                                self.emit(AudioEvent::Idle);
                            }
                            *busy = false;
                            None
                        }
                        Err(TryRecvError::Disconnected) => break,
                    }
                };
                let cmd = match cmd {
                    Some(c) => c,
                    None => match self.rx.recv() {
                        Ok(c) => c,
                        Err(_) => break,
                    },
                };
                if self.handle(cmd) {
                    break;
                }
                continue;
            }
            match self.rx.recv_timeout(POLL) {
                Ok(cmd) => {
                    if self.handle(cmd) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    let done = self.backend.as_mut().is_some_and(|b| b.finished());
                    if done {
                        if let Some(tag) = self.current.take() {
                            self.emit(AudioEvent::Finished(tag));
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        if let Some(b) = self.backend.as_mut() {
            b.stop();
        }
    }

    /// Returns true on shutdown.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Shutdown => return true,
            Cmd::Play(tags, interrupt) => match self.status {
                BackendStatus::Off => {}
                BackendStatus::NoDevice(ref reason) => {
                    if !tags.is_empty() {
                        let msg = format!("no audio output: {reason}");
                        self.emit(AudioEvent::Error(msg));
                    }
                }
                _ => {
                    if interrupt {
                        self.halt();
                    }
                    self.queue.extend(tags);
                }
            },
            Cmd::Stop => self.halt(),
            Cmd::Pause => {
                if self.current.is_some() {
                    if let Some(b) = self.backend.as_mut() {
                        b.toggle_pause();
                    }
                }
            }
            Cmd::Seek(s) => {
                if self.current.is_some() {
                    let res = self.backend.as_mut().map(|b| b.seek(s));
                    if let Some(Err(msg)) = res {
                        self.emit(AudioEvent::Error(msg));
                    }
                }
            }
        }
        false
    }

    /// Stop the current tag and drop the queue, without a `Finished` event.
    fn halt(&mut self) {
        self.queue.clear();
        if self.current.take().is_some() {
            if let Some(b) = self.backend.as_mut() {
                b.stop();
            }
        }
    }

    fn start(&mut self, tag: MediaTag) {
        self.worked = true;
        let name = match &tag {
            MediaTag::Sound(name) => name.clone(),
            MediaTag::Tts { .. } => {
                self.emit(AudioEvent::Error(
                    "text to speech is not supported yet".into(),
                ));
                return;
            }
        };
        let path = match resolve(&self.media_dir, &name) {
            Ok(p) => p,
            Err(msg) => {
                self.emit(AudioEvent::Error(msg));
                return;
            }
        };
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        match backend.play(&path) {
            Ok(()) => {
                self.current = Some(tag.clone());
                self.emit(AudioEvent::Started(tag));
            }
            Err(msg) => self.emit(AudioEvent::Error(msg)),
        }
    }
}

/// Resolves a `[sound:]` name inside `media_dir` only.
pub(crate) fn resolve(media_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let bad = name.is_empty()
        || name.contains(['/', '\\', '\0'])
        || matches!(name, "." | "..")
        || Path::new(name).is_absolute();
    if bad {
        return Err(format!("unsafe audio file name: {name}"));
    }
    let path = media_dir.join(name);
    // A media filename must not read through a symlink outside the profile.
    // Inspect the entry itself before following it (including broken links).
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("unsafe audio file name: {name}"));
    }
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("audio file not found: {name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Shared {
        log: Mutex<Vec<String>>,
        finished: AtomicBool,
    }

    struct Fake(Arc<Shared>);

    impl Backend for Fake {
        fn play(&mut self, path: &Path) -> Result<(), String> {
            let n = path.file_name().unwrap().to_string_lossy().into_owned();
            if n.starts_with("bad") {
                return Err(format!("cannot decode {n}"));
            }
            self.0.finished.store(false, Ordering::SeqCst);
            self.0.log.lock().unwrap().push(format!("play {n}"));
            Ok(())
        }
        fn stop(&mut self) {
            self.0.log.lock().unwrap().push("stop".into());
        }
        fn toggle_pause(&mut self) {
            self.0.log.lock().unwrap().push("pause".into());
        }
        fn seek(&mut self, s: f64) -> Result<(), String> {
            self.0.log.lock().unwrap().push(format!("seek {s}"));
            Ok(())
        }
        fn finished(&mut self) -> bool {
            self.0.finished.swap(false, Ordering::SeqCst)
        }
    }

    fn setup(files: &[&str]) -> (tempfile::TempDir, Handle, Arc<Shared>, Receiver<AudioEvent>) {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            std::fs::write(dir.path().join(f), b"x").unwrap();
        }
        let shared = Arc::new(Shared::default());
        let s2 = shared.clone();
        let h = Handle::spawn(dir.path().to_path_buf(), move || {
            Selected::Active(Box::new(Fake(s2)), BackendStatus::Builtin)
        });
        let rx = h.subscribe();
        (dir, h, shared, rx)
    }

    fn snd(n: &str) -> MediaTag {
        MediaTag::Sound(n.into())
    }

    fn next(rx: &Receiver<AudioEvent>) -> AudioEvent {
        rx.recv_timeout(Duration::from_secs(5)).expect("event")
    }

    #[test]
    fn plays_in_order_then_idle() {
        let (_d, h, sh, rx) = setup(&["a.mp3", "b.mp3"]);
        h.play(vec![snd("a.mp3"), snd("b.mp3")], false);
        assert!(h.is_playing());
        assert_eq!(next(&rx), AudioEvent::Started(snd("a.mp3")));
        sh.finished.store(true, Ordering::SeqCst);
        assert_eq!(next(&rx), AudioEvent::Finished(snd("a.mp3")));
        assert_eq!(next(&rx), AudioEvent::Started(snd("b.mp3")));
        sh.finished.store(true, Ordering::SeqCst);
        assert_eq!(next(&rx), AudioEvent::Finished(snd("b.mp3")));
        assert_eq!(next(&rx), AudioEvent::Idle);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!h.is_playing());
    }

    #[test]
    fn interrupt_replaces_current_and_queue() {
        let (_d, h, sh, rx) = setup(&["a.mp3", "b.mp3", "c.mp3"]);
        h.play(vec![snd("a.mp3"), snd("b.mp3")], false);
        assert_eq!(next(&rx), AudioEvent::Started(snd("a.mp3")));
        h.play(vec![snd("c.mp3")], true);
        assert_eq!(next(&rx), AudioEvent::Started(snd("c.mp3")));
        sh.finished.store(true, Ordering::SeqCst);
        assert_eq!(next(&rx), AudioEvent::Finished(snd("c.mp3")));
        assert_eq!(next(&rx), AudioEvent::Idle);
        let log = sh.log.lock().unwrap().clone();
        assert_eq!(log, ["play a.mp3", "stop", "play c.mp3"]);
    }

    #[test]
    fn append_without_interrupt() {
        let (_d, h, sh, rx) = setup(&["a.mp3", "b.mp3"]);
        h.play(vec![snd("a.mp3")], false);
        assert_eq!(next(&rx), AudioEvent::Started(snd("a.mp3")));
        h.play(vec![snd("b.mp3")], false);
        sh.finished.store(true, Ordering::SeqCst);
        assert_eq!(next(&rx), AudioEvent::Finished(snd("a.mp3")));
        assert_eq!(next(&rx), AudioEvent::Started(snd("b.mp3")));
    }

    #[test]
    fn stop_clears_and_goes_idle() {
        let (_d, h, _sh, rx) = setup(&["a.mp3", "b.mp3"]);
        h.play(vec![snd("a.mp3"), snd("b.mp3")], false);
        assert_eq!(next(&rx), AudioEvent::Started(snd("a.mp3")));
        h.stop();
        assert_eq!(next(&rx), AudioEvent::Idle);
    }

    #[test]
    fn missing_and_unsafe_files_error_then_continue() {
        let (_d, h, sh, rx) = setup(&["ok.mp3"]);
        h.play(
            vec![snd("missing.mp3"), snd("../etc/passwd"), snd("ok.mp3")],
            false,
        );
        assert!(matches!(next(&rx), AudioEvent::Error(m) if m.contains("missing.mp3")));
        assert!(matches!(next(&rx), AudioEvent::Error(m) if m.contains("unsafe")));
        assert_eq!(next(&rx), AudioEvent::Started(snd("ok.mp3")));
        sh.finished.store(true, Ordering::SeqCst);
        assert_eq!(next(&rx), AudioEvent::Finished(snd("ok.mp3")));
        assert_eq!(next(&rx), AudioEvent::Idle);
    }

    #[test]
    fn backend_play_error_is_reported() {
        let (_d, h, _sh, rx) = setup(&["bad.mp3"]);
        h.play(vec![snd("bad.mp3")], false);
        assert!(matches!(next(&rx), AudioEvent::Error(m) if m.contains("cannot decode")));
        assert_eq!(next(&rx), AudioEvent::Idle);
    }

    #[test]
    fn tts_is_unsupported() {
        let (_d, h, _sh, rx) = setup(&[]);
        h.play(
            vec![MediaTag::Tts {
                text: "hi".into(),
                lang: "en_US".into(),
                voices: vec![],
                speed: 1.0,
            }],
            false,
        );
        assert_eq!(
            next(&rx),
            AudioEvent::Error("text to speech is not supported yet".into())
        );
    }

    #[test]
    fn pause_and_seek_reach_backend_only_while_playing() {
        let (_d, h, sh, rx) = setup(&["a.mp3"]);
        h.toggle_pause();
        h.seek(5.0);
        h.play(vec![snd("a.mp3")], false);
        assert_eq!(next(&rx), AudioEvent::Started(snd("a.mp3")));
        h.toggle_pause();
        h.seek(-5.0);
        h.stop();
        assert_eq!(next(&rx), AudioEvent::Idle);
        let log = sh.log.lock().unwrap().clone();
        assert_eq!(log, ["play a.mp3", "pause", "seek -5", "stop"]);
    }

    #[test]
    fn no_device_emits_one_error_per_play() {
        let dir = tempfile::tempdir().unwrap();
        let h = Handle::spawn(dir.path().to_path_buf(), || {
            Selected::Inactive(BackendStatus::NoDevice("no card".into()))
        });
        assert_eq!(h.status(), BackendStatus::NoDevice("no card".into()));
        let rx = h.subscribe();
        h.play(vec![snd("a.mp3"), snd("b.mp3")], false);
        assert!(matches!(next(&rx), AudioEvent::Error(m) if m.contains("no card")));
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        assert!(!h.is_playing());
    }

    #[test]
    fn off_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let h = Handle::spawn(dir.path().to_path_buf(), || {
            Selected::Inactive(BackendStatus::Off)
        });
        let rx = h.subscribe();
        h.play(vec![snd("a.mp3")], false);
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        assert!(!h.is_playing());
    }

    #[test]
    fn drop_stops_playback() {
        let (_d, h, sh, rx) = setup(&["a.mp3"]);
        h.play(vec![snd("a.mp3")], false);
        assert_eq!(next(&rx), AudioEvent::Started(snd("a.mp3")));
        drop(h);
        assert_eq!(sh.log.lock().unwrap().last().unwrap(), "stop");
    }

    #[test]
    fn path_safety() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.mp3"), b"x").unwrap();
        assert!(resolve(dir.path(), "ok.mp3").is_ok());
        for bad in [
            "",
            "a/b.mp3",
            "a\\b.mp3",
            "..",
            "../x.mp3",
            "/etc/passwd",
            "a\0b",
        ] {
            let err = resolve(dir.path(), bad).unwrap_err();
            assert!(err.contains("unsafe"), "{bad}: {err}");
        }
        assert!(resolve(dir.path(), "nope.mp3")
            .unwrap_err()
            .contains("not found"));
        std::fs::write(dir.path().join("x..mp3"), b"x").unwrap();
        assert!(resolve(dir.path(), "x..mp3").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn audio_does_not_follow_media_symlinks() {
        let media = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("sound.mp3");
        std::fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(target, media.path().join("sound.mp3")).unwrap();
        assert!(resolve(media.path(), "sound.mp3")
            .unwrap_err()
            .contains("unsafe"));
    }
}
