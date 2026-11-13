//! Backend selection plus the built-in (rodio) and mpv backends.
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};

use crate::engine::{Backend, Selected};
use crate::{AudioConfig, BackendChoice, BackendStatus};

pub(crate) fn select(config: &AudioConfig) -> Selected {
    match config.backend {
        BackendChoice::Off => Selected::Inactive(BackendStatus::Off),
        BackendChoice::Builtin => builtin_or_none(),
        BackendChoice::Mpv => match find_mpv(config.mpv.as_deref()) {
            None => Selected::Inactive(BackendStatus::NoDevice("mpv not found".into())),
            Some(path) => match mpv::Mpv::start(&path) {
                Ok(m) => Selected::Active(Box::new(m), BackendStatus::Mpv),
                Err(e) => Selected::Inactive(BackendStatus::NoDevice(format!("mpv: {e}"))),
            },
        },
        BackendChoice::Auto => {
            if let Some(path) = find_mpv(config.mpv.as_deref()) {
                if let Ok(m) = mpv::Mpv::start(&path) {
                    return Selected::Active(Box::new(m), BackendStatus::Mpv);
                }
            }
            builtin_or_none()
        }
    }
}

fn builtin_or_none() -> Selected {
    match Builtin::open() {
        Ok(b) => Selected::Active(Box::new(b), BackendStatus::Builtin),
        Err(reason) => Selected::Inactive(BackendStatus::NoDevice(reason)),
    }
}

/// Explicit path if it exists, else the first `mpv` on `PATH`.
pub(crate) fn find_mpv(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("mpv"))
        .find(|c| c.is_file())
}

/// Opens `path` for the built-in player. Separate from playback so the
/// decode path is testable without an output device.
pub(crate) fn decode(path: &Path) -> Result<Decoder<std::io::BufReader<File>>, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = File::open(path).map_err(|e| format!("cannot open {name}: {e}"))?;
    let len = file.metadata().map(|m| m.len()).ok();
    let mut builder = Decoder::builder()
        .with_data(std::io::BufReader::new(file))
        .with_seekable(true);
    if let Some(len) = len {
        builder = builder.with_byte_len(len);
    }
    builder
        .build()
        .map_err(|e| format!("cannot play {name} (unsupported audio or video format: {e})"))
}

struct Builtin {
    sink: MixerDeviceSink,
    player: Option<Player>,
}

impl Builtin {
    fn open() -> Result<Self, String> {
        let mut sink = DeviceSinkBuilder::open_default_sink().map_err(|e| e.to_string())?;
        sink.log_on_drop(false);
        Ok(Self { sink, player: None })
    }
}

impl Backend for Builtin {
    fn play(&mut self, path: &Path) -> Result<(), String> {
        let source = decode(path)?;
        self.player = None; // dropping the old player stops it
        let player = Player::connect_new(self.sink.mixer());
        player.append(source);
        self.player = Some(player);
        Ok(())
    }
    fn stop(&mut self) {
        self.player = None;
    }
    fn toggle_pause(&mut self) {
        if let Some(p) = &self.player {
            if p.is_paused() {
                p.play();
            } else {
                p.pause();
            }
        }
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        let Some(p) = &self.player else {
            return Ok(());
        };
        let target = (p.get_pos().as_secs_f64() + seconds).max(0.0);
        p.try_seek(Duration::from_secs_f64(target))
            .map_err(|_| "this audio format does not support seeking".to_string())
    }
    fn finished(&mut self) -> bool {
        self.player
            .as_ref()
            .is_some_and(|p| p.empty() && !p.is_paused())
    }
}

#[cfg(unix)]
mod mpv {
    use super::Backend;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    enum Ev {
        IdleActive(bool),
        EndFile(String),
    }

    pub(super) struct Mpv {
        child: Child,
        sock: UnixStream,
        events: Receiver<Ev>,
        playing: bool,
        seen_active: bool,
        _dir: tempfile::TempDir,
    }

