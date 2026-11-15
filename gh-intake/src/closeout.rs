//! The write-back complement to gh-intake's one-way ingest (`logic.rs`): the drain of pending
//! GitHub closeouts. The lifecycle store records a closeout when a `github:` bead reaches
//! LANDED, SUPERSEDED or DROPPED; [`drain`] settles each row by point reads of that bead and
//! removes the row, or leaves it for the next drain. It reads no bead list.
//!
//! This runs only from the credentialed side. No bead notes, bodies or internal judgement reach
//! the public tracker (law-beads-is-never-public): every string sent to GitHub below is built
//! from a commit sha/subject (already public) or a fixed sentence.

use crate::ports::{Bd, Closeouts, Gh, Git, Lifecycle, Mail, Pending, Repo};
use lifecycle::provenance::{self, ExternalOutcome, Trail, TrailRow};
use std::path::{Path, PathBuf};

pub struct Ctx {
    /// `$SPIRA_RUN` — the `gh-closed/<id>` and `gh-reported/<id>` markers live under this.
    pub run: PathBuf,
}

pub struct Deps<'a> {
    pub bd: &'a dyn Bd,
    pub gh: &'a dyn Gh,
    pub git: &'a dyn Git,
    pub repo: &'a dyn Repo,
    pub mail: &'a dyn Mail,
    pub lc: &'a dyn Lifecycle,
    pub co: &'a dyn Closeouts,
}

mod run_state {
    use std::fs;
    use std::path::Path;

    /// `gh-reported/<id>`: a comment was posted on the issue (it may still be open).
    pub fn reported_exists(run: &Path, id: &str) -> bool {
        run.join("gh-reported").join(id).exists()
    }

    pub fn reported_write(run: &Path, id: &str) {
        let dir = run.join("gh-reported");
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join(id), b"");
    }

    pub fn mark_exists(run: &Path, id: &str) -> bool {
        run.join("gh-closed").join(id).exists()
    }

    pub fn mark_write(run: &Path, id: &str) {
        let dir = run.join("gh-closed");
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join(id), b"");
    }
}

/// `github:<owner/repo>#<n>` -> (repo, issue number): the repo is everything before the first
/// `#`, the number everything after the last. `None` when the number is empty or not digits.
pub fn parse_gh_ref(ext_ref: &str) -> Option<(String, String)> {
    let rest = ext_ref.strip_prefix("github:").unwrap_or(ext_ref);
    let gh_repo = rest.split('#').next().unwrap_or("").to_string();
    let issue_n = rest.rsplit('#').next().unwrap_or("").to_string();
    if issue_n.is_empty() || !issue_n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((gh_repo, issue_n))
}

pub fn close_issue_subject(ext_ref: &str, id: &str) -> String {
    format!("Close GitHub issue {ext_ref} for bead {id}")
}

/// `"Fixed in <sha_short>[ (<subject>)]\n\nhttps://github.com/<gh_repo>/commit/<sha>"`.
pub fn comment_body(sha_short: &str, subject: &str, gh_repo: &str, sha: &str) -> String {
    let t = if subject.is_empty() { String::new() } else { format!(" ({subject})") };
    format!("Fixed in {sha_short}{t}\n\nhttps://github.com/{gh_repo}/commit/{sha}")
}

pub fn ask_unlanded_body(subj: &str, dflt: &str, id: &str, ext_ref: &str) -> String {
    format!(
        "## Question\n{subj}\n\n## Default\n{dflt}\n\n{id} reached a terminal state without a commit landing on the base branch, but it links to GitHub issue {ext_ref} which is still open.\n\nSuggested public reply: \"{dflt}\"\n"
    )
}

