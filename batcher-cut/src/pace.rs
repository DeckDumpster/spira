//! The per-repo pacing a round scheduler owes its operator: hold on a red, back off on a
//! harness fault, page an orphaned open round. State lives in files under
//! `<run>/rounds-pace/<repo>/` so one repo's hold never touches another's; a person sets and
//! lifts one with `batcher pause-set` and `batcher pause-lift`, which record who did.

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
pub const OPEN_FAILED_PAGE_AFTER: u32 = 3;

#[derive(Default, Clone, PartialEq, Debug)]
pub struct State {
    pub infra: u32,
    pub no_vm: u32,
    pub paged_open: String,
    pub orphan: String,
    pub orphan_since: u64,
    pub redriven: String,
    pub pause_lifts: u32,
    pub open_failed: u32,
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
    /// The base alone reproduces the red: no member is at fault.
    BaseRed { base: String },
    Infra,
    /// The round's attribution reruns faulted: the round stays open and a person is told.
    AttributionFaulted,
    /// A green round's landing was refused: the round stays open and a person is told.
    LandRefused,
    NoVm,
    StaleImage,
    OpenFailed,
}

#[derive(PartialEq, Debug)]
pub enum Hold {
    Paused,
    CutHeld,
    Stopped,
}

#[derive(Default, PartialEq, Debug)]
pub struct Effects {
    pub pause: bool,
    /// The sha a `base:` pause is set for, in place of a clock-stamped `red:` one.
    pub base_pause: Option<String>,
    pub stop: bool,
    pub pages: Vec<String>,
}

pub const BASE_RED_WORD: &str = "base-red";

/// The local verdict a base-red round writes: `base-red:<sha>:<why>`.
pub fn base_red_verdict(base: &str, why: &str) -> String {
    format!("{BASE_RED_WORD}:{base}:{}", one_line(why))
}

fn base_red_parts(word: &str) -> Option<(&str, &str)> {
    word.strip_prefix(BASE_RED_WORD)?.strip_prefix(':')?.split_once(':')
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
        None => match verdict {
            Some((w, at)) if at >= pass_start => match base_red_parts(w) {
                Some((base, _)) => Outcome::BaseRed { base: base.to_string() },
                None => Outcome::Quiet,
            },
            _ => Outcome::Quiet,
        },
    }
}

/// Whether a pause that has stood `held_for` seconds is the owner's to lift: past its bound, and
/// not already lifted once without a green cut in between.
pub fn pause_expired(st: &State, held_for: u64, lift_after: u64) -> bool {
    held_for >= lift_after && st.pause_lifts < PAUSE_LIFTS_MAX
}

