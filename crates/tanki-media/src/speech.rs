//! Local speech engines. Utterances are data on SSIP, stdin or an argument;
//! none of the engines is invoked through a shell.
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::MediaTag;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TtsBackend {
    #[default]
    Auto,
    SpeechDispatcher,
    Espeak,
    Say,
    /// A locally installed Piper CLI and an explicitly configured ONNX model.
    Piper,
    Off,
}

#[derive(Debug, Clone, Default)]
pub struct TtsConfig {
    pub backend: TtsBackend,
    /// Override the selected engine executable; no shell expansion is performed.
    pub executable: Option<PathBuf>,
    pub piper_model: Option<PathBuf>,
}

pub(crate) fn find(exe: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|p| p.join(exe))
        .find(|p| p.is_file())
}

pub(crate) struct Speech {
    config: TtsConfig,
    child: Option<Child>,
    wav: Option<tempfile::TempDir>,
    generated: Option<PathBuf>,
    failure: Option<String>,
    #[cfg(unix)]
    dispatcher: Option<crate::dispatcher::Dispatcher>,
}

impl Speech {
    pub(crate) fn new(config: TtsConfig) -> Self {
        Self {
            config,
            child: None,
            wav: None,
            generated: None,
            failure: None,
            #[cfg(unix)]
            dispatcher: None,
        }
    }

    fn engine(&self) -> Result<(TtsBackend, PathBuf), String> {
        let candidates: &[(TtsBackend, &str)] = if cfg!(target_os = "macos") {
            &[
                (TtsBackend::Say, "say"),
                (TtsBackend::SpeechDispatcher, "spd-say"),
                (TtsBackend::Espeak, "espeak-ng"),
            ]
        } else {
            &[
                (TtsBackend::SpeechDispatcher, "spd-say"),
                (TtsBackend::Espeak, "espeak-ng"),
                (TtsBackend::Say, "say"),
            ]
        };
        let selected = match self.config.backend {
            TtsBackend::Auto => candidates
                .iter()
                .find_map(|(backend, name)| find(name).map(|path| (*backend, path))),
            TtsBackend::Off => return Err("text to speech is disabled".into()),
            backend => {
                let name = match backend {
                    TtsBackend::SpeechDispatcher => "spd-say",
                    TtsBackend::Espeak => "espeak-ng",
                    TtsBackend::Say => "say",
                    TtsBackend::Piper => "piper",
                    _ => unreachable!(),
                };
                self.config
                    .executable
                    .clone()
                    .or_else(|| find(name))
                    .map(|path| (backend, path))
            }
        };
        selected.ok_or_else(|| "no local TTS engine: install spd-say or espeak-ng (say on macOS), or configure Piper".into())
    }

