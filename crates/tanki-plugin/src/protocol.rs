use crate::manifest::Hook;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, io::BufRead};

/// Longest accepted line in either direction.
pub const MAX_LINE: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPlugin {
    Hello {
        api: u32,
        tanki_version: String,
        settings: Value,
    },
    Hook {
        name: Hook,
        card_id: i64,
        note_id: i64,
        deck: String,
        fields: BTreeMap<String, String>,
    },
    Action {
        id: String,
        card_id: i64,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FromPlugin {
    Ready { api: u32 },
    HoldReview { reason: String },
    ReleaseReview,
    Status { text: String },
    Notify { level: Level, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LineError {
    TooLong,
    Io(String),
}

/// Reads one `\n`-terminated line without buffering more than `MAX_LINE`.
/// `Ok(None)` is end of stream.
pub fn read_line(reader: &mut impl BufRead) -> Result<Option<String>, LineError> {
    let mut line = Vec::new();
    loop {
        let (done, used) = {
            let buf = reader
                .fill_buf()
                .map_err(|e| LineError::Io(e.to_string()))?;
            if buf.is_empty() {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Err(LineError::Io("stream ended inside a line".into()))
                };
            }
            match buf.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    line.extend_from_slice(&buf[..i]);
                    (true, i + 1)
                }
                None => {
                    line.extend_from_slice(buf);
                    (false, buf.len())
                }
            }
        };
        reader.consume(used);
        if line.len() > MAX_LINE {
            return Err(LineError::TooLong);
        }
        if done {
            return String::from_utf8(line)
                .map(Some)
                .map_err(|_| LineError::Io("line is not UTF-8".into()));
        }
    }
}

pub fn parse(line: &str) -> Result<FromPlugin, String> {
    serde_json::from_str(line).map_err(|e| format!("malformed plugin message: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_lines() {
        let mut ok = std::io::Cursor::new(b"a\nbc\n".to_vec());
        assert_eq!(read_line(&mut ok), Ok(Some("a".into())));
        assert_eq!(read_line(&mut ok), Ok(Some("bc".into())));
        assert_eq!(read_line(&mut ok), Ok(None));
        let mut long = std::io::Cursor::new(vec![b'x'; MAX_LINE + 10]);
        assert_eq!(read_line(&mut long), Err(LineError::TooLong));
        let mut partial = std::io::Cursor::new(b"abc".to_vec());
        assert!(matches!(read_line(&mut partial), Err(LineError::Io(_))));
    }
    #[test]
    fn rejects_unknown_messages() {
        assert!(parse(r#"{"type":"status","text":"x"}"#).is_ok());
        assert!(parse(r#"{"type":"exec","cmd":"x"}"#).is_err());
        assert!(parse(r#"{"type":"status","text":"x","extra":1}"#).is_err());
    }
}
