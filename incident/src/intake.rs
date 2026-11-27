//! The intake: one production event into a bead through the write-ahead spool, as the
//! `incident file` / `systemd` / `drain` subcommands run it, here so that a Spira caller files
//! in-process ([`file_payload`]) instead of spawning `incident.sh file`. Moved verbatim from
//! main.rs (sp-ipwlic); main.rs keeps the argv dispatch.

use std::io::Read;
use std::process::ExitCode;

use crate::decide;
use crate::ports::{Bd, Clock, Mailer};
use crate::run::{self, FileConfig, FileOutcome};
use crate::spool;

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
    &["SPIRA_RUN", "SPIRA_INCIDENT_LABEL", "SPIRA_DB", "SPIRA_WATCHER_INTERVAL_S", "SPIRA_INCIDENT_PRIORITY", "SPIRA_HOME_REPO", "SPIRA_ASK_LABEL", "SPIRA_REPO_MAP", "SPIRA_CONCIERGE_INBOX", "SPIRA_PLAN_LABEL"];

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
    pub spool_dir: std::path::PathBuf,
    pub log_path: std::path::PathBuf,
    pub fenrir_at: u32,
    pub fenrir_exempt: bool,
    pub dedup_lookback_days: i64,
    pub watcher_interval_s: i64,
    pub cause: String,
    pub labels: String,
    pub plan_labels: Option<String>,
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
}

