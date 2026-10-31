//! The per-repo pacing a round scheduler owes its operator: hold on a red, back off on a
//! harness fault, page an orphaned open round. State lives in files under
//! `<run>/rounds-pace/<repo>/` so one repo's hold never touches another's and an operator
//! lifts a hold with `rm`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use spira_config::scoped_stop;

pub const PACE_OWNER: &str = "batcher";

pub const CERTIFY_ENV_MARKER: &str = "queue round certify exited 4";
pub const INFRA_MARKERS: &[&str] = &["NO RESULT", "OVER-CAP", "cannot check out", "certify rc=75", "certify exited 75", "left no verdicts", CERTIFY_ENV_MARKER];
pub const STALE_AMI_MARKER: &str = "round AMI is stale";
pub const BACKOFF_SECS: u64 = 600;
pub const STOP_AFTER: u32 = 2;
pub const REFUSAL_WINDOW_SECS: u64 = 1800;
pub const REFUSAL_STOP_AFTER: u32 = 2;
pub const ORPHAN_REDRIVE_AFTER_SECS: u64 = 600;

#[derive(Default, Clone, PartialEq, Debug)]
pub struct State {
    pub infra: u32,
    pub backoff_until: u64,
    pub paged_open: String,
    pub orphan: String,
    pub orphan_since: u64,
    pub redriven: String,
}

#[derive(PartialEq, Debug)]
pub enum Outcome {
    Quiet,
    Red,
    Infra,
    StaleImage,
    OpenFailed,
}

#[derive(PartialEq, Debug)]
pub enum Hold {
    Paused,
    Stopped,
    Backoff(u64),
}

#[derive(Default, PartialEq, Debug)]
pub struct Effects {
    pub pause: bool,
    pub stop: bool,
    pub pages: Vec<String>,
}

/// The cut's result and the repo's local verdict (`verdict`, `at`) as written during the pass.
/// A cut that admitted nothing is `Quiet` whatever its wording: only a round that ran and did
/// not land is red.
pub fn classify(result: &Result<(), String>, verdict: Option<(&str, u64)>, pass_start: u64) -> Outcome {
    let text = result.as_ref().err().map(String::as_str).unwrap_or("");
    if text.contains(STALE_AMI_MARKER) {
        return Outcome::StaleImage;
    }
    if INFRA_MARKERS.iter().any(|m| text.contains(m)) {
        return Outcome::Infra;
    }
    let red_verdict = matches!(verdict, Some(("red", at)) if at >= pass_start);
    if text.contains("NOT LANDED") || (result.is_ok() && red_verdict) {
        return Outcome::Red;
    }
    if text.contains("OPEN FAILED") {
        return Outcome::OpenFailed;
    }
    Outcome::Quiet
}

pub fn hold(paused: bool, stopped: bool, st: &State, now: u64) -> Option<Hold> {
    if stopped {
        Some(Hold::Stopped)
    } else if paused {
        Some(Hold::Paused)
    } else if st.backoff_until > now {
        Some(Hold::Backoff(st.backoff_until))
    } else {
        None
    }
}

pub fn advance(st: &State, outcome: &Outcome, repo: &str, now: u64, detail: &str) -> (State, Effects) {
    let mut next = st.clone();
    let mut fx = Effects::default();
    match outcome {
        Outcome::Quiet => next.infra = 0,
        Outcome::Red => {
            next.infra = 0;
            fx.pause = true;
            fx.pages.push(format!("ROUND RED {repo} — attribute and eject, then rm the pause file ({detail})"));
        }
        Outcome::StaleImage => {}
        Outcome::OpenFailed => {
            fx.pages.push(format!("OPEN FAILED {repo} — the cut could not open a round ({detail})"));
        }
        Outcome::Infra => {
            next.infra += 1;
            next.backoff_until = now + BACKOFF_SECS;
            if next.infra >= STOP_AFTER {
                fx.stop = true;
                fx.pages.push(format!(
                    "ROUNDS STOPPED {repo}: {} consecutive rounds produced no verdicts — VM/pool fault, look before lifting the stop ({detail})",
                    next.infra
                ));
            }
        }
    }
    (next, fx)
}

