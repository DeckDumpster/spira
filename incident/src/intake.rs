//! The intake: one production event into a bead through its row on the control store, as the
//! `incident file` / `systemd` subcommands run it, here so that a Spira caller files
//! in-process ([`file_payload`]) instead of spawning `incident.sh file`. A raise upserts the
//! reference's row, claims it, files it and records the end on it; [`file_pending`] files the
//! rows a raiser left behind. Nothing here writes a file.

use std::io::Read;
use std::process::ExitCode;

use spira_store::incident::{Payload, Recorded, Row, State};

use crate::decide;
use crate::ports::{Bd, Clock, Mailer, Rows};
use crate::run::{self, FileConfig, FileOutcome};

/// `std::env::var`, trimmed to "set and non-empty" — no `spira_config` fallback. Used only
/// inside [`resolved_config`]'s own init, which must not call back into [`env`] (that
/// would recurse into `resolved_config()` while it is still being built).
pub fn raw_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The declared harness home: `SPIRA_HOME`, else `$SPIRA_RELEASE/spira`; neither set is a
/// refusal naming them.
pub fn spira_home_dir() -> Result<std::path::PathBuf, String> {
    spira_config::resolve::locate_home_for_process()
}

/// Wave 4.8 ("retire conf re-import seams in Rust"): this crate used to read every
/// `SPIRA_*` key straight out of its own process environment, with no snapshot and no
/// the config document load at all (wave4-decomposition.md row (b) names incident by file:
/// SPIRA_HOME_REPO, SPIRA_WATCHER_INTERVAL_S). Resolved once, lazily, and cached:
/// `spira_config::resolve_for_process`, using [`spira_home_dir`] for `SPIRA_HOME` and
/// `spira_config::resolve::derive_home_repo` for `SPIRA_REPO`. A resolution failure
/// (unreadable registry, containment refusal) yields an empty
/// [`spira_config::resolve::Resolved`] — [`env`]'s own callers see exactly the behaviour
/// this crate had before this bead, never a panic.
pub fn resolved_config() -> &'static spira_config::resolve::Resolved {
    static RESOLVED: std::sync::OnceLock<spira_config::resolve::Resolved> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(|| {
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let home = match spira_home_dir() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("incident: {e}");
                return spira_config::resolve::Resolved::default();
            }
        };
        let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
        spira_config::resolve::resolve_or_say("incident", &home, &repo, &env_map)
    })
}

/// Registered config keys (`spira/conf.d`) this crate's generic `env()` seam also serves —
/// $SPIRA_TOML only, through `spira_config::process::cfg`, never the raw environment, never
/// a fallback (per Ryan 2026-10-05: one source of config). Every other name `env()` is
/// called with is NOT in `spira/conf.d` (an ad hoc override with no registry entry, same as
/// it has always been for the bash function) and keeps the old env/registry/default chain.
pub const REGISTERED: &[&str] =
    &["SPIRA_RUN", "SPIRA_INCIDENT_LABEL", "SPIRA_DB", "SPIRA_WATCHER_INTERVAL_S", "SPIRA_INCIDENT_PRIORITY", "SPIRA_HOME_REPO", "SPIRA_ASK_LABEL", "SPIRA_REPO_MAP", "SPIRA_CONCIERGE_INBOX"];

/// The environment, then `spira_config::resolve()`'s in-process answer — never the
/// reverse, so an explicit env override still wins exactly as it did before this bead.
/// EXCEPT for a registered key (see [`REGISTERED`]), which this never reads from the
/// environment at all.
pub fn env(name: &str) -> Option<String> {
    if REGISTERED.contains(&name) {
        return spira_config::process::cfg(name).ok();
    }
    raw_env(name).or_else(|| {
        let v = resolved_config().get(name);
        (!v.is_empty()).then(|| v.to_string())
    })
}

pub fn env_or(name: &str, default: &str) -> String {
    env(name).unwrap_or_else(|| default.to_string())
}

/// An incident files below a user-reported bug (priority 3 against the user's 1) unless configured.
pub fn priority_of(configured: Option<String>) -> String {
    configured.unwrap_or_else(|| "3".to_string())
}

/// The kernel's host name, as `hostname` printed it; `?` when there is none.
pub fn hostname() -> String {
    extern "C" {
        fn gethostname(name: *mut std::ffi::c_char, len: usize) -> i32;
    }
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is writable for its whole length, and gethostname writes at most `len` bytes.
    if unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return "?".to_string();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "?".to_string())
}

pub fn unit_from_cgroup() -> String {
    let raw = std::fs::read_to_string(format!("{}/cgroup", system_paths::PROC_SELF)).unwrap_or_default();
    let first_line = raw.lines().next().unwrap_or("");
    let last = first_line.rsplit('/').next().unwrap_or("");
    if last.ends_with(".service") || last.ends_with(".timer") || last.ends_with(".scope") {
        last.to_string()
    } else {
        "?".to_string()
    }
}

/// Every value read from the environment, resolved once per run — mirrors incident.sh's
/// top-of-script variable block.
pub struct Env {
    pub db: Option<String>,
    pub fenrir_at: u32,
    pub fenrir_exempt: bool,
    pub dedup_lookback_days: i64,
    pub watcher_interval_s: i64,
    pub cause: String,
    pub labels: String,
    pub repo: Option<String>,
    pub kind: String,
    pub priority: String,
    pub actor: Option<String>,
    pub delivers_pref: Option<String>,
    pub sop_ledger: String,
    pub spira_run: String,
    pub home_repo: String,
    pub known_repos: Vec<String>,
    pub ask_label: String,
    pub express: bool,
    pub reference: Option<String>,
    pub unit: String,
    pub path: String,
    pub lock_path: std::path::PathBuf,
    pub lock_wait_s: u64,
    pub retired_keys: Vec<String>,
    /// The repository map's file: where a repository's admitted lanes are read.
    pub repo_map: Option<std::path::PathBuf>,
}

