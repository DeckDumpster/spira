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
use incident::intake::*;
use incident::run;

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
    // FORK-EDGE: unserved verb content (bd's labelled rows and its human table)
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
        // FORK-EDGE: unserved verb content
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
        units: incident::settle_real::host_units(),
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

/// The store over [`spira_config::lc_call::LcClient`], every answer JSON.
struct SpiraLc {
    client: spira_config::lc_call::LcClient,
}

impl reconciler_engine::goodput_pass::Lc for SpiraLc {
    fn call(&self, args: &[String]) -> (i32, serde_json::Value) {
        incident::alarms_pass::lc_json(&self.client, args)
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
    Ok(SpiraLc { client: spira_config::lc_call::LcClient::configured()? })
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
        // `is-enabled` is non-zero for a unit systemd does not know, so it needs no existence probe.
        unit != "?" && spira_world::sysctl::run_ok(&["is-enabled", "--quiet", &unit])
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
