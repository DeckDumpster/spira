//! `groomer deadlocked [--apply]` — the git half of `spira-claim deadlocked`
//! (spira-claim/DESIGN.md §9; landed at sp-rfodk after this crate's own rewrite started,
//! so it is ported here rather than left behind on the deleted `groomer.sh`). `spira-claim`
//! decides which poisoned candidate to lift and does the write; it deliberately does no
//! git (the same boundary `select` draws), so resolving a bead's repository and land ref,
//! and asking whether `spira/<id>` names the bead and merges cleanly into it, is this
//! crate's job — the repo map and git plumbing live beside `lib.sh`'s seam already.
//!
//! POISONED IS CHECKED TWICE, ON PURPOSE. This pass filters to poisoned candidates first
//! so git never runs over a whole partition (the git work only pays for the handful that
//! are actually poisoned) — `spira-claim` re-checks poisoned-ness itself before it writes
//! anything, so a bead cleared between this filter and that check is simply skipped there,
//! silently, never a stale write.

use std::process::{Command, Stdio};

use crate::bd::Bd;
use crate::seam::Seam;

/// One candidate's git verdict, in the exact shape `spira-claim deadlocked`'s
/// `--merge-status` expects (`spira-claim/src/deadlock.rs::parse_candidates`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Candidate {
    pub id: String,
    pub ok: bool,
    pub why: String,
    pub branch: String,
    pub base: String,
}

impl Candidate {
    fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"ok\":{},\"why\":{},\"branch\":{},\"base\":{}}}",
            json_str(&self.id),
            self.ok,
            json_str(&self.why),
            json_str(&self.branch),
            json_str(&self.base),
        )
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Serialize the candidate list the way `groomer.sh deadlocked` built its `_dl_json` —
/// a bare JSON array, one object per candidate.
pub fn candidates_json(candidates: &[Candidate]) -> String {
    let body: Vec<String> = candidates.iter().map(Candidate::to_json).collect();
    format!("[{}]", body.join(","))
}

