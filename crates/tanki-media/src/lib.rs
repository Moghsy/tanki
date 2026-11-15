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
    /// `{{tts …}}`, spoken by the selected local speech engine.
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

/// Video output is an explicit choice: never emit graphics protocol sequences
/// merely because a card contains a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoOutput {
    #[default]
    Window,
    Kitty,
    Sixel,
    Blocks,
}

#[derive(Debug, Clone, Default)]
pub struct PlaybackOptions {
    pub tts: TtsConfig,
    pub video: VideoOutput,
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
    voice: std::sync::Arc<std::sync::Mutex<VoiceState>>,
    voice_job: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Default)]
struct VoiceState {
    recorder: Option<VoiceRecorder>,
    last: Option<Recording>,
    finishing: bool,
    error: Option<String>,
}

impl Audio {
    /// Never fails: problems surface through [`Audio::status`] and events.
    pub fn start(config: AudioConfig) -> Self {
        Self::start_with_options(config, PlaybackOptions::default())
    }
    pub fn start_with_options(config: AudioConfig, options: PlaybackOptions) -> Self {
        let media_dir = config.media_dir.clone();
        Self {
            inner: engine::Handle::spawn(media_dir, move || backends::select(&config, &options)),
            voice: Default::default(),
            voice_job: Default::default(),
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
    /// Replays only a WAV produced by [`VoiceRecorder`]. The recording remains
    /// alive until queued playback releases it, even if the caller drops it.
    pub fn play_recording(&self, recording: &Recording, interrupt: bool) {
        self.inner.play_recording(recording.clone(), interrupt);
    }
    /// Starts or stops recording. WAV finalization runs off the UI thread.
    /// `true` means capture is active, `false` means it was stopped.
    pub fn toggle_recording(&self) -> Result<bool, String> {
        self.toggle_recording_with_config(RecordingConfig::default())
    }
    /// Select a recorder explicitly. The configuration is used when starting
    /// capture; stopping always finalizes the already running recorder.
    pub fn toggle_recording_with_config(&self, config: RecordingConfig) -> Result<bool, String> {
        let mut voice = self.voice.lock().unwrap();
        if voice.finishing {
            return Err("voice recording is still finishing".into());
        }
        if let Some(recorder) = voice.recorder.take() {
            voice.finishing = true;
            let state = self.voice.clone();
            let job = std::thread::Builder::new()
                .name("tanki-recorder".into())
                .spawn(move || {
                    let result = recorder.finish();
                    let mut state = state.lock().unwrap();
                    match result {
                        Ok(recording) => {
                            state.last = Some(recording);
                            state.error = None;
                        }
                        Err(error) => state.error = Some(error),
                    }
                    state.finishing = false;
                })
                .map_err(|e| {
                    voice.finishing = false;
                    format!("cannot finish recording: {e}")
                })?;
            *self.voice_job.lock().unwrap() = Some(job);
            Ok(false)
        } else {
            self.stop();
            voice.recorder = Some(VoiceRecorder::start(config)?);
            voice.error = None;
            Ok(true)
        }
    }
    pub fn replay_recording(&self) -> Result<(), String> {
        let voice = self.voice.lock().unwrap();
        if voice.recorder.is_some() || voice.finishing {
            return Err("stop recording and wait for it to finish first".into());
        }
        if let Some(error) = &voice.error {
            return Err(error.clone());
        }
        let recording = voice.last.as_ref().ok_or("record your voice first")?;
        self.play_recording(recording, true);
        Ok(())
    }
    pub fn is_recording(&self) -> bool {
        self.voice.lock().unwrap().recorder.is_some()
    }
    /// In-terminal mpv video currently owns stdout. The UI should stop drawing
    /// until this becomes false, then clear and repaint its screen.
    pub fn terminal_output_lock(&self) -> std::sync::Arc<std::sync::Mutex<()>> {
        self.inner.terminal_output_lock()
    }
    pub fn owns_terminal(&self) -> bool {
        self.inner.owns_terminal()
    }
    /// Stops and clears the queue.
    pub fn stop(&self) {
        self.inner.stop();
    }
    /// Shutdown barrier: terminal cleanup waits for mpv's last frame and for
    /// local speech cancellation. Normal UI actions should use [`Audio::stop`].
    pub fn stop_and_wait(&self) -> Result<(), String> {
        self.inner.stop_and_wait()
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

impl Drop for Audio {
    fn drop(&mut self) {
        // Stop active capture before waiting for bounded WAV finalization.
        self.voice.lock().unwrap().recorder = None;
        if let Some(job) = self.voice_job.get_mut().unwrap().take() {
            let _ = job.join();
        }
    }
}

mod backends;
#[cfg(unix)]
mod dispatcher;
mod engine;
mod recording;
mod speech;

pub use recording::{Recording, RecordingBackend, RecordingConfig, VoiceRecorder};
pub use speech::{TtsBackend, TtsConfig};

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

#[cfg(all(test, unix))]
mod recording_api_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn audio_owns_recording_and_finalizes_off_the_caller_thread() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ffmpeg");
        std::fs::write(&exe, "#!/bin/sh\nfor arg do output=$arg; done\nprintf 'RIFF0000WAVEfmt 000000000000000000000000data0000000000000000' > \"$output\"\nread answer\nsleep 0.1\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let audio = Audio::start(AudioConfig {
            backend: BackendChoice::Off,
            ..Default::default()
        });
        assert!(audio.replay_recording().unwrap_err().contains("first"));
        assert!(audio
            .toggle_recording_with_config(RecordingConfig {
                backend: RecordingBackend::Ffmpeg,
                executable: Some(exe)
            })
            .unwrap());
        assert!(audio.is_recording());
        let before = std::time::Instant::now();
        assert!(!audio.toggle_recording().unwrap());
        assert!(before.elapsed() < std::time::Duration::from_millis(90));
        assert!(!audio.is_recording());
        assert!(audio.replay_recording().unwrap_err().contains("finish"));
        for _ in 0..100 {
            if !audio.voice.lock().unwrap().finishing {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        audio.replay_recording().unwrap();
        let path = audio.voice.lock().unwrap().last.as_ref().unwrap().path();
        assert!(path.exists());
        drop(audio);
        assert!(!path.exists());
    }
}
