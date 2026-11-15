use tanki_core::*;

#[test]
fn legacy_generation_is_explicit_and_rendering_keeps_notetype_identity() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    assert!(
        matches!(actor.execute(Command::SetLatexGeneration { enabled: true })?, Response::Changes(c) if c.config)
    );
    actor.execute(Command::SetLatexGeneration { enabled: false })?;
    actor.execute(Command::AddNote {
        notetype: "Basic".into(),
        fields: vec!["[latex]x+1234567[/latex]".into(), "answer".into()],
    })?;
    let Response::CardIds(ids) = actor.execute(Command::Search { query: "".into() })? else {
        panic!("ids")
    };
    let Response::Rendered(card) = actor.execute(Command::Render { card_id: ids[0] })? else {
        panic!("card")
    };
    assert!(card.notetype_id > 0);
    assert!(card.template_question.contains("{{Front}}"));
    assert!(card.template_answer.contains("{{Back}}"));
    assert!(card.question.contains("latex-"));
    assert!(
        card.question.to_lowercase().contains("disabled"),
        "{}",
        card.question
    );
    assert!(
        std::fs::read_dir(dir.path().join("collection.media"))?.all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("latex-"))
    );
    actor.close()
}

#[test]
fn opted_in_generation_keeps_the_actor_responsive() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    actor.execute(Command::AddNote {
        notetype: "Basic".into(),
        fields: vec!["[latex]x+987654321[/latex]".into(), "answer".into()],
    })?;
    actor.execute(Command::SetLatexGeneration { enabled: true })?;
    let Response::CardIds(ids) = actor.execute(Command::Search { query: "".into() })? else {
        panic!("ids")
    };
    let start = std::time::Instant::now();
    let Response::Rendered(card) = actor.execute(Command::Render { card_id: ids[0] })? else {
        panic!("card")
    };
    assert!(card.question.contains("Generating LaTeX image"));
    actor.execute(Command::DeckTree)?;
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    // Closing cancels external tools before the backup and collection close.
    actor.close()
}

#[test]
fn rich_study_fixture_has_both_occlusion_modes_and_shipped_media() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let actor = CollectionActor::open(dir.path())?;
    actor.execute(Command::GenerateStudyFixture)?;
    let search = |query: &str| -> Result<Vec<i64>> {
        let Response::CardIds(ids) = actor.execute(Command::Search {
            query: query.into(),
        })?
        else {
            panic!("ids")
        };
        Ok(ids)
    };
    assert_eq!(search("deck:Study::Rich")?.len(), 4);
    assert_eq!(search("deck:Study::Occlusion")?.len(), 6);
    for (tag, hide_all) in [("phase4::io-all", true), ("phase4::io-one", false)] {
        let ids = search(&format!("tag:{tag}"))?;
        assert_eq!(ids.len(), 3);
        for id in ids {
            let Response::Rendered(card) = actor.execute(Command::Render { card_id: id })? else {
                panic!("card")
            };
            for shape in ["rect", "ellipse", "polygon", "text"] {
                assert!(
                    card.question.contains(&format!("data-shape=\"{shape}\"")),
                    "{}",
                    card.question
                );
            }
            assert_eq!(
                card.question.contains("data-occludeInactive=\"1\""),
                hide_all
            );
            assert!(card.answer.contains("cloze-highlight"));
            assert!(card.css.contains("--active-shape-color:#cc3355"));
        }
    }
    let ids = search("tag:phase4::legacy-latex")?;
    let Response::Rendered(card) = actor.execute(Command::Render { card_id: ids[0] })? else {
        panic!("card")
    };
    assert!(card.question.contains("class=\"latex\""));
    assert!(!card.question.contains("disabled"));
    assert!(dir
        .path()
        .join("collection.media/study-pulse.gif")
        .is_file());
    actor.close()
}