impl Env {
    pub fn load() -> Env {
        let spira_run = env_or("SPIRA_RUN", system_paths::TMP);
        let incident_label = env_or("SPIRA_INCIDENT_LABEL", "incident");
        Env {
            db: env("SPIRA_DB"),
            spool_dir: std::path::PathBuf::from(env_or("SPIRA_SPOOL", &format!("{spira_run}/incident-spool"))),
            log_path: std::path::PathBuf::from(env_or("SPIRA_INCIDENT_LOG", &format!("{spira_run}/incident.log"))),
            fenrir_at: env("SPIRA_FENRIR_AT").and_then(|v| v.parse().ok()).unwrap_or(5),
            fenrir_exempt: env("SPIRA_FENRIR_EXEMPT").as_deref() == Some("1"),
            dedup_lookback_days: env("SPIRA_INCIDENT_DEDUP_LOOKBACK").and_then(|v| v.parse().ok()).unwrap_or(7),
            watcher_interval_s: env("SPIRA_WATCHER_INTERVAL_S").and_then(|v| v.parse().ok()).unwrap_or(1800),
            cause: decide::sanitize_cause(&env_or("SPIRA_INCIDENT_CAUSE", "unrecorded")),
            labels: decide::incident_labels(env("SPIRA_INCIDENT_LABELS").as_deref(), env("SPIRA_SCOPE_LABEL").as_deref(), &incident_label),
            plan_labels: env("SPIRA_PLAN_LABEL").map(|p| decide::incident_labels(None, env("SPIRA_SCOPE_LABEL").as_deref(), &p)),
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
            spira_run,
        }
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
            plan_labels: self.plan_labels.as_deref(),
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
    let Some(map_path) = env("SPIRA_REPO_MAP") else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&map_path) else { return Vec::new() };
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

pub fn ilog(env: &Env, msg: &str) {
    let line = format!("{} incident: {msg}\n", now_iso());
    eprint!("{line}");
    if let Some(parent) = env.log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&env.log_path) {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
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
        ilog(env, &format!("alarm attempt for {reference} not recorded: {e}"));
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

/// File the incident that owns an alarm. A filing that does not land is recorded as a failed
/// alarm attempt carrying the finding text, since nothing is spooled for it.
pub fn own_alarm(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, reference: &str, body: &[u8]) {
    let provenance = decide::provenance(&env.unit, &hostname(), &env.path);
    let cause = decide::sanitize_cause(&env.cause);
    let mut cfg = env.file_config(&provenance);
    cfg.cause = &cause;
    let mut log = Vec::new();
    let outcome = with_incident_lock(&env.lock_path, env.lock_wait_s, || {
        run::alarm_owner(bd, mailer, clock, &cfg, reference, title, body, &env.labels, &mut log)
    })
    .flatten();
    for l in &log {
        ilog(env, l);
    }
    let failed = match &outcome {
        Some(FileOutcome::LaneRefused(reason)) => Some(reason.clone()),
        Some(other) => match crate::attempts::of_file(other) {
            Some(tsd::alarm::Outcome::Failed { reason }) => Some(reason),
            _ => None,
        },
        None => None,
    };
    if let Some(reason) = failed {
        let reason = reason.strip_suffix("; the event stays spooled").unwrap_or(&reason);
        let reason = format!("{reason}; not spooled — the finding text is in this attempt's message");
        record_attempt(env, "incident-alarm-owner", "incident", reference, tsd::alarm::Outcome::Failed { reason }, Some(note_of(title, body)));
        ilog(env, &format!("incident: {reference} was not filed; its text was recorded as a failed alarm attempt"));
    }
}

/// `drain_one`: process one spool entry — file it, and on success remove the spool file.
/// `exec 8>>"$lock"; flock -w "$wait" 8`: one lock for the whole intake (not one per
/// ref), so two callers racing the check-then-create pair on the SAME ref (the watchtower
/// timer and a hand-run, historically sp-aapz/sp-uvfq filed the same second) serialise
/// into a filing and a recurrence instead of two beads. `None` on a timed-out wait — the
/// caller must leave the spool entry in place; a lock held 30s by someone else is the
/// write-ahead spool behaving exactly as designed, not a loss.
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

/// `Err` is why the entry stays spooled.
pub fn drain_one(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, path: &std::path::Path) -> Result<Option<String>, String> {
    match drain_entry(env, bd, mailer, clock, path, &[], None)? {
        Settled::Here(id) => Ok(id),
        Settled::Elsewhere => Ok(None),
    }
}

enum Settled {
    /// This call removed the entry, or moved it aside.
    Here(Option<String>),
    /// Another filer removed it first; nothing was filed.
    Elsewhere,
}

/// `drain_one`, with `preface` opening the filed body and `also` the entries the same filing settles.
fn drain_entry(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, path: &std::path::Path, also: &[std::path::PathBuf], preface: Option<&str>) -> Result<Settled, String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settled::Elsewhere),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let Some(mut entry) = spool::parse_entry(&raw) else {
        ilog(env, &format!("spool entry with no REF: {} — moved aside", path.display()));
        let bad = path.with_extension("bad");
        let _ = std::fs::rename(path, &bad);
        return Ok(Settled::Here(None));
    };
    if entry.body.is_empty() {
        entry.body = format!(
            "The intake gathered NO payload for {}.\nsystemctl produced nothing — suspect the unit name.\n",
            entry.reference
        )
        .into_bytes();
    }
    if let Some(line) = preface {
        entry.body = [format!("{line}\n\n").into_bytes(), entry.body].concat();
    }
    let provenance = decide::provenance(&entry.unit, &hostname(), &entry.incident_path);
    let cause = decide::sanitize_cause(&entry.cause);
    let mut cfg = env.file_config(&provenance);
    cfg.cause = &cause;
    cfg.repo_declared = entry.repo.as_deref().or(env.repo.as_deref());
    let mut log = Vec::new();
    let alarm_label = env_or("SPIRA_ALARM_LABEL", "alarm");
    let file_labels = if entry.reference.starts_with("incident:") && !env.labels.split(',').any(|l| l == alarm_label) {
        format!("{},{}", env.labels, alarm_label)
    } else {
        env.labels.clone()
    };
    // Looked for, filed and removed under one hold, so two drains of one entry file it once.
    let locked = with_incident_lock(&env.lock_path, env.lock_wait_s, || {
        if !path.exists() {
            return None;
        }
        let outcome = run::file_one(bd, mailer, clock, &cfg, &entry.reference, &entry.title, &entry.body, &file_labels, &mut log);
        if matches!(outcome, FileOutcome::Filed(_) | FileOutcome::Skew) {
            for settled in also.iter().map(std::path::PathBuf::as_path).chain([path]) {
                if let Err(e) = std::fs::remove_file(settled) {
                    log.push(format!("{}: settled but not removed: {e}", settled.display()));
                }
            }
        }
        Some(outcome)
    });
    let Some(outcome) = locked else {
        let held = format!("another intake held {} for {}s — {} stays spooled, drain will retry", env.lock_path.display(), env.lock_wait_s, entry.reference);
        ilog(env, &held);
        record_attempt(env, "incident-file", "incident", &entry.reference, tsd::alarm::Outcome::Failed { reason: "another intake held the lock; the event stays spooled".into() }, Some(note_of(&entry.title, &entry.body)));
        return Err(held);
    };
    let Some(outcome) = outcome else { return Ok(Settled::Elsewhere) };
    for l in &log {
        ilog(env, l);
    }
    let attempt = match &outcome {
        FileOutcome::LaneRefused(reason) => {
            Some(tsd::alarm::Outcome::Failed { reason: format!("{reason}; {} stays spooled until the repo admits the lane", path.display()) })
        }
        other => crate::attempts::of_file(other),
    };
    if let Some(o) = attempt {
        let message = matches!(o, tsd::alarm::Outcome::Failed { .. }).then(|| note_of(&entry.title, &entry.body));
        record_attempt(env, "incident-file", "incident", &entry.reference, o, message);
    }
    match outcome {
        FileOutcome::Filed(id) => Ok(Settled::Here(Some(id))),
        FileOutcome::Skew => Ok(Settled::Here(None)),
        FileOutcome::Unread(_) | FileOutcome::Refused => Err(log.pop().unwrap_or_else(|| format!("{} was not filed", entry.reference))),
        FileOutcome::LaneRefused(reason) => Err(format!("{reason}; {} stays spooled until the repo admits the lane", path.display())),
    }
}

