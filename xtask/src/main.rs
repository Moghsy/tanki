use std::{error::Error, fs, path::Path, process::Command};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const PIN: &str = "4aa9c0836965b519285372b456078c6fd1614569";

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").current_dir(dir).args(args).status()?;
    if !status.success() {
        return Err(format!("git {} failed in {}", args.join(" "), dir.display()).into());
    }
    Ok(())
}

fn apply_patch(repo: &Path, patch: &Path) -> Result<()> {
    let patch = patch.canonicalize()?;
    let check = |reverse: bool| -> Result<bool> {
        let mut cmd = Command::new("git");
        cmd.current_dir(repo).args(["apply", "--check"]);
        if reverse {
            cmd.arg("--reverse");
        }
        Ok(cmd.arg(&patch).output()?.status.success())
    };
    if check(false)? {
        let status = Command::new("git")
            .current_dir(repo)
            .arg("apply")
            .arg(&patch)
            .status()?;
        if !status.success() {
            return Err(format!("failed to apply {}", patch.display()).into());
        }
        println!("applied {}", patch.display());
    } else if check(true)? {
        println!("already applied {}", patch.display());
    } else {
        return Err(format!(
            "patch does not apply cleanly in either direction: {}",
            patch.display()
        )
        .into());
    }
    Ok(())
}

fn setup(root: &Path) -> Result<()> {
    let protoc = std::env::var_os("PROTOC").unwrap_or_else(|| "protoc".into());
    let output = Command::new(&protoc)
        .arg("--version")
        .output()
        .map_err(|e| {
            format!("protoc unavailable ({protoc:?}): {e}; install it explicitly and set PROTOC")
        })?;
    if !output.status.success() {
        return Err("protoc --version failed".into());
    }
    print!("{}", String::from_utf8_lossy(&output.stdout));
    let vendor = root.join("vendor/anki");
    if !vendor.join(".git").exists() {
        git(
            root,
            &["submodule", "update", "--init", "--", "vendor/anki"],
        )?;
    }
    let head = Command::new("git")
        .current_dir(&vendor)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != PIN {
        return Err(format!(
            "vendor/anki must be pinned at {PIN}; refusing to change an existing checkout"
        )
        .into());
    }
    git(
        &vendor,
        &[
            "submodule",
            "update",
            "--init",
            "--",
            "ftl/core-repo",
            "ftl/qt-repo",
        ],
    )?;
    let mut patches = fs::read_dir(root.join("patches/anki"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    patches.retain(|path| path.extension().is_some_and(|ext| ext == "patch"));
    patches.sort();
    for patch in patches {
        apply_patch(&vendor, &patch)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [command] if command == "setup" => setup(root),
        #[cfg(feature = "fixtures")]
        [command, flag, base] if command == "fixtures" && flag == "--base" => {
            let base = Path::new(base);
            let checked = tanki_core::validate_profile(base)?;
            let profile = checked.join("tanki-fixture");
            std::fs::create_dir(&profile)?;
            let actor = tanki_core::CollectionActor::open(&profile)?;
            actor.execute(tanki_core::Command::GenerateFixture)?;
            for _ in 0..3 {
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
        _ => Err("usage: cargo xtask setup | fixtures --base EXISTING_TEMP_DIR".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_is_idempotent_and_rejects_drift() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("tanki-patch-test-{}", std::process::id()));
        fs::create_dir(&dir)?;
        let result = (|| -> Result<()> {
            git(&dir, &["init", "--quiet"])?;
            fs::write(dir.join("file.txt"), "before\n")?;
            let patch = dir.join("change.patch");
            fs::write(
                &patch,
                "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before\n+after\n",
            )?;
            apply_patch(&dir, &patch)?;
            assert_eq!(fs::read_to_string(dir.join("file.txt"))?, "after\n");
            apply_patch(&dir, &patch)?;
            assert_eq!(fs::read_to_string(dir.join("file.txt"))?, "after\n");
            fs::write(dir.join("file.txt"), "unexpected\n")?;
            assert!(apply_patch(&dir, &patch).is_err());
            assert_eq!(fs::read_to_string(dir.join("file.txt"))?, "unexpected\n");
            Ok(())
        })();
        fs::remove_dir_all(dir)?;
        result
    }
}