/// What an open round with no driver is owed this pass.
#[derive(PartialEq, Debug)]
pub enum Orphan {
    /// Seen undriven for less than [`ORPHAN_REDRIVE_AFTER_SECS`].
    Wait,
    /// Undriven long enough, and its worktree still stands at its head: certify can drive it.
    Redrive,
    /// Cannot be driven by certify, or was re-driven once and is undriven again: a person decides.
    Incident(String),
}

/// Tracks how long `open_id` has gone undriven and decides what it is owed. `fit` is `Err(why)`
/// when certify would refuse the round at once, so a re-drive would change nothing.
pub fn orphan_step(st: &State, open_id: &str, now: u64, fit: Result<(), String>) -> (State, Orphan) {
    let mut next = st.clone();
    if next.orphan != open_id {
        next.orphan = open_id.to_string();
        next.orphan_since = now;
    }
    let since = next.orphan_since;
    let verdict = if now.saturating_sub(since) < ORPHAN_REDRIVE_AFTER_SECS {
        Orphan::Wait
    } else if let Err(why) = fit {
        Orphan::Incident(format!("it cannot be re-driven: {why}"))
    } else if next.redriven == open_id {
        Orphan::Incident("it was re-driven once and has no driver again".to_string())
    } else {
        next.redriven = open_id.to_string();
        next.orphan_since = now;
        Orphan::Redrive
    };
    (next, verdict)
}

/// The open round's own ways out, run by the rounds loop; each is a seam so the loop is testable.
pub struct Orphans<'a> {
    pub fit: &'a dyn Fn(&str) -> Result<(), String>,
    pub redrive: &'a dyn Fn(&str) -> Result<(), String>,
    pub incident: &'a dyn Fn(&str, &str) -> Result<String, String>,
}

pub struct Files {
    run: PathBuf,
    repo: String,
    dir: PathBuf,
}