fn short7(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// `bd show --json`'s shape: a bare object, or a one-element array of one.
fn show_row(v: &serde_json::Value) -> Option<&serde_json::Value> {
    match v {
        serde_json::Value::Array(a) => a.first(),
        o @ serde_json::Value::Object(_) => Some(o),
        _ => None,
    }
}

fn text_of(v: &serde_json::Value, key: &str) -> String {
    show_row(v).and_then(|r| r.get(key)).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn blocks(v: &serde_json::Value, ask_id: &str) -> bool {
    let Some(row) = show_row(v) else { return false };
    row.get("dependencies")
        .and_then(|d| d.as_array())
        .map(|deps| {
            deps.iter().any(|dep| {
                dep.get("type").and_then(|x| x.as_str()) == Some("blocks") && dep.get("depends_on_id").and_then(|x| x.as_str()) == Some(ask_id)
            })
        })
        .unwrap_or(false)
}

/// The bead's `repo:` label, resolved to a checkout; empty when it names none.
fn repo_path_of(d: &Deps, bead: &serde_json::Value) -> PathBuf {
    let name = show_row(bead)
        .and_then(|r| r.get("labels"))
        .and_then(|l| l.as_array())
        .and_then(|a| a.iter().find_map(|l| l.as_str().and_then(|s| s.strip_prefix("repo:"))));
    name.and_then(|n| d.repo.root_with_git(n)).map(PathBuf::from).unwrap_or_default()
}

/// The trail from `id` to what settles it, over point reads of the machine and the store.
/// `Err` when the machine could not answer: no answer is not a missing row.
pub fn trail_of(d: &Deps, id: &str) -> Result<Trail, String> {
    let failure = std::cell::RefCell::new(None);
    let lookup = |b: &str| -> Option<TrailRow> {
        let lc = match d.lc.bead(b) {
            Ok(row) => row?,
            Err(e) => {
                *failure.borrow_mut() = Some(e);
                return None;
            }
        };
        let close_reason = if lc.state == "DROPPED" { d.bd.show_json(b).map(|v| text_of(&v, "close_reason")).filter(|r| !r.is_empty()) } else { None };
        Some(TrailRow { state: lc.state, reason: Some(lc.reason).filter(|r| !r.is_empty()), close_reason })
    };
    let trail = provenance::trace(id, &lookup);
    match failure.into_inner() {
        Some(e) => Err(format!("lifecycle machine unreachable ({e})")),
        None => Ok(trail),
    }
}

/// The public comment for a settled outcome other than a landing, built from the outcome's own
/// evidence only (law-beads-is-never-public).
pub fn outcome_comment(outcome: &ExternalOutcome, via: &[String], gh_repo: &str) -> String {
    match outcome {
        ExternalOutcome::Landed => "Fixed.".to_string(),
        ExternalOutcome::DuplicateOf(id) => format!("Closed as a duplicate of {id}."),
        ExternalOutcome::AlreadyFixedBy(c) => format!("Already fixed by {c}\n\nhttps://github.com/{gh_repo}/commit/{c}"),
        ExternalOutcome::Moot(why) => format!("No longer applicable: {why}"),
        ExternalOutcome::WontFix(who) => {
            let chain = if via.len() > 1 { format!(" (via {})", via[1..].join(" → ")) } else { String::new() };
            format!("Will not be fixed — decided by {who}{chain}.")
        }
    }
}

/// What a drain does with one pending row.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// The issue is settled: the row is removed.
    Done,
    /// A call failed: the row stays and the attempt is counted.
    Failed,
    /// Nothing to do until the world moves: the row stays untouched.
    Wait,
    /// The operator was asked, under this ask: the row stays, naming it.
    Asked(String),
}

/// Settles every pending closeout once. Reads the pending rows and points by id, never a list.
pub fn drain(d: &Deps, ctx: &Ctx) -> Vec<String> {
    let rows = match d.co.pending() {
        Ok(rows) => rows,
        Err(e) => return vec![format!("gh-closeout: could not read the pending closeouts ({e})")],
    };
    let mut log = Vec::new();
    for p in rows {
        let (verdict, lines) = settle(d, ctx, &p);
        log.extend(lines);
        let recorded = match verdict {
            Verdict::Done => d.co.done(&p.bead_id),
            Verdict::Failed => d.co.failed(&p.bead_id),
            Verdict::Wait => true,
            Verdict::Asked(ask) => d.co.asked(&p.bead_id, &ask),
        };
        if !recorded {
            log.push(format!("gh-closeout {}: could not record the outcome on the pending row", p.bead_id));
        }
    }
    log
}

fn settle(d: &Deps, ctx: &Ctx, p: &Pending) -> (Verdict, Vec<String>) {
    let id = &p.bead_id;
    if parse_gh_ref(&p.external_ref).is_none() {
        return (Verdict::Done, vec![format!("gh-closeout {id}: malformed external_ref {} — dropped", p.external_ref)]);
    }
    if let Some(ask) = p.ask_id.as_deref().filter(|a| !a.is_empty()) {
        return follow_ask(d, ctx, p, ask);
    }
    if p.outcome_state == "LANDED" {
        let sha = match d.lc.bead(id) {
            Ok(b) => b.map(|b| b.sha).filter(|s| !s.is_empty()).unwrap_or_else(|| p.sha.clone()),
            Err(e) => return (Verdict::Failed, vec![format!("gh-closeout {id}: lifecycle machine unreachable ({e})")]),
        };
        return close_landed(d, ctx, id, &sha);
    }
    match trail_of(d, id) {
        Err(e) => (Verdict::Failed, vec![format!("gh-closeout {id}: {e}")]),
        Ok(Trail::Pending(_)) => (Verdict::Wait, Vec::new()),
        Ok(Trail::Settled { outcome: ExternalOutcome::Landed, via }) => {
            let last = via.last().map(String::as_str).unwrap_or(id);
            match d.lc.bead(last) {
                Ok(Some(b)) if !b.sha.is_empty() => close_landed(d, ctx, id, &b.sha),
                Ok(_) => (Verdict::Failed, vec![format!("gh-closeout {id}: {last} is LANDED with no commit on its row")]),
                Err(e) => (Verdict::Failed, vec![format!("gh-closeout {id}: lifecycle machine unreachable ({e})")]),
            }
        }
        Ok(Trail::Settled { outcome, via }) => {
            let (done, log) = report_outcome(d, ctx, id, &p.external_ref, &outcome, &via);
            (if done { Verdict::Done } else { Verdict::Failed }, log)
        }
        Ok(Trail::Broken(_)) => ask_unlanded(d, ctx, id, &p.external_ref),
    }
}

fn close_landed(d: &Deps, ctx: &Ctx, id: &str, sha: &str) -> (Verdict, Vec<String>) {
    let repo_path = d.bd.show_json(id).map(|v| repo_path_of(d, &v)).unwrap_or_default();
    let log = gh_issue_closeout(d, ctx, id, sha, &repo_path);
    (if run_state::mark_exists(&ctx.run, id) { Verdict::Done } else { Verdict::Failed }, log)
}

/// Comments the outcome of a settled, non-landing trail on the issue, once per bead, and closes
/// the issue when the outcome means the reporter's problem is gone. True when nothing is left.
fn report_outcome(d: &Deps, ctx: &Ctx, id: &str, ext: &str, outcome: &ExternalOutcome, via: &[String]) -> (bool, Vec<String>) {
    let mut log = Vec::new();
    let Some((gh_repo, issue_n)) = parse_gh_ref(ext) else { return (true, log) };
    if !run_state::reported_exists(&ctx.run, id) {
        let body = outcome_comment(outcome, via, &gh_repo);
        if !d.gh.issue_comment(&gh_repo, &issue_n, &body) {
            log.push(format!("gh-closeout {id}: could not comment the outcome on {ext}"));
            return (false, log);
        }
        run_state::reported_write(&ctx.run, id);
    }
    if !outcome.closes_issue() {
        log.push(format!("gh-closeout {id}: reported {outcome:?} on {ext}"));
        return (true, log);
    }
    if run_state::mark_exists(&ctx.run, id) || d.gh.issue_close(&gh_repo, &issue_n) {
        run_state::mark_write(&ctx.run, id);
        log.push(format!("gh-closeout {id}: reported {outcome:?} and closed {ext}"));
        return (true, log);
    }
    log.push(format!("gh-closeout {id}: could not close {ext}"));
    (false, log)
}

/// `gh-intake closeout <bead-id> <landed-sha> <repo-path>`: comment on and close the bead's
/// issue. The comment is posted once however many times the close is retried.
pub fn gh_issue_closeout(d: &Deps, ctx: &Ctx, id: &str, sha: &str, repo_path: &Path) -> Vec<String> {
    let mut log = Vec::new();
    if run_state::mark_exists(&ctx.run, id) {
        return log;
    }

    let ext_ref = d.bd.show_json(id).map(|v| text_of(&v, "external_ref")).unwrap_or_default();
    if !ext_ref.starts_with("github:") {
        return log;
    }

    let Some((gh_repo, issue_n)) = parse_gh_ref(&ext_ref) else {
        log.push(format!("gh-closeout {id}: malformed external_ref {ext_ref} — skipping"));
        return log;
    };

    if d.gh.issue_state(&gh_repo, &issue_n) == "CLOSED" {
        run_state::mark_write(&ctx.run, id);
        return log;
    }

    let sha_short = d.git.rev_parse_short(repo_path, sha).unwrap_or_else(|| short7(sha));
    if !run_state::reported_exists(&ctx.run, id) {
        let subject = d.git.subject_of(repo_path, sha).unwrap_or_default();
        if !d.gh.issue_comment(&gh_repo, &issue_n, &comment_body(&sha_short, &subject, &gh_repo, sha)) {
            log.push(format!("gh-closeout {id}: could not comment on {ext_ref}"));
            return log;
        }
        run_state::reported_write(&ctx.run, id);
    }
    if d.gh.issue_close(&gh_repo, &issue_n) {
        run_state::mark_write(&ctx.run, id);
        log.push(format!("gh-closeout {id}: closed {ext_ref} as {sha_short}"));
    } else {
        log.push(format!("gh-closeout {id}: could not close {ext_ref}"));
    }
    log
}

/// Asks the operator what to tell an issue whose bead ended with no landing, once: the ask's id
/// goes on the pending row, which is what keeps it from being asked again.
fn ask_unlanded(d: &Deps, ctx: &Ctx, id: &str, ext_ref: &str) -> (Verdict, Vec<String>) {
    let Some((gh_repo, issue_n)) = parse_gh_ref(ext_ref) else { return (Verdict::Done, Vec::new()) };
    if d.gh.issue_state(&gh_repo, &issue_n) == "CLOSED" {
        run_state::mark_write(&ctx.run, id);
        return (Verdict::Done, vec![format!("gh-closeout {id}: {ext_ref} already closed on forge — skipping")]);
    }
    let subj = close_issue_subject(ext_ref, id);
    let dflt = "post a comment explaining the resolution and close the issue";
    let body = ask_unlanded_body(&subj, dflt, id, ext_ref);
    match d.mail.send_question("Landing gate <gate@spira>", &subj, dflt, id, body.as_bytes()) {
        Ok(ask) if !ask.is_empty() => (Verdict::Asked(ask.clone()), vec![format!("gh-closeout {id}: asked operator about {ext_ref} ({ask})")]),
        Ok(_) => (Verdict::Failed, vec![format!("gh-closeout {id}: ask sent but named no ask bead")]),
        Err(e) if !e.is_empty() => (Verdict::Failed, vec![format!("gh-closeout {id}: ask refused ({e})")]),
        Err(_) => (Verdict::Failed, vec![format!("gh-closeout {id}: ask refused — probe fault: mail produced no reason")]),
    }
}

/// A row that already has an ask: done once the issue is closed on GitHub (the ask is closed
/// with it) or the operator has answered the ask; until then the ask must not block the bead.
fn follow_ask(d: &Deps, ctx: &Ctx, p: &Pending, ask: &str) -> (Verdict, Vec<String>) {
    let id = &p.bead_id;
    let mut log = Vec::new();
    let Some((gh_repo, issue_n)) = parse_gh_ref(&p.external_ref) else { return (Verdict::Done, log) };
    let ask_row = d.bd.show_json(ask);
    let answered = ask_row.as_ref().and_then(show_row).is_some_and(|r| spira_config::nonwork::row_closed(spira_config::nonwork::Kind::Ask, r));
    if d.gh.issue_state(&gh_repo, &issue_n) == "CLOSED" {
        run_state::mark_write(&ctx.run, id);
        if !answered {
            let reason = format!("{} is closed on GitHub — resolved automatically; nothing further for the operator.\n", p.external_ref);
            let _ = d.bd.close(ask, &reason);
        }
        log.push(format!("gh-closeout {id}: {} found closed — resolved ask {ask}", p.external_ref));
        return (Verdict::Done, log);
    }
    if answered {
        run_state::mark_write(&ctx.run, id);
        log.push(format!("gh-closeout {id}: ask {ask} already answered — marker written, no re-ask"));
        return (Verdict::Done, log);
    }
    if d.bd.show_json(id).is_some_and(|v| blocks(&v, ask)) {
        let _ = d.bd.dep_remove(id, ask);
        let _ = d.bd.dep_relate(ask, id);
        log.push(format!("gh-closeout {id}: converted blocking ask {ask} to relates_to"));
    }
    (Verdict::Wait, log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{LcBead, Pending};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[test]
    fn parse_gh_ref_splits_on_first_and_last_hash() {
        assert_eq!(parse_gh_ref("github:fixture/testrepo#7"), Some(("fixture/testrepo".into(), "7".into())));
        assert_eq!(parse_gh_ref("github:fixture/testrepo#"), None);
        assert_eq!(parse_gh_ref("github:fixture/testrepo#7x"), None);
        assert_eq!(parse_gh_ref("github:"), None);
    }

    #[test]
    fn comment_body_cites_sha_and_commit_link_with_and_without_subject() {
        let b = comment_body("abc1234", "a fix", "fixture/testrepo", "abc1234full");
        assert_eq!(b, "Fixed in abc1234 (a fix)\n\nhttps://github.com/fixture/testrepo/commit/abc1234full");
        let b2 = comment_body("abc1234", "", "fixture/testrepo", "abc1234full");
        assert_eq!(b2, "Fixed in abc1234\n\nhttps://github.com/fixture/testrepo/commit/abc1234full");
    }

    /// The bead store, the lifecycle machine and the pending record in one fake, so one log
    /// shows every read a drain made.
    #[derive(Default)]
    struct World {
        calls: RefCell<Vec<String>>,
        show: RefCell<BTreeMap<String, serde_json::Value>>,
        beads: RefCell<BTreeMap<String, LcBead>>,
        rows: RefCell<BTreeMap<String, Pending>>,
    }

    impl World {
        fn log(&self, what: String) {
            self.calls.borrow_mut().push(what);
        }
        fn bead(&self, id: &str, ext: &str, state: &str, reason: &str, sha: &str, close_reason: &str) {
            self.show.borrow_mut().insert(
                id.into(),
                serde_json::json!({"external_ref": ext, "close_reason": close_reason, "labels": ["repo:fixture"]}),
            );
            self.beads.borrow_mut().insert(id.into(), LcBead { state: state.into(), sha: sha.into(), reason: reason.into() });
            if matches!(state, "LANDED" | "SUPERSEDED" | "DROPPED") && ext.starts_with("github:") && !id.starts_with("sp-succ") {
                self.rows.borrow_mut().insert(
                    id.into(),
                    Pending { bead_id: id.into(), external_ref: ext.into(), outcome_state: state.into(), sha: sha.into(), ask_id: None, attempts: 0 },
                );
            }
        }
        fn kinds(&self) -> Vec<String> {
            self.calls.borrow().iter().map(|c| c.split('(').next().unwrap_or("").to_string()).collect()
        }
    }

    impl Bd for World {
        fn by_ref(&self, _: &str) -> Result<Vec<crate::store::KnownRef>, String> {
            Err("closeout reads no reference".into())
        }
        fn create(&self, _: &str, _: &str, _: &str, _: &str, _: &[u8]) -> Option<String> {
            None
        }
        fn note(&self, _: &str, _: &str) -> bool {
            true
        }
        fn close(&self, id: &str, _: &str) -> bool {
            self.log(format!("close({id})"));
            true
        }
        fn show_json(&self, id: &str) -> Option<serde_json::Value> {
            self.log(format!("show_json({id})"));
            self.show.borrow().get(id).cloned()
        }
        fn dep_remove(&self, id: &str, other: &str) -> bool {
            self.log(format!("dep_remove({id},{other})"));
            true
        }
        fn dep_relate(&self, from: &str, to: &str) -> bool {
            self.log(format!("dep_relate({from},{to})"));
            true
        }
    }

    impl Lifecycle for World {
        fn bead(&self, id: &str) -> Result<Option<LcBead>, String> {
            self.log(format!("bead({id})"));
            Ok(self.beads.borrow().get(id).cloned())
        }
    }

    impl Closeouts for World {
        fn pending(&self) -> Result<Vec<Pending>, String> {
            self.log("pending()".into());
            Ok(self.rows.borrow().values().cloned().collect())
        }
        fn done(&self, id: &str) -> bool {
            self.log(format!("done({id})"));
            self.rows.borrow_mut().remove(id).is_some()
        }
        fn asked(&self, id: &str, ask: &str) -> bool {
            self.log(format!("asked({id},{ask})"));
            self.rows.borrow_mut().get_mut(id).map(|r| r.ask_id = Some(ask.into())).is_some()
        }
        fn failed(&self, id: &str) -> bool {
            self.log(format!("failed({id})"));
            self.rows.borrow_mut().get_mut(id).map(|r| r.attempts += 1).is_some()
        }
    }

    #[derive(Default)]
    struct FakeGh {
        state: RefCell<BTreeMap<String, String>>,
        comments: RefCell<Vec<(String, String, String)>>,
        closes: RefCell<Vec<(String, String)>>,
        fail_close: RefCell<bool>,
    }

    impl Gh for FakeGh {
        fn issue_state(&self, repo: &str, n: &str) -> String {
            self.state.borrow().get(&format!("{repo}#{n}")).cloned().unwrap_or_else(|| "OPEN".into())
        }
        fn issue_comment(&self, repo: &str, n: &str, body: &str) -> bool {
            self.comments.borrow_mut().push((repo.into(), n.into(), body.into()));
            true
        }
        fn issue_close(&self, repo: &str, n: &str) -> bool {
            if *self.fail_close.borrow() {
                return false;
            }
            self.closes.borrow_mut().push((repo.into(), n.into()));
            true
        }
    }

    struct FakeGit;

    impl Git for FakeGit {
        fn rev_parse_short(&self, _: &Path, sha: &str) -> Option<String> {
            Some(sha.chars().take(7).collect())
        }
        fn subject_of(&self, _: &Path, _: &str) -> Option<String> {
            None
        }
    }

    struct FakeRepo;

    impl Repo for FakeRepo {
        fn root_with_git(&self, name: &str) -> Option<String> {
            (name == "fixture").then(|| "/r".to_string())
        }
        fn all_names(&self) -> Vec<String> {
            vec!["fixture".to_string()]
        }
    }

    #[derive(Default)]
    struct FakeMail {
        sent: RefCell<Vec<String>>,
    }

    impl Mail for FakeMail {
        fn send_operator_note(&self, _: &str, _: &[u8]) -> bool {
            true
        }
        fn send_question(&self, _: &str, subject: &str, _: &str, _: &str, _: &[u8]) -> Result<String, String> {
            self.sent.borrow_mut().push(subject.to_string());
            Ok("sp-ask1".to_string())
        }
    }

    fn drain_over(w: &World, gh: &FakeGh, mail: &FakeMail, run: &Path) -> Vec<String> {
        let d = Deps { bd: w, gh, git: &FakeGit, repo: &FakeRepo, mail, lc: w, co: w };
        drain(&d, &Ctx { run: run.to_path_buf() })
    }

    fn mixed_world() -> World {
        let w = World::default();
        w.bead("sp-l", "github:fixture/testrepo#1", "LANDED", "", "aaaaaaa1", "");
        w.bead("sp-w", "github:fixture/testrepo#2", "DROPPED", "unwanted", "", "won't-fix by operator");
        w.bead("sp-s", "github:fixture/testrepo#3", "SUPERSEDED", "sp-succ", "", "");
        w.bead("sp-succ", "", "LANDED", "", "bbbbbbb2", "");
        w
    }

    #[test]
    fn a_drain_reads_only_the_pending_rows_and_points_by_id_and_settles_each_issue_once() {
        let tmp = testkit::TempDir::new("gh-intake-drain-points");
        let (w, gh, mail) = (mixed_world(), FakeGh::default(), FakeMail::default());
        drain_over(&w, &gh, &mail, tmp.path());
        let kinds = w.kinds();
        assert!(kinds.iter().all(|k| matches!(k.as_str(), "pending" | "bead" | "show_json" | "done")), "{:?}", w.calls.borrow());
        assert_eq!(kinds.iter().filter(|k| *k == "pending").count(), 1);
        assert_eq!(gh.comments.borrow().len(), 3, "one comment per issue: {:?}", gh.comments.borrow());
        let closed: Vec<String> = gh.closes.borrow().iter().map(|(_, n)| n.clone()).collect();
        assert_eq!(closed, ["1", "3"], "the won't-fix is reported but its issue stays open");
        assert!(w.rows.borrow().is_empty(), "every row is done");
        assert!(mail.sent.borrow().is_empty());
        let (before, comments) = (w.calls.borrow().len(), gh.comments.borrow().len());
        drain_over(&w, &gh, &mail, tmp.path());
        assert_eq!(gh.comments.borrow().len(), comments, "a second drain does nothing");
        assert_eq!(w.calls.borrow().len(), before + 1, "the second drain only asks what is pending");
    }

    #[test]
    fn a_failed_close_leaves_the_row_pending_and_the_next_drain_closes_the_issue_once() {
        let tmp = testkit::TempDir::new("gh-intake-drain-retry");
        let w = World::default();
        w.bead("sp-l", "github:fixture/testrepo#1", "LANDED", "", "aaaaaaa1", "");
        let (gh, mail) = (FakeGh::default(), FakeMail::default());
        *gh.fail_close.borrow_mut() = true;
        drain_over(&w, &gh, &mail, tmp.path());
        assert_eq!(w.rows.borrow()["sp-l"].attempts, 1, "the failed attempt is counted");
        *gh.fail_close.borrow_mut() = false;
        drain_over(&w, &gh, &mail, tmp.path());
        assert!(w.rows.borrow().is_empty());
        assert_eq!(gh.closes.borrow().len(), 1);
        assert_eq!(gh.comments.borrow().len(), 1, "the retry does not comment again");
    }

    #[test]
    fn a_landed_closeout_comments_the_machines_merge_sha_not_the_tip_recorded_at_enqueue() {
        let tmp = testkit::TempDir::new("gh-intake-drain-sha");
        let w = World::default();
        w.bead("sp-l", "github:fixture/testrepo#1", "LANDED", "", "bbbbbbb2", "");
        w.rows.borrow_mut().get_mut("sp-l").unwrap().sha = "aaaaaaa1".into();
        let (gh, mail) = (FakeGh::default(), FakeMail::default());
        drain_over(&w, &gh, &mail, tmp.path());
        let comments = gh.comments.borrow();
        assert!(comments.iter().all(|c| format!("{c:?}").contains("bbbbbbb")), "{comments:?}");
        assert!(!comments.is_empty());
    }

    #[test]
    fn a_successor_still_in_flight_leaves_the_row_untouched() {
        let tmp = testkit::TempDir::new("gh-intake-drain-inflight");
        let w = World::default();
        w.bead("sp-s", "github:fixture/testrepo#3", "SUPERSEDED", "sp-succ", "", "");
        w.bead("sp-succ", "", "CERTIFIED", "", "", "");
        let (gh, mail) = (FakeGh::default(), FakeMail::default());
        drain_over(&w, &gh, &mail, tmp.path());
        assert_eq!(w.rows.borrow()["sp-s"].attempts, 0);
        assert!(gh.comments.borrow().is_empty() && mail.sent.borrow().is_empty());
    }

    #[test]
    fn a_drop_with_no_named_outcome_asks_once_and_records_the_ask_on_the_row() {
        let tmp = testkit::TempDir::new("gh-intake-drain-ask");
        let w = World::default();
        w.bead("sp-d", "github:fixture/testrepo#4", "DROPPED", "unwanted", "", "residue");
        let (gh, mail) = (FakeGh::default(), FakeMail::default());
        drain_over(&w, &gh, &mail, tmp.path());
        drain_over(&w, &gh, &mail, tmp.path());
        assert_eq!(mail.sent.borrow().len(), 1, "the recorded ask is not asked again");
        assert_eq!(w.rows.borrow()["sp-d"].ask_id.as_deref(), Some("sp-ask1"));
        assert!(gh.comments.borrow().is_empty());
    }

    #[test]
    fn a_row_whose_issue_closed_on_github_closes_its_ask_by_id_and_is_done() {
        let tmp = testkit::TempDir::new("gh-intake-drain-stale-ask");
        let w = World::default();
        w.bead("sp-d", "github:fixture/testrepo#4", "DROPPED", "unwanted", "", "residue");
        w.rows.borrow_mut().get_mut("sp-d").unwrap().ask_id = Some("sp-ask1".into());
        w.show.borrow_mut().insert("sp-ask1".into(), serde_json::json!({"status": "open"}));
        let (gh, mail) = (FakeGh::default(), FakeMail::default());
        gh.state.borrow_mut().insert("fixture/testrepo#4".into(), "CLOSED".into());
        drain_over(&w, &gh, &mail, tmp.path());
        assert!(w.calls.borrow().iter().any(|c| c == "close(sp-ask1)"), "{:?}", w.calls.borrow());
        assert!(w.rows.borrow().is_empty());
        assert!(gh.comments.borrow().is_empty(), "an issue closed elsewhere is not commented on");
    }

    #[test]
    fn a_closeout_for_a_bead_with_no_github_reference_posts_nothing() {
        let tmp = testkit::TempDir::new("gh-intake-closeout-no-ref");
        let w = World::default();
        w.bead("sp-b", "", "LANDED", "", "deadbeef", "");
        let gh = FakeGh::default();
        let d = Deps { bd: &w, gh: &gh, git: &FakeGit, repo: &FakeRepo, mail: &FakeMail::default(), lc: &w, co: &w };
        let log = gh_issue_closeout(&d, &Ctx { run: tmp.path().to_path_buf() }, "sp-b", "deadbeef", Path::new("/r"));
        assert!(log.is_empty() && gh.comments.borrow().is_empty());
    }
}
