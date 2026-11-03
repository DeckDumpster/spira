//! incident — CLI entry point. DESIGN.md has the subcommand contract in full; this file
//! only reads the environment and argv, builds a `run::FileConfig`, and dispatches.
//!
//!   incident systemd <unit>           file an incident for a failed systemd user unit
//!   incident file <title> [-|<file>]  file one from an arbitrary payload
//!   incident alarm <title> [-|<file>]  note a detector's condition in the Concierge inbox, no bead
//!   incident drain                    file everything the spool is holding
//!   incident list                     open incidents
//!   incident settle                   close incidents whose fixes landed and whose detector is quiet
//!   incident collapse <dup> --of <keep>  mark <dup> a duplicate of <keep> and label it for the meter
//!   incident alarms-pass [--no-fallback]  the alarms component's minute pass
//!   incident floor-cleared            exit 0 once the goodput rate is back at its floor
//!   incident page --no-fallback --probe <mailbox> [--interval S] | --probe-health [--max-age S]
//!
//! `backfill-ref-labels`, `retire-unsatisfiable-delivers` and `repair-mismatch-delivers`
//! are retired (DESIGN.md §Decisions): one-time migrations with no live caller, already
//! believed applied everywhere the ref: label and the delivers: schema matter.

use std::io::Read;
use std::process::ExitCode;

use incident::decide;
use incident::ports::{Bd, Clock, Mailer};
use incident::real::{RealBd, RealClock, RealMailer};
use incident::run::{self, FileConfig, FileOutcome};
use incident::spool;

