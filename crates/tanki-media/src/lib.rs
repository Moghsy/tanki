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
    inner: engine::Handle,
}

impl Audio {
    /// Never fails: problems surface through [`Audio::status`] and events.
    pub fn start(config: AudioConfig) -> Self {
        let media_dir = config.media_dir.clone();
        Self {
            inner: engine::Handle::spawn(media_dir, move || backends::select(&config)),
        }
    }
    pub fn status(&self) -> BackendStatus {
        self.inner.status()
    }
    /// Queues `tags`. With `interrupt`, first stops the current tag and
    /// clears the queue (Anki's `interruptAudioWhenAnswering`). Interrupted
    /// or stopped tags emit no `Finished`; `Idle` follows once the queue is empty.
    pub fn play(&self, tags: Vec<MediaTag>, interrupt: bool) {
        self.inner.play(tags, interrupt);
    }
    /// Stops and clears the queue.
    pub fn stop(&self) {
        self.inner.stop();
    }
    pub fn toggle_pause(&self) {
        self.inner.toggle_pause();
    }
    /// Seeks the current tag by `seconds` (negative = back).
    pub fn seek(&self, seconds: f64) {
        self.inner.seek(seconds);
    }
    /// Something is playing or queued.
    pub fn is_playing(&self) -> bool {
        self.inner.is_playing()
    }
    /// A new receiver for playback events.
    pub fn subscribe(&self) -> Receiver<AudioEvent> {
        self.inner.subscribe()
    }
}

mod backends;
mod engine;

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
        if autoplay {
            question.to_vec()
        } else {
            Vec::new()
        }
    }
    /// Tags to play when the answer is shown: only the answer's own tags.
    pub fn on_show_answer(answer: &[MediaTag], autoplay: bool) -> Vec<MediaTag> {
        if autoplay {
            answer.to_vec()
        } else {
            Vec::new()
        }
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
        match side {
            Side::Question => question.to_vec(),
            Side::Answer if skip_question_when_replaying_answer => answer.to_vec(),
            Side::Answer => question.iter().chain(answer).cloned().collect(),
        }
    }
}

#[cfg(test)]
mod rules_tests {
    use super::rules::*;
    use super::MediaTag;

    fn s(n: &str) -> MediaTag {
        MediaTag::Sound(n.into())
    }

    #[test]
    fn question_autoplay() {
        let q = [s("q.mp3")];
        assert_eq!(on_show_question(&q, true), vec![s("q.mp3")]);
        assert!(on_show_question(&q, false).is_empty());
    }

    #[test]
    fn answer_plays_only_its_own() {
        let a = [s("a.mp3")];
        assert_eq!(on_show_answer(&a, true), vec![s("a.mp3")]);
        assert!(on_show_answer(&a, false).is_empty());
        assert!(on_show_answer(&[], true).is_empty());
    }

    #[test]
    fn replay_rules() {
        let (q, a) = ([s("q")], [s("a")]);
        assert_eq!(replay(Side::Question, &q, &a, false), vec![s("q")]);
        assert_eq!(replay(Side::Question, &q, &a, true), vec![s("q")]);
        assert_eq!(replay(Side::Answer, &q, &a, false), vec![s("q"), s("a")]);
        assert_eq!(replay(Side::Answer, &q, &a, true), vec![s("a")]);
    }
}
