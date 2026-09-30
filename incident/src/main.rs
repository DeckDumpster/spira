//! incident — CLI entry point. DESIGN.md has the subcommand contract in full; this file
//! only reads the environment and argv, builds a `run::FileConfig`, and dispatches.
//!
//!   incident systemd <unit>           file an incident for a failed systemd user unit
//!   incident file <title> [-|<file>]  file one from an arbitrary payload
//!   incident drain                    file everything the spool is holding
//!   incident list                     open incidents
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

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_or(name: &str, default: &str) -> String {
    env(name).unwrap_or_else(|| default.to_string())
}

fn hostname() -> String {
    std::process::Command::new("hostname")
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
    sin_at: u32,
    sin_exempt: bool,
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
    unit: String,
    path: String,
}

impl Env {
    fn load() -> Env {
        let spira_run = env_or("SPIRA_RUN", "/tmp");
        let incident_label = env_or("SPIRA_INCIDENT_LABEL", "incident");
        Env {
            db: env("SPIRA_DB"),
            spool_dir: std::path::PathBuf::from(env_or("SPIRA_SPOOL", &format!("{spira_run}/incident-spool"))),
            log_path: std::path::PathBuf::from(env_or("SPIRA_INCIDENT_LOG", &format!("{spira_run}/incident.log"))),
            sin_at: env("SPIRA_SIN_AT").and_then(|v| v.parse().ok()).unwrap_or(5),
            sin_exempt: env("SPIRA_SIN_EXEMPT").as_deref() == Some("1"),
            dedup_lookback_days: env("SPIRA_INCIDENT_DEDUP_LOOKBACK").and_then(|v| v.parse().ok()).unwrap_or(7),
            watcher_interval_s: env("SPIRA_WATCHER_INTERVAL_S").and_then(|v| v.parse().ok()).unwrap_or(1800),
            cause: decide::sanitize_cause(&env_or("SPIRA_INCIDENT_CAUSE", "unrecorded")),
            labels: env("SPIRA_INCIDENT_LABELS").unwrap_or_else(|| format!("spira,{incident_label}")),
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
            unit: env_or("SPIRA_INCIDENT_UNIT", &unit_from_cgroup()),
            path: env_or("SPIRA_INCIDENT_PATH", "?"),
            spira_run,
        }
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
            sin_at: self.sin_at,
            sin_exempt: self.sin_exempt,
            watcher_interval_s: self.watcher_interval_s,
            dedup_lookback_days: self.dedup_lookback_days,
            repo_declared: self.repo.as_deref(),
            delivers_pref: self.delivers_pref.as_deref(),
            sop_ledger: &self.sop_ledger,
            spira_run: &self.spira_run,
            home_repo: &self.home_repo,
            known_repos: &self.known_repos,
            ask_label: &self.ask_label,
            provenance,
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
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` — the `ilog` timestamp.
fn now_iso() -> String {
    incident::run::iso_now_public(now_epoch())
}

/// `drain_one`: process one spool entry — file it, and on success remove the spool file.
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
            "The intake gathered NO payload for {}.\nsystemctl/journalctl produced nothing — suspect the unit name or a journal this user cannot read.\n",
            entry.reference
        )
        .into_bytes();
    }
    let provenance = run::provenance(&entry.unit, &hostname(), &entry.incident_path);
    let cause = decide::sanitize_cause(&entry.cause);
    let mut cfg = env.file_config(&provenance);
    cfg.cause = &cause;
    let mut log = Vec::new();
    let outcome = run::file_one(bd, mailer, clock, &cfg, &entry.reference, &entry.title, &entry.body, &env.labels, &mut log);
    for l in &log {
        ilog(env, l);
    }
    match outcome {
        FileOutcome::Filed(id) => {
            let _ = std::fs::remove_file(path);
            Ok(Some(id))
        }
        FileOutcome::Unreachable | FileOutcome::Refused => Err(()),
    }
}

fn require_db(env: &Env) -> ExitCode {
    eprintln!("incident: refusing to file — SPIRA_DB is not set — refusing to file rather than let bd resolve one on its own");
    let _ = env;
    ExitCode::FAILURE
}

fn cmd_systemd(env: &Env, bd: &dyn Bd, mailer: &dyn Mailer, clock: &dyn Clock, unit: &str) -> ExitCode {
    let reference = format!("incident:{unit}");
    let when = now_iso();
    let systemctl = std::process::Command::new("systemctl")
        .args([
            "--user", "show", unit, "-p", "Result", "-p", "ExecMainStatus", "-p", "ExecMainCode", "-p", "NRestarts", "-p", "ActiveState",
            "-p", "SubState", "-p", "InvocationID", "-p", "ExecMainStartTimestamp",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_else(|| "(systemctl unavailable)\n".to_string());
    let journal = std::process::Command::new("journalctl")
        .args(["--user", "-u", unit, "-n", "40", "--no-pager"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_else(|| "(journal unavailable)\n".to_string());
    let payload = format!("unit: {unit}\nhost: {}\nwhen: {when}\n\n## systemctl\n{systemctl}\n## journal (last 40)\n{journal}", hostname());

    let entry = spool::SpoolEntry {
        reference: reference.clone(),
        title: format!("incident: {unit} failed"),
        unit: unit.to_string(),
        incident_path: unit.to_string(),
        cause: env.cause.clone(),
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
    let db = match env.db.as_deref() {
        Some(d) => d,
        None => return require_db(env),
    };
    // `bd list` in its OWN default (non-JSON) rendering — the bash's `list` prints bd's
    // human-readable table for an operator to read, not the machine `--json` shape the
    // dedup scan needs. Filters the same four leading-character classes the bash's
    // `grep -vE '^💡|^warning|^  Fix|^  Or'` drops (bd's own tip/warning chrome).
    match std::process::Command::new(std::env::var("SPIRA_BD").unwrap_or_else(|_| "bd".into()))
        .args(["-C", db, "list", "--status", "open,in_progress", "--limit", "0", "--label", &env.labels])
        .output()
    {
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env_cfg = Env::load();
    if env_cfg.db.is_none() && !matches!(args.first().map(String::as_str), None) {
        // `list` handles its own missing-DB message via require_db for parity with the
        // bash's per-subcommand behaviour; every other subcommand refuses up front.
    }

    let bd = RealBd::from_env();
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
        _ => {
            eprintln!("incident.sh — turn a production event into a bead Ops can claim\n\n  incident systemd <unit>           file an incident for a failed systemd user unit\n  incident file <title> [-|<file>]  file one from an arbitrary payload\n  incident drain                    file everything the spool is holding\n  incident list                     open incidents");
            ExitCode::from(1)
        }
    }
}