fn git_ref_exists(path: &str, branch: &str) -> bool {
    Command::new("git")
        .args(["-C", path, "show-ref", "--verify", "-q", &format!("refs/heads/{branch}")])
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_log_subjects(path: &str, branch: &str, n: u32) -> String {
    Command::new("git")
        .args(["-C", path, "log", "--format=%s%n%b", "-n", &n.to_string(), branch])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn git_merges_cleanly(path: &str, base: &str, branch: &str) -> bool {
    Command::new("git")
        .args(["-C", path, "merge-tree", "--write-tree", base, branch])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Judge one poisoned bead: does `spira/<id>` exist, name the bead, and merge cleanly
/// into its repo's land base? Mirrors `groomer.sh deadlocked`'s git checks exactly,
/// including the wording of each `why` (spira-claim prints it verbatim as KEEP's reason).
pub fn judge_one(seam: &dyn Seam, id: &str) -> Candidate {
    let branch = format!("spira/{id}");
    let repo = seam.bead_repo(id).unwrap_or_default().trim().to_string();
    let path = seam.repo_root(&repo).unwrap_or_default().trim().to_string();
    if path.is_empty() {
        return Candidate { id: id.to_string(), ok: false, why: format!("repo:{repo} is not in the repo map — cannot look at its branch"), branch, base: String::new() };
    }
    if !git_ref_exists(&path, &branch) {
        return Candidate { id: id.to_string(), ok: false, why: format!("no branch {branch} in {repo} — nothing was committed"), branch, base: String::new() };
    }
    let base = seam.land_base(&path).unwrap_or_default().trim().to_string();
    if base.is_empty() {
        return Candidate { id: id.to_string(), ok: false, why: format!("{repo} cannot say what it lands on — not judging its branch"), branch, base };
    }
    let subjects = git_log_subjects(&path, &branch, 200);
    if !subjects.contains(id) {
        return Candidate { id: id.to_string(), ok: false, why: format!("{branch} exists but no commit on it names {id}"), branch, base };
    }
    if !git_merges_cleanly(&path, &base, &branch) {
        return Candidate { id: id.to_string(), ok: false, why: format!("{branch} does not merge into {base} — a person has to resolve it"), branch, base };
    }
    Candidate { id: id.to_string(), ok: true, why: String::new(), branch, base }
}

/// The non-enforce path reads bd's own `label list` rendering, which is never a bare
/// label per line — real `bd label list <id>` prints a header, then `  - <label>` rows
/// (verified against a live `bd`: `🏷️ Labels for <id>:` / `  - plan` / `  - spira-poison`).
/// The just-landed bash's `grep -qx spira-poison` (exact whole-line match) can never match
/// that output — a bug this port does not reproduce; `groomer.sh`'s own `triage-poison`
/// case already used the substring form (`grep -q spira-poison`) for exactly this reason.
fn is_poisoned(bd: &dyn Bd, seam: &dyn Seam, id: &str, enforce: bool) -> bool {
    if enforce {
        seam.lc_held_poison(id)
    } else {
        bd.label_list(id).map(|text| text.contains("spira-poison")).unwrap_or(false)
    }
}

/// `groomer deadlocked [--apply]`: gather every poisoned candidate across the whole
/// roster, judge each one's git state, and hand the verdicts to `spira-claim deadlocked`
/// — which decides and writes. Returns the exit code and combined output to print.
pub fn run(bd: &dyn Bd, seam: &dyn Seam, db: &str, apply: bool, enforce: bool, spira_claim_bin: &str) -> (i32, String) {
    let members = seam.all_partition_members().unwrap_or_default();
    let mut candidates = Vec::new();
    for id in members.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if is_poisoned(bd, seam, id, enforce) {
            candidates.push(judge_one(seam, id));
        }
    }
    let json = candidates_json(&candidates);

    let tmp = std::env::temp_dir().join(format!(".groomer-deadlocked.{}.json", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &json) {
        return (1, format!("groomer: deadlocked: could not write merge-status: {e}\n"));
    }
    let mut args: Vec<&str> = vec!["deadlocked"];
    if apply {
        args.push("--apply");
    }
    args.extend(["--actor", "groomer", "--merge-status"]);
    let tmp_str = tmp.to_string_lossy().into_owned();
    args.push(&tmp_str);
    args.extend(["--db", db]);
    let out = Command::new(spira_claim_bin).args(&args).stdin(Stdio::null()).output();
    let _ = std::fs::remove_file(&tmp);
    match out {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code().unwrap_or(1), text)
        }
        Err(e) => (1, format!("groomer: deadlocked: could not run {spira_claim_bin}: {e}\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bd::fake::FakeBd;
    use crate::seam::fake::FakeSeam;

    #[test]
    fn candidate_json_round_trips_the_shape_spira_claim_expects() {
        let c = Candidate { id: "sp-1".into(), ok: true, why: String::new(), branch: "spira/sp-1".into(), base: "origin/main".into() };
        let json = candidates_json(&[c]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["id"], "sp-1");
        assert_eq!(v[0]["ok"], true);
        assert_eq!(v[0]["branch"], "spira/sp-1");
        assert_eq!(v[0]["base"], "origin/main");
    }

    #[test]
    fn candidate_json_escapes_quotes_and_backslashes_in_why() {
        let c = Candidate { id: "sp-1".into(), ok: false, why: "does not merge into \"base\" — a \\ person".into(), branch: "spira/sp-1".into(), base: String::new() };
        let json = candidates_json(&[c]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["why"], "does not merge into \"base\" — a \\ person");
    }

    #[test]
    fn empty_candidate_list_is_a_bare_empty_array() {
        assert_eq!(candidates_json(&[]), "[]");
    }

    #[test]
    fn judge_one_refuses_a_bead_whose_repo_is_unmapped() {
        let seam = FakeSeam::new();
        seam.repos.borrow_mut().insert("sp-1".into(), "ghost-repo".into());
        // no entry in `roots` for "ghost-repo" -> repo_root returns empty
        let c = judge_one(&seam, "sp-1");
        assert!(!c.ok);
        assert!(c.why.contains("not in the repo map"), "{}", c.why);
    }

    #[test]
    fn is_poisoned_without_enforce_checks_the_label_list_exactly() {
        let bd = FakeBd::new();
        bd.set_labels("sp-1", &["plan", "spira-poison"]);
        let seam = FakeSeam::new();
        assert!(is_poisoned(&bd, &seam, "sp-1", false));
        bd.set_labels("sp-2", &["plan"]);
        assert!(!is_poisoned(&bd, &seam, "sp-2", false));
    }

    #[test]
    fn is_poisoned_with_enforce_asks_the_lifecycle_machine_not_the_label() {
        let bd = FakeBd::new();
        bd.set_labels("sp-1", &["plan"]); // no spira-poison label
        let seam = FakeSeam::new();
        seam.held_poison.borrow_mut().insert("sp-1".to_string());
        assert!(is_poisoned(&bd, &seam, "sp-1", true), "enforce path must consult the lifecycle hold, not bd's label");
    }

    #[test]
    fn run_invokes_spira_claim_with_an_empty_array_when_nothing_is_poisoned() {
        let bd = FakeBd::new();
        bd.set_labels("sp-1", &["plan"]); // not poisoned
        let seam = FakeSeam::new();
        *seam.partition_members.borrow_mut() = "sp-1\n".to_string();
        let (code, _) = run(&bd, &seam, "/db", false, false, "true");
        assert_eq!(code, 0, "the `true` stub always exits 0 — this just proves run() reaches and calls it");
        assert!(seam.log().iter().any(|c| c == "all_partition_members"));
    }
}