pub fn hold(paused: bool, stopped: bool, cut_held: bool) -> Option<Hold> {
    if stopped {
        Some(Hold::Stopped)
    } else if cut_held {
        Some(Hold::CutHeld)
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
                "ROUND RED {repo}{again} — attribute and eject, then batcher pause-lift --repo {repo} --by concierge --reason \"<why>\" ({detail})"
            ));
        }
        Outcome::BaseRed { base } => {
            next.infra = 0;
            next.no_vm = 0;
            fx.base_pause = Some(base.clone());
            fx.pages.push(format!(
                "BASE RED {repo} at {base} — {detail}; no member is at fault: revert or fix the change named on main (law-a-landing-stop-is-reverted-first); rounds resume by themselves when the landing ref moves"
            ));
        }
        Outcome::StaleImage => {}
        Outcome::AttributionFaulted => {
            fx.pages.push(format!("ATTRIBUTION FAULTED {repo} — the round is left open, no member charged ({detail})"));
        }
        Outcome::LandRefused => {
            fx.pages.push(format!("LANDING REFUSED {repo} — {detail}"));
        }
        Outcome::NoVm => {
            next.no_vm += 1;
            if next.no_vm >= NO_VM_STOP_AFTER {
                fx.stop = true;
                fx.pages.push(format!("ROUNDS STOPPED {repo}: {} consecutive rounds could not run — no round VM, look before lifting the stop ({detail})", next.no_vm));
            }
        }
        Outcome::OpenFailed => {
            next.open_failed += 1;
            if next.open_failed == OPEN_FAILED_PAGE_AFTER {
                fx.pages.push(format!(
                    "OPEN FAILED {repo} — the cut could not open a round: {} opens in a row failed; last: {detail}",
                    next.open_failed
                ));
            }
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

/// A round opened: the run of failed opens is over.
pub fn opened(st: &State) -> State {
    State {
        open_failed: 0,
        ..st.clone()
    }
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
    /// A `hold:` pause is a person's decision and has no age the batcher acts on.
    pub fn pause_since(&self) -> Option<u64> {
        if fs::read_to_string(self.pause_file()).unwrap_or_default().starts_with("hold:") {
            return None;
        }
        self.pause_set_at()
    }
    pub fn pause_text(&self) -> Option<String> {
        fs::read_to_string(self.pause_file()).ok()
    }
    pub fn pause_set_at(&self) -> Option<u64> {
        let p = self.pause_file();
        let meta = fs::metadata(&p).ok()?;
        let text = fs::read_to_string(&p).unwrap_or_default();
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
    fn record_pause_event(&self, line: String, kind: &str, by: &str, fields: Vec<(&str, String)>, now: u64) {
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.run.join("landing.log"))
        {
            let _ = writeln!(f, "{line}");
        }
        spira_otel::rounds::emit(
            &self.run,
            "batcher",
            kind,
            &self.repo,
            "",
            by,
            &fields,
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(now),
        );
    }
    /// Writes the pause file and records who set it and why.
    pub fn set_pause(&self, text: &str, by: &str, reason: &str, now: u64) -> Result<(), String> {
        let p = self.pause_file();
        fs::create_dir_all(&self.dir)
            .and_then(|_| fs::write(&p, text))
            .map_err(|e| format!("{}: {e}", p.display()))?;
        let kind = if text.starts_with("hold:") {
            "hold"
        } else if text.starts_with("base:") {
            "base"
        } else {
            "red"
        };
        let reason = one_line(reason);
        self.record_pause_event(
            format!("QUEUE ROUND-PAUSE {now} repo={} by={by} kind={kind} reason={reason}", self.repo),
            "pause",
            by,
            vec![("pause_kind", kind.to_string()), ("reason", reason)],
            now,
        );
        Ok(())
    }
    /// Removes the pause file, a `hold:` too, and records who lifted it, why and how long it stood.
    /// `None`, and nothing written, when there is no pause.
    pub fn lift_pause(&self, by: &str, reason: &str, now: u64) -> Result<Option<u64>, String> {
        let Some(since) = self.pause_set_at() else {
            return Ok(None);
        };
        let p = self.pause_file();
        fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let held = now.saturating_sub(since);
        let reason = one_line(reason);
        self.record_pause_event(
            format!("QUEUE ROUND-PAUSE-LIFT {now} repo={} by={by} held_s={held} reason={reason}", self.repo),
            "pause-lift",
            by,
            vec![("held_s", held.to_string()), ("pause_since", since.to_string()), ("reason", reason)],
            now,
        );
        Ok(Some(held))
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
            open_failed: kv
                .get("open_failed")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }
    pub fn save(&self, st: &State) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let body = format!(
            "infra={}\nno_vm={}\npaged_open={}\norphan={}\norphan_since={}\nredriven={}\npause_lifts={}\nopen_failed={}\n",
            st.infra, st.no_vm, st.paged_open, st.orphan, st.orphan_since, st.redriven, st.pause_lifts, st.open_failed
        );
        fs::write(self.dir.join("state"), body).map_err(|e| format!("rounds-pace state: {e}"))
    }
    fn stamp_pause(&self, now: u64) -> Result<(), String> {
        self.set_pause(&format!("red:{now}\n"), PACE_OWNER, "a red round", now)
    }
}

pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
    pub inbox: &'a inbox_rules::Sink,
    pub stamp: &'a dyn Fn(u64) -> String,
    pub redrive_after: u64,
    pub pause_lift_after: u64,
    /// The landing ref's sha now; a `base:` pause set at another sha is lifted.
    pub landing_head: Option<String>,
}

fn page(sm: &Seams, text: &str) -> Result<(), String> {
    sm.inbox.record(&format!("{} [watch:rounds] {text}", (sm.stamp)(sm.now))).map(|_| ())
}

/// A round refused the same way again is paged once. `inbox` is absent when it is not configured.
pub fn stop_on_repeat(run: &Path, inbox: Option<&inbox_rules::Sink>, repo: &str, batch: &str, msg: &str, now: u64, stamp: &str) {
    if is_stale_image(msg) {
        return;
    }
    let files = Files::new(run, repo);
    match files.record_refusal(repo, &refusal_key(msg, batch), now) {
        Ok(Some(page)) => {
            println!("batcher {repo}: {page}");
            if let Some(inbox) = inbox {
                if let Err(e) = inbox.record(&format!("{stamp} [watch:rounds] {page}")) {
                    eprintln!("batcher {repo}: the concierge inbox: {e}");
                }
            }
        }
        Ok(None) => {}
        Err(e) => println!("batcher {repo}: could not record the refusal: {e}"),
    }
}

