use crate::API;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Closed catalogue of events tanki can deliver. Unknown names fail to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hook {
    AnswerRevealed,
}

/// Things a plugin may ask the host to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    HoldReview,
    Status,
    Notify,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub api: u32,
    pub command: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub manifest: Manifest,
    pub dir: PathBuf,
    /// SHA-256 of the exact `plugin.toml` bytes; the user's grant pins it.
    pub digest: String,
}

fn ident(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if !ident(&self.name) {
            return Err("name must be 1-64 characters of a-z, 0-9, - or _".into());
        }
        if self.api != API {
            return Err(format!("unsupported api {} (host speaks {API})", self.api));
        }
        let Some(program) = self.command.first() else {
            return Err("command is empty".into());
        };
        if self.command.iter().any(|a| a.contains('\0')) {
            return Err("command contains a NUL".into());
        }
        let relative = program.strip_prefix("./");
        match relative {
            Some(rel) => {
                if rel.is_empty()
                    || Path::new(rel)
                        .components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return Err("command path must stay inside the plugin directory".into());
                }
            }
            None if program.contains('/') => {
                return Err("command must be ./relative or a bare program name".into())
            }
            None => {}
        }
        for field in &self.fields {
            if field.is_empty() || field.len() > 128 || field.chars().any(char::is_control) {
                return Err(format!("invalid field name {field:?}"));
            }
        }
        for action in &self.actions {
            if !ident(&action.id) || action.title.is_empty() || action.title.len() > 80 {
                return Err(format!("invalid action {:?}", action.id));
            }
        }
        Ok(())
    }
}

/// Reads and validates `<dir>/plugin.toml`. The directory name must match.
pub fn load(dir: &Path) -> Result<Loaded, String> {
    let path = dir.join("plugin.toml");
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() > 64 * 1024 {
        return Err("plugin.toml is larger than 64 KiB".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let manifest: Manifest = toml::from_str(text).map_err(|e| e.to_string())?;
    manifest.validate()?;
    if dir.file_name().and_then(|n| n.to_str()) != Some(manifest.name.as_str()) {
        return Err("directory name must equal the manifest name".into());
    }
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(Loaded {
        manifest,
        dir: dir.to_owned(),
        digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(extra: &str) -> Result<Manifest, String> {
        let text = format!("name='x'\nversion='1'\napi=1\ncommand=['./run']\n{extra}");
        let m: Manifest = toml::from_str(&text).map_err(|e| e.to_string())?;
        m.validate()?;
        Ok(m)
    }
    #[test]
    fn accepts_a_declared_manifest() {
        let m = manifest(
            "hooks=['answer_revealed']\ncapabilities=['hold_review']\nfields=['CodeTour']",
        )
        .unwrap();
        assert_eq!(m.hooks, [Hook::AnswerRevealed]);
    }
    #[test]
    fn rejects_unknown_hooks_capabilities_and_keys() {
        assert!(manifest("hooks=['rm_everything']").is_err());
        assert!(manifest("capabilities=['shell']").is_err());
        assert!(manifest("extra=1").is_err());
    }
    #[test]
    fn rejects_escaping_commands() {
        for bad in ["/bin/sh", "./../x", "../x", "bin/x", "./"] {
            let text = format!("name='x'\nversion='1'\napi=1\ncommand=['{bad}']");
            let m: Manifest = toml::from_str(&text).unwrap();
            assert!(m.validate().is_err(), "{bad}");
        }
    }
    #[test]
    fn rejects_other_api_and_bad_names() {
        let m: Manifest = toml::from_str("name='X'\nversion='1'\napi=1\ncommand=['p']").unwrap();
        assert!(m.validate().is_err());
        let m: Manifest = toml::from_str("name='x'\nversion='1'\napi=2\ncommand=['p']").unwrap();
        assert!(m.validate().is_err());
    }
}