pub const BATCH_KEY: &str = "SPIRA_INCIDENT_DRAIN_BATCH";

pub fn parse_batch(raw: &str) -> Result<usize, String> {
    raw.trim().parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| format!("{BATCH_KEY} must be a positive number of references, got {raw:?} — refusing"))
}

pub fn configured_batch() -> Result<usize, String> {
    parse_batch(&spira_config::process::cfg(BATCH_KEY)?)
}

/// What one pass over the spool did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// References settled by their own filing, and malformed entries moved aside.
    pub filed: usize,
    /// The other entries those filings settled.
    pub duplicates: usize,
    /// Entries a filing was tried for and that stay, oldest reference first, each with why.
    pub stuck: Vec<(std::path::PathBuf, String)>,
    /// References past the pass's batch, left for the next one.
    pub held_back: usize,
    /// The entries of those references.
    pub held_entries: usize,
}

impl DrainReport {
    pub fn drained(&self) -> usize {
        self.filed + self.duplicates
    }

    pub fn still_spooled(&self) -> usize {
        self.stuck.len() + self.held_entries
    }
}

/// One pass: each of the oldest `batch` references is filed once, from its newest event, and
/// that filing settles every entry of the reference.
pub fn drain_spool(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, batch: usize) -> DrainReport {
    let mut report = DrainReport::default();
    let paths = match spool::entries(&env.spool_dir) {
        Ok(p) => p,
        Err(e) => {
            let why = format!("the spool {} cannot be read: {e}", env.spool_dir.display());
            ilog(env, &why);
            report.stuck.push((env.spool_dir.clone(), why));
            return report;
        }
    };
    let mut groups: Vec<Vec<std::path::PathBuf>> = Vec::new();
    let mut group_of = std::collections::HashMap::new();
    for p in paths {
        let raw = match std::fs::read(&p) {
            Ok(raw) => raw,
            // Another drain settled it since the listing.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                let why = format!("{}: {e}", p.display());
                report.stuck.push((p, why));
                continue;
            }
        };
        let Some(entry) = spool::parse_entry(&raw) else {
            match drain_one(env, bd, mailer, clock, &p) {
                Ok(_) => report.filed += 1,
                Err(why) => report.stuck.push((p, why)),
            }
            continue;
        };
        let at = *group_of.entry(decide::bound_ref(&entry.reference)).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[at].push(p);
    }
    for (i, group) in groups.into_iter().enumerate() {
        if i >= batch {
            report.held_back += 1;
            report.held_entries += group.len();
            continue;
        }
        let Some((oldest, newest)) = group.first().zip(group.last()) else { continue };
        let stamp = |p: &std::path::Path| spool::stamp_of(p).unwrap_or("an unrecorded time").to_string();
        let preface = (group.len() > 1).then(|| format!("{} events of this reference were spooled from {} to {}; this is the newest.", group.len(), stamp(oldest), stamp(newest)));
        match drain_entry(env, bd, mailer, clock, newest, &group[..group.len() - 1], preface.as_deref()) {
            Ok(Settled::Here(_)) => {
                report.filed += 1;
                report.duplicates += group.len() - 1;
            }
            // Whoever removed the newest settles the rest, or the next pass does.
            Ok(Settled::Elsewhere) => {}
            Err(why) => report.stuck.extend(group.into_iter().map(|p| (p, why.clone()))),
        }
    }
    if report.held_back > 0 {
        ilog(env, &format!("held back {} reference(s) for the next pass", report.held_back));
    }
    report
}

