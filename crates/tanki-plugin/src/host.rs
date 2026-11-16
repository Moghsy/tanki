//! One running plugin process: spawn, bounded stdio, kill on drop.
use crate::{
    manifest::Loaded,
    protocol::{self, FromPlugin, LineError, ToPlugin},
};
use std::{
    io::{BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::{
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread,
};

/// Messages from the reader thread.
#[derive(Debug)]
pub enum Incoming {
    Msg(FromPlugin),
    /// stdout closed.
    Closed,
    Bad(String),
}

pub struct Host {
    child: Child,
    to_plugin: SyncSender<String>,
    from_plugin: Receiver<Incoming>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

/// Variables a plugin may inherit. Everything else is removed.
fn inherited(name: &str) -> bool {
    matches!(name, "HOME" | "PATH" | "TERM" | "LANG")
        || name.starts_with("XDG_")
        || name.starts_with("HERDR_")
}

impl Host {
    pub fn spawn(plugin: &Loaded) -> Result<Self, String> {
        let program = &plugin.manifest.command[0];
        let program = match program.strip_prefix("./") {
            Some(rel) => plugin.dir.join(rel),
            None => program.into(),
        };
        let mut command = Command::new(program);
        command
            .args(&plugin.manifest.command[1..])
            .current_dir(&plugin.dir)
            .env_clear()
            .env("TANKI_PLUGIN_API", crate::API.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in std::env::vars_os() {
            if name.to_str().is_some_and(inherited) {
                command.env(name, value);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("no stderr")?;

        let (to_plugin, lines) = mpsc::sync_channel::<String>(64);
        thread::spawn(move || {
            for line in lines {
                if stdin
                    .write_all(line.as_bytes())
                    .and_then(|()| stdin.write_all(b"\n"))
                    .and_then(|()| stdin.flush())
                    .is_err()
                {
                    break;
                }
            }
        });

        // A full channel blocks this thread, which stops reading and so applies
        // backpressure to a flooding plugin.
        let (tx, from_plugin) = mpsc::sync_channel(256);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let item = match protocol::read_line(&mut reader) {
                    Ok(Some(line)) => match protocol::parse(&line) {
                        Ok(message) => Incoming::Msg(message),
                        Err(e) => Incoming::Bad(e),
                    },
                    Ok(None) => Incoming::Closed,
                    Err(LineError::TooLong) => Incoming::Bad("message too long".into()),
                    Err(LineError::Io(e)) => Incoming::Bad(e),
                };
                let last = !matches!(item, Incoming::Msg(_));
                if tx.send(item).is_err() || last {
                    break;
                }
            }
        });

        // Keep only the first 4 KiB of stderr for diagnostics; discard the rest.
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stderr);
        thread::spawn(move || {
            let mut chunk = [0u8; 1024];
            while let Ok(n) = stderr_pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut kept = sink.lock().unwrap();
                let room = 4096usize.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        });
        Ok(Self {
            child,
            to_plugin,
            from_plugin,
            stderr,
        })
    }

    /// Queues a message. An error means the plugin is not reading and should
    /// be dropped.
    pub fn send(&self, message: &ToPlugin) -> Result<(), String> {
        let line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        if line.len() > protocol::MAX_LINE {
            return Err("outgoing message too long".into());
        }
        self.to_plugin.try_send(line).map_err(|e| match e {
            TrySendError::Full(_) => "plugin is not reading its input".to_owned(),
            TrySendError::Disconnected(_) => "plugin input closed".to_owned(),
        })
    }

    /// Up to `max` pending messages, without blocking.
    pub fn drain(&self, max: usize) -> Vec<Incoming> {
        let mut out = Vec::new();
        while out.len() < max {
            match self.from_plugin.try_recv() {
                Ok(item) => out.push(item),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    out.push(Incoming::Closed);
                    break;
                }
            }
        }
        out
    }

    pub fn stderr_excerpt(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap())
            .trim()
            .to_owned()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
