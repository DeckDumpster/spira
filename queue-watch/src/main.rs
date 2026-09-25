//! queue-watch — streams merge-queue transitions for every `mode = "queue"` repository in
//! spira.toml, derived from queue state and the forge, never from log lines.
//!
//!   queue-watch watch  [--interval S] [--ticks N] [--json]   loop; one line per event
//!   queue-watch health                                      exit non-zero when the last
//!                                                           poll is stale or was blind;
//!                                                           an install with no queue-mode
//!                                                           repo is idle, and healthy
//!
//! Common flags: --run DIR (SPIRA_RUN), --db DIR (SPIRA_DB), --home DIR (SPIRA_HOME, where
//! forge.sh lives), --config FILE (spira.toml; default search: SPIRA_TOML, $SPIRA_REPO,
//! $XDG_CONFIG_HOME/spira, /etc/spira).
//!
//! Runs as a watchd `daemon` row, so a reader latches on with `watchd.sh tail queue-watch`
//! instead of hand-rolling a pipeline over the queue's log.

mod core;
mod io;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::core::{step, Event, Limits, RepoState};
use crate::io::{snapshot, Env, Repo};

struct Opts {
    cmd: String,
    interval: u64,
    ticks: Option<u64>,
    json: bool,
    run: Option<PathBuf>,
    db: Option<PathBuf>,
    home: Option<PathBuf>,
    config: Option<PathBuf>,
}

fn usage() -> ExitCode {
    eprintln!("usage: queue-watch watch|health [--interval S] [--ticks N] [--json] [--run DIR] [--db DIR] [--home DIR] [--config FILE]");
    ExitCode::from(2)
}

fn parse() -> Result<Opts, String> {
    let mut a = env::args().skip(1);
    let cmd = a.next().ok_or("missing command")?;
    let mut o = Opts {
        cmd,
        interval: 30,
        ticks: None,
        json: false,
        run: env::var_os("SPIRA_RUN").map(PathBuf::from),
        db: env::var_os("SPIRA_DB").map(PathBuf::from),
        home: env::var_os("SPIRA_HOME").map(PathBuf::from),
        config: env::var_os("SPIRA_TOML").map(PathBuf::from),
    };
    while let Some(f) = a.next() {
        let mut val = || a.next().ok_or(format!("{f} needs a value"));
        match f.as_str() {
            "--interval" => o.interval = val()?.parse().map_err(|_| "--interval takes seconds")?,
            "--ticks" => o.ticks = Some(val()?.parse().map_err(|_| "--ticks takes a count")?),
            "--json" => o.json = true,
            "--run" => o.run = Some(val()?.into()),
            "--db" => o.db = Some(val()?.into()),
            "--home" => o.home = Some(val()?.into()),
            "--config" => o.config = Some(val()?.into()),
            _ => return Err(format!("unknown flag {f}")),
        }
    }
    if o.interval == 0 {
        return Err("--interval must be at least 1".into());
    }
    Ok(o)
}

fn find_config(o: &Opts) -> Option<PathBuf> {
    if let Some(c) = &o.config {
        return Some(c.clone());
    }
    let mut cands = Vec::new();
    if let Some(r) = env::var_os("SPIRA_REPO") {
        cands.push(PathBuf::from(r).join("spira.toml"));
    }
    let xdg = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(x) = xdg {
        cands.push(x.join("spira/spira.toml"));
    }
    cands.push(PathBuf::from("/etc/spira/spira.toml"));
    cands.into_iter().find(|p| p.is_file())
}

/// Why there is nothing to watch, as opposed to something being wrong.
enum NoRepos {
    Idle(String),
    Fatal(String),
}