impl Env {
    pub fn load() -> Env {
        let spira_run = env_or("SPIRA_RUN", system_paths::TMP);
        let incident_label = env_or("SPIRA_INCIDENT_LABEL", "incident");
        Env {
            db: env("SPIRA_DB"),
            fenrir_at: env("SPIRA_FENRIR_AT").and_then(|v| v.parse().ok()).unwrap_or(5),
            fenrir_exempt: env("SPIRA_FENRIR_EXEMPT").as_deref() == Some("1"),
            dedup_lookback_days: env("SPIRA_INCIDENT_DEDUP_LOOKBACK").and_then(|v| v.parse().ok()).unwrap_or(7),
            watcher_interval_s: env("SPIRA_WATCHER_INTERVAL_S").and_then(|v| v.parse().ok()).unwrap_or(1800),
            cause: decide::sanitize_cause(&env_or("SPIRA_INCIDENT_CAUSE", "unrecorded")),
            labels: decide::incident_labels(env("SPIRA_INCIDENT_LABELS").as_deref(), env("SPIRA_SCOPE_LABEL").as_deref(), &incident_label),
            repo: env("SPIRA_INCIDENT_REPO"),
            kind: env_or("SPIRA_INCIDENT_TYPE", "bug"),
            priority: priority_of(env("SPIRA_INCIDENT_PRIORITY")),
            actor: env("SPIRA_INCIDENT_ACTOR").or_else(|| env("BEADS_ACTOR")),
            delivers_pref: env("SPIRA_INCIDENT_DELIVERS"),
            sop_ledger: env_or("SPIRA_SOP_LEDGER", &format!("{spira_run}/sop/applied.jsonl")),
            home_repo: env_or("SPIRA_HOME_REPO", "spira"),
            known_repos: repo_names(),
            ask_label: env_or("SPIRA_ASK_LABEL", "needs-ryan"), // literal-ok: Rust fallback mirroring lib.sh's own default when SPIRA_ASK_LABEL is unset
            express: env("SPIRA_INCIDENT_EXPRESS").as_deref() == Some("1"),
            reference: env("SPIRA_INCIDENT_REF"),
            unit: env_or("SPIRA_INCIDENT_UNIT", &unit_from_cgroup()),
            path: env_or("SPIRA_INCIDENT_PATH", "?"),
            lock_path: std::path::PathBuf::from(env_or("SPIRA_INCIDENT_LOCK", &format!("{spira_run}/incident.lock"))),
            lock_wait_s: env("SPIRA_INCIDENT_LOCK_WAIT").and_then(|v| v.parse().ok()).unwrap_or(30),
            retired_keys: spira_home_dir().ok().and_then(|h| spira_config::delta::delta_text(&h).ok().flatten()).map(|t| decide::delta_removed_keys(&t)).unwrap_or_default(),
            repo_map: env("SPIRA_REPO_MAP").map(std::path::PathBuf::from),
            spira_run,
        }
    }

    /// The lanes `repo` admits now; `None` when the repository map does not name it.
    pub fn lanes_of(&self, repo: &str) -> Option<String> {
        lanes_in(self.repo_map.as_deref()?, repo)
    }

    pub fn spira_run_path(&self, name: &str) -> std::path::PathBuf {
        std::path::Path::new(&self.spira_run).join(name)
    }

    pub fn provenance(&self) -> String {
        decide::provenance(&self.unit, &hostname(), &self.path)
    }

    pub fn file_config<'a>(&'a self, provenance: &'a str) -> FileConfig<'a> {
        FileConfig {
            db: self.db.as_deref().unwrap_or(""),
            kind: &self.kind,
            priority: &self.priority,
            actor: self.actor.as_deref(),
            cause: &self.cause,
            fenrir_at: self.fenrir_at,
            fenrir_exempt: self.fenrir_exempt,
            watcher_interval_s: self.watcher_interval_s,
            dedup_lookback_days: self.dedup_lookback_days,
            repo_declared: self.repo.as_deref(),
            delivers_pref: self.delivers_pref.as_deref(),
            sop_ledger: &self.sop_ledger,
            spira_run: &self.spira_run,
            home_repo: &self.home_repo,
            known_repos: &self.known_repos,
            ask_label: &self.ask_label,
            express: self.express,
            provenance,
            retired_keys: &self.retired_keys,
        }
    }
}

/// `repo_names` (lib.sh): every name in `$SPIRA_REPO_MAP`, a `|`-delimited file whose
/// first column is the name and whose row must carry more than one column. No fallback
/// path: `spira/incident.sh`'s shim sources `lib.sh` before exec'ing this binary, and
/// `conf.sh` guarantees `SPIRA_REPO_MAP` is exported by then — this crate only ever needs
/// to read the variable, never derive the path itself.
pub fn repo_names() -> Vec<String> {
    env("SPIRA_REPO_MAP").map(|p| repo_names_in(std::path::Path::new(&p))).unwrap_or_default()
}

