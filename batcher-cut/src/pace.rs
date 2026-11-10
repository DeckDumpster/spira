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

pub const STALE_AMI_MARKER: &str = "round AMI is stale";
pub const STOP_AFTER: u32 = 2;
pub const NO_VM_STOP_AFTER: u32 = 3;
pub const REFUSAL_WINDOW_SECS: u64 = 1800;
pub const REFUSAL_STOP_AFTER: u32 = 2;
pub const PAUSE_LIFTS_MAX: u32 = 1;

#[derive(Default, Clone, PartialEq, Debug)]
pub struct State {
    pub infra: u32,
    pub no_vm: u32,
    pub paged_open: String,
    pub orphan: String,
    pub orphan_since: u64,
    pub redriven: String,
    pub pause_lifts: u32,
}

/// Why a cut failed, decided where the failure is produced; the pacing reads the kind and never
/// the message.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CutKind {
    /// The harness produced no verdict (VM lost, over cap, certify environment fault).
    Infra,
    StaleImage,
    /// Any other failure: fails the pass, never pauses or backs off.
    Other,
}

#[derive(Clone, PartialEq, Debug)]
pub struct CutError {
    pub kind: CutKind,
    pub msg: String,
}

impl CutError {
    pub fn new(kind: CutKind, msg: impl Into<String>) -> CutError {
        CutError {
            kind,
            msg: msg.into(),
        }
    }
}

impl std::fmt::Display for CutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl From<String> for CutError {
    fn from(msg: String) -> CutError {
        CutError::new(CutKind::Other, msg)
    }
}

impl From<&str> for CutError {
    fn from(msg: &str) -> CutError {
        CutError::new(CutKind::Other, msg)
    }
}

#[derive(PartialEq, Debug)]
pub enum Outcome {
    Quiet,
    Red,
    Infra,
    NoVm,
    StaleImage,
    OpenFailed,
}