/// A held forge repo with CERTIFIED work says so in the Concierge inbox, once per change of the
/// waiting set, so a hold nobody remembers cannot sit on landable work in silence.
pub fn page_held(run: &Path, inbox: Option<&inbox_rules::Sink>, repo: &str, waiting: &[&str], stamp: &str) {
    let marker = run.join("rounds-held").join(repo);
    if waiting.is_empty() {
        let _ = fs::remove_file(&marker);
        return;
    }
    let ids = waiting.join(",");
    if fs::read_to_string(&marker).ok().as_deref() == Some(ids.as_str()) {
        return;
    }
    let Some(inbox) = inbox else {
        return;
    };
    let wrote = inbox.record(&format!("{stamp} [watch:rounds] BATCHES HELD {repo}: SPIRA_BATCHER_FORGE_ENABLE=0 with CERTIFIED work waiting: {ids}"));
    if wrote.is_ok() && marker.parent().is_some_and(|d| fs::create_dir_all(d).is_ok()) {
        let _ = fs::write(&marker, &ids);
    }
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
    page(sm, &format!("PAUSE UNATTENDED {}: {reason}, held {age}s with work waiting — attribute and eject, then batcher pause-lift --repo {} --by concierge --reason \"<why>\"", sm.repo, sm.repo))?;
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
    let pause_text = fs::read_to_string(files.pause_file()).unwrap_or_default();
    let base_pause = pause_text.trim().strip_prefix("base:");
    if let (Some(at), Some(head)) = (base_pause, sm.landing_head.as_deref()) {
        if at != head {
            files.lift_pause(PACE_OWNER, &format!("landing ref moved {at} → {head}"), sm.now)?;
            println!("batcher rounds: {}: landing ref moved {at} → {head} — base-red pause lifted, cutting", sm.repo);
        }
    }
    if let (Some(since), None) = (files.pause_since(), base_pause) {
        if pause_expired(&st, sm.now.saturating_sub(since), sm.pause_lift_after) {
            files.lift_pause(
                PACE_OWNER,
                &format!("held past its {}s bound", sm.pause_lift_after),
                sm.now,
            )?;
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
    if let Some(h) = hold(files.pause_file().exists(), files.stopped(), false) {
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
    let detail = match v.as_ref().and_then(|(w, _)| base_red_parts(w)) {
        Some((_, why)) if matches!(outcome, Outcome::BaseRed { .. }) => why.to_string(),
        _ => res.as_ref().err().map(|e| e.msg.clone()).unwrap_or_default(),
    };
    let (next, fx) = advance(&st, &outcome, sm.repo, &detail);
    st = next;
    if fx.pause {
        files.stamp_pause(sm.now)?;
    }
    if let Some(base) = &fx.base_pause {
        files.set_pause(&format!("base:{base}\n"), PACE_OWNER, &format!("base red at {base}"), sm.now)?;
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
        run_at(d, repo, now, open, verdict, res, None)
    }
    fn run_at(
        d: &Path,
        repo: &str,
        now: u64,
        open: Option<&str>,
        verdict: Option<(&str, u64)>,
        res: Result<(), CutError>,
        landing_head: Option<&str>,
    ) -> bool {
        let inbox = quiet_sink(d);
        let sm = Seams {
            repo,
            now,
            inbox: &inbox,
            stamp: &stamp,
            redrive_after: WAIT,
            pause_lift_after: LIFT,
            landing_head: landing_head.map(String::from),
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
    fn quiet_sink(d: &Path) -> inbox_rules::Sink {
        inbox_rules::Sink {
            inbox: d.join("inbox.log"),
            cursor: d.join("inbox.cursor"),
            bus: inbox_rules::Bus::Off,
            wake: String::new(),
            pattern: String::new(),
            name: "batcher".into(),
        }
    }
    fn inbox(d: &Path) -> String {
        fs::read_to_string(d.join("inbox.log")).unwrap_or_default()
    }

    fn stale_page(d: &Path, repo: &str, now: u64, work: bool) -> String {
        let log = quiet_sink(d);
        let sm = Seams {
            repo,
            now,
            inbox: &log,
            stamp: &stamp,
            redrive_after: WAIT,
            pause_lift_after: LIFT,
            landing_head: None,
        };
        page_stale_pause(&Files::new(d, repo), &sm, 3600, || work).unwrap();
        inbox(d)
    }

    #[test_id(td278tn)]
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

    #[test_id(tswyuad)]
    #[test]
    fn a_cut_hold_holds_and_a_stop_outranks_it() {
        assert_eq!(hold(false, false, true), Some(Hold::CutHeld));
        assert_eq!(hold(true, false, true), Some(Hold::CutHeld));
        assert_eq!(hold(false, true, true), Some(Hold::Stopped));
        assert_eq!(hold(false, false, false), None);
    }

    #[test_id(tma6rzh)]
    #[test]
    fn an_operator_hold_is_never_paged_as_unattended() {
        let t = dir("stale-hold");
        let d = t.path();
        fs::create_dir_all(d.join("rounds-pace/a")).unwrap();
        fs::write(Files::new(d, "a").pause_file(), "hold: release freeze\n").unwrap();
        assert_eq!(stale_page(d, "a", u64::MAX / 2, true), "");
    }

    #[test_id(t4xjgph)]
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

    #[test_id(tq5joa6)]
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

    #[test_id(t56f3dj)]
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

    #[test_id(tx4xsd0)]
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

    fn base_red_cut(d: &Path, now: u64, head: Option<&str>) -> bool {
        let word = base_red_verdict("abc", "unit-tests red; last change touching it: c0de fix");
        run_at(d, "a", now, None, Some((&word, now)), Ok(()), head)
    }

    #[test_id(t3hvh16)]
    #[test]
    fn a_base_red_pauses_at_the_sha_pages_once_and_leaves_the_lift_count() {
        let st = State { pause_lifts: 1, ..State::default() };
        let (next, fx) = advance(&st, &Outcome::BaseRed { base: "abc".into() }, "a", "why");
        assert_eq!(next.pause_lifts, 1);
        assert_eq!(fx.base_pause.as_deref(), Some("abc"));
        assert!(!fx.pause);
        assert_eq!(fx.pages.len(), 1);
        assert!(fx.pages[0].starts_with("BASE RED a at abc — why;"), "{:?}", fx.pages);
    }

    #[test_id(tw46pu4)]
    #[test]
    fn a_base_red_verdict_is_classified_by_its_word_with_the_sha() {
        let w = base_red_verdict("abc", "x\ny");
        assert_eq!(classify(&Ok(()), Some((&w, 5)), 5), Outcome::BaseRed { base: "abc".into() });
        assert_eq!(classify(&Ok(()), Some((&w, 4)), 5), Outcome::Quiet);
    }

    #[test_id(t99653y)]
    #[test]
    fn a_base_pause_holds_while_the_landing_ref_stays_and_lifts_when_it_moves() {
        let t = dir("base-pause");
        let d = t.path();
        assert!(base_red_cut(d, 1000, Some("abc")));
        let f = Files::new(d, "a");
        assert_eq!(fs::read_to_string(f.pause_file()).unwrap(), "base:abc\n");
        assert!(inbox(d).contains("BASE RED a at abc — unit-tests red"), "{}", inbox(d));
        assert!(landing_log(d).contains("by=batcher kind=base reason=base red at abc"));
        assert!(!run_at(d, "a", 1100, None, None, Ok(()), Some("abc")), "same head: held");
        assert!(run_at(d, "a", 1200, None, None, Ok(()), Some("def")), "moved head: lifts and cuts");
        assert!(!f.pause_file().exists());
        let log = landing_log(d);
        assert!(log.contains("by=batcher") && log.contains("reason=landing ref moved abc → def"), "{log}");
        assert_eq!(event(d, "kind").last(), Some(&Some("pause-lift".to_string())));
    }

    #[test_id(t9zfma5)]
    #[test]
    fn a_stale_base_pause_pages_with_its_text_as_the_reason() {
        let t = dir("base-stale");
        let d = t.path();
        let f = Files::new(d, "a");
        f.set_pause("base:abc\n", PACE_OWNER, "base red at abc", 10).unwrap();
        let since = f.pause_since().unwrap();
        assert!(stale_page(d, "a", since + 4000, true).contains("PAUSE UNATTENDED a: base:abc, held"));
    }

    #[test_id(tiss80d)]
    #[test]
    fn a_base_pause_is_never_lifted_by_the_clock() {
        let t = dir("base-pause-clock");
        let d = t.path();
        assert!(base_red_cut(d, 1000, Some("abc")));
        assert!(!run_at(d, "a", 1000 + LIFT * 3, None, None, Ok(()), Some("abc")));
        assert!(Files::new(d, "a").pause_file().exists());
    }

    fn event(d: &Path, key: &str) -> Vec<Option<String>> {
        spira_otel::logs::read_all(&spira_otel::dir_of(d), "batcher")
            .iter()
            .map(|r| {
                r.iter()
                    .find(|(n, _)| n == key)
                    .and_then(|(_, v)| v["stringValue"].as_str().map(String::from))
            })
            .collect()
    }
    fn landing_log(d: &Path) -> String {
        fs::read_to_string(d.join("landing.log")).unwrap_or_default()
    }

    #[test_id(thwp8xj)]
    #[test]
    fn a_person_lifting_a_red_pause_records_who_and_how_long_it_stood() {
        let t = dir("lift-by-hand");
        let d = t.path();
        let f = Files::new(d, "a");
        fs::create_dir_all(f.pause_file().parent().unwrap()).unwrap();
        fs::write(f.pause_file(), "red:1000\n").unwrap();
        let held = f.lift_pause("concierge", "eject done", 1090).unwrap();
        assert_eq!(held, Some(90));
        assert!(!f.pause_file().exists());
        let log = landing_log(d);
        assert_eq!(log.matches("QUEUE ROUND-PAUSE-LIFT").count(), 1, "{log}");
        assert!(log.contains("by=concierge held_s=90 reason=eject done"), "{log}");
        assert_eq!(event(d, "kind"), [Some("pause-lift".to_string())]);
        assert_eq!(event(d, "by"), [Some("concierge".to_string())]);
        assert_eq!(event(d, "held_s"), [Some("90".to_string())]);
    }

    #[test_id(tl6lura)]
    #[test]
    fn the_batchers_pause_keeps_its_text_and_records_the_batcher() {
        let t = dir("set-by-batcher");
        let d = t.path();
        let f = Files::new(d, "a");
        f.set_pause("red:5\n", PACE_OWNER, "a red round", 5).unwrap();
        assert_eq!(fs::read_to_string(f.pause_file()).unwrap(), "red:5\n");
        assert!(landing_log(d).contains("QUEUE ROUND-PAUSE 5 repo=a by=batcher kind=red reason=a red round"));
        assert_eq!(event(d, "pause_kind"), [Some("red".to_string())]);
    }

    #[test_id(ttqjw2w)]
    #[test]
    fn lifting_no_pause_returns_none_and_writes_nothing() {
        let t = dir("lift-none");
        let d = t.path();
        assert_eq!(Files::new(d, "a").lift_pause("concierge", "x", 9).unwrap(), None);
        assert_eq!(landing_log(d), "");
        assert!(event(d, "kind").is_empty());
    }

    #[test_id(t0s576q)]
    #[test]
    fn the_clock_lift_records_the_batcher_as_the_one_who_lifted() {
        let t = dir("lift-by-clock");
        let d = t.path();
        assert!(run(d, "a", 1000, None, Some(("red", 1000)), Ok(())));
        assert!(run(d, "a", 1000 + LIFT, None, None, Ok(())));
        assert!(landing_log(d).contains(&format!("QUEUE ROUND-PAUSE-LIFT {} repo=a by=batcher held_s={LIFT}", 1000 + LIFT)));
    }

    #[test_id(tpwtttw)]
    #[test]
    fn a_held_pause_is_read_by_no_age_and_lifted_only_by_a_person() {
        let t = dir("hold-by-hand");
        let d = t.path();
        let f = Files::new(d, "a");
        f.set_pause("hold:working", "operator", "working", 7).unwrap();
        assert_eq!(f.pause_since(), None);
        assert!(landing_log(d).contains("by=operator kind=hold"));
        assert!(f.lift_pause("operator", "done", 20).unwrap().is_some());
        assert!(!f.pause_file().exists());
    }

    #[test_id(ty9v3hn)]
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

    #[test_id(t1tkgae)]
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

    #[test_id(t1gzma5)]
    #[test]
    fn an_operator_hold_is_never_lifted_by_the_clock() {
        let t = dir("lift-hold");
        let d = t.path();
        fs::create_dir_all(d.join("rounds-pace/a")).unwrap();
        fs::write(Files::new(d, "a").pause_file(), "hold: release freeze\n").unwrap();
        assert!(!run(d, "a", u64::MAX / 2, None, None, Ok(())));
        assert!(Files::new(d, "a").pause_file().exists());
    }

    #[test_id(tbsn2x5)]
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

    #[test_id(txrznw6)]
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

    #[test_id(t31dhku)]
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

    #[test_id(trufxnm)]
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

    #[test_id(tk9guom)]
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

    #[test_id(tzu0y5e)]
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

    #[test_id(tq34x6i)]
    #[test]
    fn open_failed_pages_once_on_the_third_consecutive_failure_with_the_last_detail() {
        let mut st = State::default();
        let mut pages = Vec::new();
        for d in ["one", "two", "three", "four"] {
            let (next, fx) = advance(&st, &Outcome::OpenFailed, "a", d);
            assert!(!fx.pause && !fx.stop);
            pages.push(fx.pages);
            st = next;
        }
        assert!(pages[0].is_empty() && pages[1].is_empty() && pages[3].is_empty());
        assert_eq!(pages[2].len(), 1);
        assert!(pages[2][0].starts_with("OPEN FAILED a") && pages[2][0].contains("last: three"));
    }

    #[test_id(t0p8z13)]
    #[test]
    fn a_successful_open_resets_the_failed_open_count() {
        let mut st = State::default();
        for _ in 0..2 {
            st = advance(&st, &Outcome::OpenFailed, "a", "x").0;
        }
        let st = opened(&st);
        assert_eq!(st.open_failed, 0);
        let (_, fx) = advance(&st, &Outcome::OpenFailed, "a", "x");
        assert!(fx.pages.is_empty());
    }

    #[test_id(to31hur)]
    #[test]
    fn open_failed_count_round_trips_through_the_files() {
        let t = dir("open-failed");
        let f = Files::new(t.path(), "a");
        f.save(&State { open_failed: 2, ..State::default() }).unwrap();
        assert_eq!(f.load().open_failed, 2);
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
        let inbox_p = quiet_sink(d);
        let sm = Seams {
            repo: "a",
            now,
            inbox: &inbox_p,
            stamp: &stamp,
            redrive_after: WAIT,
            pause_lift_after: LIFT,
            landing_head: None,
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

    #[test_id(tefukt3)]
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

    #[test_id(t6zdb9y)]
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

    #[test_id(t2n931f)]
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

    #[test_id(tn7ip7a)]
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

    #[test_id(t8y9jja)]
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

    #[test_id(tjrcrto)]
    #[test]
    fn a_driver_seen_resets_the_orphan_wait() {
        let t = dir("orphan-driven");
        let (d, calls) = (t.path(), Calls::default());
        orphan_pass(d, 1000, "b-1", false, Ok(()), &calls);
        orphan_pass(d, 1000 + WAIT - 10, "b-1", true, Ok(()), &calls);
        orphan_pass(d, 1000 + WAIT, "b-1", false, Ok(()), &calls);
        assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
    }

    #[test_id(tw6nql8)]
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

    #[test_id(tzp3cas)]
    #[test]
    fn one_repos_red_does_not_hold_another() {
        let t = dir("repos");
        let d = t.path();
        run(d, "a", 1000, None, Some(("red", 1000)), Ok(()));
        let ran = run(d, "b", 1010, None, None, Ok(()));
        assert!(ran);
        assert!(!Files::new(d, "b").pause_file().exists());
    }

    #[test_id(tgcvblc)]
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

    #[test_id(tfsm8k6)]
    #[test]
    fn a_stale_image_refusal_is_recognised_and_a_plain_one_is_not() {
        assert!(is_stale_image(
            "cannot certify the round: round AMI is stale: acquire failed"
        ));
        assert!(!is_stale_image(
            "cannot certify the round: acquire failed: pool exhausted"
        ));
    }

    #[test_id(t7v5bb3)]
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

    #[test_id(tq4pcic)]
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

    #[test_id(tp5rtwj)]
    #[test]
    fn classify_an_ok_cut_is_red_only_on_a_red_verdict_written_this_pass() {
        assert_eq!(classify(&Ok(()), Some(("red", 5)), 5), Outcome::Red);
        assert_eq!(classify(&Ok(()), Some(("red", 4)), 5), Outcome::Quiet);
        assert_eq!(classify(&Ok(()), Some(("green", 9)), 5), Outcome::Quiet);
        assert_eq!(classify(&Ok(()), None, 5), Outcome::Quiet);
    }

    #[test_id(twbvw5a)]
    #[test]
    fn a_faulted_attribution_pages_at_once_and_neither_pauses_nor_counts_toward_a_stop() {
        let st = State::default();
        let (next, fx) = advance(&st, &Outcome::AttributionFaulted, "spira", "its reruns of test-x.sh could not run");
        assert!(!fx.pause && !fx.stop);
        assert_eq!(next.infra, st.infra);
        assert_eq!(fx.pages.len(), 1);
        assert!(fx.pages[0].starts_with("ATTRIBUTION FAULTED spira") && fx.pages[0].contains("test-x.sh"), "{:?}", fx.pages);
    }

    #[test_id(tp64mbu)]
    #[test]
    fn a_refused_landing_pages_at_once_and_neither_pauses_nor_counts_toward_a_stop() {
        let st = State { infra: 1, ..State::default() };
        let (next, fx) = advance(&st, &Outcome::LandRefused, "spira", "round r9 refused");
        assert!(!fx.pause && !fx.stop);
        assert_eq!(next, st);
        assert_eq!(fx.pages, ["LANDING REFUSED spira — round r9 refused"]);
    }
}