/// `std::env::var`, trimmed to "set and non-empty" — no `spira_config` fallback. Used only
/// inside [`resolved_config`]'s own init, which must not call back into [`env`] (that
/// would recurse into `resolved_config()` while it is still being built).
fn raw_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The declared harness home: `SPIRA_HOME`, else `$SPIRA_RELEASE/spira`; neither set is a
/// refusal naming them.
fn spira_home_dir() -> Result<std::path::PathBuf, String> {
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
fn resolved_config() -> &'static spira_config::resolve::Resolved {
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
const REGISTERED: &[&str] =
    &["SPIRA_RUN", "SPIRA_INCIDENT_LABEL", "SPIRA_DB", "SPIRA_WATCHER_INTERVAL_S", "SPIRA_INCIDENT_PRIORITY", "SPIRA_HOME_REPO", "SPIRA_ASK_LABEL", "SPIRA_REPO_MAP", "SPIRA_CONCIERGE_INBOX"];

/// The environment, then `spira_config::resolve()`'s in-process answer — never the
/// reverse, so an explicit env override still wins exactly as it did before this bead.
/// EXCEPT for a registered key (see [`REGISTERED`]), which this never reads from the
/// environment at all.
fn env(name: &str) -> Option<String> {
    if REGISTERED.contains(&name) {
        return spira_config::process::cfg(name).ok();
    }
    raw_env(name).or_else(|| {
        let v = resolved_config().get(name);
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn env_or(name: &str, default: &str) -> String {
    env(name).unwrap_or_else(|| default.to_string())
}

fn hostname() -> String {
    spira_config::bounded::bounded("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".to_string())
}

fn unit_from_cgroup() -> String {
    let raw = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
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
struct Env {
    db: Option<String>,
    spool_dir: std::path::PathBuf,
    log_path: std::path::PathBuf,
    fenrir_at: u32,
    fenrir_exempt: bool,
    dedup_lookback_days: i64,
    watcher_interval_s: i64,
    cause: String,
    labels: String,
    repo: Option<String>,
    kind: String,
    priority: String,
    actor: Option<String>,
    delivers_pref: Option<String>,
    sop_ledger: String,
    spira_run: String,
    home_repo: String,
    known_repos: Vec<String>,
    ask_label: String,
    express: bool,
    unit: String,
    path: String,
    lock_path: std::path::PathBuf,
    lock_wait_s: u64,
    retired_keys: Vec<String>,
}

impl Env {
    fn load() -> Env {
        let spira_run = env_or("SPIRA_RUN", "/tmp");
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
            repo: env("SPIRA_INCIDENT_REPO"),
            kind: env_or("SPIRA_INCIDENT_TYPE", "bug"),
            // conf.sh:1717 `: "${SPIRA_INCIDENT_PRIORITY:=3}"` — the bash never carried its
            // own default; it relied on conf.sh setting one before incident.sh's `bdq
            // create --priority "$SPIRA_INCIDENT_PRIORITY"` ran. Verified against a real
            // `bd create` in this session: an unset/empty --priority silently became 3.
            priority: env_or("SPIRA_INCIDENT_PRIORITY", "3"),
            actor: env("SPIRA_INCIDENT_ACTOR").or_else(|| env("BEADS_ACTOR")),
            delivers_pref: env("SPIRA_INCIDENT_DELIVERS"),
            sop_ledger: env_or("SPIRA_SOP_LEDGER", &format!("{spira_run}/sop/applied.jsonl")),
            home_repo: env_or("SPIRA_HOME_REPO", "spira"),
            known_repos: repo_names(),
            ask_label: env_or("SPIRA_ASK_LABEL", "needs-ryan"), // literal-ok: Rust fallback mirroring lib.sh's own default when SPIRA_ASK_LABEL is unset
            express: env("SPIRA_INCIDENT_EXPRESS").as_deref() == Some("1"),
            unit: env_or("SPIRA_INCIDENT_UNIT", &unit_from_cgroup()),
            path: env_or("SPIRA_INCIDENT_PATH", "?"),
            lock_path: std::path::PathBuf::from(env_or("SPIRA_INCIDENT_LOCK", &format!("{spira_run}/incident.lock"))),
            lock_wait_s: env("SPIRA_INCIDENT_LOCK_WAIT").and_then(|v| v.parse().ok()).unwrap_or(30),
            retired_keys: spira_home_dir().ok().and_then(|h| std::fs::read_to_string(h.join("config-delta.toml")).ok()).map(|t| decide::delta_removed_keys(&t)).unwrap_or_default(),
            spira_run,
        }
    }

    fn spira_run_path(&self, name: &str) -> std::path::PathBuf {
        std::path::Path::new(&self.spira_run).join(name)
    }

    fn provenance(&self) -> String {
        run::provenance(&self.unit, &hostname(), &self.path)
    }

    fn file_config<'a>(&'a self, provenance: &'a str) -> FileConfig<'a> {
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
fn repo_names() -> Vec<String> {
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

fn ilog(env: &Env, msg: &str) {
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

fn now_epoch() -> i64 {
    spira_config::vtime::now_epoch() as i64
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` — the `ilog` timestamp.
fn now_iso() -> String {
    incident::run::iso_now_public(now_epoch())
}

/// One alarm attempt in the run directory's alarm-attempt family; a row that cannot be written is
/// said on stderr, never silently dropped.
fn record_attempt(env: &Env, caller: &str, kind: &str, reference: &str, outcome: tsd::alarm::Outcome, message: Option<tsd::alarm::Message>) {
    let a = tsd::alarm::Attempt { at: now_epoch().max(0) as u64, caller: caller.into(), kind: kind.into(), reference: reference.into(), outcome, message };
    if let Err(e) = a.write(std::path::Path::new(&env.spira_run), &hostname()) {
        ilog(env, &format!("alarm attempt for {reference} not recorded: {e}"));
    }
}

/// What a retry sends when an incident reached no one: the event as a note to the Concierge.
fn note_of(title: &str, body: &[u8]) -> tsd::alarm::Message {
    tsd::alarm::Message {
        mailbox: "concierge".into(),
        subject: title.to_string(),
        kind: "note".into(),
        class: None,
        default: String::new(),
        body: format!("## Note\n{}\n", String::from_utf8_lossy(body)),
    }
}

/// `drain_one`: process one spool entry — file it, and on success remove the spool file.
/// `exec 8>>"$lock"; flock -w "$wait" 8`: one lock for the whole intake (not one per
/// ref), so two callers racing the check-then-create pair on the SAME ref (the watchtower
/// timer and a hand-run, historically sp-aapz/sp-uvfq filed the same second) serialise
/// into a filing and a recurrence instead of two beads. `None` on a timed-out wait — the
/// caller must leave the spool entry in place; a lock held 30s by someone else is the
/// write-ahead spool behaving exactly as designed, not a loss.
fn with_incident_lock<R>(lock_path: &std::path::Path, wait_s: u64, f: impl FnOnce() -> R) -> Option<R> {
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

fn drain_one(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, path: &std::path::Path) -> Result<Option<String>, ()> {
    let raw = std::fs::read(path).map_err(|_| ())?;
    let Some(mut entry) = spool::parse_entry(&raw) else {
        ilog(env, &format!("spool entry with no REF: {} — moved aside", path.display()));
        let bad = path.with_extension("bad");
        let _ = std::fs::rename(path, &bad);
        return Ok(None);
    };
    if entry.body.is_empty() {
        entry.body = format!(
            "The intake gathered NO payload for {}.\nsystemctl produced nothing — suspect the unit name.\n",
            entry.reference
        )
        .into_bytes();
    }
    let provenance = run::provenance(&entry.unit, &hostname(), &entry.incident_path);
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
    let locked = with_incident_lock(&env.lock_path, env.lock_wait_s, || {
        run::file_one(bd, mailer, clock, &cfg, &entry.reference, &entry.title, &entry.body, &file_labels, &mut log)
    });
    let Some(outcome) = locked else {
        ilog(env, &format!("another intake held {} for {}s — {} stays spooled, drain will retry", env.lock_path.display(), env.lock_wait_s, entry.reference));
        record_attempt(env, "incident-file", "incident", &entry.reference, tsd::alarm::Outcome::Failed { reason: "another intake held the lock; the event stays spooled".into() }, Some(note_of(&entry.title, &entry.body)));
        return Err(());
    };
    for l in &log {
        ilog(env, l);
    }
    if let Some(o) = incident::attempts::of_file(&outcome) {
        let message = matches!(o, tsd::alarm::Outcome::Failed { .. }).then(|| note_of(&entry.title, &entry.body));
        record_attempt(env, "incident-file", "incident", &entry.reference, o, message);
    }
    match outcome {
        FileOutcome::Filed(id) => {
            let _ = std::fs::remove_file(path);
            Ok(Some(id))
        }
        FileOutcome::Skew => {
            let _ = std::fs::remove_file(path);
            Ok(None)
        }
        FileOutcome::Unreachable | FileOutcome::Refused => Err(()),
    }
}

fn require_db(env: &Env) -> ExitCode {
    eprintln!("incident: refusing to file — SPIRA_DB is not set — refusing to file rather than let bd resolve one on its own");
    let _ = env;
    ExitCode::FAILURE
}

fn systemd_payload(unit: &str, host: &str, when: &str, systemctl: &str) -> String {
    format!("unit: {unit}\nhost: {host}\nwhen: {when}\n\n## systemctl\n{systemctl}\nlog: systemctl --user status {unit}\n")
}

fn cmd_systemd(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, unit: &str) -> ExitCode {
    let reference = format!("incident:{unit}");
    let when = now_iso();
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
    if std::fs::write(&sp, spool::format_entry(&entry)).is_err() {
        return ExitCode::FAILURE;
    }
    match drain_one(env, bd, mailer, clock, &sp) {
        Ok(_) => ExitCode::SUCCESS,
        Err(()) => {
            ilog(env, &format!("spooled {reference} at {}", sp.display()));
            ExitCode::FAILURE
        }
    }
}

fn cmd_file(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, src: &str) -> ExitCode {
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
    if std::fs::write(&sp, spool::format_entry(&entry)).is_err() {
        return ExitCode::FAILURE;
    }
    match drain_one(env, bd, mailer, clock, &sp) {
        Ok(_) => ExitCode::SUCCESS,
        Err(()) => {
            ilog(env, &format!("spooled {reference} at {}", sp.display()));
            ExitCode::FAILURE
        }
    }
}

fn cmd_alarm(env: &Env, bd: Option<&dyn Bd>, mailer: &dyn Mailer, clock: &dyn Clock, title: &str) -> ExitCode {
    let reference = env.repo_override_ref().unwrap_or_else(|| format!("incident:{title}"));
    let Some(inbox) = self::env("SPIRA_CONCIERGE_INBOX") else {
        eprintln!("incident: SPIRA_CONCIERGE_INBOX is not declared — refusing");
        return ExitCode::FAILURE;
    };
    let state_path = env.spira_run_path("incident-alarms.tsv");
    let was_due = std::cell::Cell::new(false);
    let noted = with_incident_lock(&env.lock_path, env.lock_wait_s, || {
        let state = std::fs::read_to_string(&state_path).unwrap_or_default();
        let (due, next) = incident::alarm::decide(&state, &reference, now_epoch());
        if !due {
            return true;
        }
        was_due.set(true);
        if let Some(parent) = std::path::Path::new(&inbox).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = format!("{} {}\n", now_iso(), incident::alarm::line(title, &reference));
        let wrote = std::fs::OpenOptions::new().create(true).append(true).open(&inbox).and_then(|mut f| {
            use std::io::Write;
            f.write_all(line.as_bytes())
        });
        wrote.is_ok() && std::fs::write(&state_path, next).is_ok()
    });
    if was_due.get() || noted.is_none() {
        let outcome = match noted {
            Some(true) => tsd::alarm::Outcome::Delivered { id: format!("inbox:{reference}") },
            Some(false) => tsd::alarm::Outcome::Failed { reason: format!("the note was not written to {inbox}") },
            None => tsd::alarm::Outcome::Failed { reason: "another intake held the lock".into() },
        };
        let message = matches!(outcome, tsd::alarm::Outcome::Failed { .. }).then(|| note_of(title, incident::alarm::line(title, &reference).as_bytes()));
        record_attempt(env, "incident-alarm", "alarm", &reference, outcome, message);
    }
    if noted == Some(true) {
        if let (true, Some(bd), Some(_)) = (was_due.get(), bd, env.db.as_deref()) {
            own_alarm(env, bd, mailer, clock, title, &reference);
        }
        println!("{}", if was_due.get() { "noted" } else { "deduped" });
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn own_alarm(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, title: &str, reference: &str) {
    let mut body = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut body);
    let provenance = run::provenance(&env.unit, &hostname(), &env.path);
    let cause = decide::sanitize_cause(&env.cause);
    let mut cfg = env.file_config(&provenance);
    cfg.cause = &cause;
    let mut log = Vec::new();
    let _ = with_incident_lock(&env.lock_path, env.lock_wait_s, || {
        run::alarm_owner(bd, mailer, clock, &cfg, reference, title, &body, &env.labels, &mut log)
    });
    for l in &log {
        ilog(env, l);
    }
}

fn cmd_drain(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock) -> ExitCode {
    let mut n = 0;
    let mut stuck = 0;
    let entries = std::fs::read_dir(&env.spool_dir).into_iter().flatten().flatten();
    let mut paths: Vec<_> = entries.map(|e| e.path()).filter(|p| p.is_file() && p.extension().map(|e| e != "bad").unwrap_or(true)).collect();
    paths.sort();
    for p in paths {
        match drain_one(env, bd, mailer, clock, &p) {
            Ok(_) => n += 1,
            Err(()) => stuck += 1,
        }
    }
    println!("drained {n}, still spooled {stuck}");
    if stuck == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn cmd_list(env: &Env, _bd: &dyn Bd) -> ExitCode {
    if env.db.is_none() {
        return require_db(env);
    }
    // bd's human table for an operator to read. Which incidents are open is each bead's
    // lifecycle row: the labelled beads come from bd as content, the unfinished ones are kept,
    // and bd renders those by id, minus its tip/warning chrome.
    let bdq = match spira_config::release_env::tool("bdq") {
        Ok(b) => b,
        Err(e) => {
            eprintln!("incident: list: {e}");
            return ExitCode::from(2);
        }
    };
    let lc = match spira_config::lc_state::list().map(spira_config::lc_state::index) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("incident: list: cannot tell which incidents are open: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Both reads are bounded (spira-lint call-deadline): an operator's `list` waits 5 s at most.
    let labelled = std::process::Command::new("timeout")
        .arg("5")
        .arg(&bdq)
        .args(["list", "--all", "--limit", "0", "--json", "--label", &env.labels])
        .output();
    let ids: Vec<String> = match labelled {
        Ok(out) if out.status.success() => {
            let v: serde_json::Value = serde_json::from_str(incident::real::json_only_pub(&String::from_utf8_lossy(&out.stdout))).unwrap_or(serde_json::Value::Null);
            v.as_array()
                .map(|a| a.iter().filter_map(|b| b.get("id").and_then(|x| x.as_str())).filter(|id| lc.get(*id).is_some_and(|r| !r.past_builder())).map(str::to_string).collect())
                .unwrap_or_default()
        }
        Ok(_) => {
            eprintln!("incident: list: bd list failed");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("incident: list: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !ids.is_empty() {
        match std::process::Command::new("timeout").arg("5").arg(&bdq).args(["list", "--all", "--limit", "0", "--id", &ids.join(",")]).output() {
            Ok(out) => {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if line.starts_with("💡") || line.starts_with("warning") || line.starts_with("  Fix") || line.starts_with("  Or") {
                        continue;
                    }
                    println!("{line}");
                }
            }
            Err(e) => eprintln!("incident: list: {e}"),
        }
    }
    let spooled = std::fs::read_dir(&env.spool_dir).into_iter().flatten().flatten().filter(|e| e.path().extension().map(|x| x != "bad").unwrap_or(true)).count();
    if spooled > 0 {
        println!("\n{spooled} event(s) still in the spool — run: incident drain");
    }
    let bad = std::fs::read_dir(&env.spool_dir).into_iter().flatten().flatten().filter(|e| e.path().extension() == Some(std::ffi::OsStr::new("bad"))).count();
    if bad > 0 {
        println!("{bad} malformed spool entr(ies) in {}", env.spool_dir.display());
    }
    ExitCode::SUCCESS
}

/// Close the incidents whose fixes have landed and whose detector is quiet; leave the rest.
/// Exit 1 only when the candidates could not be read — a skipped incident is not a failure.
fn cmd_settle(env: &Env, bd: &RealBd) -> ExitCode {
    let home = match spira_home_dir() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("incident: settle: {e}");
            return ExitCode::from(1);
        }
    };
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let settler = incident::settle_real::RealSettler {
        bd,
        db: env.db.clone().unwrap_or_default(),
        incident_label: env_or("SPIRA_INCIDENT_LABEL", "incident"),
        home_repo: env.home_repo.clone(),
        registry: spira_config::repos::Registry::from_env(env_map, &home),
        release_sha: raw_env("SPIRA_RELEASE").and_then(|r| incident::settle_real::release_sha_of(&r)),
    };
    match incident::settle::settle(&settler) {
        Ok(outcomes) => {
            let waiting_path = env.log_path.with_extension("settle-waiting");
            let prev = std::fs::read_to_string(&waiting_path).unwrap_or_default();
            let (fresh, state) = incident::settle::changed_waiting(&prev, &outcomes);
            for o in fresh {
                ilog(env, &format!("settle: {o:?}"));
            }
            let _ = std::fs::write(&waiting_path, state);
            let closed = outcomes.iter().filter(|o| matches!(o, incident::settle::Outcome::Closed { .. })).count();
            println!("settled {closed} of {}", outcomes.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("incident settle: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `spira-lc` as a program on the launcher's PATH, every answer JSON.
struct SpiraLc {
    program: String,
}

impl reconciler_engine::goodput_pass::Lc for SpiraLc {
    fn call(&self, args: &[String]) -> (i32, serde_json::Value) {
        let out = spira_config::bounded::bounded(&self.program).args(args).stdin(std::process::Stdio::null()).output();
        match out {
            Ok(o) => {
                let code = o.status.code().unwrap_or(-1);
                let v = serde_json::from_str(String::from_utf8_lossy(&o.stdout).trim())
                    .unwrap_or_else(|_| serde_json::json!({ "cannot_tell": format!("{} answered non-JSON: {}", self.program, String::from_utf8_lossy(&o.stderr).trim()) }));
                (code, v)
            }
            Err(e) => (-1, serde_json::json!({ "cannot_tell": format!("{}: {e}", self.program) })),
        }
    }
}

struct UreqHttp;

impl incident::alarms_pass::Http for UreqHttp {
    fn get(&self, url: &str, deadline: std::time::Duration) -> Result<String, String> {
        let r = ureq::AgentBuilder::new().timeout(deadline).build().get(url).call().map_err(|e| e.to_string())?;
        Ok(format!("status {}", r.status()))
    }
}

fn spira_lc() -> Result<SpiraLc, String> {
    Ok(SpiraLc { program: spira_config::lifecycle_row::lc_bin()? })
}

fn alarms_config(env: &Env, fallback: bool) -> Result<incident::alarms_pass::Config, String> {
    use spira_config::process::{cfg, cfg_parse};
    let causal_map = spira_home_dir()?.parent().map(|r| r.join("reconciler/causal-map.toml"));
    Ok(incident::alarms_pass::Config {
        run_dir: std::path::PathBuf::from(&env.spira_run),
        host: hostname(),
        deadman_secs: cfg_parse::<u64>("SPIRA_RECONCILER_DEADMAN_SECS")?,
        heartbeat_url: cfg("SPIRA_RECONCILER_HEARTBEAT_URL")?,
        fallback,
        floor_proposal: causal_map.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| incident::alarms_pass::floor_proposal(&t)),
    })
}

/// Runs `f` with the real ports: spira-lc, mail in-process, the mailbox fallback, systemd's view
/// of the goodput timer and the wall clock.
fn with_ports<R>(env: &Env, f: impl FnOnce(&incident::alarms_pass::Ports) -> R) -> Result<R, String> {
    use incident::alarms_pass::{FileSwitch, MailPrimary, MaildirFallback, Ports, GOODPUT_UNIT};
    let mail_env = mail::env::Env::load()?;
    let bd = mail::bead::BdCli { bdq: spira_config::release_env::tool("bdq")?, conn_retries: mail_env.bd_conn_retries };
    let fallback = MaildirFallback { mail_root: mail_env.mail_root.clone(), mute: mail_env.mute };
    let primary = MailPrimary { env: mail_env, bd };
    let instance = spira_world::instance()?;
    let timer_on = || {
        let unit = spira_config::unit::resolve_unit(GOODPUT_UNIT, "timer", &instance, &spira_world::sysctl::systemctl_bin());
        unit != "?" && spira_world::sysctl::unit_exists(&unit) && spira_world::sysctl::run_ok(&["is-enabled", "--quiet", &unit])
    };
    let run_dir = std::path::PathBuf::from(&env.spira_run);
    let switch = FileSwitch { run_dir: &run_dir, timer_on: &timer_on };
    let lc = spira_lc()?;
    let ports = Ports { lc: &lc, primary: &primary, fallback: &fallback, http: &UreqHttp, switch: &switch, clock: &runtime::ports::WallClock };
    Ok(f(&ports))
}

/// `incident alarms-pass [--no-fallback]`.
fn cmd_alarms_pass(env: &Env, args: &[String]) -> ExitCode {
    let fallback = !args.iter().any(|a| a == "--no-fallback");
    let result = alarms_config(env, fallback).and_then(|cfg| with_ports(env, |p| incident::alarms_pass::run(p, &cfg))).and_then(|r| r);
    match result {
        Ok(r) => {
            println!(
                "incident: alarms-pass: {} sent, {} noted, {} retried, {} undelivered, deadman {:?}, heartbeat {}, floor {}, published={}",
                r.sent.len(),
                r.noted.len(),
                r.retried.len(),
                r.undelivered_open,
                r.deadman,
                r.heartbeat.as_deref().unwrap_or("-"),
                r.floor.as_deref().unwrap_or("-"),
                r.published
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("incident: alarms-pass: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `incident floor-cleared`: 0 when the goodput rate is at least its floor, 1 while it is not or
/// cannot be told, so a MOOT-WHEN never reads a blind check as cleared.
fn cmd_floor_cleared() -> ExitCode {
    match spira_lc().and_then(|lc| incident::alarms_pass::floor_cleared(&lc)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("incident: floor-cleared: {e}");
            ExitCode::from(2)
        }
    }
}

/// `incident page --no-fallback --probe <mailbox> [--interval S]` sends the probe through the
/// primary path alone, once or every S seconds; `incident page --probe-health [--max-age S]` exits
/// 0 only when the latest probe delivered within S seconds.
fn cmd_page(env: &Env, args: &[String]) -> ExitCode {
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if args.iter().any(|a| a == "--probe-health") {
        let max_age = flag("--max-age").and_then(|v| v.parse().ok()).unwrap_or(7200);
        return match incident::alarms_pass::probe_health(std::path::Path::new(&env.spira_run), now_epoch().max(0) as u64, max_age) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                println!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    let (Some(mailbox), true) = (flag("--probe"), args.iter().any(|a| a == "--no-fallback")) else {
        eprintln!("usage: incident page --no-fallback --probe <mailbox> [--interval S] | incident page --probe-health [--max-age S]");
        return ExitCode::from(2);
    };
    let interval = flag("--interval").and_then(|v| v.parse::<u64>().ok());
    loop {
        let r = alarms_config(env, false).and_then(|cfg| with_ports(env, |p| incident::alarms_pass::probe(p, &cfg, &mailbox))).and_then(|r| r);
        match &r {
            Ok(id) => println!("{} probe delivered as {id}", now_iso()),
            Err(e) => println!("{} probe FAILED: {e}", now_iso()),
        }
        let Some(secs) = interval else {
            return if r.is_ok() { ExitCode::SUCCESS } else { ExitCode::FAILURE };
        };
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

impl Env {
    fn repo_override_ref(&self) -> Option<String> {
        env("SPIRA_INCIDENT_REF")
    }
}

/// `date -u +%Y%m%dT%H%M%SZ` — the spool filename's timestamp component. Built by
/// stripping the punctuation from `iso_now_public`'s output, so this binary keeps exactly
/// one date formatter.
fn spool_stamp() -> String {
    let iso = incident::run::iso_now_public(now_epoch());
    iso.chars().filter(|c| *c != '-' && *c != ':').collect()
}

fn main() -> ExitCode {
    if let Err(e) = spira_config::release_env::ensure_release_path() {
        eprintln!("incident: {e}");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env_cfg = Env::load();
    if env_cfg.db.is_none() && !args.first().map(String::as_str).is_none() {
        // `list` handles its own missing-DB message via require_db for parity with the
        // bash's per-subcommand behaviour; every other subcommand refuses up front.
    }

    match args.first().map(String::as_str) {
        Some("alarms-pass") => return cmd_alarms_pass(&env_cfg, &args[1..]),
        Some("floor-cleared") => return cmd_floor_cleared(),
        Some("page") => return cmd_page(&env_cfg, &args[1..]),
        _ => {}
    }

    if args.first().map(String::as_str) == Some("alarm") {
        let Some(title) = args.get(1) else {
            eprintln!("usage: incident alarm <title> [-|<file>]");
            return ExitCode::from(2);
        };
        let bd = match RealBd::from_env() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("incident: {e}");
                return ExitCode::from(2);
            }
        };
        return cmd_alarm(&env_cfg, Some(&bd as &dyn Bd), &RealMailer, &RealClock, title);
    }

    let bd = match RealBd::from_env() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("incident: {e}");
            return ExitCode::from(2);
        }
    };
    let mailer = RealMailer;
    let clock = RealClock;
    let provenance = env_cfg.provenance();
    let _ = &provenance;

    match args.first().map(String::as_str) {
        Some("systemd") => {
            let Some(unit) = args.get(1) else {
                eprintln!("usage: incident systemd <unit>");
                return ExitCode::from(2);
            };
            if env_cfg.db.is_none() {
                return require_db(&env_cfg);
            }
            cmd_systemd(&env_cfg, &bd, &mailer, &clock, unit)
        }
        Some("file") => {
            let Some(title) = args.get(1) else {
                eprintln!("usage: incident file <title> [-|<file>]");
                return ExitCode::from(2);
            };
            if env_cfg.db.is_none() {
                return require_db(&env_cfg);
            }
            let src = args.get(2).map(String::as_str).unwrap_or("-");
            cmd_file(&env_cfg, &bd, &mailer, &clock, title, src)
        }
        Some("drain") => {
            if env_cfg.db.is_none() {
                return require_db(&env_cfg);
            }
            cmd_drain(&env_cfg, &bd, &mailer, &clock)
        }
        Some("list") => cmd_list(&env_cfg, &bd),
        Some("settle") => {
            if env_cfg.db.is_none() {
                return require_db(&env_cfg);
            }
            cmd_settle(&env_cfg, &bd)
        }
        Some("collapse") => {
            let (Some(dup), Some("--of"), Some(keep), None) = (args.get(1), args.get(2).map(String::as_str), args.get(3), args.get(4)) else {
                eprintln!("usage: incident collapse <dup> --of <keep>");
                return ExitCode::from(2);
            };
            let Some(db) = env_cfg.db.as_deref() else {
                return require_db(&env_cfg);
            };
            match run::collapse(&bd, db, dup, keep) {
                Ok(()) => {
                    println!("collapsed {dup} into {keep}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("incident collapse: {e}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!("incident.sh — turn a production event into a bead Ops can claim\n\n  incident systemd <unit>           file an incident for a failed systemd user unit\n  incident file <title> [-|<file>]  file one from an arbitrary payload\n  incident drain                    file everything the spool is holding\n  incident list                     open incidents\n  incident settle                   close incidents whose fix landed and detector is quiet\n  incident collapse <dup> --of <keep>  mark a duplicate incident\n  incident alarms-pass [--no-fallback]  the alarms component's minute pass\n  incident floor-cleared            exit 0 once the goodput rate is back at its floor\n  incident page --no-fallback --probe <mailbox> [--interval S] | --probe-health [--max-age S]");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_payload_carries_the_systemctl_facts_and_reads_no_journal() {
        let p = systemd_payload("spira-x.service", "h", "2026-01-01T00:00:00Z", "Result=exit-code\nExecMainStatus=3\n");
        assert!(p.contains("Result=exit-code") && p.contains("ExecMainStatus=3"), "{p}");
        assert!(p.contains("systemctl --user status spira-x.service"), "{p}");
        assert!(!p.contains("journal"), "{p}");
    }

    /// A NON-registered name still falls back to `spira_config::resolve()` between the raw
    /// environment and the caller's own default — unchanged by the `SPIRA_WATCHER_INTERVAL_S`
    /// migration below, since `SPIRA_NO_SUCH_KEY_AT_ALL_EVER` is not in `spira/conf.d`.
    #[test]
    fn a_non_registered_name_still_falls_back_through_the_registry() {
        assert_eq!(env("SPIRA_NO_SUCH_KEY_AT_ALL_EVER"), None);
    }
}
