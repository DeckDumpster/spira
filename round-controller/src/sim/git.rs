//! A fake of the repositories' landing refs: each repo's ref and the commits in its history. The
//! fake queue's `land` moves the ref to the round's head, as a real landing does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde::Deserialize;

use crate::ports::Git;

/// `[[git]]`: a repo's landing ref and the commits already in its history.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSeed {
    pub repo: String,
    pub landing: String,
    #[serde(default)]
    pub history: Vec<String>,
}

#[derive(Default)]
struct Repo {
    landing: String,
    history: BTreeSet<String>,
    unreadable: Option<String>,
}

/// A repo never seeded has no checkout: every read of it is refused, naming it.
#[derive(Default)]
pub struct FakeGit(Mutex<BTreeMap<String, Repo>>);

/// No repo has a checkout: for a test whose code never reads a landing ref.
pub static NO_GIT: FakeGit = FakeGit::new();

impl FakeGit {
    pub const fn new() -> FakeGit {
        FakeGit(Mutex::new(BTreeMap::new()))
    }

    pub fn seed(&self, s: &GitSeed) {
        let mut history: BTreeSet<String> = s.history.iter().cloned().collect();
        history.insert(s.landing.clone());
        crate::lock(&self.0).insert(
            s.repo.clone(),
            Repo {
                landing: s.landing.clone(),
                history,
                unreadable: None,
            },
        );
    }

    /// The ref moves to `sha`, which joins its history.
    pub fn advance(&self, repo: &str, sha: &str) {
        let mut repos = crate::lock(&self.0);
        let r = repos.entry(repo.to_string()).or_default();
        r.landing = sha.to_string();
        r.history.insert(sha.to_string());
    }

    /// Every read of `repo` is refused with `why` from now on.
    pub fn break_repo(&self, repo: &str, why: &str) {
        crate::lock(&self.0)
            .entry(repo.to_string())
            .or_default()
            .unreadable = Some(why.to_string());
    }

    fn read<T>(&self, repo: &str, f: impl FnOnce(&Repo) -> T) -> Result<T, String> {
        match crate::lock(&self.0).get(repo) {
            None => Err(format!("{repo}: no checkout or landing ref resolves")),
            Some(Repo {
                unreadable: Some(why),
                ..
            }) => Err(why.clone()),
            Some(r) => Ok(f(r)),
        }
    }
}

impl Git for FakeGit {
    fn landing_sha(&self, repo: &str) -> Result<String, String> {
        self.read(repo, |r| r.landing.clone())
    }

    fn landed(&self, repo: &str, commit: &str) -> Result<bool, String> {
        self.read(repo, |r| r.history.contains(commit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seeded_ref_holds_its_history_and_an_advance_moves_it() {
        let g = FakeGit::default();
        g.seed(&GitSeed {
            repo: "r".into(),
            landing: "h1".into(),
            history: vec!["h0".into()],
        });
        assert_eq!(g.landing_sha("r").unwrap(), "h1");
        assert_eq!((g.landed("r", "h0"), g.landed("r", "h2")), (Ok(true), Ok(false)));
        g.advance("r", "h2");
        assert_eq!((g.landing_sha("r").unwrap(), g.landed("r", "h2")), ("h2".into(), Ok(true)));
        assert!(g.landed("other", "h1").unwrap_err().contains("other"));
        g.break_repo("r", "index.lock exists");
        assert_eq!(g.landing_sha("r").unwrap_err(), "index.lock exists");
    }
}
