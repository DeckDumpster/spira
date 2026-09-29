//! The tree certificate (queue/DESIGN.md §8 D12, DESIGN.md "Decisions"): the durable record
//! that something judged a repository's tree fit to land. The gate writes `PASS`/`gate`,
//! batcher-cut writes `GREEN`/`round`, and `queue land-local` refuses any head whose tree has
//! neither.
//!
//! `${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}/trees/<repo>/<tree>`, key=value lines. It is matched
//! by `(repo, tree)` alone: the recorded `harness=` is for the reader, never for the match, so a
//! newer gate binary never unsays an older PASS of the same content.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Who certified the tree, and the only verdict that source may record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The per-branch gate: `verdict=PASS`.
    Gate,
    /// A round's full corpus on this exact tree: `verdict=GREEN`.
    Round,
}

impl Source {
    pub fn word(self) -> &'static str {
        match self {
            Source::Gate => "gate",
            Source::Round => "round",
        }
    }
    /// The verdict word this source certifies with.
    pub fn verdict(self) -> &'static str {
        match self {
            Source::Gate => "PASS",
            Source::Round => "GREEN",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cert {
    pub source: Source,
    pub tree: String,
    pub repo: String,
    /// The commit judged (the gate revision, or the round head).
    pub rev: String,
    /// The branch (gate) or round branch/head label.
    pub branch: String,
    pub by: String,
    pub when: String,
    pub at: u64,
    /// sha256 of the gate's harness (gate.sh, exclude.sh, skew.sh, the binary); `-` for a round.
    pub harness: String,
    pub suites: String,
}

/// `${SPIRA_VERDICTS:-<run>/verdicts}` — the gate's own resolution of the verdict directory.
pub fn verdicts_dir(spira_verdicts: Option<&str>, run: &Path) -> PathBuf {
    match spira_verdicts.filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => run.join("verdicts"),
    }
}

/// A git object id: 40 or 64 lowercase hex characters. Anything else never becomes a path.
pub fn is_object_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A repository name safe as one path component.
pub fn is_repo_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// `<verdicts>/trees/<repo>/<tree>`; None when either would not be one safe path component.
pub fn path(verdicts: &Path, repo: &str, tree: &str) -> Option<PathBuf> {
    (is_repo_name(repo) && is_object_id(tree)).then(|| verdicts.join("trees").join(repo).join(tree))
}

fn one_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("").trim();
    if l.is_empty() {
        "-".into()
    } else {
        l.to_string()
    }
}

pub fn render(c: &Cert) -> String {
    format!(
        "verdict={}\nsource={}\ntree={}\nrepo={}\nrev={}\nbranch={}\nby={}\nwhen={}\nat={}\nharness={}\nsuites={}\n",
        c.source.verdict(),
        c.source.word(),
        c.tree,
        c.repo,
        one_line(&c.rev),
        one_line(&c.branch),
        one_line(&c.by),
        one_line(&c.when),
        c.at,
        one_line(&c.harness),
        one_line(&c.suites),
    )
}

/// Parse a certificate. None unless `verdict=` and `source=` are a matched pair (PASS/gate or
/// GREEN/round) and `tree=`/`repo=` are present.
pub fn parse(text: &str) -> Option<Cert> {
    let get = |k: &str| {
        text.lines()
            .filter_map(|l| l.split_once('=').filter(|(kk, _)| *kk == k).map(|(_, v)| v.to_string()))
            .next_back()
    };
    let source = match (get("verdict")?.as_str(), get("source")?.as_str()) {
        ("PASS", "gate") => Source::Gate,
        ("GREEN", "round") => Source::Round,
        _ => return None,
    };
    let tree = get("tree").filter(|t| !t.is_empty())?;
    let repo = get("repo").filter(|r| !r.is_empty())?;
    let or = |k: &str| get(k).unwrap_or_else(|| "-".into());
    Some(Cert {
        source,
        tree,
        repo,
        rev: or("rev"),
        branch: or("branch"),
        by: or("by"),
        when: or("when"),
        at: get("at").and_then(|a| a.parse().ok()).unwrap_or(0),
        harness: or("harness"),
        suites: or("suites"),
    })
}

