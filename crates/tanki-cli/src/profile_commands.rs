//! `tanki profiles list|create|rename|delete` (PLAN 8.13). Owned by the
//! Maintenance stream (docs/implementation/phase-5-interfaces.md). Works on
//! the base folder's subfolders without opening a collection, except to
//! create one; never reads `prefs21.db` or touches `addons21/` (PLAN 14).
use clap::{Args, Subcommand};
use std::io::{IsTerminal, Write};
use std::path::Path;
use tanki_core::maintenance::profiles;

#[derive(Args, Debug)]
pub struct ProfileArgs {
    #[command(subcommand)]
    pub action: ProfileAction,
}

#[derive(Subcommand, Debug)]
pub enum ProfileAction {
    /// List the profiles in the base folder.
    List,
    /// Create a profile with a new empty collection.
    Create { name: String },
    /// Rename a profile (moves its folder and tanki's settings for it).
    Rename { old: String, new: String },
    /// Delete a profile: its folder moves to `<base>/.tanki-trash`.
    Delete {
        name: String,
        /// Type the profile's name again to confirm (asked at a terminal).
        #[arg(long)]
        confirm: Option<String>,
    },
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn print_json(value: &serde_json::Value) -> Result<()> {
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string_pretty(value)?
    )?;
    Ok(())
}

/// `base` is the validated base folder; `config` is tanki's config file.
pub fn run_profiles(
    base: &Path,
    config: Option<&Path>,
    args: &ProfileArgs,
    json: bool,
) -> Result<()> {
    match &args.action {
        ProfileAction::List => {
            let found = profiles::list(base)?;
            let last = config
                .and_then(|file| tanki_core::config::Config::load(file).ok())
                .and_then(|c| c.last_profile);
            if json {
                return print_json(&serde_json::json!({
                    "profiles": found.iter().map(|p| serde_json::json!({
                        "name": p.name, "collection_bytes": p.collection_bytes,
                        "last_used": last.as_deref() == Some(p.name.as_str()),
                    })).collect::<Vec<_>>(),
                }));
            }
            let mut out = std::io::stdout().lock();
            for p in &found {
                let mark = if last.as_deref() == Some(p.name.as_str()) {
                    "*"
                } else {
                    " "
                };
                writeln!(out, "{mark} {}", p.name)?;
            }
            if found.is_empty() {
                writeln!(out, "No profiles in {}", base.display())?;
            }
            Ok(())
        }
        ProfileAction::Create { name } => {
            let path = profiles::create(base, name)?;
            if json {
                print_json(&serde_json::json!({"created": name, "path": path}))
            } else {
                writeln!(std::io::stdout().lock(), "Created profile {name}")?;
                Ok(())
            }
        }
        ProfileAction::Rename { old, new } => {
            let change = profiles::rename(base, old, new, config)?;
            if json {
                print_json(&serde_json::json!({
                    "renamed": old, "to": new,
                    "config_updated": change.config_updated,
                    "credentials_updated": change.credentials_updated,
                }))
            } else {
                writeln!(std::io::stdout().lock(), "Renamed profile {old} to {new}")?;
                Ok(())
            }
        }
        ProfileAction::Delete { name, confirm } => {
            let typed = match confirm {
                Some(typed) => typed.clone(),
                None if std::io::stdin().is_terminal() => {
                    eprint!("Type the profile name ({name}) to delete it: ");
                    std::io::stderr().flush()?;
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim_end_matches(['\r', '\n']).to_owned()
                }
                None => {
                    return Err(format!(
                        "Refusing to delete profile {name} without --confirm {name}"
                    )
                    .into())
                }
            };
            let trash = profiles::delete(base, name, &typed, None, config)?;
            if json {
                print_json(&serde_json::json!({"deleted": name, "moved_to": trash}))
            } else {
                writeln!(
                    std::io::stdout().lock(),
                    "Deleted profile {name}; its folder is now {}",
                    trash.display()
                )?;
                Ok(())
            }
        }
    }
}
