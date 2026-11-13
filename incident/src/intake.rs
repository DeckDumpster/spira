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
        run::provenance(&self.unit, &hostname(), &self.path)
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

// `Err(())` is "the entry stays spooled", already logged; there is nothing more to carry.
#[allow(clippy::result_unit_err)]
pub fn drain_one(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, path: &std::path::Path) -> Result<Option<String>, ()> {
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
    if let Some(o) = crate::attempts::of_file(&outcome) {
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
    if std::fs::write(&sp, spool::format_entry(&entry)).is_err() {
        return Err(());
    }
    drain_one(env, bd, mailer, clock, &sp).inspect_err(|()| ilog(env, &format!("spooled {reference} at {}", sp.display())))
}

/// `incident file <title> -` in-process, with the host's ports: what a Spira caller runs where
/// it used to spawn `incident.sh file`. `Err` names why nothing was filed.
pub fn file_payload(title: &str, body: &[u8]) -> Result<(), String> {
    file_payload_with(title, body, |_| {})
}

/// `file_payload` with the per-call fields (`priority`, `express`, `reference`, ...) set on the
/// loaded environment by `tune`, so a caller never writes them into the process environment.
pub fn file_payload_with(title: &str, body: &[u8], tune: impl FnOnce(&mut Env)) -> Result<(), String> {
    let mut env = Env::load();
    tune(&mut env);
    if env.db.is_none() {
        let _ = require_db(&env);
        return Err("SPIRA_DB is not set".into());
    }
    let bd = crate::real::RealBd::from_env()?;
    if file_body(&env, &bd, &crate::real::RealMailer, &crate::real::RealClock, title, body.to_vec()) {
        Ok(())
    } else {
        Err(format!("incident file {title:?}: not filed; it stays spooled in {}", env.spool_dir.display()))
    }
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
    use super::{filed_answer, Env};

    #[test]
    fn the_filed_answer_is_one_json_object_naming_the_bead_or_null() {
        assert_eq!(filed_answer(Some("sp-abc")), r#"{"id":"sp-abc"}"#);
        assert_eq!(filed_answer(None), r#"{"id":null}"#);
    }

    #[test]
    fn a_reference_set_on_the_loaded_env_is_the_filings_dedup_reference() {
        let mut env = Env::load();
        env.reference = Some("incident:throughput-stall:alpha:7".into());
        assert_eq!(env.repo_override_ref().as_deref(), Some("incident:throughput-stall:alpha:7"));
    }
}
