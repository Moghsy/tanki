//! Own-voice recordings are temporary and never enter collection.media.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecordingBackend {
    #[default]
    Auto,
    PwRecord,
    Ffmpeg,
}

#[derive(Debug, Clone, Default)]
pub struct RecordingConfig {
    pub backend: RecordingBackend,
    pub executable: Option<PathBuf>,
}

/// A finished WAV; clones retain the temporary directory while audio is queued.
#[derive(Debug, Clone)]
pub struct Recording {
    dir: Arc<tempfile::TempDir>,
}
impl Recording {
    pub fn path(&self) -> PathBuf {
        self.dir.path().join("voice.wav")
    }
}

pub struct VoiceRecorder {
    child: Option<Child>,
    backend: RecordingBackend,
    dir: Arc<tempfile::TempDir>,
}

impl VoiceRecorder {
    pub fn start(config: RecordingConfig) -> Result<Self, String> {
        let (backend, exe) = match config.backend {
            RecordingBackend::Auto => {
                if let Some(exe) = crate::speech::find("pw-record") {
                    (RecordingBackend::PwRecord, exe)
                } else {
                    (
                        RecordingBackend::Ffmpeg,
                        crate::speech::find("ffmpeg")
                            .ok_or("recording needs pw-record or ffmpeg")?,
                    )
                }
            }
            backend => {
                let name = if backend == RecordingBackend::PwRecord {
                    "pw-record"
                } else {
                    "ffmpeg"
                };
                (
                    backend,
                    config
                        .executable
                        .or_else(|| crate::speech::find(name))
                        .ok_or("recording executable not found")?,
                )
            }
        };
        let dir = Arc::new(
            tempfile::Builder::new()
                .prefix("tanki-voice-")
                .tempdir()
                .map_err(|e| e.to_string())?,
        );
        let mut cmd = Command::new(exe);
        match backend {
            RecordingBackend::PwRecord => {
                cmd.args(["--rate", "44100", "--channels", "1", "--format", "s16"]);
            }
            RecordingBackend::Ffmpeg => {
                cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
                #[cfg(target_os = "macos")]
                cmd.args(["-f", "avfoundation", "-i", ":0"]);
                #[cfg(target_os = "windows")]
                cmd.args(["-f", "dshow", "-i", "audio=default"]);
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                cmd.args(["-f", "pulse", "-i", "default"]);
                cmd.args(["-ac", "1", "-ar", "44100", "-c:a", "pcm_s16le"]);
            }
            _ => unreachable!(),
        }
        let child = cmd
            .arg(dir.path().join("voice.wav"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot record: {e}"))?;
        Ok(Self {
            child: Some(child),
            backend,
            dir,
        })
    }

    /// Finish gracefully so the recorder writes the WAV header. A bounded wait
    /// prevents a broken helper from holding up terminal restoration.
    pub fn finish(mut self) -> Result<Recording, String> {
        let child = self.child.as_mut().ok_or("recorder already stopped")?;
        if self.backend == RecordingBackend::Ffmpeg {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(b"q\n");
            }
        } else {
            #[cfg(unix)]
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGINT);
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("recorder did not finish; recording discarded".into());
                }
            }
        }
        self.child = None;
        let recording = Recording {
            dir: self.dir.clone(),
        };
        validate_wav(&recording.path())?;
        Ok(recording)
    }
}

fn validate_wav(path: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|_| "no voice captured; check microphone and recording backend".to_string())?;
    let mut header = [0; 12];
    file.read_exact(&mut header)
        .map_err(|_| "recording is empty".to_string())?;
    if &header[..4] != b"RIFF"
        || &header[8..] != b"WAVE"
        || file.metadata().map_or(0, |m| m.len()) <= 44
    {
        return Err("recorder did not produce a usable WAV".into());
    }
    Ok(())
}
impl Drop for VoiceRecorder {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn graceful_finish_keeps_temporary_wav_until_last_owner_drops() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ffmpeg");
        // Find the final positional argument, write a synthetic WAV, and wait
        // for ffmpeg's normal 'q' command. Never use the host microphone.
        std::fs::write(&exe, "#!/bin/sh\nfor arg do output=$arg; done\nprintf 'RIFF0000WAVEfmt 000000000000000000000000data0000000000000000' > \"$output\"\nread answer\ntest \"$answer\" = q\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let recorder = VoiceRecorder::start(RecordingConfig {
            backend: RecordingBackend::Ffmpeg,
            executable: Some(exe),
        })
        .unwrap();
        let recording = recorder.finish().unwrap();
        let path = recording.path();
        assert!(path.is_file());
        let second = recording.clone();
        drop(recording);
        assert!(path.is_file());
        drop(second);
        assert!(!path.exists());
    }
    #[test]
    fn invalid_recording_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.wav");
        std::fs::write(&path, b"not a waveform").unwrap();
        assert!(validate_wav(&path).is_err());
    }
}