#[derive(PartialEq, Debug)]
pub enum Hold {
    Paused,
    Stopped,
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
pub fn classify(
    result: &Result<(), CutError>,
    verdict: Option<(&str, u64)>,
    pass_start: u64,
) -> Outcome {
    match result.as_ref().err().map(|e| e.kind) {
        Some(CutKind::StaleImage) => Outcome::StaleImage,
        Some(CutKind::Infra) => Outcome::Infra,
        Some(CutKind::Other) => Outcome::Quiet,
        None if matches!(verdict, Some(("red", at)) if at >= pass_start) => Outcome::Red,
        None => Outcome::Quiet,
    }
}

/// Whether a pause that has stood `held_for` seconds is the owner's to lift: past its bound, and
/// not already lifted once without a green cut in between.
pub fn pause_expired(st: &State, held_for: u64, lift_after: u64) -> bool {
    held_for >= lift_after && st.pause_lifts < PAUSE_LIFTS_MAX
}

pub fn hold(paused: bool, stopped: bool) -> Option<Hold> {
    if stopped {
        Some(Hold::Stopped)
    } else if paused {
        Some(Hold::Paused)
    } else {
        None
    }
}

pub fn advance(st: &State, outcome: &Outcome, repo: &str, detail: &str) -> (State, Effects) {
    let mut next = st.clone();
    let mut fx = Effects::default();
    match outcome {
        Outcome::Quiet => {
            next.infra = 0;
            next.no_vm = 0;
            next.pause_lifts = 0;
        }
        Outcome::Red => {
            next.infra = 0;
            next.no_vm = 0;
            fx.pause = true;
            let again = if st.pause_lifts > 0 {
                " (lifted once and red again — the clock will not lift it)"
            } else {
                ""
            };
            fx.pages.push(format!(
                "ROUND RED {repo}{again} — attribute and eject, then rm the pause file ({detail})"
            ));
        }
        Outcome::StaleImage => {}
        Outcome::NoVm => {
            next.no_vm += 1;
            if next.no_vm >= NO_VM_STOP_AFTER {
                fx.stop = true;
                fx.pages.push(format!("ROUNDS STOPPED {repo}: {} consecutive rounds could not run — no round VM, look before lifting the stop ({detail})", next.no_vm));
            }
        }
        Outcome::OpenFailed => {
            fx.pages.push(format!(
                "OPEN FAILED {repo} — the cut could not open a round ({detail})"
            ));
        }
        Outcome::Infra => {
            next.infra += 1;
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
    /// Seen undriven for less than the configured wait.
    Wait,
    /// Undriven long enough, and its worktree still stands at its head: certify can drive it.
    Redrive,
    /// Cannot be driven by certify, or was re-driven once and is undriven again: a person decides.
    Incident(String),
}

/// Tracks how long `open_id` has gone undriven and decides what it is owed. `fit` is `Err(why)`
/// when certify would refuse the round at once, so a re-drive would change nothing.
pub fn orphan_step(
    st: &State,
    open_id: &str,
    now: u64,
    wait: u64,
    fit: Result<(), String>,
) -> (State, Orphan) {
    let mut next = st.clone();
    if next.orphan != open_id {
        next.orphan = open_id.to_string();
        next.orphan_since = now;
    }
    let since = next.orphan_since;
    let verdict = if now.saturating_sub(since) < wait {
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
    pub opened: &'a dyn Fn(&str) -> bool,
}

pub struct Files {
    run: PathBuf,
    repo: String,
    dir: PathBuf,
}

impl Files {
    pub fn new(run: &Path, repo: &str) -> Files {
        Files {
            run: run.to_path_buf(),
            repo: repo.to_string(),
            dir: run.join("rounds-pace").join(repo),
        }
    }
    pub fn pause_file(&self) -> PathBuf {
        self.dir.join("pause")
    }
    /// When the pause was set: the stamp it carries, else (a hand-made file) its mtime.
    pub fn pause_since(&self) -> Option<u64> {
        let p = self.pause_file();
        let meta = fs::metadata(&p).ok()?;
        let text = fs::read_to_string(&p).unwrap_or_default();
        if text.starts_with("hold:") {
            return None;
        }
        let stamped = text
            .trim()
            .strip_prefix("red:")
            .and_then(|t| t.parse().ok());
        stamped.or_else(|| {
            meta.modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
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
            no_vm: kv.get("no_vm").and_then(|v| v.parse().ok()).unwrap_or(0),
            paged_open: kv.get("paged_open").cloned().unwrap_or_default(),
            orphan: kv.get("orphan").cloned().unwrap_or_default(),
            orphan_since: kv
                .get("orphan_since")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            redriven: kv.get("redriven").cloned().unwrap_or_default(),
            pause_lifts: kv
                .get("pause_lifts")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }
    pub fn save(&self, st: &State) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let body = format!(
            "infra={}\nno_vm={}\npaged_open={}\norphan={}\norphan_since={}\nredriven={}\npause_lifts={}\n",
            st.infra, st.no_vm, st.paged_open, st.orphan, st.orphan_since, st.redriven, st.pause_lifts
        );
        fs::write(self.dir.join("state"), body).map_err(|e| format!("rounds-pace state: {e}"))
    }
    fn stamp_pause(&self, now: u64) -> Result<(), String> {
        let p = self.pause_file();
        fs::create_dir_all(&self.dir)
            .and_then(|_| fs::write(&p, format!("red:{now}\n")))
            .map_err(|e| format!("{}: {e}", p.display()))
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
        .filter(|t| {
            !(t.len() >= 40 && t.chars().all(|c| c.is_ascii_hexdigit())) && !t.contains(batch)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn note_refusal(prev: &Refusals, key: &str, now: u64) -> Refusals {
    let repeat = prev.key == key && now.saturating_sub(prev.at) <= REFUSAL_WINDOW_SECS;
    Refusals {
        key: key.to_string(),
        at: now,
        count: if repeat { prev.count + 1 } else { 1 },
    }
}

impl Files {
    fn refusals_path(&self) -> PathBuf {
        self.dir.join("refusals")
    }
    pub fn load_refusals(&self) -> Refusals {
        let text = fs::read_to_string(self.refusals_path()).unwrap_or_default();
        let mut lines = text.lines();
        let (at, count) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
        Refusals {
            at: at.parse().unwrap_or(0),
            count: count.parse().unwrap_or(0),
            key: lines.collect::<Vec<_>>().join(" "),
        }
    }
    pub fn clear_refusals(&self) {
        let _ = fs::remove_file(self.refusals_path());
    }
    /// Records a refusal; at the second like it inside the window the repo is stopped and the
    /// text to page is returned.
    pub fn record_refusal(
        &self,
        repo: &str,
        key: &str,
        now: u64,
    ) -> Result<Option<String>, String> {
        let next = note_refusal(&self.load_refusals(), key, now);
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::write(
            self.refusals_path(),
            format!("{}\n{}\n{}\n", next.at, next.count, next.key),
        )
        .map_err(|e| format!("rounds-pace refusals: {e}"))?;
        if next.count < REFUSAL_STOP_AFTER {
            return Ok(None);
        }
        let why = format!(
            "{} consecutive rounds refused for the same reason ({key})",
            next.count
        );
        self.stop_rounds(&why, now)?;
        Ok(Some(format!("ROUNDS STOPPED {repo}: {why} — look before lifting: world.sh unpause --repo {repo} --lanes rounds")))
    }
    pub fn stop_rounds(&self, why: &str, now: u64) -> Result<(), String> {
        let until = format!(
            "the fault is fixed; then world.sh unpause --repo {} --lanes rounds",
            self.repo
        );
        scoped_stop::pause_repo(
            &self.run,
            &self.repo,
            &["rounds".to_string()],
            PACE_OWNER,
            why,
            &until,
            now,
        )
        .map(|_| ())
    }
    fn abandon_refused_path(&self) -> PathBuf {
        self.dir.join("abandon-refused")
    }
    /// Remembers a round whose abandon the queue refused: batch, cause, then the reason.
    pub fn record_abandon_refused(&self, batch: &str, cause: &str, why: &str) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let why = why.split_whitespace().collect::<Vec<_>>().join(" ");
        fs::write(self.abandon_refused_path(), format!("{batch}\n{cause}\n{why}\n"))
            .map_err(|e| format!("rounds-pace abandon-refused: {e}"))
    }
    /// The round (batch, cause, reason) whose abandon was refused and has not since succeeded.
    pub fn abandon_refused(&self) -> Option<(String, String, String)> {
        let text = fs::read_to_string(self.abandon_refused_path()).ok()?;
        let mut lines = text.lines();
        let batch = lines.next().filter(|b| !b.is_empty())?.to_string();
        let cause = lines.next().unwrap_or("").to_string();
        Some((batch, cause, lines.collect::<Vec<_>>().join(" ")))
    }
    pub fn clear_abandon_refused(&self) {
        let _ = fs::remove_file(self.abandon_refused_path());
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
    pub redrive_after: u64,
    pub pause_lift_after: u64,
}

fn page(sm: &Seams, text: &str) -> Result<(), String> {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sm.inbox)
        .map_err(|e| format!("{}: {e}", sm.inbox.display()))?;
    writeln!(f, "{} [watch:rounds] {text}", (sm.stamp)(sm.now))
        .map_err(|e| format!("{}: {e}", sm.inbox.display()))
}

/// Pages the Concierge once per pause that has outlived `bound` seconds while the repo has work
/// waiting; a lifted pause forgets the page so the next one pages afresh. A `hold:` pause is a
/// person's decision and is never paged.
pub fn page_stale_pause(
    files: &Files,
    sm: &Seams,
    bound: u64,
    waiting: impl FnOnce() -> bool,
) -> Result<(), String> {
    let marker = files.dir.join("pause-paged");
    let Some(since) = files.pause_since() else {
        let _ = fs::remove_file(&marker);
        return Ok(());
    };
    let age = sm.now.saturating_sub(since);
    if age <= bound
        || fs::read_to_string(&marker).ok().as_deref() == Some(since.to_string().as_str())
        || !waiting()
    {
        return Ok(());
    }
    let text = fs::read_to_string(files.pause_file()).unwrap_or_default();
    let reason = match text.trim() {
        t if t.is_empty() || t.starts_with("red:") => "a red round",
        t => t,
    };
    page(sm, &format!("PAUSE UNATTENDED {}: {reason}, held {age}s with work waiting — attribute and eject, then rm run/rounds-pace/{}/pause", sm.repo, sm.repo))?;
    fs::write(&marker, since.to_string()).map_err(|e| format!("{}: {e}", marker.display()))
}

/// One repo's paced cut. Returns the cut's own result so a failure still fails the pass.
pub fn paced(
    files: &Files,
    sm: &Seams,
    open_round: impl FnOnce() -> Result<Option<String>, String>,
    driven: impl FnOnce(&str) -> bool,
    verdict: impl FnOnce() -> Option<(String, u64)>,
    orphans: &Orphans,
    cut: impl FnOnce() -> Result<(), CutError>,
) -> Result<(), CutError> {
    let mut st = files.load();
    if let Some(since) = files.pause_since() {
        if pause_expired(&st, sm.now.saturating_sub(since), sm.pause_lift_after) {
            fs::remove_file(files.pause_file())
                .map_err(|e| format!("{}: {e}", files.pause_file().display()))?;
            st.pause_lifts += 1;
            files.save(&st)?;
            println!(
                "batcher rounds: {}: pause held {}s past its {}s bound — lifted, cutting",
                sm.repo,
                sm.now.saturating_sub(since),
                sm.pause_lift_after
            );
            page(
                sm,
                &format!(
                    "PAUSE LIFTED {} after {}s — cutting again; a second red stays paused",
                    sm.repo,
                    sm.now.saturating_sub(since)
                ),
            )?;
        }
    }
    if let Some(h) = hold(files.pause_file().exists(), files.stopped()) {
        println!("batcher rounds: {}: held ({h:?}) — no cut", sm.repo);
        return Ok(());
    }
    if let Some(id) = open_round()? {
        if driven(&id) {
            println!(
                "batcher rounds: {}: round {id} is open and being certified — not re-cut",
                sm.repo
            );
            if !st.orphan.is_empty() {
                files.save(&State {
                    orphan: String::new(),
                    orphan_since: 0,
                    ..st
                })?;
            }
            return Ok(());
        }
        let wait = if st.redriven != id && (orphans.opened)(&id) {
            0
        } else {
            sm.redrive_after
        };
        let (mut next, step) = orphan_step(&st, &id, sm.now, wait, (orphans.fit)(&id));
        match step {
            Orphan::Wait => println!(
                "batcher rounds: {}: round {id} is open with no driver — not re-cut",
                sm.repo
            ),
            Orphan::Redrive => match (orphans.redrive)(&id) {
                Ok(()) => println!(
                    "batcher rounds: {}: round {id} had no driver; re-driving it once",
                    sm.repo
                ),
                Err(e) => println!(
                    "batcher rounds: {}: round {id} had no driver; the re-drive did not start: {e}",
                    sm.repo
                ),
            },
            Orphan::Incident(why) => {
                if next.paged_open != id {
                    match (orphans.incident)(&id, &why) {
                        Ok(inc) => {
                            println!("batcher rounds: {}: round {id} is stranded ({why}) — filed {inc}", sm.repo);
                            next.paged_open = id.clone();
                            page(sm, &format!("ROUND OPEN {} {id} with no driver — finish, eject or abandon it ({why}; incident {inc})", sm.repo))?;
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
    let detail = res
        .as_ref()
        .err()
        .map(|e| e.msg.clone())
        .unwrap_or_default();
    let (next, fx) = advance(&st, &outcome, sm.repo, &detail);
    st = next;
    if fx.pause {
        files.stamp_pause(sm.now)?;
    }
    if fx.stop {
        files.stop_rounds(
            &format!(
                "{} consecutive rounds produced no verdicts ({detail})",
                st.infra
            ),
            sm.now,
        )?;
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
    fn run(
        d: &Path,
        repo: &str,
        now: u64,
        open: Option<&str>,
        verdict: Option<(&str, u64)>,
        res: Result<(), CutError>,
    ) -> bool {
        let inbox = d.join("inbox.log");
        let sm = Seams {
            repo,
            now,
            inbox: &inbox,
            stamp: &stamp,
            redrive_after: WAIT,
            pause_lift_after: LIFT,
        };
        let cut_ran = Cell::new(false);
        let none = Orphans {
            fit: &|_| Ok(()),
            redrive: &|_| Ok(()),
            incident: &|_, _| Ok(String::new()),
            opened: &|_| false,
        };
        let r = paced(
            &Files::new(d, repo),
            &sm,
            || Ok(open.map(String::from)),
            |_| false,
            || verdict.map(|(s, a)| (s.to_string(), a)),
            &none,
            || {
                cut_ran.set(true);
                res
            },
        );
        let _ = r;
        cut_ran.get()
    }
    fn inbox(d: &Path) -> String {
        fs::read_to_string(d.join("inbox.log")).unwrap_or_default()
    }

    fn stale_page(d: &Path, repo: &str, now: u64, work: bool) -> String {
        let log = d.join("inbox.log");
        let sm = Seams {
            repo,
            now,
            inbox: &log,
            stamp: &stamp,
            redrive_after: WAIT,
            pause_lift_after: LIFT,
        };
        page_stale_pause(&Files::new(d, repo), &sm, 3600, || work).unwrap();
        inbox(d)
    }

    #[test]
    fn a_pause_older_than_a_round_with_work_waiting_pages_once_naming_repo_reason_and_age() {
        let t = dir("stale-page");
        let d = t.path();
        let files = Files::new(d, "pokebot");
        fs::create_dir_all(d.join("rounds-pace/pokebot")).unwrap();
        fs::write(
            files.pause_file(),
            "queue round certify judged the round red\n",
        )
        .unwrap();
        let since = files.pause_since().unwrap();
        assert_eq!(
            stale_page(d, "pokebot", since + 3600, true),
            "",
            "a pause no older than the bound is a round in flight"
        );
        assert_eq!(
            stale_page(d, "pokebot", since + 7200, false),
            "",
            "no work waiting, nothing is lost"
        );
        let out = stale_page(d, "pokebot", since + 7200, true);
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(
            out.contains("PAUSE UNATTENDED pokebot")
                && out.contains("certify judged the round red")
                && out.contains("7200s"),
            "{out}"
        );
        assert_eq!(
            stale_page(d, "pokebot", since + 9000, true).lines().count(),
            1,
            "the same pause is paged once"
        );
        fs::remove_file(files.pause_file()).unwrap();
        stale_page(d, "pokebot", since + 9500, true);
        fs::write(files.pause_file(), "red:1\n").unwrap();
        assert_eq!(
            stale_page(d, "pokebot", 1 + 7200, true).lines().count(),
            2,
            "a new pause pages afresh"
        );
    }

    #[test]
    fn an_operator_hold_is_never_paged_as_unattended() {
        let t = dir("stale-hold");
        let d = t.path();
        fs::create_dir_all(d.join("rounds-pace/a")).unwrap();
        fs::write(Files::new(d, "a").pause_file(), "hold: release freeze\n").unwrap();
        assert_eq!(stale_page(d, "a", u64::MAX / 2, true), "");
    }

    #[test]
    fn a_round_with_no_vm_never_pauses_and_the_third_in_a_row_stops_and_pages() {
        let mut st = State::default();
        for i in 0..5u32 {
            let (next, fx) = advance(&st, &Outcome::NoVm, "r", "acquire failed");
            let brake = i + 1 >= NO_VM_STOP_AFTER;
            assert_eq!(
                (fx.pause, fx.stop, fx.pages.len(), next.infra),
                (false, brake, usize::from(brake), 0)
            );
            st = next;
        }
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
        let page = f
            .record_refusal("a", "other", 1040)
            .unwrap()
            .expect("the second like refusal alarms");
        assert!(page.contains("ROUNDS STOPPED a") && f.stopped());
        let g = Files::new(t.path(), "b");
        g.record_refusal("b", "k", 1000).unwrap();
        assert_eq!(
            g.record_refusal("b", "k", 1000 + REFUSAL_WINDOW_SECS + 1)
                .unwrap(),
            None,
            "outside the window it is a first"
        );
    }

    #[test]
    fn a_not_landed_round_pauses_and_pages_and_the_pause_holds_the_next_cut() {
        let t = dir("red");
        let d = t.path();
        let ran = run(d, "a", 1000, None, Some(("red", 1000)), Ok(()));
        assert!(ran);
        assert!(Files::new(d, "a").pause_file().exists());
        assert!(inbox(d).contains("T1000 [watch:rounds] ROUND RED a"));
        let ran = run(d, "a", 1000 + LIFT - 1, None, None, Ok(()));
        assert!(!ran, "paused repo must not cut");
        fs::remove_file(Files::new(d, "a").pause_file()).unwrap();
        let ran = run(d, "a", 3000, None, None, Ok(()));
        assert!(ran, "rm of the pause file lifts it");
    }

    #[test]
    fn a_pause_held_past_its_bound_is_lifted_and_the_repo_cut_without_a_person() {
        let t = dir("lift");
        let d = t.path();
        assert!(run(d, "a", 1000, None, Some(("red", 1000)), Ok(())));
        assert!(
            !run(d, "a", 1000 + LIFT - 1, None, None, Ok(())),
            "inside the bound the pause holds"
        );
        assert!(
            run(d, "a", 1000 + LIFT, None, None, Ok(())),
            "at the bound the owner lifts it and cuts"
        );
        assert!(!Files::new(d, "a").pause_file().exists());
        assert!(inbox(d).contains("PAUSE LIFTED a"));
    }

    #[test]
    fn a_round_red_again_after_the_auto_lift_stays_paused_for_a_person() {
        let t = dir("lift-twice");
        let d = t.path();
        run(d, "a", 1000, None, Some(("red", 1000)), Ok(()));
        run(
            d,
            "a",
            1000 + LIFT,
            None,
            Some(("red", 1000 + LIFT)),
            Ok(()),
        );
        assert!(Files::new(d, "a").pause_file().exists());
        assert!(inbox(d).contains("lifted once and red again"));
        assert!(
            !run(d, "a", 1000 + 10 * LIFT, None, None, Ok(())),
            "a second red is not lifted by the clock"
        );
        assert!(Files::new(d, "a").pause_file().exists());
    }

    #[test]
    fn a_quiet_cut_after_the_auto_lift_restores_the_next_lift() {
        let t = dir("lift-reset");
        let d = t.path();
        run(d, "a", 1000, None, Some(("red", 1000)), Ok(()));
        run(d, "a", 1000 + LIFT, None, None, Ok(()));
        run(d, "a", 5000, None, Some(("red", 5000)), Ok(()));
        assert!(
            run(d, "a", 5000 + LIFT, None, None, Ok(())),
            "a green cycle between reds earns a fresh lift"
        );
    }

    #[test]
    fn an_operator_hold_is_never_lifted_by_the_clock() {
        let t = dir("lift-hold");
        let d = t.path();
        fs::create_dir_all(d.join("rounds-pace/a")).unwrap();
        fs::write(Files::new(d, "a").pause_file(), "hold: release freeze\n").unwrap();
        assert!(!run(d, "a", u64::MAX / 2, None, None, Ok(())));
        assert!(Files::new(d, "a").pause_file().exists());
    }

    #[test]
    fn a_hand_made_pause_file_with_no_stamp_ages_from_its_mtime() {
        let t = dir("lift-hand");
        let d = t.path();
        fs::create_dir_all(d.join("rounds-pace/a")).unwrap();
        fs::write(Files::new(d, "a").pause_file(), "").unwrap();
        assert!(
            !run(d, "a", 1, None, None, Ok(())),
            "an unstamped pause is not lifted at once"
        );
    }

    #[test]
    fn a_red_local_verdict_written_in_the_pass_is_a_red_round_but_a_stale_one_is_not() {
        let t = dir("verdict");
        let d = t.path();
        run(d, "a", 1000, None, Some(("red", 999)), Ok(()));
        assert!(
            !Files::new(d, "a").pause_file().exists(),
            "a verdict from before the pass is not this pass's"
        );
        run(d, "a", 1000, None, Some(("red", 1005)), Ok(()));
        assert!(Files::new(d, "a").pause_file().exists());
    }

    #[test]
    fn a_round_that_ejected_every_member_is_recut_at_once_not_paused() {
        let t = dir("recut");
        let d = t.path();
        assert!(run(
            d,
            "a",
            1000,
            None,
            Some((crate::io::RECUT_VERDICT, 1005)),
            Ok(())
        ));
        assert!(!Files::new(d, "a").pause_file().exists());
        assert_eq!(inbox(d), "");
        assert!(
            run(d, "a", 1010, None, None, Ok(())),
            "the next pass cuts again"
        );
    }

    #[test]
    fn a_cut_that_admitted_nothing_never_pauses_whatever_its_wording() {
        let t = dir("empty");
        let d = t.path();
        for (i, msg) in ["nothing admissible", "pool empty", "queue is empty"]
            .iter()
            .enumerate()
        {
            run(
                d,
                "a",
                1000 + i as u64 * 100,
                None,
                None,
                Err(CutError::from(*msg)),
            );
            run(d, "a", 1050 + i as u64 * 100, None, None, Ok(()));
        }
        assert!(!Files::new(d, "a").pause_file().exists());
        assert!(!Files::new(d, "a").stopped());
        assert_eq!(inbox(d), "");
    }

    #[test]
    fn a_round_with_no_verdicts_is_cut_again_at_once_and_the_second_in_a_row_stops_and_pages() {
        let t = dir("infra");
        let d = t.path();
        let infra = || Err(CutError::new(CutKind::Infra, "round left no verdicts"));
        run(d, "a", 1000, None, None, infra());
        assert_eq!(inbox(d), "");
        let ran = run(d, "a", 1001, None, None, infra());
        assert!(ran, "no wait after a harness fault");
        assert!(Files::new(d, "a").stopped());
        assert!(inbox(d).contains("ROUNDS STOPPED a: 2 consecutive"));
        let ran = run(d, "a", 9_000_000, None, None, Ok(()));
        assert!(
            !ran,
            "stopped stays stopped until the operator lifts the scoped stop"
        );
    }

    #[test]
    fn a_clean_round_resets_the_infra_streak() {
        let t = dir("reset");
        let d = t.path();
        run(
            d,
            "a",
            1000,
            None,
            None,
            Err(CutError::new(CutKind::Infra, "over cap")),
        );
        run(d, "a", 2000, None, None, Ok(()));
        assert_eq!(Files::new(d, "a").load().infra, 0);
        run(
            d,
            "a",
            3000,
            None,
            None,
            Err(CutError::new(CutKind::Infra, "over cap")),
        );
        assert!(!Files::new(d, "a").stopped());
    }

    #[test]
    fn an_open_failed_outcome_pages_every_time_and_does_not_pause() {
        let (st, mut pages) = (State::default(), 0);
        for _ in [1000, 1100] {
            let (next, fx) = advance(&st, &Outcome::OpenFailed, "a", "x");
            assert!(!fx.pause && !fx.stop && next == st);
            pages += fx
                .pages
                .iter()
                .filter(|p| p.starts_with("OPEN FAILED a"))
                .count();
        }
        assert_eq!(pages, 2);
    }

    const WAIT: u64 = 300;
    const LIFT: u64 = 700;

    type Calls = std::cell::RefCell<Vec<String>>;

    fn orphan_pass(
        d: &Path,
        now: u64,
        id: &str,
        driven: bool,
        fit: Result<(), String>,
        calls: &Calls,
    ) -> bool {
        orphan_pass_in(d, now, id, driven, fit, calls, false)
    }

    fn orphan_pass_in(
        d: &Path,
        now: u64,
        id: &str,
        driven: bool,
        fit: Result<(), String>,
        calls: &Calls,
        opened: bool,
    ) -> bool {
        let inbox_p = d.join("inbox.log");
        let sm = Seams {
            repo: "a",
            now,
            inbox: &inbox_p,
            stamp: &stamp,
            redrive_after: WAIT,
            pause_lift_after: LIFT,
        };
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
        let opened_f = |_: &str| opened;
        let o = Orphans {
            fit: &fit_f,
            redrive: &redrive_f,
            incident: &incident_f,
            opened: &opened_f,
        };
        let cut = Cell::new(false);
        paced(
            &Files::new(d, "a"),
            &sm,
            || Ok(Some(id.into())),
            |_| driven,
            || None,
            &o,
            || {
                cut.set(true);
                Ok(())
            },
        )
        .unwrap();
        cut.get()
    }

    #[test]
    fn an_open_round_with_no_driver_is_redriven_once_after_the_wait_and_never_re_cut() {
        let t = dir("orphan");
        let (d, calls) = (t.path(), Calls::default());
        assert!(!orphan_pass(d, 1000, "b-1", false, Ok(()), &calls));
        assert!(!orphan_pass(
            d,
            1000 + WAIT - 1,
            "b-1",
            false,
            Ok(()),
            &calls
        ));
        assert!(
            calls.borrow().is_empty(),
            "not before the wait: {:?}",
            calls.borrow()
        );
        assert!(!orphan_pass(d, 1000 + WAIT, "b-1", false, Ok(()), &calls));
        assert_eq!(*calls.borrow(), vec!["redrive b-1".to_string()]);
    }

    #[test]
    fn a_round_still_in_phase_opened_with_no_driver_gets_its_certify_on_the_first_tick_and_only_once(
    ) {
        let t = dir("orphan-opened");
        let (d, calls) = (t.path(), Calls::default());
        assert!(!orphan_pass_in(d, 1000, "b-1", false, Ok(()), &calls, true));
        assert_eq!(
            *calls.borrow(),
            vec!["redrive b-1".to_string()],
            "no wait for a round nobody has driven"
        );
        orphan_pass_in(d, 1001, "b-1", false, Ok(()), &calls, true);
        assert_eq!(
            calls.borrow().len(),
            1,
            "a certify that has not yet moved the phase is not an incident: {:?}",
            calls.borrow()
        );
    }

    #[test]
    fn a_redriven_round_that_is_undriven_again_files_one_incident_and_no_second_redrive() {
        let t = dir("orphan-twice");
        let (d, calls) = (t.path(), Calls::default());
        let n = WAIT;
        orphan_pass(d, 1000, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + n, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + n + 1, "b-1", false, Ok(()), &calls);
        assert_eq!(
            calls.borrow().len(),
            1,
            "the re-driven round gets its own wait: {:?}",
            calls.borrow()
        );
        orphan_pass(d, 1000 + 2 * n, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + 3 * n, "b-1", false, Ok(()), &calls);
        let c = calls.borrow();
        assert_eq!(
            c.iter().filter(|x| x.starts_with("redrive")).count(),
            1,
            "{c:?}"
        );
        assert_eq!(
            c.iter().filter(|x| x.starts_with("incident b-1")).count(),
            1,
            "{c:?}"
        );
    }

    #[test]
    fn a_round_whose_worktree_moved_on_is_not_redriven_and_files_an_incident_naming_why() {
        let t = dir("orphan-moved");
        let (d, calls) = (t.path(), Calls::default());
        orphan_pass(
            d,
            1000,
            "b-1",
            false,
            Err("the worktree is at d7aa8f08, not 07da4601".into()),
            &calls,
        );
        orphan_pass(
            d,
            1000 + WAIT,
            "b-1",
            false,
            Err("the worktree is at d7aa8f08, not 07da4601".into()),
            &calls,
        );
        let c = calls.borrow();
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(
            c[0].starts_with("incident b-1") && c[0].contains("d7aa8f08"),
            "{c:?}"
        );
    }

    #[test]
    fn a_stranded_round_is_filed_and_paged_once_however_many_passes_follow() {
        let t = dir("orphan-once");
        let (d, calls) = (t.path(), Calls::default());
        let moved = || Err("the worktree is at d7aa8f08, not 07da4601".to_string());
        for k in 0..6 {
            orphan_pass(d, 1000 + k * WAIT, "b-1", false, moved(), &calls);
        }
        let c = calls.borrow();
        assert_eq!(
            c.iter().filter(|x| x.starts_with("incident")).count(),
            1,
            "{c:?}"
        );
        assert_eq!(inbox(d).matches("ROUND OPEN a b-1").count(), 1);
    }

    #[test]
    fn a_driver_seen_resets_the_orphan_wait() {
        let t = dir("orphan-driven");
        let (d, calls) = (t.path(), Calls::default());
        orphan_pass(d, 1000, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + WAIT - 10, "b-1", true, Ok(()), &calls);
        orphan_pass(d, 1000 + WAIT, "b-1", false, Ok(()), &calls);
        assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
    }

    #[test]
    fn an_open_round_a_certify_is_driving_is_never_acted_on() {
        let t = dir("driven");
        let (d, calls) = (t.path(), Calls::default());
        for k in 0..5 {
            assert!(!orphan_pass(
                d,
                1000 + k * WAIT,
                "b-1",
                true,
                Ok(()),
                &calls
            ));
        }
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn one_repos_red_does_not_hold_another() {
        let t = dir("repos");
        let d = t.path();
        run(d, "a", 1000, None, Some(("red", 1000)), Ok(()));
        let ran = run(d, "b", 1010, None, None, Ok(()));
        assert!(ran);
        assert!(!Files::new(d, "b").pause_file().exists());
    }

    #[test]
    fn a_stale_round_image_never_counts_toward_the_stop() {
        let t = dir("stale-ami");
        let d = t.path();
        let stale = "certify rc=75: round AMI is stale: acquire failed: clone refused";
        let err = || CutError::new(CutKind::StaleImage, stale);
        assert_eq!(classify(&Err(err()), None, 0), Outcome::StaleImage);
        for at in [1000, 2000, 3000] {
            run(d, "a", at, None, None, Err(err()));
        }
        assert!(!Files::new(d, "a").stopped());
        assert_eq!(Files::new(d, "a").load().infra, 0);
    }

    #[test]
    fn a_stale_image_refusal_is_recognised_and_a_plain_one_is_not() {
        assert!(is_stale_image(
            "cannot certify the round: round AMI is stale: acquire failed"
        ));
        assert!(!is_stale_image(
            "cannot certify the round: acquire failed: pool exhausted"
        ));
    }

    #[test]
    fn a_certify_that_always_exits_4_gets_two_attempts_and_one_alert() {
        let t = dir("exit4");
        let d = t.path();
        let env = || {
            Err(CutError::new(
                CutKind::Infra,
                "refused — cannot certify the round: no VM",
            ))
        };
        let mut attempts = 0;
        for i in 0..10u64 {
            if run(d, "a", 1000 + i, None, None, env()) {
                attempts += 1;
            }
        }
        assert_eq!(attempts, 2);
        assert_eq!(inbox(d).matches("ROUNDS STOPPED a").count(), 1);
    }

    #[test]
    fn classify_reads_the_kind_and_never_the_words() {
        let words = "NOT LANDED OPEN FAILED OVER-CAP certify rc=75 round AMI is stale";
        let of = |k| classify(&Err(CutError::new(k, words)), None, 0);
        assert_eq!(of(CutKind::Infra), Outcome::Infra);
        assert_eq!(of(CutKind::StaleImage), Outcome::StaleImage);
        assert_eq!(of(CutKind::Other), Outcome::Quiet);
        assert_eq!(
            classify(&Err(CutError::from(words)), Some(("red", 9)), 0),
            Outcome::Quiet,
            "a failed cut is judged by its kind, not the verdict file"
        );
    }

    #[test]
    fn classify_an_ok_cut_is_red_only_on_a_red_verdict_written_this_pass() {
        assert_eq!(classify(&Ok(()), Some(("red", 5)), 5), Outcome::Red);
        assert_eq!(classify(&Ok(()), Some(("red", 4)), 5), Outcome::Quiet);
        assert_eq!(classify(&Ok(()), Some(("green", 9)), 5), Outcome::Quiet);
        assert_eq!(classify(&Ok(()), None, 5), Outcome::Quiet);
    }
}
