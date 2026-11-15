//! Minimal local SSIP client. Keeping its connection lets interruption cancel
//! only tanki's messages; global `spd-say --cancel` would stop other clients.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub(crate) struct Dispatcher {
    input: BufReader<UnixStream>,
    output: UnixStream,
    partial: String,
    done: bool,
}
impl Dispatcher {
    pub(crate) fn connect() -> Result<Self, String> {
        let path = if let Some(address) = std::env::var_os("SPEECHD_ADDRESS") {
            let address = address.to_string_lossy();
            address
                .strip_prefix("unix_socket:")
                .ok_or("speech-dispatcher requires a local unix socket")?
                .into()
        } else {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::path::PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
                })
                .join("speech-dispatcher/speechd.sock")
        };
        Self::at(&path)
    }
    fn at(path: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(path)
            .map_err(|e| format!("speech-dispatcher is unavailable: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(200)))
            .map_err(|e| e.to_string())?;
        let mut client = Self {
            input: BufReader::new(stream.try_clone().map_err(|e| e.to_string())?),
            output: stream,
            partial: String::new(),
            done: false,
        };
        client.command("SET SELF CLIENT_NAME tanki:reviewer:tanki")?;
        client.command("SET SELF NOTIFICATION END ON")?;
        client.command("SET SELF NOTIFICATION CANCEL ON")?;
        client.command("SET SELF PRIORITY TEXT")?;
        Ok(client)
    }
    fn reply(&mut self) -> Result<Option<String>, String> {
        match self.input.read_line(&mut self.partial) {
            Ok(0) => Err("speech-dispatcher disconnected".into()),
            Ok(_) => {
                let line = std::mem::take(&mut self.partial);
                if line.starts_with("702 ") || line.starts_with("703 ") {
                    self.done = true;
                }
                Ok(Some(line))
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e.to_string()),
        }
    }
    fn command(&mut self, command: &str) -> Result<(), String> {
        self.output
            .write_all(command.as_bytes())
            .and_then(|_| self.output.write_all(b"\r\n"))
            .map_err(|e| e.to_string())?;
        loop {
            let line = self.reply()?.ok_or("speech-dispatcher timed out")?;
            let code = line
                .get(..3)
                .and_then(|c| c.parse::<u16>().ok())
                .unwrap_or(0);
            if (700..800).contains(&code) {
                continue;
            }
            if (200..300).contains(&code) && line.as_bytes().get(3) == Some(&b' ') {
                return Ok(());
            }
            if code >= 300 || code == 0 {
                return Err(format!(
                    "speech-dispatcher rejected request: {}",
                    line.trim()
                ));
            }
        }
    }
    pub(crate) fn speak(
        &mut self,
        text: &str,
        lang: &str,
        voice: Option<&String>,
        speed: f32,
    ) -> Result<(), String> {
        if !lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err("invalid speech language".into());
        }
        self.command(&format!("SET SELF LANGUAGE {lang}"))?;
        self.command(&format!(
            "SET SELF RATE {}",
            (((speed - 1.0) * 100.0).round() as i32).clamp(-100, 100)
        ))?;
        if let Some(voice) = voice {
            if voice.contains(['\r', '\n']) {
                return Err("invalid speech voice".into());
            }
            self.command(&format!("SET SELF SYNTHESIS_VOICE {voice}"))?;
        }
        self.command("SPEAK")?;
        // SSIP's dot terminator must be escaped in message data, including a
        // malicious card containing an entire line of protocol commands.
        let mut body = String::new();
        for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
            if line.starts_with('.') {
                body.push('.');
            }
            body.push_str(line);
            body.push_str("\r\n");
        }
        body.push('.');
        self.done = false;
        self.command(&body)
    }
    pub(crate) fn finished(&mut self) -> Result<bool, String> {
        self.input
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(1)))
            .map_err(|e| e.to_string())?;
        let result = (|| {
            while !self.done && self.reply()?.is_some() {}
            Ok(self.done)
        })();
        let _ = self
            .input
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(200)));
        result
    }
    pub(crate) fn pause(&mut self, paused: bool) {
        let _ = self.command(if paused { "PAUSE SELF" } else { "RESUME SELF" });
    }
    pub(crate) fn stop(&mut self) {
        let _ = self.command("CANCEL SELF");
        self.done = true;
    }
}
impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[test]
    fn escaped_data_and_controls_remain_on_our_connection() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("speech.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let observed = log.clone();
        let server = std::thread::spawn(move || {
            let (mut writer, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(writer.try_clone().unwrap());
            let mut message = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                observed.lock().unwrap().push(line.clone());
                if line == "SPEAK\r\n" {
                    message = true;
                    writer.write_all(b"230 OK RECEIVING DATA\r\n").unwrap();
                } else if message && line == ".\r\n" {
                    message = false;
                    writer
                        .write_all(
                            b"225-1\r\n225 OK MESSAGE QUEUED\r\n702-1\r\n702-1\r\n702 END\r\n",
                        )
                        .unwrap();
                } else if !message {
                    writer.write_all(b"200 OK\r\n").unwrap();
                }
            }
        });
        let mut dispatcher = Dispatcher::at(&socket).unwrap();
        dispatcher
            .speak("hello\n.\nCANCEL ALL\n!-! STOP ALL", "en-US", None, 1.0)
            .unwrap();
        assert!(dispatcher.finished().unwrap());
        dispatcher.pause(true);
        dispatcher.pause(false);
        dispatcher.stop();
        drop(dispatcher);
        server.join().unwrap();
        let log = log.lock().unwrap();
        assert!(log.iter().any(|line| line == "..\r\n"));
        assert!(log.iter().any(|line| line == "CANCEL SELF\r\n"));
        assert!(log.iter().any(|line| line == "PAUSE SELF\r\n"));
    }
}