pub fn drain_report(r: &DrainReport) -> String {
    format!("drained {}, still spooled {}", r.drained(), r.still_spooled())
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

    let entry = spool::SpoolEntry {
        reference: reference.clone(),
        title: format!("incident: {unit} failed"),
        unit: unit.to_string(),
        incident_path: unit.to_string(),
        cause: env.cause.clone(),
        repo: env.repo.clone(),
        body: payload.into_bytes(),
    };
    let _ = std::fs::create_dir_all(&env.spool_dir);
    let stamp = spool_stamp();
    let sp = spool::entry_path(&env.spool_dir, &stamp, &reference, std::process::id());
    if spool::write_entry(&sp, &entry).is_err() {
        return ExitCode::FAILURE;
    }
    match drain_one(env, bd, mailer, clock, &sp) {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => {
            ilog(env, &format!("spooled {reference} at {}", sp.display()));
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
/// or null when nothing was filed (activation skew, or a spool entry with no REF set aside).
/// Callers deserialise it.
pub fn filed_answer(id: Option<&str>) -> String {
    serde_json::json!({ "id": id }).to_string()
}

/// `incident file <title>` with its payload in hand: spooled, then filed. `false` when the
/// event could not be spooled or stays spooled for `drain`.
pub fn file_body(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, body: Vec<u8>) -> bool {
    file_body_id(env, bd, mailer, clock, title, body).is_ok()
}

/// `file_body`, naming the bead the event landed on: `Ok(None)` is a skew with nothing filed,
/// `Err` is an event left spooled.
#[allow(clippy::result_unit_err)]
pub fn file_body_id(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, body: Vec<u8>) -> Result<Option<String>, ()> {
    let reference = env.repo_override_ref().unwrap_or_else(|| {
        let slug: String = title.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' }).take(60).collect();
        format!("incident:{slug}")
    });
    let entry = spool::SpoolEntry {
        reference: reference.clone(),
        title: title.to_string(),
        unit: env.unit.clone(),
        incident_path: env.path.clone(),
        cause: env.cause.clone(),
        repo: env.repo.clone(),
        body,
    };
    let _ = std::fs::create_dir_all(&env.spool_dir);
    let stamp = spool_stamp();
    let sp = spool::entry_path(&env.spool_dir, &stamp, &reference, std::process::id());
    if spool::write_entry(&sp, &entry).is_err() {
        return Err(());
    }
    drain_one(env, bd, mailer, clock, &sp).map_err(|_| ilog(env, &format!("spooled {reference} at {}", sp.display())))
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
    file_body_id(&env, &bd, &crate::real::RealMailer, &crate::real::RealClock, title, body.to_vec())
        .map_err(|()| format!("incident file {title:?}: not filed; it stays spooled in {}", env.spool_dir.display()))
}

impl Env {
    pub fn repo_override_ref(&self) -> Option<String> {
        self.reference.clone()
    }
}

/// `date -u +%Y%m%dT%H%M%SZ` — the spool filename's timestamp component. Built by
/// stripping the punctuation from `iso_now_public`'s output, so this binary keeps exactly
/// one date formatter.
pub fn spool_stamp() -> String {
    let iso = crate::run::iso_now_public(now_epoch());
    iso.chars().filter(|c| *c != '-' && *c != ':').collect()
}


#[cfg(test)]
mod tests {
    const BATCH: usize = 20;
    use super::{drain_one, drain_report, drain_spool, parse_batch, DrainReport, filed_answer, own_alarm, priority_of, with_incident_lock, Env};
    use crate::fake::{FakeBd, FakeMailer, FixedClock};
    use crate::spool::{format_entry, SpoolEntry};
    use std::cell::RefCell;
    use std::sync::{mpsc, Arc, Mutex};

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

    fn env_in(dir: &std::path::Path) -> Env {
        Env {
            db: Some("db".into()),
            spool_dir: dir.join("spool"),
            log_path: dir.join("incident.log"),
            fenrir_at: 5,
            fenrir_exempt: false,
            dedup_lookback_days: 7,
            watcher_interval_s: 1800,
            cause: "unrecorded".into(),
            labels: "spira,incident".into(),
            plan_labels: None,
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
        }
    }

    fn spool(env: &Env, name: &str, raw: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(&env.spool_dir).unwrap();
        let p = env.spool_dir.join(name);
        std::fs::write(&p, raw).unwrap();
        p
    }

    fn entry(reference: &str) -> String {
        let e = SpoolEntry { reference: reference.into(), title: "t".into(), unit: "u".into(), incident_path: "p".into(), cause: "c".into(), repo: None, body: b"payload".to_vec() };
        String::from_utf8(format_entry(&e)).unwrap()
    }


    #[test_id(tid9pph)]
    #[test]
    fn an_alarm_whose_owner_cannot_be_filed_records_its_text_as_a_failed_attempt() {
        let dir = testkit::TempDir::new("own-alarm-refused");
        let env = env_in(&dir);
        let bd = FakeBd { create_error: Some("bead: repo:spira does not admit lane incident — refused by its admitted lanes (plan,groom,spike)".into()), ..FakeBd::new() };
        bd.rows.borrow_mut().push(crate::decide::BeadRow {
            id: "sp-old".into(),
            status: crate::decide::BeadStatus::Terminal,
            external_ref: Some("incident:unit-drift".into()),
            labels: vec![format!("ref:{}", crate::decide::ref_hash("incident:unit-drift"))],
            closed_at: Some("2026-10-08T00:00:00Z".into()),
        });
        own_alarm(&env, &bd, &FakeMailer::default(), &FixedClock(1_792_800_000), "UNIT DRIFT: x", "incident:unit-drift", b"UNSHIPPED /u/x.service (unit file this install does not ship)\n");
        let rows = tsd::alarm::read(std::path::Path::new(&env.spira_run));
        let row = rows.iter().find(|a| a.reference == "incident:unit-drift").expect("an attempt is recorded");
        let tsd::alarm::Outcome::Failed { reason } = &row.outcome else { panic!("{row:?}") };
        assert!(reason.contains("repo:spira"), "{reason}");
        assert!(row.message.as_ref().unwrap().body.contains("UNSHIPPED /u/x.service"));
        assert!(std::fs::read_to_string(&env.log_path).unwrap().contains("recorded as a failed alarm attempt"));
    }


    #[test_id(t56wsj2)]
    #[test]
    fn an_alarm_with_no_bead_history_records_no_owner_attempt() {
        let dir = testkit::TempDir::new("own-alarm-none");
        let env = env_in(&dir);
        own_alarm(&env, &FakeBd::new(), &FakeMailer::default(), &FixedClock(1_792_800_000), "t", "incident:never-filed", b"p");
        assert!(tsd::alarm::read(std::path::Path::new(&env.spira_run)).is_empty());
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

    // covers: UC-ops-detection-remediation-02
    #[test_id(t3wf6yv)]
    #[test]
    fn a_filer_that_cannot_get_the_lock_in_time_leaves_its_spool_entry_in_place() {
        let dir = testkit::TempDir::new("incident-lock-held");
        let env = env_in(&dir);
        let p = spool(&env, "20260101T000000Z-a", &entry("incident:held"));
        let bd = FakeBd::new();
        let mailer = FakeMailer::default();
        let held = with_incident_lock(&env.lock_path, 0, || drain_spool(&env, &bd, &mailer, &FixedClock(1_000_000), BATCH));
        assert_eq!(held.map(|r| (r.drained(), r.stuck.len())), Some((0, 1)));
        assert!(p.exists(), "the entry stays spooled for the next drain");
        assert!(bd.created.borrow().is_empty());
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(t21puk7)]
    #[test]
    fn an_unreachable_bd_keeps_the_entry_spooled_and_a_later_drain_files_it() {
        let dir = testkit::TempDir::new("incident-drain-unreachable");
        let env = env_in(&dir);
        let p = spool(&env, "20260101T000000Z-a", &entry("incident:later"));
        let mailer = FakeMailer::default();
        let down = FakeBd { reachable: false, next_id: RefCell::new(1), ..Default::default() };
        let report = drain_spool(&env, &down, &mailer, &FixedClock(1_000_000), BATCH);
        assert_eq!((report.drained(), report.stuck.len()), (0, 1));
        assert_eq!(drain_report(&report), "drained 0, still spooled 1");
        assert!(p.exists() && down.created.borrow().is_empty());

        let up = FakeBd::new();
        let report = drain_spool(&env, &up, &mailer, &FixedClock(1_000_000), BATCH);
        assert_eq!(drain_report(&report), "drained 1, still spooled 0");
        assert!(!p.exists());
        assert_eq!(up.created.borrow().len(), 1);
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(tqrjzkn)]
    #[test]
    fn a_lane_refusal_surfaces_as_a_failed_attempt_naming_the_repo_and_the_spool_file() {
        let dir = testkit::TempDir::new("incident-drain-lane");
        let env = env_in(&dir);
        let p = spool(&env, "20260101T000000Z-a", &entry("incident:lane"));
        let bd = FakeBd { create_error: Some("bead: repo:walled does not admit lane incident — refused by its admitted lanes (plan)\nbead: override".into()), ..FakeBd::new() };
        let report = drain_spool(&env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), BATCH);
        assert_eq!((report.drained(), report.stuck.len()), (0, 1));
        assert!(p.exists());
        let attempts = tsd::alarm::read(std::path::Path::new(&env.spira_run));
        let tsd::alarm::Outcome::Failed { reason } = &attempts.last().expect("an attempt is recorded").outcome else { panic!("{attempts:?}") };
        assert!(reason.contains("repo:walled") && reason.contains(&p.display().to_string()), "{reason}");
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(t2etwc7)]
    #[test]
    fn an_entry_with_no_ref_is_moved_aside_and_never_filed() {
        let dir = testkit::TempDir::new("incident-drain-noref");
        let env = env_in(&dir);
        let p = spool(&env, "20260101T000000Z-a", "TITLE: orphan\n--\nbody");
        let bd = FakeBd::new();
        let mailer = FakeMailer::default();
        assert_eq!(drain_spool(&env, &bd, &mailer, &FixedClock(1_000_000), BATCH).drained(), 1);
        assert!(!p.exists() && p.with_extension("bad").exists());
        assert!(bd.created.borrow().is_empty());
        assert_eq!(drain_spool(&env, &bd, &mailer, &FixedClock(1_000_000), BATCH), DrainReport::default(), "a .bad entry is not drained again");
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(tiiwyfr)]
    #[test]
    fn an_entry_spooled_before_causes_existed_drains_as_unrecorded() {
        let dir = testkit::TempDir::new("incident-drain-precause");
        let env = env_in(&dir);
        let old = "REF: incident:old\nTITLE: t\nUNIT: u\nINCIDENT_PATH: p\n--\npayload";
        let bd = FakeBd::new();
        let mailer = FakeMailer::default();
        for name in ["20260101T000000Z-a", "20260101T000001Z-b"] {
            spool(&env, name, old);
            assert_eq!(drain_spool(&env, &bd, &mailer, &FixedClock(1_000_000), BATCH).drained(), 1);
        }
        let facts = bd.facts.borrow();
        assert_eq!(facts.len(), 1, "the second entry recurs the first");
        assert_eq!(facts[0].2, "unrecorded");
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(tsjuvom)]
    #[test]
    fn a_blind_dedup_records_its_pass_and_the_stores_error_as_the_attempt_reason() {
        let dir = testkit::TempDir::new("incident-drain-blind");
        let env = env_in(&dir);
        let p = spool(&env, "20260101T000000Z-a", &entry("incident:blind"));
        let bd = FakeBd { lc_down: true, ..FakeBd::new() };
        drain_spool(&env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), BATCH);
        let attempts = tsd::alarm::read(std::path::Path::new(&env.spira_run));
        let tsd::alarm::Outcome::Failed { reason } = &attempts.last().expect("an attempt is recorded").outcome else { panic!("{attempts:?}") };
        assert!(reason.contains("Unfinished label-keyed pass") && reason.contains("spira-lc: down"), "{reason}");
        assert!(!reason.contains("could not be reached"), "{reason}");
        assert!(p.exists());
    }

    // covers: UC-ops-detection-remediation-10
    #[test_id(t7zh7fk)]
    #[test]
    fn an_alarm_owner_whose_create_fails_records_the_create_error() {
        let dir = testkit::TempDir::new("own-alarm-create-failed");
        let env = env_in(&dir);
        let bd = FakeBd { create_error: Some("boom: disk full".into()), ..FakeBd::new() };
        bd.rows.borrow_mut().push(crate::decide::BeadRow {
            id: "sp-old".into(),
            status: crate::decide::BeadStatus::Terminal,
            external_ref: Some("incident:unit-drift".into()),
            labels: vec![format!("ref:{}", crate::decide::ref_hash("incident:unit-drift"))],
            closed_at: Some("2026-10-08T00:00:00Z".into()),
        });
        own_alarm(&env, &bd, &FakeMailer::default(), &FixedClock(1_792_800_000), "UNIT DRIFT: x", "incident:unit-drift", b"body");
        let rows = tsd::alarm::read(std::path::Path::new(&env.spira_run));
        let row = rows.iter().find(|a| a.reference == "incident:unit-drift").expect("an attempt is recorded");
        let tsd::alarm::Outcome::Failed { reason } = &row.outcome else { panic!("{row:?}") };
        assert!(reason.contains("create failed (boom: disk full)") && reason.contains("not spooled"), "{reason}");
    }

    #[test_id(tkvxiks)]
    #[test]
    fn an_event_spooled_while_the_store_is_down_is_filed_once_when_it_returns() {
        let dir = testkit::TempDir::new("incident-drain-store-down");
        let env = env_in(&dir);
        let p = spool(&env, "20260101T000000Z-a", &entry("incident:outage"));
        let (mailer, clock) = (FakeMailer::default(), FixedClock(1_000_000));
        let down = FakeBd { lc_down: true, ..FakeBd::new() };
        let report = drain_spool(&env, &down, &mailer, &clock, BATCH);
        assert!(p.exists() && down.created.borrow().is_empty());
        assert!(report.stuck[0].1.contains("spira-lc: down"), "{report:?}");

        let up = FakeBd { rows: RefCell::new(down.rows.borrow().clone()), ..FakeBd::new() };
        drain_spool(&env, &up, &mailer, &clock, BATCH);
        assert_eq!(up.created.borrow().len(), 1);
        assert!(crate::spool::entries(&env.spool_dir).unwrap().is_empty());
        assert_eq!(drain_spool(&env, &up, &mailer, &clock, BATCH), DrainReport::default());
        assert_eq!(up.created.borrow().len(), 1);
    }

    #[test_id(tzpzzvf)]
    #[test]
    fn a_hundred_events_of_one_reference_file_one_item_carrying_the_count() {
        let dir = testkit::TempDir::new("incident-drain-hundred");
        let env = env_in(&dir);
        for i in 0..100 {
            let e = SpoolEntry { reference: "incident:flood".into(), title: "t".into(), unit: "u".into(), incident_path: "p".into(), cause: "c".into(), repo: None, body: format!("event {i}").into_bytes() };
            spool(&env, &format!("20260101T00{:02}{:02}Z-flood-1", i / 60, i % 60), &String::from_utf8(format_entry(&e)).unwrap());
        }
        let bd = FakeBd::new();
        let report = drain_spool(&env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), BATCH);
        assert_eq!((report.filed, report.duplicates, report.stuck.len()), (1, 99, 0));
        assert_eq!(drain_report(&report), "drained 100, still spooled 0");
        assert_eq!(bd.created.borrow().len(), 1);
        assert!(bd.facts.borrow().is_empty(), "{:?}", bd.facts.borrow());
        let body = bd.bodies.borrow().values().next().cloned().unwrap();
        assert!(body.starts_with("100 events of this reference were spooled from 20260101T000000Z to 20260101T000139Z; this is the newest."), "{body}");
        assert!(body.contains("event 99") && !body.contains("event 98"), "{body}");
        assert!(crate::spool::entries(&env.spool_dir).unwrap().is_empty());
    }

    #[test_id(trbl8tj)]
    #[test]
    fn a_pass_files_at_most_a_batch_of_references_oldest_first_and_says_what_it_held_back() {
        let dir = testkit::TempDir::new("incident-drain-batch");
        let env = env_in(&dir);
        spool(&env, "20260101T000000Z-c", &entry("incident:first"));
        spool(&env, "20260101T000001Z-a", &entry("incident:second"));
        let third = spool(&env, "20260101T000002Z-b", &entry("incident:third"));
        spool(&env, "20260101T000003Z-c", &entry("incident:first"));
        let bd = FakeBd::new();
        let report = drain_spool(&env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), 2);
        assert_eq!((report.filed, report.duplicates, report.held_back), (2, 1, 1));
        let refs: Vec<String> = bd.created.borrow().iter().map(|c| c.1.clone()).collect();
        assert_eq!(refs, ["incident:first", "incident:second"]);
        assert_eq!(crate::spool::entries(&env.spool_dir).unwrap(), [third]);
        assert_eq!(drain_report(&report), "drained 3, still spooled 1");
        assert!(std::fs::read_to_string(&env.log_path).unwrap().contains("held back 1 reference(s) for the next pass"));
    }

    #[test_id(tz4mpo9)]
    #[test]
    fn a_reference_that_cannot_be_filed_keeps_every_one_of_its_entries() {
        let dir = testkit::TempDir::new("incident-drain-group-stuck");
        let env = env_in(&dir);
        let a = spool(&env, "20260101T000000Z-a", &entry("incident:walled"));
        let b = spool(&env, "20260101T000001Z-a", &entry("incident:walled"));
        let bd = FakeBd { create_error: Some("bead: repo:walled does not admit lane incident".into()), ..FakeBd::new() };
        let report = drain_spool(&env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), BATCH);
        assert_eq!(report.stuck.iter().map(|s| s.0.clone()).collect::<Vec<_>>(), [a.clone(), b.clone()]);
        assert!(report.stuck[0].1.contains("repo:walled"), "{report:?}");
        assert!(a.exists() && b.exists());
    }

    #[test_id(tclfp9y)]
    #[test]
    fn a_drain_leaves_an_entry_still_being_written_alone() {
        let dir = testkit::TempDir::new("incident-drain-partial");
        let env = env_in(&dir);
        let partial = spool(&env, ".20260101T000000Z-a-1", "");
        let bd = FakeBd::new();
        assert_eq!(drain_spool(&env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), BATCH), DrainReport::default());
        assert!(partial.exists() && !partial.with_extension("bad").exists());
    }

    #[test_id(te9p14f)]
    #[test]
    fn a_batch_that_would_file_nothing_is_refused() {
        assert_eq!(parse_batch("20"), Ok(20));
        for bad in ["0", "", "many"] {
            assert!(parse_batch(bad).unwrap_err().contains("SPIRA_INCIDENT_DRAIN_BATCH"), "{bad:?}");
        }
    }

    #[test_id(tpez4hk)]
    #[test]
    fn an_entry_another_filer_already_removed_is_not_filed_again() {
        let dir = testkit::TempDir::new("incident-drain-raced");
        let env = env_in(&dir);
        let bd = FakeBd::new();
        let gone = env.spool_dir.join("20260101T000000Z-a-1");
        assert_eq!(drain_one(&env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), &gone), Ok(None));
        assert!(bd.created.borrow().is_empty());
    }

    #[test_id(t9km7dn)]
    #[test]
    fn two_drains_of_one_spool_file_each_reference_once() {
        let dir = testkit::TempDir::new("incident-drain-twice");
        let mut env = env_in(&dir);
        env.lock_wait_s = 30;
        for name in ["20260101T000000Z-a-1", "20260101T000001Z-a-1", "20260101T000002Z-a-1"] {
            spool(&env, name, &entry("incident:contended"));
        }
        let env = &env;
        let (inside_tx, inside_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        struct Paused(mpsc::Sender<()>, mpsc::Receiver<()>);
        impl crate::ports::Clock for Paused {
            fn now(&self) -> i64 {
                let _ = self.0.send(());
                let _ = self.1.recv();
                1_000_000
            }
        }
        let (first, second) = std::thread::scope(|scope| {
            // The clock is read inside the lock, before the filing.
            let first = scope.spawn(move || {
                let bd = FakeBd::new();
                drain_spool(env, &bd, &FakeMailer::default(), &Paused(inside_tx, go_rx), BATCH);
                bd.created.take()
            });
            inside_rx.recv().unwrap();
            let second = scope.spawn(move || {
                let bd = FakeBd::new();
                let report = drain_spool(env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), BATCH);
                (bd.created.take(), report)
            });
            drop(go_tx);
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_eq!(first.len(), 1);
        assert_eq!(second, (vec![], DrainReport::default()));
    }

    const LIVE_MAP: &str = "spira | /r/spira | push | origin/main | | true | plan,groom,spike,incident\ndumpstar | /r/dumpstar | push | origin/main | | true | plan\n";

    /// Drain one spooled event through a door that judges as the live one does.
    fn drain_one_event(tag: &str, reference: &str, repo: Option<&str>) -> (DrainReport, FakeBd, usize) {
        let dir = testkit::TempDir::new(tag);
        let mut env = env_in(&dir);
        env.repo = None;
        env.known_repos = vec!["spira".into(), "dumpstar".into()];
        env.plan_labels = Some("spira,plan".into());
        let e = SpoolEntry { reference: reference.into(), title: "t".into(), unit: "u".into(), incident_path: "p".into(), cause: "c".into(), repo: repo.map(str::to_string), body: b"payload".to_vec() };
        spool(&env, "20260101T000000Z-a", &String::from_utf8(format_entry(&e)).unwrap());
        let bd = FakeBd { admits: Some(LIVE_MAP.into()), ..FakeBd::new() };
        let report = drain_spool(&env, &bd, &FakeMailer::default(), &FixedClock(1_000_000), BATCH);
        let left = crate::spool::entries(&env.spool_dir).unwrap().len();
        (report, bd, left)
    }

    fn only_create(bd: &FakeBd) -> (Vec<String>, String) {
        let labels = bd.create_labels.borrow().values().next().expect("one create").split(',').map(str::to_string).collect();
        (labels, bd.bodies.borrow().values().next().cloned().unwrap())
    }

    // covers: UC-ops-detection-remediation-05
    #[test_id(t26bog2)]
    #[test]
    fn a_product_repos_landing_ref_red_files_against_that_repo_in_the_plan_lane() {
        let (report, bd, left) = drain_one_event("route-product-red", "basefail-local:dumpstar:unit-tests:FAIL:1a2b3c4d", Some("dumpstar"));
        assert_eq!((report.filed, report.stuck.clone(), left), (1, vec![], 0));
        let (labels, body) = only_create(&bd);
        assert!(labels.contains(&"repo:dumpstar".to_string()) && labels.contains(&"plan".to_string()), "{labels:?}");
        assert!(!labels.iter().any(|l| l == "incident" || l == "alarm"), "{labels:?}");
        assert!(bead::has_build_spec(&body), "{body}");
    }

    // covers: UC-ops-detection-remediation-05
    #[test_id(tit421s)]
    #[test]
    fn a_harness_ref_that_declares_no_repo_files_against_the_home_repo_in_the_incident_lane() {
        let (report, bd, left) = drain_one_event("route-undeclared", "incident:refused-ContentOnBase-from-LANDED--Terminal--by-landing-pass", None);
        assert_eq!((report.filed, report.stuck.clone(), left), (1, vec![], 0));
        let (labels, _) = only_create(&bd);
        assert!(labels.contains(&"repo:spira".to_string()) && labels.contains(&"incident".to_string()), "{labels:?}");
        assert!(!bd.rows.borrow()[0].labels.iter().any(|l| l == "needs-repo-triage"), "{:?}", bd.rows.borrow()[0].labels);
    }

    // covers: UC-ops-detection-remediation-05
    #[test_id(teiezgo)]
    #[test]
    fn a_harness_fault_naming_a_product_repo_files_on_the_home_repo_and_keeps_the_repo_it_concerns() {
        let (report, bd, left) = drain_one_event("route-concerns", "land-deploy-fault:dumpstar:abc123", Some("dumpstar"));
        assert_eq!((report.filed, report.stuck.clone(), left), (1, vec![], 0));
        let (labels, body) = only_create(&bd);
        assert!(labels.contains(&"repo:spira".to_string()) && labels.contains(&"incident".to_string()), "{labels:?}");
        assert!(body.starts_with("Concerns repository: dumpstar\n\n"), "{body}");
    }

    // covers: UC-ops-detection-remediation-05
    #[test_id(t0vhkmq)]
    #[test]
    fn a_home_repo_stall_naming_a_product_in_its_ref_stays_in_the_incident_lane() {
        let (report, bd, left) = drain_one_event("route-stall", "incident:throughput-stall:dumpstar:1795797689", Some("spira"));
        assert_eq!((report.filed, report.stuck.clone(), left), (1, vec![], 0));
        let (labels, _) = only_create(&bd);
        assert!(labels.contains(&"repo:spira".to_string()) && labels.contains(&"incident".to_string()), "{labels:?}");
    }
}
