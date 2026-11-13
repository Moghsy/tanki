mod setup;
use std::path::Path;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [command] if command == "setup" => setup::setup(root),
        #[cfg(feature = "fixtures")]
        [command, flag, base]
            if matches!(command.as_str(), "fixtures" | "study-fixtures") && flag == "--base" =>
        {
            let study = command == "study-fixtures";
            let base = Path::new(base);
            let checked = tanki_core::validate_profile(base)?;
            let profile = checked.join("tanki-fixture");
            std::fs::create_dir(&profile)?;
            let actor = tanki_core::CollectionActor::open(&profile)?;
            actor.execute(if study {
                tanki_core::Command::GenerateStudyFixture
            } else {
                tanki_core::Command::GenerateFixture
            })?;
            // The golden-render fixture retains its original three reviews.
            // Leave the study fixture's queues ready for interactive review.
            for _ in 0..if study { 0 } else { 3 } {
                if let tanki_core::Response::Queue(queue) =
                    actor.execute(tanki_core::Command::Queue { limit: 1 })?
                {
                    if let Some(card) = queue.cards.first() {
                        actor.execute(tanki_core::Command::Answer {
                            card_id: card.id,
                            rating: tanki_core::Rating::Good,
                            answered_at_millis: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)?
                                .as_millis() as i64,
                            milliseconds_taken: 3000,
                        })?;
                    }
                }
            }
            actor.close()?;
            println!("{}", profile.display());
            Ok(())
        }
        _ => Err(
            "usage: cargo xtask setup | fixtures|study-fixtures --base EXISTING_TEMP_DIR".into(),
        ),
    }
}
