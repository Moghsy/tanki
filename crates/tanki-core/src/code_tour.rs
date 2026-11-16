//! Saved-tour references are note content, never commands or filesystem paths.
use serde::{Deserialize, Serialize};

pub const FIELD: &str = "CodeTour";
pub const NOTETYPE: &str = "Tanki Code Tour";
const PREFIX: &str = "code-tour://saved/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CodeTourRef(String);

impl CodeTourRef {
    pub fn from_id(id: &str) -> Result<Self, String> {
        if id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(id.to_owned()))
        } else {
            Err("A tour ID must contain 32 lowercase hexadecimal characters".into())
        }
    }
    pub fn id(&self) -> &str {
        &self.0
    }
    pub fn uri(&self) -> String {
        format!("{PREFIX}{}", self.0)
    }
}
impl TryFrom<String> for CodeTourRef {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        Self::from_id(
            value
                .strip_prefix(PREFIX)
                .ok_or("Expected code-tour://saved/TOUR_ID")?,
        )
    }
}
impl From<CodeTourRef> for String {
    fn from(value: CodeTourRef) -> Self {
        value.uri()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attachment {
    Saved(CodeTourRef),
    Invalid(String),
}

/// A single editor wrapper is accepted; multiple blocks and arbitrary HTML are
/// rejected. This deliberately does not strip tags and concatenate their text.
pub fn parse_field(value: &str) -> Option<Attachment> {
    let parse = || -> Result<Option<CodeTourRef>, String> {
        if value.len() > 4096 {
            return Err("CodeTour field exceeds 4096 bytes".into());
        }
        let mut text = value.trim();
        for tag in ["div", "p", "span"] {
            let open = format!("<{tag}>");
            let close = format!("</{tag}>");
            if let Some(inner) = text
                .strip_prefix(&open)
                .and_then(|s| s.strip_suffix(&close))
            {
                text = inner.trim();
                break;
            }
        }
        let mut decoded = String::new();
        while !text.is_empty() {
            if let Some(rest) = text.strip_prefix('&') {
                let (entity, tail) = rest
                    .split_once(';')
                    .ok_or("Invalid entity in CodeTour field")?;
                let ch = match entity {
                    "nbsp" => ' ',
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    _ => {
                        let n = if let Some(n) = entity.strip_prefix("#x") {
                            u32::from_str_radix(n, 16).ok()
                        } else {
                            entity.strip_prefix('#').and_then(|n| n.parse().ok())
                        };
                        n.and_then(char::from_u32)
                            .ok_or("Unsupported entity in CodeTour field")?
                    }
                };
                decoded.push(ch);
                text = tail;
            } else {
                let ch = text.chars().next().unwrap();
                decoded.push(ch);
                text = &text[ch.len_utf8()..];
            }
        }
        if decoded.chars().any(char::is_control) {
            return Err("Controls are not allowed in CodeTour references".into());
        }
        let decoded = decoded.trim();
        if decoded.is_empty() {
            return Ok(None);
        }
        Ok(Some(CodeTourRef::try_from(decoded.to_owned())?))
    };
    match parse() {
        Ok(Some(reference)) => Some(Attachment::Saved(reference)),
        Ok(None) => None,
        Err(message) => Some(Attachment::Invalid(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "0123456789abcdef0123456789abcdef";
    #[test]
    fn canonical_and_editor_wrappers() {
        let reference = CodeTourRef::from_id(ID).unwrap();
        for text in [
            reference.uri(),
            format!(" <div>{}</div> ", reference.uri()),
            format!("<p>code-tour:&#47;/saved/{ID}</p>"),
        ] {
            assert_eq!(
                parse_field(&text),
                Some(Attachment::Saved(reference.clone()))
            );
        }
        assert_eq!(parse_field(" <div>&nbsp;</div> "), None);
        assert!(serde_json::from_str::<CodeTourRef>("\"../../bad\"").is_err());
    }
    #[test]
    fn rejects_ambiguous_and_executable_content() {
        for text in [
            format!("code-tour://saved/{ID}?exec=bad"),
            format!("code-tour://saved/{ID}#box"),
            format!("<div>code-tour://saved/{ID}</div><div>x</div>"),
            format!("code-tour://saved/{ID}\u{1b}"),
            format!("code-tour://saved/{ID};touch /tmp/no"),
            "x".repeat(4097),
            "code-tour://saved/UPPERCASE".into(),
        ] {
            assert!(
                matches!(parse_field(&text), Some(Attachment::Invalid(_))),
                "{text}"
            );
        }
    }
}