impl Files {
    pub fn new(run: &Path, repo: &str) -> Files {
        Files { run: run.to_path_buf(), repo: repo.to_string(), dir: run.join("rounds-pace").join(repo) }
    }
    pub fn pause_file(&self) -> PathBuf {
        self.dir.join("pause")
    }
    pub fn load(&self) -> State {
        let kv: BTreeMap<String, String> = fs::read_to_string(self.dir.join("state"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        State {
            infra: kv.get("infra").and_then(|v| v.parse().ok()).unwrap_or(0),
            backoff_until: kv.get("backoff_until").and_then(|v| v.parse().ok()).unwrap_or(0),
            paged_open: kv.get("paged_open").cloned().unwrap_or_default(),
            orphan: kv.get("orphan").cloned().unwrap_or_default(),
            orphan_since: kv.get("orphan_since").and_then(|v| v.parse().ok()).unwrap_or(0),
            redriven: kv.get("redriven").cloned().unwrap_or_default(),
        }
    }
    pub fn save(&self, st: &State) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let body = format!(
            "infra={}\nbackoff_until={}\npaged_open={}\norphan={}\norphan_since={}\nredriven={}\n",
            st.infra, st.backoff_until, st.paged_open, st.orphan, st.orphan_since, st.redriven
        );
        fs::write(self.dir.join("state"), body).map_err(|e| format!("rounds-pace state: {e}"))
    }
    fn touch(&self, p: PathBuf) -> Result<(), String> {
        fs::create_dir_all(&self.dir).and_then(|_| fs::write(&p, "")).map_err(|e| format!("{}: {e}", p.display()))
    }
}

#[derive(Default, Clone, PartialEq, Debug)]
pub struct Refusals {
    pub key: String,
    pub at: u64,
    pub count: u32,
}

/// A refusal's text with the per-round tokens (batch id, object ids) removed, so the same
/// refusal on a fresh round compares equal.
pub fn is_stale_image(msg: &str) -> bool {
    msg.contains(STALE_AMI_MARKER)
}

pub fn refusal_key(msg: &str, batch: &str) -> String {
    msg.split_whitespace()
        .filter(|t| !(t.len() >= 40 && t.chars().all(|c| c.is_ascii_hexdigit())) && !t.contains(batch))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn note_refusal(prev: &Refusals, key: &str, now: u64) -> Refusals {
    let repeat = prev.key == key && now.saturating_sub(prev.at) <= REFUSAL_WINDOW_SECS;
    Refusals { key: key.to_string(), at: now, count: if repeat { prev.count + 1 } else { 1 } }
}

impl Files {
    fn refusals_path(&self) -> PathBuf {
        self.dir.join("refusals")
    }
    pub fn load_refusals(&self) -> Refusals {
        let text = fs::read_to_string(self.refusals_path()).unwrap_or_default();
        let mut lines = text.lines();
        let (at, count) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
        Refusals { at: at.parse().unwrap_or(0), count: count.parse().unwrap_or(0), key: lines.collect::<Vec<_>>().join(" ") }
    }
    pub fn clear_refusals(&self) {
        let _ = fs::remove_file(self.refusals_path());
    }
    /// Records a refusal; at the second like it inside the window the repo is stopped and the
    /// text to page is returned.
    pub fn record_refusal(&self, repo: &str, key: &str, now: u64) -> Result<Option<String>, String> {
        let next = note_refusal(&self.load_refusals(), key, now);
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::write(self.refusals_path(), format!("{}\n{}\n{}\n", next.at, next.count, next.key)).map_err(|e| format!("rounds-pace refusals: {e}"))?;
        if next.count < REFUSAL_STOP_AFTER {
            return Ok(None);
        }
        let why = format!("{} consecutive rounds refused for the same reason ({key})", next.count);
        self.stop_rounds(&why, now)?;
        Ok(Some(format!("ROUNDS STOPPED {repo}: {why} — look before lifting: world.sh unpause --repo {repo} --lanes rounds")))
    }
    pub fn stop_rounds(&self, why: &str, now: u64) -> Result<(), String> {
        let until = format!("the fault is fixed; then world.sh unpause --repo {} --lanes rounds", self.repo);
        scoped_stop::pause_repo(&self.run, &self.repo, &["rounds".to_string()], PACE_OWNER, why, &until, now).map(|_| ())
    }
    pub fn stopped(&self) -> bool {
        scoped_stop::stop_for(&self.run, &self.repo, "rounds").is_some()
    }
}

pub struct Seams<'a> {
    pub repo: &'a str,
    pub now: u64,
    pub inbox: &'a Path,
    pub stamp: &'a dyn Fn(u64) -> String,
}

fn page(sm: &Seams, text: &str) -> Result<(), String> {
    let mut f = fs::OpenOptions::new().create(true).append(true).open(sm.inbox).map_err(|e| format!("{}: {e}", sm.inbox.display()))?;
    writeln!(f, "{} [watch:rounds] {text}", (sm.stamp)(sm.now)).map_err(|e| format!("{}: {e}", sm.inbox.display()))
}

/// One repo's paced cut. Returns the cut's own result so a failure still fails the pass.
pub fn paced(
    files: &Files,
    sm: &Seams,
    open_round: impl FnOnce() -> Result<Option<String>, String>,
    driven: impl FnOnce(&str) -> bool,
    verdict: impl FnOnce() -> Option<(String, u64)>,
    orphans: &Orphans,
    cut: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut st = files.load();
    if let Some(h) = hold(files.pause_file().exists(), files.stopped(), &st, sm.now) {
        println!("batcher rounds: {}: held ({h:?}) — no cut", sm.repo);
        return Ok(());
    }
    if let Some(id) = open_round()? {
        if driven(&id) {
            println!("batcher rounds: {}: round {id} is open and being certified — not re-cut", sm.repo);
            if !st.orphan.is_empty() {
                files.save(&State { orphan: String::new(), orphan_since: 0, ..st })?;
            }
            return Ok(());
        }
        let (mut next, step) = orphan_step(&st, &id, sm.now, (orphans.fit)(&id));
        match step {
            Orphan::Wait => println!("batcher rounds: {}: round {id} is open with no driver — not re-cut", sm.repo),
            Orphan::Redrive => match (orphans.redrive)(&id) {
                Ok(()) => println!("batcher rounds: {}: round {id} had no driver; re-driving it once", sm.repo),
                Err(e) => println!("batcher rounds: {}: round {id} had no driver; the re-drive did not start: {e}", sm.repo),
            },
            Orphan::Incident(why) => {
                if next.paged_open != id {
                    match (orphans.incident)(&id, &why) {
                        Ok(inc) => {
                            println!("batcher rounds: {}: round {id} is stranded ({why}) — filed {inc}", sm.repo);
                            next.paged_open = id.clone();
                        }
                        Err(e) => println!("batcher rounds: {}: round {id} is stranded ({why}) — could not file the incident: {e}", sm.repo),
                    }
                }
            }
        }
        files.save(&next)?;
        return Ok(());
    }
    let res = cut();
    let v = verdict();
    let outcome = classify(&res, v.as_ref().map(|(s, at)| (s.as_str(), *at)), sm.now);
    let detail = res.as_ref().err().cloned().unwrap_or_default();
    let (next, fx) = advance(&st, &outcome, sm.repo, sm.now, &detail);
    st = next;
    if fx.pause {
        files.touch(files.pause_file())?;
    }
    if fx.stop {
        files.stop_rounds(&format!("{} consecutive rounds produced no verdicts ({detail})", st.infra), sm.now)?;
    }
    for p in &fx.pages {
        page(sm, p)?;
    }
    files.save(&st)?;
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn dir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("pace-{tag}"))
    }
    fn stamp(n: u64) -> String {
        format!("T{n}")
    }
    fn run(d: &Path, repo: &str, now: u64, open: Option<&str>, verdict: Option<(&str, u64)>, res: Result<(), String>) -> bool {
        let inbox = d.join("inbox.log");
        let sm = Seams { repo, now, inbox: &inbox, stamp: &stamp };
        let cut_ran = Cell::new(false);
        let none = Orphans { fit: &|_| Ok(()), redrive: &|_| Ok(()), incident: &|_, _| Ok(String::new()) };
        let r = paced(&Files::new(d, repo), &sm, || Ok(open.map(String::from)), |_| false, || verdict.map(|(s, a)| (s.to_string(), a)), &none, || {
            cut_ran.set(true);
            res
        });
        let _ = r;
        cut_ran.get()
    }
    fn inbox(d: &Path) -> String {
        fs::read_to_string(d.join("inbox.log")).unwrap_or_default()
    }

    #[test]
    fn a_refusal_repeated_inside_the_window_stops_the_repo_once_and_a_new_reason_does_not() {
        let t = dir("refusal");
        let f = Files::new(t.path(), "a");
        let key = refusal_key("refused at 0123456789abcdef0123456789abcdef01234567 — round r-9 holds [x] but built from [y]", "r-9");
        assert!(!key.contains("0123456789abcdef"));
        assert_eq!(f.record_refusal("a", &key, 1000).unwrap(), None);
        assert!(!f.stopped(), "one abandon only records");
        assert_eq!(f.record_refusal("a", "other", 1010).unwrap(), None);
        let page = f.record_refusal("a", "other", 1040).unwrap().expect("the second like refusal alarms");
        assert!(page.contains("ROUNDS STOPPED a") && f.stopped());
        let g = Files::new(t.path(), "b");
        g.record_refusal("b", "k", 1000).unwrap();
        assert_eq!(g.record_refusal("b", "k", 1000 + REFUSAL_WINDOW_SECS + 1).unwrap(), None, "outside the window it is a first");
    }

    #[test]
    fn a_not_landed_round_pauses_and_pages_and_the_pause_holds_the_next_cut() {
        let t = dir("red");
        let d = t.path();
        let ran = run(d, "a", 1000, None, None, Err("NOT LANDED: suite x red".into()));
        assert!(ran);
        assert!(Files::new(d, "a").pause_file().exists());
        assert!(inbox(d).contains("T1000 [watch:rounds] ROUND RED a"));
        let ran = run(d, "a", 2000, None, None, Ok(()));
        assert!(!ran, "paused repo must not cut");
        fs::remove_file(Files::new(d, "a").pause_file()).unwrap();
        let ran = run(d, "a", 3000, None, None, Ok(()));
        assert!(ran, "rm of the pause file lifts it");
    }

    #[test]
    fn a_red_local_verdict_written_in_the_pass_is_a_red_round_but_a_stale_one_is_not() {
        let t = dir("verdict");
        let d = t.path();
        run(d, "a", 1000, None, Some(("red", 999)), Ok(()));
        assert!(!Files::new(d, "a").pause_file().exists(), "a verdict from before the pass is not this pass's");
        run(d, "a", 1000, None, Some(("red", 1005)), Ok(()));
        assert!(Files::new(d, "a").pause_file().exists());
    }

    #[test]
    fn a_cut_that_admitted_nothing_never_pauses_whatever_its_wording() {
        let t = dir("empty");
        let d = t.path();
        for (i, msg) in ["nothing admissible", "pool empty", "queue is empty"].iter().enumerate() {
            run(d, "a", 1000 + i as u64 * 100, None, None, Err(msg.to_string()));
            run(d, "a", 1050 + i as u64 * 100, None, None, Ok(()));
        }
        assert!(!Files::new(d, "a").pause_file().exists());
        assert!(!Files::new(d, "a").stopped());
        assert_eq!(inbox(d), "");
    }

    #[test]
    fn a_round_with_no_verdicts_backs_off_and_the_second_in_a_row_stops_and_pages() {
        let t = dir("infra");
        let d = t.path();
        let infra = || Err("round left no verdicts".to_string());
        run(d, "a", 1000, None, None, infra());
        assert_eq!(inbox(d), "");
        let ran = run(d, "a", 1000 + BACKOFF_SECS - 1, None, None, infra());
        assert!(!ran, "inside the backoff there is no retry");
        let ran = run(d, "a", 1000 + BACKOFF_SECS, None, None, infra());
        assert!(ran);
        assert!(Files::new(d, "a").stopped());
        assert!(inbox(d).contains("ROUNDS STOPPED a: 2 consecutive"));
        let ran = run(d, "a", 9_000_000, None, None, Ok(()));
        assert!(!ran, "stopped stays stopped until the operator lifts the scoped stop");
    }

    #[test]
    fn a_clean_round_resets_the_infra_streak() {
        let t = dir("reset");
        let d = t.path();
        run(d, "a", 1000, None, None, Err("OVER-CAP".into()));
        run(d, "a", 2000, None, None, Ok(()));
        assert_eq!(Files::new(d, "a").load().infra, 0);
        run(d, "a", 3000, None, None, Err("OVER-CAP".into()));
        assert!(!Files::new(d, "a").stopped());
    }

    #[test]
    fn a_repeated_open_failed_pages_every_time_and_does_not_pause() {
        let t = dir("openfail");
        let d = t.path();
        run(d, "a", 1000, None, None, Err("OPEN FAILED: x".into()));
        run(d, "a", 1100, None, None, Err("OPEN FAILED: x".into()));
        assert_eq!(inbox(d).matches("OPEN FAILED a").count(), 2);
        assert!(!Files::new(d, "a").pause_file().exists());
    }

    type Calls = std::cell::RefCell<Vec<String>>;

    fn orphan_pass(d: &Path, now: u64, id: &str, driven: bool, fit: Result<(), String>, calls: &Calls) -> bool {
        let inbox_p = d.join("inbox.log");
        let sm = Seams { repo: "a", now, inbox: &inbox_p, stamp: &stamp };
        let (fit_f, redrive_f, incident_f) = (
            |_: &str| fit.clone(),
            |id: &str| {
                calls.borrow_mut().push(format!("redrive {id}"));
                Ok(())
            },
            |id: &str, why: &str| {
                calls.borrow_mut().push(format!("incident {id}: {why}"));
                Ok("sp-inc".to_string())
            },
        );
        let o = Orphans { fit: &fit_f, redrive: &redrive_f, incident: &incident_f };
        let cut = Cell::new(false);
        paced(&Files::new(d, "a"), &sm, || Ok(Some(id.into())), |_| driven, || None, &o, || {
            cut.set(true);
            Ok(())
        })
        .unwrap();
        cut.get()
    }

    #[test]
    fn an_open_round_with_no_driver_is_redriven_once_after_the_wait_and_never_re_cut() {
        let t = dir("orphan");
        let (d, calls) = (t.path(), Calls::default());
        assert!(!orphan_pass(d, 1000, "b-1", false, Ok(()), &calls));
        assert!(!orphan_pass(d, 1000 + ORPHAN_REDRIVE_AFTER_SECS - 1, "b-1", false, Ok(()), &calls));
        assert!(calls.borrow().is_empty(), "not before the wait: {:?}", calls.borrow());
        assert!(!orphan_pass(d, 1000 + ORPHAN_REDRIVE_AFTER_SECS, "b-1", false, Ok(()), &calls));
        assert_eq!(*calls.borrow(), vec!["redrive b-1".to_string()]);
    }

    #[test]
    fn a_redriven_round_that_is_undriven_again_files_one_incident_and_no_second_redrive() {
        let t = dir("orphan-twice");
        let (d, calls) = (t.path(), Calls::default());
        let n = ORPHAN_REDRIVE_AFTER_SECS;
        orphan_pass(d, 1000, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + n, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + n + 1, "b-1", false, Ok(()), &calls);
        assert_eq!(calls.borrow().len(), 1, "the re-driven round gets its own wait: {:?}", calls.borrow());
        orphan_pass(d, 1000 + 2 * n, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + 3 * n, "b-1", false, Ok(()), &calls);
        let c = calls.borrow();
        assert_eq!(c.iter().filter(|x| x.starts_with("redrive")).count(), 1, "{c:?}");
        assert_eq!(c.iter().filter(|x| x.starts_with("incident b-1")).count(), 1, "{c:?}");
    }

    #[test]
    fn a_round_whose_worktree_moved_on_is_not_redriven_and_files_an_incident_naming_why() {
        let t = dir("orphan-moved");
        let (d, calls) = (t.path(), Calls::default());
        orphan_pass(d, 1000, "b-1", false, Err("the worktree is at d7aa8f08, not 07da4601".into()), &calls);
        orphan_pass(d, 1000 + ORPHAN_REDRIVE_AFTER_SECS, "b-1", false, Err("the worktree is at d7aa8f08, not 07da4601".into()), &calls);
        let c = calls.borrow();
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(c[0].starts_with("incident b-1") && c[0].contains("d7aa8f08"), "{c:?}");
    }

    #[test]
    fn a_driver_seen_resets_the_orphan_wait() {
        let t = dir("orphan-driven");
        let (d, calls) = (t.path(), Calls::default());
        orphan_pass(d, 1000, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + ORPHAN_REDRIVE_AFTER_SECS - 10, "b-1", true, Ok(()), &calls);
        orphan_pass(d, 1000 + ORPHAN_REDRIVE_AFTER_SECS, "b-1", false, Ok(()), &calls);
        assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
    }

    #[test]
    fn an_open_round_a_certify_is_driving_is_never_acted_on() {
        let t = dir("driven");
        let (d, calls) = (t.path(), Calls::default());
        for k in 0..5 {
            assert!(!orphan_pass(d, 1000 + k * ORPHAN_REDRIVE_AFTER_SECS, "b-1", true, Ok(()), &calls));
        }
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn one_repos_red_does_not_hold_another() {
        let t = dir("repos");
        let d = t.path();
        run(d, "a", 1000, None, None, Err("NOT LANDED".into()));
        let ran = run(d, "b", 1010, None, None, Ok(()));
        assert!(ran);
        assert!(!Files::new(d, "b").pause_file().exists());
    }

    #[test]
    fn a_stale_round_image_never_counts_toward_the_stop() {
        let t = dir("stale-ami");
        let d = t.path();
        let stale = "certify rc=75: round AMI is stale: acquire failed: clone refused";
        assert_eq!(classify(&Err(stale.into()), None, 0), Outcome::StaleImage);
        for at in [1000, 2000, 3000] {
            run(d, "a", at, None, None, Err(stale.into()));
        }
        assert!(!Files::new(d, "a").stopped());
        assert_eq!(Files::new(d, "a").load().infra, 0);
    }

    #[test]
    fn a_stale_image_refusal_is_recognised_and_a_plain_one_is_not() {
        assert!(is_stale_image("cannot certify the round: round AMI is stale: acquire failed"));
        assert!(!is_stale_image("cannot certify the round: acquire failed: pool exhausted"));
    }

    #[test]
    fn a_certify_that_always_exits_4_gets_two_attempts_and_one_alert() {
        let t = dir("exit4");
        let d = t.path();
        let env = || Err(format!("refused — cannot certify the round: {CERTIFY_ENV_MARKER}: no VM"));
        let mut attempts = 0;
        for i in 0..10u64 {
            if run(d, "a", 1000 + i * (BACKOFF_SECS + 1), None, None, env()) {
                attempts += 1;
            }
        }
        assert_eq!(attempts, 2);
        assert_eq!(inbox(d).matches("ROUNDS STOPPED a").count(), 1);
    }

    #[test]
    fn the_matcher_could_have_found_a_marker() {
        assert_eq!(classify(&Err("x certify rc=75".into()), None, 0), Outcome::Infra);
        assert_eq!(classify(&Err("queue round certify exited 75: INCOMPLETE reason=vm-lost".into()), None, 0), Outcome::Infra);
        assert_eq!(classify(&Err("fine".into()), None, 0), Outcome::Quiet);
    }
}
