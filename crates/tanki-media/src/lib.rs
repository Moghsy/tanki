//! tanki-media: audio playback with Anki's autoplay, replay and interrupt
//! rules (PLAN 6.5, R3 §4.3). Does not depend on `anki` or `tanki-core`.
//!
//! Interface fixed in the Phase 1 "agree first" step. The media stream may
//! add items; changing or removing the items below needs the lead.
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// One AV tag from a rendered card (tanki-core `AudioTag`, mapped by the TUI).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaTag {
    /// `[sound:…]`: a file name in the media folder (already reduced to its
    /// last path component).
    Sound(String),
    /// `{{tts …}}`. Phase 1 may report it as unsupported.
    Tts {
        text: String,
        lang: String,
        voices: Vec<String>,
        speed: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendChoice {
    /// mpv over JSON IPC when an `mpv` executable is found, else the built-in player.
    #[default]
    Auto,
    Mpv,
    Builtin,
    /// No audio at all.
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioConfig {
    /// `<profile>/collection.media`.
    pub media_dir: PathBuf,
    pub backend: BackendChoice,
    /// Explicit mpv path; otherwise `mpv` is looked up in `PATH`.
    pub mpv: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendStatus {
    Mpv,
    Builtin,
    /// No output device (typical over SSH); the UI shows an indicator.
    NoDevice(String),
    Off,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioEvent {
    Started(MediaTag),
    Finished(MediaTag),
    /// The queue ran out (auto-advance "wait for audio" listens for this).
    Idle,
    /// A tag could not be played; the message is for the status line.
    Error(String),
}

/// The audio player. Owns a background thread; dropping it stops playback.
pub struct Audio {
    _private: (),
}

impl Audio {
    /// Never fails: problems surface through [`Audio::status`] and events.
    pub fn start(config: AudioConfig) -> Self {
        let _ = config;
        unimplemented!("media stream")
    }
    pub fn status(&self) -> BackendStatus {
        unimplemented!("media stream")
    }
    /// Queues `tags`. With `interrupt`, first stops the current tag and
    /// clears the queue (Anki's `interruptAudioWhenAnswering`).
    pub fn play(&self, tags: Vec<MediaTag>, interrupt: bool) {
        let _ = (tags, interrupt);
        unimplemented!("media stream")
    }
    /// Stops and clears the queue.
    pub fn stop(&self) {
        unimplemented!("media stream")
    }
    pub fn toggle_pause(&self) {
        unimplemented!("media stream")
    }
    /// Seeks the current tag by `seconds` (negative = back).
    pub fn seek(&self, seconds: f64) {
        let _ = seconds;
        unimplemented!("media stream")
    }
    /// Something is playing or queued.
    pub fn is_playing(&self) -> bool {
        unimplemented!("media stream")
    }
    /// A new receiver for playback events.
    pub fn subscribe(&self) -> Receiver<AudioEvent> {
        unimplemented!("media stream")
    }
}

/// Anki's rules for which tags play when (R2 §2, R3 §4.3). Pure functions.
pub mod rules {
    use super::MediaTag;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Side {
        Question,
        Answer,
    }

    /// Tags to play when the question is shown (empty unless `autoplay`).
    pub fn on_show_question(question: &[MediaTag], autoplay: bool) -> Vec<MediaTag> {
        let _ = (question, autoplay);
        unimplemented!("media stream")
    }
    /// Tags to play when the answer is shown: only the answer's own tags.
    pub fn on_show_answer(answer: &[MediaTag], autoplay: bool) -> Vec<MediaTag> {
        let _ = (answer, autoplay);
        unimplemented!("media stream")
    }
    /// Replay key: the question side replays question tags; the answer side
    /// replays answer tags, preceded by question tags unless
    /// `skip_question_when_replaying_answer`.
    pub fn replay(
        side: Side,
        question: &[MediaTag],
        answer: &[MediaTag],
        skip_question_when_replaying_answer: bool,
    ) -> Vec<MediaTag> {
        let _ = (side, question, answer, skip_question_when_replaying_answer);
        unimplemented!("media stream")
    }
}
