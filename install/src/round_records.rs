//! The queue's record files: the round files, `<queue dir>/<repo>/round` and `round.<batch>`, and
//! the forge batch's, `open`, `prepared` and `forge-state`. Once spira-lc serves the batch machine
//! from the control store the store's batch records replace them, so install removes them — and
//! refuses while one still names a batch the store holds open, since that batch has to be drained
//! (landed or abandoned) first, not orphaned.

use std::path::{Path, PathBuf};

use lifecycle::batch::BatchState;

/// One round file and the batch it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundFile {
    pub path: PathBuf,
    pub batch: String,
}

/// The forge batch's files: its open record, the batcher's prepared next round, and the verdict's
/// last view of the forge.
const FORGE_FILES: [&str; 3] = ["open", "prepared", "forge-state"];

fn is_round_file(name: &str) -> bool {
    name == "round"
        || name.strip_prefix("round.").is_some_and(|b| !b.is_empty())
        || FORGE_FILES.contains(&name)
}

/// Every round file in every repo directory under `queue_dir`, by path.
pub fn round_files(queue_dir: &Path) -> Vec<RoundFile> {
    let mut out: Vec<RoundFile> = std::fs::read_dir(queue_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|repo| repo.path().is_dir())
        .flat_map(|repo| std::fs::read_dir(repo.path()).into_iter().flatten().flatten())
        .filter(|f| f.file_name().to_str().is_some_and(is_round_file))
        .map(|f| {
            let text = std::fs::read_to_string(f.path()).unwrap_or_default();
            let batch = text
                .lines()
                .find_map(|l| l.strip_prefix("batch_id="))
                .unwrap_or("")
                .trim()
                .to_string();
            RoundFile { path: f.path(), batch }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The files to delete; Err names each file whose round the store holds open, or cannot say
/// about, and the step that clears it. `state` answers a batch's machine state (`Ok(None)`: the
/// store holds no such round, so the file is stale).
pub fn plan(
    files: &[RoundFile],
    state: impl Fn(&str) -> Result<Option<String>, String>,
) -> Result<Vec<PathBuf>, String> {
    let mut held = Vec::new();
    for f in files.iter().filter(|f| !f.batch.is_empty()) {
        match state(&f.batch) {
            Ok(None) => {}
            Ok(Some(s)) if BatchState::from_str(&s).is_some_and(BatchState::is_terminal) => {}
            Ok(Some(s)) => held.push(format!("{} names round {}, which is {s}", f.path.display(), f.batch)),
            Err(e) => held.push(format!("{} names round {}, whose state the store cannot tell: {e}", f.path.display(), f.batch)),
        }
    }
    if !held.is_empty() {
        return Err(format!(
            "{} — drain first: land or abandon each, a round with `queue round land|abandon <batch>` until `queue round status` reads round=none, a forge batch with `queue verdict <repo>` once its PR merges or `queue abandon <repo>`, then install again",
            held.join("; ")
        ));
    }
    Ok(files.iter().map(|f| f.path.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_dir(tag: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(&format!("install-round-records-{tag}"));
        for (repo, name, batch) in [
            ("spira", "round", "spira-landed"),
            ("spira", "round.spira-settled", "spira-settled"),
            ("spira", "round-seq", ""),
            ("spira", "round-staged", "spira-staged"),
            ("other", "round", "other-gone"),
        ] {
            std::fs::create_dir_all(d.join(repo)).unwrap();
            std::fs::write(d.join(repo).join(name), format!("batch_id={batch}\n")).unwrap();
        }
        d
    }

    fn states(open: &'static str) -> impl Fn(&str) -> Result<Option<String>, String> {
        move |b| {
            Ok(match b {
                "spira-landed" => Some("LANDED".into()),
                "spira-settled" => Some(open.into()),
                _ => None,
            })
        }
    }

    #[test]
    fn round_files_naming_ended_or_unknown_rounds_are_deleted_and_nothing_else_is() {
        let d = queue_dir("ended");
        let files = round_files(d.path());
        let names: Vec<String> = files.iter().map(|f| f.path.strip_prefix(d.path()).unwrap().display().to_string()).collect();
        assert_eq!(names, ["other/round", "spira/round", "spira/round.spira-settled"]);
        let gone = plan(&files, states("SETTLED")).unwrap();
        assert_eq!(gone, files.iter().map(|f| f.path.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn the_forge_batch_files_are_removed_too_and_one_naming_an_open_batch_is_refused() {
        let d = testkit::TempDir::new("install-round-records-forge");
        std::fs::create_dir_all(d.join("deck")).unwrap();
        for (name, text) in [
            ("open", "pr=12\nbranch=spira/queue/x\nbatch_id=deck-open\n"),
            ("prepared", "head=h\nparent=p\n"),
            ("forge-state", "batch_id=deck-open\nci=pending\n"),
            ("capacity-backoff", "until=9\n"),
        ] {
            std::fs::write(d.join("deck").join(name), text).unwrap();
        }
        let files = round_files(d.path());
        let names: Vec<String> = files.iter().map(|f| f.path.strip_prefix(d.path()).unwrap().display().to_string()).collect();
        assert_eq!(names, ["deck/forge-state", "deck/open", "deck/prepared"]);
        let open = |b: &str| Ok((b == "deck-open").then(|| "OPEN".to_string()));
        let e = plan(&files, open).unwrap_err();
        assert!(e.contains("deck/open names round deck-open, which is OPEN") && e.contains("queue abandon"), "{e}");
        let landed = |_: &str| Ok(Some("LANDED".to_string()));
        assert_eq!(plan(&files, landed).unwrap().len(), 3);
    }

    #[test]
    fn a_round_file_naming_an_open_round_is_refused_by_name_with_the_drain_step() {
        let d = queue_dir("open");
        let files = round_files(d.path());
        let e = plan(&files, states("CI_RUNNING")).unwrap_err();
        assert!(e.contains("round.spira-settled names round spira-settled, which is CI_RUNNING"), "{e}");
        assert!(!e.contains("spira-landed") && e.contains("drain first"), "{e}");
        let e = plan(&files, |_| Err("lc-serve is down".to_string())).unwrap_err();
        assert!(e.contains("whose state the store cannot tell: lc-serve is down"), "{e}");
    }
}