/// [`repo_names`] of the map at `map_path`.
pub fn repo_names_in(map_path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(map_path) else { return Vec::new() };
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let mut cols = l.split('|');
            let name = cols.next()?.trim();
            let has_more = cols.next().is_some();
            if !name.is_empty() && has_more {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// The lanes `repo` admits in the map at `map_path`, comma-joined in one order, so two
/// readings of an unchanged row compare equal.
pub fn lanes_in(map_path: &std::path::Path, repo: &str) -> Option<String> {
    let text = std::fs::read_to_string(map_path).ok()?;
    let section = bead::repos_by_name(&text).remove(repo)?;
    let mut lanes: Vec<String> = section.lanes.into_iter().map(bead::lane_label).collect();
    lanes.sort();
    lanes.dedup();
    Some(lanes.join(","))
}

pub fn ilog(msg: &str) {
    eprintln!("{} incident: {msg}", now_iso());
}

pub fn now_epoch() -> i64 {
    spira_config::vtime::now_epoch() as i64
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` — the `ilog` timestamp.
pub fn now_iso() -> String {
    crate::run::iso_now_public(now_epoch())
}

/// One alarm attempt in the run directory's alarm-attempt family; a row that cannot be written is
/// said on stderr, never silently dropped.
pub fn record_attempt(env: &Env, caller: &str, kind: &str, reference: &str, outcome: tsd::alarm::Outcome, message: Option<tsd::alarm::Message>) {
    let a = tsd::alarm::Attempt { at: now_epoch().max(0) as u64, caller: caller.into(), kind: kind.into(), reference: reference.into(), outcome, message };
    if let Err(e) = a.write(std::path::Path::new(&env.spira_run), &hostname()) {
        ilog(&format!("alarm attempt for {reference} not recorded: {e}"));
    }
}

/// What a retry sends when an incident reached no one: the event as a note to the Concierge.
pub fn note_of(title: &str, body: &[u8]) -> tsd::alarm::Message {
    tsd::alarm::Message {
        mailbox: "concierge".into(),
        subject: title.to_string(),
        kind: "note".into(),
        class: None,
        default: String::new(),
        body: format!("## Note\n{}\n", String::from_utf8_lossy(body)),
    }
}

/// File the incident that owns an alarm, when its reference has a bead nobody is working. It
/// is raised like any event, so a filing that does not land is held on its row.
pub fn own_alarm(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, reference: &str, body: &[u8]) {
    let provenance = decide::provenance(&env.unit, &hostname(), &env.path);
    let cfg = env.file_config(&provenance);
    if !run::alarm_wants_filing(bd, clock, &cfg, reference) {
        return;
    }
    let mut payload = payload_of(env, reference, title, body, &env.unit, &env.path);
    payload.lookback_days = Some(run::alarm_lookback(env.dedup_lookback_days));
    if let Err(why) = raise_and_file(env, bd, mailer, clock, payload) {
        ilog(&why);
    }
}

/// One lock around the alarm note's read-then-write of its state file
/// (`exec 8>>"$lock"; flock -w "$wait" 8`). `None` on a timed-out wait.
pub fn with_incident_lock<R>(lock_path: &std::path::Path, wait_s: u64, f: impl FnOnce() -> R) -> Option<R> {
    use std::os::unix::io::AsRawFd;
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(lock_path).ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_s);
    loop {
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            let r = f();
            // The fd (and its flock) is released when `file` drops at the end of this
            // scope — exactly once, whether `f` returned normally or the lock loop above
            // never ran again, since this is the only path that reaches here.
            return Some(r);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// How long a filer holds its claim; one that dies is finished by [`file_pending`] after it.
pub const CLAIM_SECS: i64 = 600;
const RETRY_FIRST_SECS: i64 = 60;
const RETRY_MAX_SECS: i64 = 3_600;

/// The delay before the next try of a row that has failed `tries` times to reach the content
/// store: doubling from a minute to an hour.
pub fn retry_delay(tries: u32) -> i64 {
    (RETRY_FIRST_SECS << tries.min(6)).min(RETRY_MAX_SECS)
}

/// The repository a payload files into, whose lanes decide whether a refusal still stands.
pub fn route_repo(home_repo: &str, known_repos: &[String], payload: &Payload) -> String {
    if let Some(labelled) = payload.labels.split(',').find_map(|l| l.trim().strip_prefix("repo:")) {
        return labelled.to_string();
    }
    match &payload.repo {
        Some(r) if r == home_repo || known_repos.contains(r) => r.clone(),
        _ => home_repo.to_string(),
    }
}

fn payload_of(env: &Env, reference: &str, title: &str, body: &[u8], unit: &str, path: &str) -> Payload {
    Payload {
        reference: reference.to_string(),
        title: title.to_string(),
        body: String::from_utf8_lossy(body).into_owned(),
        unit: unit.to_string(),
        path: path.to_string(),
        cause: env.cause.clone(),
        repo: env.repo.clone(),
        kind: env.kind.clone(),
        priority: env.priority.clone(),
        express: env.express,
        actor: env.actor.clone(),
        labels: env.labels.clone(),
        fenrir_exempt: env.fenrir_exempt,
        lookback_days: None,
    }
}

/// An `incident:` reference files carrying the alarm label too.
fn with_alarm_label(mut payload: Payload) -> Payload {
    let alarm = env_or("SPIRA_ALARM_LABEL", "alarm");
    if payload.reference.starts_with("incident:") && !payload.labels.split(',').any(|l| l == alarm) {
        payload.labels = format!("{},{alarm}", payload.labels);
    }
    payload
}

/// Files a claimed row and records how it ended on it. `Err` is why it is still unfiled;
/// `Ok(None)` is an event that rightly files nothing.
#[allow(clippy::too_many_arguments)]
fn file_claimed(
    env: &Env,
    rows: &dyn Rows,
    bd: &dyn Bd,
    mailer: &dyn Mailer,
    clock: &dyn Clock,
    lanes_now: &dyn Fn(&str) -> Option<String>,
    key: &str,
    row: &Row,
    now: i64,
) -> Result<Option<String>, String> {
    let p = &row.payload;
    let provenance = decide::provenance(&p.unit, &hostname(), &p.path);
    let cause = decide::sanitize_cause(&p.cause);
    let cfg = FileConfig {
        kind: &p.kind,
        priority: &p.priority,
        actor: p.actor.as_deref(),
        cause: &cause,
        fenrir_exempt: p.fenrir_exempt,
        repo_declared: p.repo.as_deref(),
        express: p.express,
        dedup_lookback_days: p.lookback_days.unwrap_or(env.dedup_lookback_days),
        ..env.file_config(&provenance)
    };
    let mut body = if p.body.is_empty() {
        format!("The intake gathered NO payload for {}.\nsystemctl produced nothing — suspect the unit name.\n", p.reference)
    } else {
        p.body.clone()
    };
    if row.events > 1 {
        let at = |t: i64| crate::run::iso_now_public(t);
        body = format!("{} events of this reference were raised from {} to {}; this is the newest.\n\n{body}", row.events, at(row.first_at), at(row.last_at));
    }
    let mut log = Vec::new();
    let outcome = run::file_one(bd, mailer, clock, &cfg, &p.reference, &p.title, body.as_bytes(), &p.labels, &mut log);
    for l in &log {
        ilog(l);
    }
    let lanes = || lanes_now(&route_repo(&env.home_repo, &env.known_repos, p));
    let (recorded, answer) = match outcome {
        FileOutcome::Filed(id) => (Recorded::Filed { bead: Some(id.clone()) }, Ok(Some(id))),
        FileOutcome::Skew => (Recorded::Filed { bead: None }, Ok(None)),
        FileOutcome::LaneRefused(reason) => (Recorded::Refused { reason: reason.clone(), lanes: lanes() }, Err((State::Refused, reason))),
        FileOutcome::Refused => {
            let reason = log.pop().unwrap_or_else(|| "the filing was refused by its guards".into());
            (Recorded::Refused { reason: reason.clone(), lanes: lanes() }, Err((State::Refused, reason)))
        }
        FileOutcome::Unread(why) => (Recorded::Unread { reason: why.clone(), retry_at: now + retry_delay(row.tries) }, Err((State::Unread, why))),
    };
    if let Err(e) = rows.record(key, &recorded) {
        ilog(&format!("{}: the filing's end was not recorded on its row ({e}); it is filed again once the claim lapses", p.reference));
    }
    answer.map_err(|(state, why)| format!("{} was not filed; its row is {}: {why}", p.reference, state.as_str()))
}

fn claimant() -> String {
    format!("{}:{}", hostname(), std::process::id())
}

/// Raises the payload onto its reference's row, claims the row and files it. `Err` names the
/// row's state and reason, or the store error when the event could not be raised at all: there
/// is no other place it is kept.
fn raise_and_file(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, payload: Payload) -> Result<Option<String>, String> {
    let rows = bd.incident_rows();
    let reference = payload.reference.clone();
    let key = rows.raise(&payload).map_err(|e| format!("{reference} was not raised: the control store: {e}"))?;
    let row = rows.claim(&key, &claimant(), CLAIM_SECS).map_err(|e| format!("{reference} was raised and not filed by this call: {e}"))?;
    file_claimed(env, &*rows, bd, mailer, clock, &|repo| env.lanes_of(repo), &key, &row, clock.now())
}

/// Whether [`file_pending`] files `row` at `now`: a pending one; a filing one whose claim
/// lapsed; a refused one whose repository admits other lanes than it was refused under; an
/// unread one whose retry time has come.
pub fn eligible(row: &Row, lanes_now: Option<&str>, now: i64) -> bool {
    match row.state {
        State::Pending => true,
        State::Filing => row.claim.as_ref().is_none_or(|c| c.until <= now),
        State::Refused => row.lanes.as_deref() != lanes_now,
        State::Unread => row.retry_at.is_none_or(|t| t <= now),
    }
}

pub const BATCH_KEY: &str = "SPIRA_INCIDENT_DRAIN_BATCH";

pub fn parse_batch(raw: &str) -> Result<usize, String> {
    raw.trim().parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| format!("{BATCH_KEY} must be a positive number of references, got {raw:?} — refusing"))
}

/// What one pass over the rows did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FileReport {
    /// Rows whose claimed filing ended well.
    pub filed: usize,
    /// Rows tried and still unfiled, each with why.
    pub failed: Vec<(String, String)>,
    /// Rows this pass claimed.
    pub claimed: usize,
    /// Eligible rows past the pass's batch, left for the next one.
    pub held_back: usize,
    /// Why the rows could not be listed.
    pub unreadable: Option<String>,
}

impl FileReport {
    pub fn line(&self) -> String {
        format!("filed {}, still unfiled {}, held back {}", self.filed, self.failed.len(), self.held_back)
    }
}

/// One pass: files up to `batch` [`eligible`] rows, oldest first.
#[allow(clippy::too_many_arguments)]
pub fn file_pending(
    env: &Env,
    rows: &dyn Rows,
    bd: &dyn Bd,
    mailer: &dyn Mailer,
    clock: &dyn Clock,
    lanes_now: &dyn Fn(&str) -> Option<String>,
    now: i64,
    batch: usize,
) -> FileReport {
    let mut report = FileReport::default();
    let listed = match rows.list() {
        Ok(l) => l,
        Err(e) => {
            ilog(&format!("the incident rows cannot be read: {e}"));
            report.unreadable = Some(e);
            return report;
        }
    };
    let due = listed.into_iter().filter(|(_, r)| eligible(r, lanes_now(&route_repo(&env.home_repo, &env.known_repos, &r.payload)).as_deref(), now));
    for (i, (key, listed)) in due.enumerate() {
        if i >= batch {
            report.held_back += 1;
            continue;
        }
        let reference = listed.payload.reference;
        let claimed = match listed.state {
            State::Refused | State::Unread => rows.retry(Some(&key)).map(drop),
            State::Pending | State::Filing => Ok(()),
        }
        .and_then(|()| rows.claim(&key, &claimant(), CLAIM_SECS));
        let row = match claimed {
            Ok(row) => row,
            Err(why) => {
                report.failed.push((reference, why));
                continue;
            }
        };
        report.claimed += 1;
        match file_claimed(env, rows, bd, mailer, clock, lanes_now, &key, &row, now) {
            Ok(_) => report.filed += 1,
            Err(why) => report.failed.push((reference, why)),
        }
    }
    if report.held_back > 0 {
        ilog(&format!("held back {} incident(s) for the next pass", report.held_back));
    }
    report
}

pub fn require_db(env: &Env) -> ExitCode {
    eprintln!("incident: refusing to file — SPIRA_DB is not set — refusing to file rather than let bd resolve one on its own");
    let _ = env;
    ExitCode::FAILURE
}

pub fn systemd_payload(unit: &str, host: &str, when: &str, systemctl: &str) -> String {
    format!("unit: {unit}\nhost: {host}\nwhen: {when}\n\n## systemctl\n{systemctl}\nlog: systemctl --user status {unit}\n")
}

pub fn cmd_systemd(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, unit: &str) -> ExitCode {
    let reference = format!("incident:{unit}");
    let when = now_iso();
    // FORK-EDGE: needs Units facts carrying Result, ExecMainStatus/Code, NRestarts, SubState,
    // InvocationID and ExecMainStartTimestamp — the payload quotes systemctl's own properties.
    let systemctl = spira_config::bounded::bounded("systemctl")
        .args([
            "--user", "show", unit, "-p", "Result", "-p", "ExecMainStatus", "-p", "ExecMainCode", "-p", "NRestarts", "-p", "ActiveState",
            "-p", "SubState", "-p", "InvocationID", "-p", "ExecMainStartTimestamp",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_else(|| "(systemctl unavailable)\n".to_string());
    let payload = systemd_payload(unit, &hostname(), &when, &systemctl);

    let raised = with_alarm_label(payload_of(env, &reference, &format!("incident: {unit} failed"), payload.as_bytes(), unit, unit));
    match raise_and_file(env, bd, mailer, clock, raised) {
        Ok(_) => ExitCode::SUCCESS,
        Err(why) => {
            ilog(&why);
            ExitCode::FAILURE
        }
    }
}

pub fn cmd_file(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, src: &str) -> ExitCode {
    let mut body = Vec::new();
    if src == "-" {
        let _ = std::io::stdin().read_to_end(&mut body);
    } else {
        match std::fs::read(src) {
            Ok(b) => body = b,
            Err(e) => {
                eprintln!("incident: cannot read {src}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    match file_body_id(env, bd, mailer, clock, title, body) {
        Ok(id) => {
            println!("{}", filed_answer(id.as_deref()));
            ExitCode::SUCCESS
        }
        Err(()) => ExitCode::FAILURE,
    }
}

/// What `incident file` says on stdout once the event is settled: the bead it filed or bumped,
/// or null when nothing was filed (activation skew).
/// Callers deserialise it.
pub fn filed_answer(id: Option<&str>) -> String {
    serde_json::json!({ "id": id }).to_string()
}

/// `incident file <title>` with its payload in hand: raised, then filed. `false` when it was
/// not filed.
pub fn file_body(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, body: Vec<u8>) -> bool {
    file_body_id(env, bd, mailer, clock, title, body).is_ok()
}

/// `file_body`, naming the bead the event landed on: `Ok(None)` is a skew with nothing filed,
/// `Err` is an event not filed, whose why is on stderr.
#[allow(clippy::result_unit_err)]
pub fn file_body_id(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, body: Vec<u8>) -> Result<Option<String>, ()> {
    file_event(env, bd, mailer, clock, title, &body).map_err(|why| ilog(&why))
}

/// [`file_body_id`] with why it was not filed: the row's state and reason, or the store error.
/// The dedup reference is `env.reference`, else the title's slug.
pub fn file_event(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, body: &[u8]) -> Result<Option<String>, String> {
    let reference = env.repo_override_ref().unwrap_or_else(|| {
        let slug: String = title.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' }).take(60).collect();
        format!("incident:{slug}")
    });
    let payload = with_alarm_label(payload_of(env, &reference, title, body, &env.unit, &env.path));
    raise_and_file(env, bd, mailer, clock, payload)
}

/// `incident file <title> -` in-process, with the host's ports: what a Spira caller runs where
/// it used to spawn `incident.sh file`. `Err` names why nothing was filed.
pub fn file_payload(title: &str, body: &[u8]) -> Result<(), String> {
    file_payload_with(title, body, |_| {})
}

/// `file_payload` with the per-call fields (`priority`, `express`, `reference`, ...) set on the
/// loaded environment by `tune`, so a caller never writes them into the process environment.
pub fn file_payload_with(title: &str, body: &[u8], tune: impl FnOnce(&mut Env)) -> Result<(), String> {
    file_payload_with_id(title, body, tune).map(|_| ())
}

/// `file_payload_with`, naming the bead the event landed on (`None` is a skew with nothing filed).
pub fn file_payload_with_id(title: &str, body: &[u8], tune: impl FnOnce(&mut Env)) -> Result<Option<String>, String> {
    let mut env = Env::load();
    tune(&mut env);
    if env.db.is_none() {
        let _ = require_db(&env);
        return Err("SPIRA_DB is not set".into());
    }
    let bd = crate::real::RealBd::from_env()?;
    file_event(&env, &bd, &crate::real::RealMailer, &crate::real::RealClock, title, body).map_err(|why| format!("incident file {title:?}: {why}"))
}

impl Env {
    pub fn repo_override_ref(&self) -> Option<String> {
        self.reference.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{cmd_systemd, eligible, file_body, file_body_id, file_event, file_pending, filed_answer, own_alarm, parse_batch, priority_of, retry_delay, with_incident_lock, Env, FileReport};
    use crate::fake::{FakeBd, FakeMailer, FixedClock};
    use crate::ports::Rows as _;
    use spira_store::incident::{Claim, Payload, Row, State, BODY_CAP};
    use std::sync::{mpsc, Arc, Mutex};

    const BATCH: usize = 20;
    const NOW: i64 = 1_792_800_000;
    const WALLED: &str = "bead: repo:walled does not admit lane incident — refused by its admitted lanes (plan)\nbead: override";

    // covers: UC-ops-detection-remediation-36
    #[test_id(tjlkart)]
    #[test]
    fn an_incident_defaults_behind_a_reported_bug_and_a_configured_priority_wins() {
        let home = testkit::manifest_dir().join("../spira");
        let reg = spira_config::registry::Registry::load(&home).unwrap();
        let default_of = |k: &str| -> String { reg.get(k).and_then(|k| k.default_expr()).unwrap().to_string() };
        let incident: u32 = default_of("SPIRA_INCIDENT_PRIORITY").parse().unwrap();
        let reported: u32 = default_of("SPIRA_GH_INTAKE_PRIORITY").parse().unwrap();
        assert!(incident > reported, "incident priority {incident} must sit behind a reported bug's {reported}");
        assert_eq!(priority_of(None), incident.to_string());
        assert_eq!(priority_of(Some("1".into())), "1");
    }

    /// The run directory every path of `env` is under, and what it holds.
    fn written(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn env_in(dir: &std::path::Path) -> Env {
        Env {
            db: Some("db".into()),
            fenrir_at: 5,
            fenrir_exempt: false,
            dedup_lookback_days: 7,
            watcher_interval_s: 1800,
            cause: "unrecorded".into(),
            labels: "spira,incident".into(),
            repo: Some("spira".into()),
            kind: "bug".into(),
            priority: "3".into(),
            actor: None,
            delivers_pref: None,
            sop_ledger: format!("{}/sop/applied.jsonl", dir.display()),
            spira_run: dir.to_str().unwrap().into(),
            home_repo: "spira".into(),
            known_repos: vec!["spira".into()],
            ask_label: "needs-operator".into(), // literal-ok: test fixture
            express: false,
            reference: None,
            unit: "u.service".into(),
            path: "?".into(),
            lock_path: dir.join("incident.lock"),
            lock_wait_s: 0,
            retired_keys: vec![],
            repo_map: None,
        }
    }

    fn bd_refusing(error: &str) -> FakeBd {
        FakeBd { create_error: Some(error.into()), ..FakeBd::new() }
    }

    fn payload(reference: &str) -> Payload {
        Payload { reference: reference.into(), title: "t".into(), body: "payload".into(), unit: "u".into(), path: "p".into(), cause: "c".into(), repo: Some("spira".into()), kind: "bug".into(), priority: "3".into(), labels: "spira,incident".into(), ..Payload::default() }
    }

    fn raised(bd: &FakeBd, reference: &str) -> String {
        (&bd.incidents).raise(&payload(reference)).unwrap()
    }

    fn pass(env: &Env, bd: &FakeBd, lanes: &str, now: i64, batch: usize) -> FileReport {
        let lanes = lanes.to_string();
        bd.incidents.now.set(now);
        file_pending(env, &&bd.incidents, bd, &FakeMailer::default(), &FixedClock(now), &move |_| Some(lanes.clone()), now, batch)
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(teqnacw)]
    #[test]
    fn ten_raises_of_a_refused_reference_are_one_row_and_one_create_and_write_no_file() {
        let dir = testkit::TempDir::new("incident-ten-raises");
        let env = env_in(&dir);
        let mut bd = bd_refusing(WALLED);
        let untouched = |step: &str| assert_eq!(written(&dir), Vec::<String>::new(), "{step}: no spool, log, lock or tsd under the run dir");
        for i in 0..10 {
            assert!(!file_body(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "walled", format!("event {i}").into_bytes()));
            untouched("a raise");
        }
        let row = bd.incidents.only();
        assert_eq!((row.events, row.state, row.payload.body.as_str()), (10, State::Refused, "event 9"));
        assert!(row.reason.as_deref().unwrap().contains("repo:walled"), "{row:?}");
        assert_eq!(*bd.create_calls.borrow(), 1);
        let why = file_event(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "walled", b"again").unwrap_err();
        assert!(why.contains("state refused") && why.contains("repo:walled"), "{why}");

        bd.create_error = None;
        assert_eq!(pass(&env, &bd, "incident,plan", NOW, BATCH).filed, 1);
        untouched("the filing");
        assert_eq!(bd.created.borrow().len(), 1);
        assert!(bd.incidents.held.borrow().is_empty());
        let body = bd.bodies.borrow().values().next().cloned().unwrap();
        assert!(body.starts_with("11 events of this reference were raised from ") && body.contains("again"), "{body}");
        assert_eq!(pass(&env, &bd, "incident,plan", NOW, BATCH), FileReport::default());
        assert_eq!(*bd.create_calls.borrow(), 2);
        untouched("an idle pass");
    }

    #[test_id(t5jey5e)]
    #[test]
    fn a_refused_row_waits_until_its_repository_admits_other_lanes() {
        let dir = testkit::TempDir::new("incident-refused-waits");
        let mut env = env_in(&dir);
        let map = dir.join("repos.psv");
        let admits = |lanes: &str| std::fs::write(&map, format!("spira|/src|queue.local|main|||{lanes}\n")).unwrap();
        admits("plan");
        env.repo_map = Some(map.clone());
        let mut bd = bd_refusing(WALLED);
        assert!(!file_body(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "walled", b"p".to_vec()));
        let before = bd.incidents.only();
        assert_eq!(before.lanes, env.lanes_of("spira"), "the refusal records the lanes it was judged against");
        assert_eq!(before.lanes.as_deref(), Some("plan"));
        bd.create_error = None;
        *bd.create_calls.borrow_mut() = 0;
        for _ in 0..2 {
            assert_eq!(pass(&env, &bd, "plan", NOW + 100_000, BATCH), FileReport::default());
        }
        assert_eq!((*bd.create_calls.borrow(), bd.incidents.only()), (0, before));
        assert_eq!(pass(&env, &bd, "incident,plan", NOW + 100_000, BATCH).filed, 1);
        assert_eq!(*bd.create_calls.borrow(), 1);
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(ta62ak4)]
    #[test]
    fn an_unreadable_content_store_is_retried_on_a_doubling_delay_and_files_once() {
        let dir = testkit::TempDir::new("incident-unread");
        let env = env_in(&dir);
        let down = FakeBd { lc_down: true, ..FakeBd::new() };
        down.incidents.now.set(NOW);
        let why = file_event(&env, &down, &FakeMailer::default(), &FixedClock(NOW), "blind", b"p").unwrap_err();
        assert!(why.contains("spira-lc: down") && why.contains("Unfinished label-keyed pass"), "{why}");
        assert_eq!(down.incidents.only().retry_at, Some(NOW + 60));
        assert_eq!(pass(&env, &down, "", NOW + 59, BATCH), FileReport::default());
        assert_eq!(pass(&env, &down, "", NOW + 60, BATCH).failed.len(), 1);
        assert_eq!(down.incidents.only().retry_at, Some(NOW + 60 + 120));

        let up = FakeBd::new();
        up.incidents.held.replace(down.incidents.held.take());
        assert_eq!(pass(&env, &up, "", NOW + 180, BATCH).filed, 1);
        assert_eq!(up.created.borrow().len(), 1);
        assert!(up.incidents.held.borrow().is_empty());
    }

    #[test_id(tpm4qhm)]
    #[test]
    fn the_retry_delay_doubles_from_a_minute_to_an_hour() {
        assert_eq!([0, 1, 2, 5, 6, 7, 40].map(retry_delay), [60, 120, 240, 1_920, 3_600, 3_600, 3_600]);
    }

    #[test_id(t7l8eiw)]
    #[test]
    fn a_claim_that_lapsed_is_filed_and_a_live_one_is_left_alone() {
        let dir = testkit::TempDir::new("incident-lapsed-claim");
        let env = env_in(&dir);
        let bd = FakeBd::new();
        bd.incidents.now.set(NOW);
        for (reference, until) in [("incident:dead-raiser", NOW - 1), ("incident:live-raiser", NOW + 100)] {
            let key = raised(&bd, reference);
            let mut held = bd.incidents.held.borrow_mut();
            let row = held.get_mut(&key).unwrap();
            row.state = State::Filing;
            row.claim = Some(Claim { by: "gone".into(), until, events: 1 });
        }
        assert_eq!(pass(&env, &bd, "", NOW, BATCH).filed, 1);
        let refs: Vec<String> = bd.created.borrow().iter().map(|c| c.1.clone()).collect();
        assert_eq!(refs, ["incident:dead-raiser"]);
        assert_eq!(bd.incidents.only().claim.unwrap().by, "gone");
    }

    #[test_id(t7bz3qj)]
    #[test]
    fn a_row_is_eligible_only_once_its_cause_has_moved() {
        let row = |state| Row { state, lanes: Some("plan".into()), retry_at: Some(50), claim: Some(Claim { until: 50, ..Claim::default() }), ..Row::default() };
        assert!(eligible(&row(State::Pending), None, 0));
        for state in [State::Filing, State::Unread] {
            assert!(!eligible(&row(state), None, 49) && eligible(&row(state), None, 50), "{state:?}");
        }
        assert!(!eligible(&row(State::Refused), Some("plan"), i64::MAX) && eligible(&row(State::Refused), Some("incident,plan"), 0));
    }

    // covers: UC-ops-detection-remediation-01
    #[test_id(ty4fuaq)]
    #[test]
    fn a_raise_after_its_row_was_filed_and_deleted_bumps_the_existing_bead() {
        let dir = testkit::TempDir::new("incident-recur-after-filed");
        let env = env_in(&dir);
        let bd = FakeBd::new();
        let first = file_body_id(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "again", b"one".to_vec()).unwrap();
        assert!(bd.incidents.held.borrow().is_empty());
        let second = file_body_id(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "again", b"two".to_vec()).unwrap();
        assert_eq!(first, second);
        assert_eq!((bd.created.borrow().len(), bd.facts.borrow().len()), (1, 1));
        let body = bd.bodies.borrow().values().next().cloned().unwrap();
        assert_eq!(body, "one", "a single event files its own payload with no preface");
    }

    #[test_id(tepfcaw)]
    #[test]
    fn a_raise_the_store_cannot_take_is_refused_naming_the_store_and_files_nothing() {
        let dir = testkit::TempDir::new("incident-store-down");
        let env = env_in(&dir);
        let bd = FakeBd::new();
        bd.incidents.down.set(true);
        let why = file_event(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "t", b"p").unwrap_err();
        assert!(why.contains("the control store: store not running"), "{why}");
        assert_eq!(*bd.create_calls.borrow(), 0);
        assert_eq!(written(&dir), Vec::<String>::new());
    }

    #[test_id(t0gzgxo)]
    #[test]
    fn a_failed_systemd_unit_is_raised_under_its_own_reference_with_the_alarm_label() {
        let dir = testkit::TempDir::new("incident-systemd");
        let env = env_in(&dir);
        let bd = bd_refusing(WALLED);
        cmd_systemd(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "spira-incident-test.service");
        let row = bd.incidents.only();
        assert_eq!((row.payload.reference.as_str(), row.payload.unit.as_str()), ("incident:spira-incident-test.service", "spira-incident-test.service"));
        assert!(row.payload.labels.split(',').any(|l| l == "alarm"), "{}", row.payload.labels);
    }

    #[test_id(t0yrwqi)]
    #[test]
    fn a_pass_files_at_most_a_batch_of_rows_oldest_first_and_says_what_it_held_back() {
        let dir = testkit::TempDir::new("incident-batch");
        let env = env_in(&dir);
        let bd = FakeBd::new();
        for (i, reference) in ["incident:z-first", "incident:a-second", "incident:m-third"].iter().enumerate() {
            bd.incidents.now.set(NOW + i as i64);
            raised(&bd, reference);
        }
        let report = pass(&env, &bd, "", NOW + 10, 2);
        assert_eq!((report.filed, report.held_back), (2, 1));
        let refs: Vec<String> = bd.created.borrow().iter().map(|c| c.1.clone()).collect();
        assert_eq!(refs, ["incident:z-first", "incident:a-second"]);
        assert_eq!(report.line(), "filed 2, still unfiled 0, held back 1");
    }

    #[test_id(tm570cg)]
    #[test]
    fn a_long_payload_is_held_capped_and_files_saying_so() {
        let dir = testkit::TempDir::new("incident-capped");
        let env = env_in(&dir);
        let bd = FakeBd::new();
        file_body(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "big", vec![b'x'; BODY_CAP * 2]);
        let body = bd.bodies.borrow().values().next().cloned().unwrap();
        assert!(body.len() <= BODY_CAP && body.contains("body capped at"), "{}", body.len());
    }

    fn closed_owner(bd: &FakeBd) {
        bd.rows.borrow_mut().push(crate::decide::BeadRow {
            id: "sp-old".into(),
            status: crate::decide::BeadStatus::Terminal,
            external_ref: Some("incident:unit-drift".into()),
            labels: vec![format!("ref:{}", crate::decide::ref_hash("incident:unit-drift"))],
            closed_at: Some("2026-10-08T00:00:00Z".into()),
        });
    }

    #[test_id(t3o7ntf)]
    #[test]
    fn an_alarm_whose_owner_is_refused_is_held_on_its_row_with_the_finding() {
        let dir = testkit::TempDir::new("own-alarm-refused");
        let env = env_in(&dir);
        let bd = bd_refusing("bead: repo:spira does not admit lane incident — refused by its admitted lanes (plan,groom,spike)");
        closed_owner(&bd);
        own_alarm(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "UNIT DRIFT: x", "incident:unit-drift", b"UNSHIPPED /u/x.service (unit file this install does not ship)\n");
        let row = bd.incidents.only();
        assert_eq!(row.state, State::Refused);
        assert!(row.reason.as_deref().unwrap().contains("repo:spira"), "{row:?}");
        assert!(row.payload.body.contains("UNSHIPPED /u/x.service"));
        assert_eq!(written(&dir), Vec::<String>::new(), "no tsd row, no log");
    }

    #[test_id(tr3bymw)]
    #[test]
    fn an_alarm_with_no_bead_history_raises_no_row() {
        let dir = testkit::TempDir::new("own-alarm-none");
        let env = env_in(&dir);
        let bd = FakeBd::new();
        own_alarm(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "t", "incident:never-filed", b"p");
        assert!(bd.incidents.held.borrow().is_empty());
        assert_eq!(written(&dir), Vec::<String>::new());
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(tidlcpb)]
    #[test]
    fn an_alarm_owner_whose_create_fails_holds_the_create_error_and_its_wider_lookback() {
        let dir = testkit::TempDir::new("own-alarm-create-failed");
        let env = env_in(&dir);
        let mut bd = bd_refusing("boom: disk full");
        closed_owner(&bd);
        bd.incidents.now.set(NOW);
        own_alarm(&env, &bd, &FakeMailer::default(), &FixedClock(NOW), "UNIT DRIFT: x", "incident:unit-drift", b"body");
        let row = bd.incidents.only();
        assert_eq!(row.state, State::Unread);
        assert!(row.reason.as_deref().unwrap().contains("create failed (boom: disk full)"), "{row:?}");

        bd.create_error = None;
        assert_eq!(pass(&env, &bd, "", NOW + 60, BATCH).filed, 1);
        let new = bd.bodies.borrow().values().next().cloned().unwrap();
        assert!(new.contains("sp-old"), "the owner outside the filing window is still cited: {new}");
    }

    #[test_id(t34c4aj)]
    #[test]
    fn the_filed_answer_is_one_json_object_naming_the_bead_or_null() {
        assert_eq!(filed_answer(Some("sp-abc")), r#"{"id":"sp-abc"}"#);
        assert_eq!(filed_answer(None), r#"{"id":null}"#);
    }

    #[test_id(tjtd9er)]
    #[test]
    fn a_reference_set_on_the_loaded_env_is_the_filings_dedup_reference() {
        let mut env = Env::load();
        env.reference = Some("incident:throughput-stall:alpha:7".into());
        assert_eq!(env.repo_override_ref().as_deref(), Some("incident:throughput-stall:alpha:7"));
    }

    // covers: UC-ops-detection-remediation-02
    #[test_id(tqa3ntb)]
    #[test]
    fn two_filers_of_one_ref_serialise_into_one_creation_and_one_recurrence() {
        let dir = testkit::TempDir::new("incident-lock-serialise");
        let lock = dir.join("incident.lock");
        let beads: Arc<Mutex<Vec<&'static str>>> = Arc::default();
        let (inside_tx, inside_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (second_tx, second_rx) = mpsc::channel();
        let first = {
            let (lock, beads) = (lock.clone(), beads.clone());
            std::thread::spawn(move || {
                with_incident_lock(&lock, 30, || {
                    let seen_none = beads.lock().unwrap().is_empty();
                    inside_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                    beads.lock().unwrap().push(if seen_none { "create" } else { "recur" });
                })
            })
        };
        inside_rx.recv().unwrap();
        let second = {
            let (lock, beads) = (lock.clone(), beads.clone());
            std::thread::spawn(move || {
                second_tx.send(()).unwrap();
                with_incident_lock(&lock, 30, || {
                    let seen_none = beads.lock().unwrap().is_empty();
                    beads.lock().unwrap().push(if seen_none { "create" } else { "recur" });
                })
            })
        };
        second_rx.recv().unwrap();
        go_tx.send(()).unwrap();
        assert!(first.join().unwrap().is_some());
        assert!(second.join().unwrap().is_some());
        assert_eq!(*beads.lock().unwrap(), vec!["create", "recur"]);
    }

    #[test_id(te9p14f)]
    #[test]
    fn a_batch_that_would_file_nothing_is_refused() {
        assert_eq!(parse_batch("20"), Ok(20));
        for bad in ["0", "", "many"] {
            assert!(parse_batch(bad).unwrap_err().contains("SPIRA_INCIDENT_DRAIN_BATCH"), "{bad:?}");
        }
    }
}