/// Every `mode = "queue"` repository. An install with none, or with no spira.toml yet, is
/// IDLE — a fact about the install, said once and reported by health — never a crash loop.
/// A spira.toml that does not parse is fatal: that is a fault someone must fix.
fn queue_repos(cfg: Option<PathBuf>, home: &Path) -> Result<Vec<Repo>, NoRepos> {
    let Some(cfg) = cfg else { return Err(NoRepos::Idle("no spira.toml found".into())) };
    let cfg = cfg.as_path();
    let text = fs::read_to_string(cfg).map_err(|e| NoRepos::Fatal(format!("{}: {e}", cfg.display())))?;
    let doc = spira_config::validate(&text).map_err(|e| NoRepos::Fatal(format!("{}: {e}", cfg.display())))?;
    // A repo may name its own forge script; otherwise the harness's own (SPIRA_FORGE, which
    // conf.sh defaults to forge.sh beside the rest of the harness).
    let default_forge = env::var_os("SPIRA_FORGE").map(PathBuf::from).unwrap_or_else(|| home.join("forge.sh"));
    let repos: Vec<Repo> = doc
        .repo
        .iter()
        .filter(|(_, r)| r.mode == spira_config::LandMode::Queue)
        .map(|(name, r)| Repo {
            name: name.clone(),
            path: PathBuf::from(&r.path),
            base: r.base.clone().unwrap_or_else(|| "origin/main".into()),
            forge: r.forge.as_ref().map(PathBuf::from).unwrap_or_else(|| default_forge.clone()),
        })
        .collect();
    if repos.is_empty() {
        return Err(NoRepos::Idle(format!("{}: no repository has mode = \"queue\"", cfg.display())));
    }
    Ok(repos)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// UTC ISO-8601 without a date crate (Howard Hinnant's civil-from-days).
fn iso(t: u64) -> String {
    let days = (t / 86_400) as i64;
    let secs = t % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

fn render(t: u64, repo: &str, e: &Event, json: bool) -> String {
    if json {
        serde_json::json!({"ts": iso(t), "repo": repo, "kind": e.kind, "pr": e.pr, "ids": e.ids, "text": e.text}).to_string()
    } else {
        format!("{} {repo} {}: {}", iso(t), e.kind, e.text)
    }
}

/// Nothing to watch: say it once, keep the health record fresh, and stay up so a daemon row
/// on an install without a queue does not become a restart loop.
fn idle(o: &Opts, run: &Path, why: &str) -> Result<(), String> {
    println!("{} - idle: {why}; nothing to watch", iso(now()));
    let mut tick = 0u64;
    loop {
        write_idle(run, now(), o.interval, why);
        tick += 1;
        if o.ticks.map(|n| tick >= n).unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(o.interval));
    }
}

fn write_idle(run: &Path, t: u64, interval: u64, why: &str) {
    let p = health_file(run);
    let _ = fs::create_dir_all(p.parent().unwrap());
    let tmp = p.with_extension("health.tmp");
    if fs::write(&tmp, format!("idle {t} {interval} 0 {}\n", why.replace(char::is_whitespace, "_"))).is_ok() {
        let _ = fs::rename(&tmp, &p);
    }
}

fn health_file(run: &Path) -> PathBuf {
    run.join("watchd").join("queue-watch.health")
}

fn write_health(run: &Path, t: u64, interval: u64, repos: usize, blind: &[String]) {
    let p = health_file(run);
    let _ = fs::create_dir_all(p.parent().unwrap());
    let body = if blind.is_empty() {
        format!("ok {t} {interval} {repos}\n")
    } else {
        format!("blind {t} {interval} {repos} {}\n", blind.join(","))
    };
    let tmp = p.with_extension("health.tmp");
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, &p);
    }
}

fn watch(o: &Opts) -> Result<(), String> {
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let repos = match queue_repos(find_config(o), &home) {
        Ok(r) => r,
        Err(NoRepos::Fatal(e)) => return Err(e),
        Err(NoRepos::Idle(why)) => return idle(o, &run, &why),
    };
    let env_ = Env {
        queue_dir: env::var_os("SPIRA_QUEUE_DIR").map(PathBuf::from).unwrap_or_else(|| run.join("queue")),
        landstate: run.join("landstate"),
        db: o.db.clone(),
        bd: env::var("SPIRA_BD").unwrap_or_else(|_| "bd".into()),
        express_label: env::var("SPIRA_EXPRESS_LABEL").unwrap_or_else(|_| "express".into()),
    };
    let lim = Limits {
        idle_stall_secs: env::var("QUEUE_WATCH_IDLE_STALL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(600),
        head_stall_secs: env::var("QUEUE_WATCH_HEAD_STALL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(2700),
    };
    let mut states: BTreeMap<String, RepoState> = BTreeMap::new();
    let mut tick = 0u64;
    let stdout = std::io::stdout();
    loop {
        let t = now();
        let mut blind = Vec::new();
        for r in &repos {
            let prev = states.remove(&r.name).unwrap_or_default();
            let snap = snapshot(&env_, r, &prev, t);
            if !snap.errors.is_empty() {
                blind.push(r.name.clone());
            }
            let (next, evs) = step(&prev, &snap, lim);
            let mut out = stdout.lock();
            for e in &evs {
                let _ = writeln!(out, "{}", render(t, &r.name, e, o.json));
            }
            let _ = out.flush();
            states.insert(r.name.clone(), next);
        }
        write_health(&run, t, o.interval, repos.len(), &blind);
        tick += 1;
        if o.ticks.map(|n| tick >= n).unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(o.interval));
    }
}

/// The watchd health probe. Fails when the watcher has never polled, when its last poll is
/// older than three intervals (it is hung or dead), or when that poll was blind.
fn health(o: &Opts) -> Result<(), String> {
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let p = health_file(&run);
    let text = fs::read_to_string(&p).map_err(|_| format!("never polled ({} absent)", p.display()))?;
    let f: Vec<&str> = text.split_whitespace().collect();
    let (st, t, iv) = match f.as_slice() {
        [st, t, iv, ..] => (*st, t.parse::<u64>().unwrap_or(0), iv.parse::<u64>().unwrap_or(30)),
        _ => return Err(format!("unreadable health record: {text:?}")),
    };
    let age = now().saturating_sub(t);
    if age > 3 * iv {
        return Err(format!("last poll {age}s ago (interval {iv}s) — the watcher is hung or dead"));
    }
    match st {
        "ok" => Ok(()),
        "idle" => {
            println!("idle: {}", f.get(4).unwrap_or(&"?").replace('_', " "));
            Ok(())
        }
        _ => Err(format!("last poll was blind: {}", f.get(4).unwrap_or(&"?"))),
    }
}

fn main() -> ExitCode {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("queue-watch: {e}");
            return usage();
        }
    };
    let r = match o.cmd.as_str() {
        "watch" => watch(&o),
        "health" => health(&o),
        _ => return usage(),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("queue-watch: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::iso;

    #[test]
    fn iso_is_utc_civil_time() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_790_303_413), "2026-09-25T02:30:13Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
    }
}
