#[path = "../../src/setup.rs"]
mod setup;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().skip(1).collect::<Vec<_>>() != ["setup"] {
        return Err("usage: cargo xtask setup; fixtures: cargo run -p xtask --features fixtures -- fixtures --base DIR".into());
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    setup::setup(&root)
}