    pub(crate) fn play(&mut self, tag: &MediaTag) -> Result<(), String> {
        let MediaTag::Tts {
            text,
            lang,
            voices,
            speed,
        } = tag
        else {
            return Err("not a speech tag".into());
        };
        self.stop();
        if !speed.is_finite() || *speed <= 0.0 {
            return Err("invalid TTS speed".into());
        }
        if lang.contains('\0') || voices.iter().any(|v| v.contains('\0')) || text.contains('\0') {
            return Err("invalid NUL in speech tag".into());
        }
        // Bound a single pipe write so an absent/blocked engine cannot freeze the
        // audio queue or allocate unlimited input from an untrusted card.
        if text.len() > 64 * 1024 {
            return Err("speech text exceeds 64 KiB".into());
        }
        let (backend, exe) = self.engine()?;
        let speed = speed.clamp(0.1, 4.0);
        let lang = lang.replace('_', "-");
        #[cfg(unix)]
        if backend == TtsBackend::SpeechDispatcher && self.config.executable.is_none() {
            match crate::dispatcher::Dispatcher::connect() {
                Ok(mut dispatcher) => {
                    dispatcher.speak(text, &lang, voices.first(), speed)?;
                    self.dispatcher = Some(dispatcher);
                    return Ok(());
                }
                Err(error) => {
                    if self.config.backend != TtsBackend::Auto || find("espeak-ng").is_none() {
                        return Err(error);
                    }
                    let mut fallback = self.config.clone();
                    fallback.backend = TtsBackend::Espeak;
                    let original = std::mem::replace(&mut self.config, fallback);
                    let result = self.play(tag);
                    self.config = original;
                    return result;
                }
            }
        }
        let mut cmd = Command::new(exe);
        match backend {
            TtsBackend::SpeechDispatcher => {
                cmd.args([
                    "--wait",
                    "--application-name",
                    "tanki",
                    "--language",
                    &lang,
                    "--rate",
                    &(((speed - 1.0) * 100.0).round() as i32)
                        .clamp(-100, 100)
                        .to_string(),
                ]);
                if let Some(v) = voices.first() {
                    cmd.args(["--synthesis-voice", v]);
                }
                cmd.arg("--").arg(text);
            }
            TtsBackend::Espeak => {
                cmd.args([
                    "--stdin",
                    "-v",
                    voices.first().unwrap_or(&lang),
                    "-s",
                    &(175.0 * speed).round().to_string(),
                ]);
            }
            TtsBackend::Say => {
                cmd.args(["-r", &(170.0 * speed).round().to_string(), "-f", "-"]);
                if let Some(v) = voices.first() {
                    cmd.args(["-v", v]);
                } else {
                    let output = Command::new(cmd.get_program())
                        .args(["-v", "?"])
                        .stdin(Stdio::null())
                        .stderr(Stdio::null())
                        .output()
                        .map_err(|e| format!("cannot list macOS voices: {e}"))?;
                    let voice = say_voice(&String::from_utf8_lossy(&output.stdout), &lang)
                        .ok_or_else(|| format!("no installed macOS voice for {lang}"))?;
                    cmd.args(["-v", &voice]);
                }
            }
            TtsBackend::Piper => {
                let model = self
                    .config
                    .piper_model
                    .as_ref()
                    .ok_or("Piper requires a local ONNX model")?;
                if !model.is_file() {
                    return Err("Piper model not found".into());
                }
                let dir = tempfile::Builder::new()
                    .prefix("tanki-tts-")
                    .tempdir()
                    .map_err(|e| e.to_string())?;
                let output = dir.path().join("speech.wav");
                cmd.arg("--model")
                    .arg(model)
                    .arg("--output_file")
                    .arg(&output)
                    .args(["--length_scale", &(1.0 / speed).to_string()]);
                self.generated = Some(output);
                self.wav = Some(dir);
            }
            _ => unreachable!(),
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_command(&mut cmd).map_err(|e| format!("cannot start TTS: {e}"))?;
        let mut input = child.stdin.take().ok_or("TTS stdin unavailable")?;
        // The helper owns the pipe. Killing the child on interruption unblocks
        // the writer even when the speech daemon stops accepting input.
        let text = text.clone();
        std::thread::spawn(move || {
            let _ = input.write_all(text.as_bytes());
            let _ = input.write_all(b"\n");
        });
        self.child = Some(child);
        Ok(())
    }

    /// Returns true once the CLI has finished; Piper then has a WAV to play.
    pub(crate) fn finished(&mut self) -> bool {
        #[cfg(unix)]
        if let Some(dispatcher) = self.dispatcher.as_mut() {
            return match dispatcher.finished() {
                Ok(done) => done,
                Err(error) => {
                    self.failure = Some(error);
                    true
                }
            };
        }
        let Some(child) = self.child.as_mut() else {
            return true;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.child = None;
                if !status.success() {
                    self.failure = Some(format!("TTS engine exited with {status}"));
                    self.generated = None;
                }
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.failure = Some(format!("TTS engine failed: {e}"));
                self.stop();
                true
            }
        }
    }
    pub(crate) fn take_generated(&mut self) -> Option<PathBuf> {
        self.generated.take()
    }
    pub(crate) fn take_error(&mut self) -> Option<String> {
        self.failure.take()
    }
    pub(crate) fn stop(&mut self) {
        #[cfg(unix)]
        {
            self.dispatcher = None;
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.generated = None;
        self.wav = None;
        self.failure = None;
    }
    pub(crate) fn pause(&mut self, paused: bool) {
        #[cfg(unix)]
        if let Some(dispatcher) = self.dispatcher.as_mut() {
            dispatcher.pause(paused);
            return;
        }
        #[cfg(unix)]
        if let Some(child) = &self.child {
            // The owned child PID cannot be reused before it is reaped.
            unsafe {
                libc::kill(
                    child.id() as libc::pid_t,
                    if paused { libc::SIGSTOP } else { libc::SIGCONT },
                );
            }
        }
        #[cfg(not(unix))]
        let _ = paused;
    }
}
impl Drop for Speech {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn spawn_command(command: &mut Command) -> std::io::Result<Child> {
    // A just-written executable on overlay filesystems can briefly return
    // ETXTBSY even after its creator closes it. Retry only that transient case.
    for _ in 0..10 {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            result => return result,
        }
    }
    command.spawn()
}

fn say_voice(list: &str, lang: &str) -> Option<String> {
    let desired = lang.replace('-', "_");
    let voices: Vec<_> = list
        .lines()
        .filter_map(|line| {
            let entry = line.split('#').next()?.trim();
            let (name, locale) = entry.rsplit_once(char::is_whitespace)?;
            Some((name.trim(), locale))
        })
        .collect();
    voices
        .iter()
        .find(|(_, locale)| *locale == desired)
        .or_else(|| {
            voices
                .iter()
                .find(|(_, locale)| locale.split('_').next() == desired.split('_').next())
        })
        .map(|(name, _)| (*name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[cfg(unix)]
    fn executable(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("speech");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn tag(text: &str) -> MediaTag {
        MediaTag::Tts {
            text: text.into(),
            lang: "en_US".into(),
            voices: vec!["-voice $(never)".into()],
            speed: 1.2,
        }
    }
    #[cfg(unix)]
    #[test]
    fn text_and_voice_are_data_and_child_completion_is_polled() {
        let dir = tempfile::tempdir().unwrap();
        let exe = executable(
            dir.path(),
            "printf '%s\\n' \"$@\" > \"$0.args\"\ncat > \"$0.text\"",
        );
        let mut speech = Speech::new(TtsConfig {
            backend: TtsBackend::Espeak,
            executable: Some(exe.clone()),
            ..Default::default()
        });
        speech
            .play(&tag("-- dangerous; $(touch /tmp/never)\nhello"))
            .unwrap();
        for _ in 0..100 {
            if speech.finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(speech.finished());
        assert!(speech.take_error().is_none());
        let args = std::fs::read_to_string(exe.with_extension("args")).unwrap();
        assert!(args.contains("-voice $(never)"));
        let text = std::fs::read_to_string(exe.with_extension("text")).unwrap();
        assert!(text.contains("$(touch /tmp/never)"));
    }
    #[test]
    fn rejects_invalid_speed_and_explicit_off() {
        let mut s = Speech::new(TtsConfig {
            backend: TtsBackend::Off,
            ..Default::default()
        });
        assert!(s.play(&tag("hi")).unwrap_err().contains("disabled"));
        let mut t = tag("hi");
        if let MediaTag::Tts { speed, .. } = &mut t {
            *speed = f32::NAN;
        }
        assert!(s.play(&t).unwrap_err().contains("speed"));
    }
    #[test]
    fn macos_selects_a_voice_name_for_the_requested_language() {
        let list = "Alice it_IT # Ciao\nEddy (English (US)) en_US # Hello\nDaniel en_GB # Hello";
        assert_eq!(
            say_voice(list, "en-US").as_deref(),
            Some("Eddy (English (US))")
        );
        assert_eq!(
            say_voice(list, "en-AU").as_deref(),
            Some("Eddy (English (US))")
        );
        assert_eq!(say_voice(list, "ja-JP"), None);
    }

    #[cfg(unix)]
    #[test]
    fn each_engine_receives_its_own_language_rate_and_stdin_flags() {
        for (backend, required) in [
            (
                TtsBackend::SpeechDispatcher,
                "--language\nen-US\n--rate\n20",
            ),
            (TtsBackend::Say, "-r\n204\n-f\n-"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let exe = executable(
                dir.path(),
                "printf '%s\\n' \"$@\" > \"$0.args\"\ncat > \"$0.text\"",
            );
            let mut speech = Speech::new(TtsConfig {
                backend,
                executable: Some(exe.clone()),
                ..Default::default()
            });
            speech.play(&tag("hello")).unwrap();
            for _ in 0..100 {
                if speech.finished() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(speech.finished());
            let args = std::fs::read_to_string(exe.with_extension("args")).unwrap();
            assert!(args.contains(required), "{backend:?}: {args}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn stop_reaps_the_owned_process_and_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let exe = executable(dir.path(), "cat > /dev/null\nexec sleep 30");
        let mut speech = Speech::new(TtsConfig {
            backend: TtsBackend::Espeak,
            executable: Some(exe),
            ..Default::default()
        });
        speech.play(&tag("hello")).unwrap();
        let pid = speech.child.as_ref().unwrap().id();
        speech.stop();
        assert!(speech.child.is_none());
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        let exe = executable(dir.path(), "exit 3");
        speech.config.executable = Some(exe);
        speech.play(&tag("hello")).unwrap();
        for _ in 0..100 {
            if speech.finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(speech.take_error().unwrap().contains("3"));
    }

    #[cfg(unix)]
    #[test]
    fn piper_needs_explicit_local_model_and_returns_generated_waveform() {
        let dir = tempfile::tempdir().unwrap();
        let exe = executable(dir.path(), "while test $# -gt 0; do if test \"$1\" = --output_file; then shift; output=$1; fi; shift; done\ncat > /dev/null\nprintf 'waveform' > \"$output\"");
        let mut speech = Speech::new(TtsConfig {
            backend: TtsBackend::Piper,
            executable: Some(exe),
            ..Default::default()
        });
        assert!(speech.play(&tag("hello")).unwrap_err().contains("model"));
        let model = dir.path().join("voice.onnx");
        std::fs::write(&model, b"model").unwrap();
        speech.config.piper_model = Some(model);
        speech.play(&tag("hello")).unwrap();
        for _ in 0..100 {
            if speech.finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let wav = speech.take_generated().unwrap();
        assert_eq!(std::fs::read(&wav).unwrap(), b"waveform");
        speech.stop();
        assert!(!wav.exists());
    }
}