/// Whether `text` certifies exactly `(repo, tree)`. The harness hash is never compared.
pub fn certifies(text: &str, repo: &str, tree: &str) -> Option<Cert> {
    parse(text).filter(|c| c.tree == tree && c.repo == repo)
}

/// Write the certificate atomically (temp + rename), creating its directory.
pub fn write(verdicts: &Path, c: &Cert) -> io::Result<PathBuf> {
    let p = path(verdicts, &c.repo, &c.tree)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("not a certifiable (repo, tree): ({}, {})", c.repo, c.tree)))?;
    let dir = p.parent().unwrap_or(verdicts);
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}", c.tree, std::process::id()));
    fs::write(&tmp, render(c))?;
    fs::rename(&tmp, &p).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "0123456789abcdef0123456789abcdef01234567";

    fn c(source: Source) -> Cert {
        Cert {
            source,
            tree: T.into(),
            repo: "spira".into(),
            rev: "abc".into(),
            branch: "spira/sp-x".into(),
            by: "me".into(),
            when: "2026-09-29T18:00:00Z".into(),
            at: 1_790_000_000,
            harness: "h".into(),
            suites: "-".into(),
        }
    }

    #[test]
    fn a_certificate_round_trips_for_both_sources() {
        for s in [Source::Gate, Source::Round] {
            assert_eq!(parse(&render(&c(s))), Some(c(s)));
        }
    }

    #[test]
    fn only_the_matched_verdict_source_pairs_count() {
        let t = render(&c(Source::Gate));
        assert!(parse(&t.replace("verdict=PASS", "verdict=FAIL")).is_none());
        assert!(parse(&t.replace("verdict=PASS", "verdict=GREEN")).is_none(), "GREEN is a round's word");
        assert!(parse(&t.replace(&format!("tree={T}\n"), "")).is_none());
    }

    #[test]
    fn it_certifies_only_its_own_tree_and_repo_whatever_the_harness() {
        let t = render(&c(Source::Gate));
        assert!(certifies(&t, "spira", T).is_some());
        assert!(certifies(&t.replace("harness=h", "harness=an-older-gate"), "spira", T).is_some());
        assert!(certifies(&t, "other", T).is_none());
        assert!(certifies(&t, "spira", &"f".repeat(40)).is_none());
    }

    #[test]
    fn hostile_names_never_become_paths() {
        let v = Path::new("/v");
        assert!(path(v, "../x", T).is_none());
        assert!(path(v, "spira", "../../etc").is_none());
        assert!(path(v, "spira", "HEAD").is_none());
        assert_eq!(path(v, "spira", T), Some(PathBuf::from(format!("/v/trees/spira/{T}"))));
    }

    #[test]
    fn free_text_is_one_line_in_the_file() {
        let mut x = c(Source::Round);
        x.by = "a\nverdict=PASS".into();
        let t = render(&x);
        assert_eq!(t.lines().filter(|l| l.starts_with("verdict=")).count(), 1);
    }

    #[test]
    fn write_is_atomic_and_readable_back() {
        let d = testkit::TempDir::new("gate-cert");
        let p = write(&d, &c(Source::Round)).unwrap();
        assert!(certifies(&fs::read_to_string(&p).unwrap(), "spira", T).is_some());
        assert_eq!(fs::read_dir(p.parent().unwrap()).unwrap().count(), 1, "no temp file left");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_verdicts_dir_follows_the_gate() {
        assert_eq!(verdicts_dir(None, Path::new("/r")), PathBuf::from("/r/verdicts"));
        assert_eq!(verdicts_dir(Some(""), Path::new("/r")), PathBuf::from("/r/verdicts"));
        assert_eq!(verdicts_dir(Some("/v"), Path::new("/r")), PathBuf::from("/v"));
    }
}
