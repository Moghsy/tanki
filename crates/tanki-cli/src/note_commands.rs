//! CLI stream (Phase 3): `tanki add`, `tanki edit`, `tanki search` and
//! `tanki delete` (PLAN 8.4, 8.5, 8.7, 9).
use tanki_core::CollectionActor;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Add a note from flags, a note file, or stdin.
#[derive(clap::Args)]
pub struct AddArgs {}
/// Edit matching notes in $EDITOR.
#[derive(clap::Args)]
pub struct EditArgs {}
/// Search cards or notes.
#[derive(clap::Args)]
pub struct SearchArgs {}
/// Delete matching notes.
#[derive(clap::Args)]
pub struct DeleteArgs {}

/// Errors with their own exit code (PLAN 9): 26 = a note changed since it
/// was read (edit conflict).
#[derive(Debug)]
pub struct NoteCliError {
    pub message: String,
    pub code: u8,
}
impl NoteCliError {
    pub fn exit_code(&self) -> u8 {
        self.code
    }
}
impl std::fmt::Display for NoteCliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for NoteCliError {}

fn todo(name: &str) -> Result<()> {
    Err(format!("tanki {name} is not implemented yet").into())
}
pub fn run_add(_actor: &CollectionActor, _args: &AddArgs, _json: bool) -> Result<()> {
    todo("add")
}
pub fn run_edit(_actor: &CollectionActor, _args: &EditArgs, _json: bool) -> Result<()> {
    todo("edit")
}
pub fn run_search(_actor: &CollectionActor, _args: &SearchArgs, _json: bool) -> Result<()> {
    todo("search")
}
pub fn run_delete(_actor: &CollectionActor, _args: &DeleteArgs, _json: bool) -> Result<()> {
    todo("delete")
}