    impl Mpv {
        pub(super) fn start(exe: &Path) -> Result<Self, String> {
            let dir = tempfile::Builder::new()
                .prefix("tanki-mpv-")
                .tempdir()
                .map_err(|e| e.to_string())?;
            let sock_path = dir.path().join("ipc.sock");
            let mut child = Command::new(exe)
                .args([
                    "--idle",
                    "--no-terminal",
                    "--force-window=no",
                    "--audio-display=no",
                    "--keep-open=no",
                    "--autoload-files=no",
                    "--gapless-audio=no",
                    "--no-ytdl",
                ])
                .arg(format!("--input-ipc-server={}", sock_path.display()))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("cannot start mpv: {e}"))?;
            let deadline = Instant::now() + Duration::from_secs(3);
            let sock = loop {
                match UnixStream::connect(&sock_path) {
                    Ok(s) => break s,
                    Err(e) => {
                        let exited = matches!(child.try_wait(), Ok(Some(_)));
                        if exited || Instant::now() > deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(format!("mpv did not open its IPC socket: {e}"));
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                }
            };
            let reader = sock.try_clone().map_err(|e| e.to_string())?;
            let (tx, events) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    if let Some(ev) = parse_event(&line) {
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                }
            });
            let mut m = Self {
                child,
                sock,
                events,
                playing: false,
                seen_active: false,
                _dir: dir,
            };
            m.send(&serde_json::json!(["observe_property", 1, "idle-active"]))?;
            Ok(m)
        }

        fn send(&mut self, command: &serde_json::Value) -> Result<(), String> {
            let mut line = serde_json::json!({ "command": command }).to_string();
            line.push('\n');
            self.sock
                .write_all(line.as_bytes())
                .map_err(|e| format!("mpv connection lost: {e}"))
        }
    }

    fn parse_event(line: &str) -> Option<Ev> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        match v.get("event")?.as_str()? {
            "property-change" if v.get("name")?.as_str()? == "idle-active" => {
                Some(Ev::IdleActive(v.get("data")?.as_bool()?))
            }
            "end-file" => Some(Ev::EndFile(
                v.get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string(),
            )),
            _ => None,
        }
    }

    impl Backend for Mpv {
        fn play(&mut self, path: &Path) -> Result<(), String> {
            // pause persists across files in mpv
            self.send(&serde_json::json!(["set_property", "pause", false]))?;
            self.send(&serde_json::json!([
                "loadfile",
                path.to_string_lossy(),
                "replace"
            ]))?;
            while self.events.try_recv().is_ok() {}
            self.playing = true;
            self.seen_active = false;
            Ok(())
        }
        fn stop(&mut self) {
            let _ = self.send(&serde_json::json!(["stop"]));
            self.playing = false;
        }
        fn toggle_pause(&mut self) {
            let _ = self.send(&serde_json::json!(["cycle", "pause"]));
        }
        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            self.send(&serde_json::json!(["seek", seconds, "relative"]))
        }
        fn finished(&mut self) -> bool {
            if !self.playing {
                return false;
            }
            while let Ok(ev) = self.events.try_recv() {
                match ev {
                    Ev::IdleActive(false) => self.seen_active = true,
                    Ev::IdleActive(true) if self.seen_active => {
                        self.playing = false;
                        return true;
                    }
                    Ev::EndFile(reason) if reason == "eof" || reason == "error" => {
                        self.playing = false;
                        return true;
                    }
                    _ => {}
                }
            }
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.playing = false;
                return true;
            }
            false
        }
    }

    impl Drop for Mpv {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_events() {
            assert!(matches!(
                parse_event(
                    r#"{"event":"property-change","id":1,"name":"idle-active","data":true}"#
                ),
                Some(Ev::IdleActive(true))
            ));
            assert!(matches!(
                parse_event(r#"{"event":"end-file","reason":"eof"}"#),
                Some(Ev::EndFile(r)) if r == "eof"
            ));
            assert!(parse_event(r#"{"event":"seek"}"#).is_none());
            assert!(parse_event("garbage").is_none());
        }

        #[test]
        fn mpv_roundtrip_when_installed() {
            let Some(exe) = super::super::find_mpv(None) else {
                eprintln!("mpv not on PATH: skipping");
                return;
            };
            // --ao=null is not set by Mpv::start, so only check start/stop/drop.
            let mut m = Mpv::start(&exe).expect("start mpv");
            assert!(!m.finished());
            m.stop();
        }
    }
}

#[cfg(not(unix))]
mod mpv {
    use super::Backend;
    use std::path::Path;
    pub(super) struct Mpv;
    impl Mpv {
        pub(super) fn start(_exe: &Path) -> Result<Self, String> {
            Err("mpv IPC is only supported on Unix".into())
        }
    }
    impl Backend for Mpv {
        fn play(&mut self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn toggle_pause(&mut self) {}
        fn seek(&mut self, _: f64) -> Result<(), String> {
            Ok(())
        }
        fn finished(&mut self) -> bool {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path) {
        let samples: u32 = 800; // 0.1 s at 8 kHz, mono, 16 bit
        let data_len = samples * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&16000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        b.resize(b.len() + data_len as usize, 0);
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn decodes_generated_wav() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.wav");
        write_wav(&p);
        let dec = decode(&p).expect("decode wav");
        assert_eq!(rodio::Source::sample_rate(&dec).get(), 8000);
        assert_eq!(dec.count(), 800);
    }

    #[test]
    fn undecodable_file_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("clip.webm");
        std::fs::write(&p, b"not media at all").unwrap();
        let err = decode(&p).err().expect("must fail");
        assert!(err.contains("clip.webm"), "{err}");
    }

    #[test]
    fn find_mpv_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_mpv(Some(&dir.path().join("none"))), None);
        let p = dir.path().join("mpv");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(find_mpv(Some(&p)), Some(p));
    }

    #[test]
    fn builtin_plays_wav_or_reports_no_device() {
        use crate::{Audio, AudioEvent, MediaTag};
        let dir = tempfile::tempdir().unwrap();
        write_wav(&dir.path().join("t.wav"));
        let audio = Audio::start(AudioConfig {
            media_dir: dir.path().to_path_buf(),
            backend: BackendChoice::Builtin,
            mpv: None,
        });
        match audio.status() {
            BackendStatus::NoDevice(_) => {
                let rx = audio.subscribe();
                audio.play(vec![MediaTag::Sound("t.wav".into())], false);
                assert!(matches!(
                    rx.recv_timeout(Duration::from_secs(5)),
                    Ok(AudioEvent::Error(_))
                ));
            }
            BackendStatus::Builtin => {
                // A device exists, but the test must stay silent: only check
                // that the decode path above works and shut down cleanly.
            }
            other => panic!("unexpected status {other:?}"),
        }
    }

    #[test]
    fn off_backend_status() {
        let audio = crate::Audio::start(AudioConfig {
            backend: BackendChoice::Off,
            ..Default::default()
        });
        assert_eq!(audio.status(), BackendStatus::Off);
    }

    #[test]
    fn explicit_mpv_missing_reports_no_device() {
        let audio = crate::Audio::start(AudioConfig {
            backend: BackendChoice::Mpv,
            mpv: Some("/nonexistent/mpv".into()),
            ..Default::default()
        });
        assert!(matches!(audio.status(), BackendStatus::NoDevice(_)));
    }
}
