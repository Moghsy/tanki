//! Terminal capability diagnostics. This crate never opens a collection.
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphicsMode {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Iterm,
    Blocks,
    None,
}
#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    pub graphics: GraphicsMode,
    pub show_image: bool,
}
#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub summary: String,
}
/// Fixed Phase 0 interface; implementations probe before starting an event reader.
pub trait TerminalDoctor {
    fn run(&mut self, options: &DoctorOptions) -> io::Result<DoctorReport>;
}
