//! queue-watch — streams merge-queue transitions for every `queue`/`queue.forge`/
//! `queue.local` repository in spira.toml, derived from queue state and the forge, never
//! from log lines.
//!
//!   queue-watch watch  [--interval S] [--ticks N] [--json]   loop; one line per event
//!   queue-watch health                                      exit non-zero when the last
//!                                                           poll is stale, was blind, or
//!                                                           found no queue-mode repo — idle
//!                                                           is DEGRADED, never healthy
//!
//! Common flags: --run DIR (SPIRA_RUN), --db DIR (SPIRA_DB), --home DIR (SPIRA_HOME, where
//! forge.sh lives), --config FILE (spira.toml; default search: SPIRA_TOML,
//! $XDG_CONFIG_HOME/spira, /etc/spira — no $SPIRA_REPO tier; matches conf.sh's bash search
//! as of sp-9hwim, sp-hconl).
//!
//! Runs as a watchd `daemon` row, so a reader latches on with `watchd tail queue-watch`
//! instead of hand-rolling a pipeline over the queue's log.

mod core;
mod io;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
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
        config: None,
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

/// Why there is nothing to watch, as opposed to something being wrong.
enum NoRepos {
    Idle(String),
    Fatal(String),
}

/// Every `queue`/`queue.forge`/`queue.local` repository (sp-o1jm6, epic sp-hq9x8). An install
/// with none, or with no spira.toml yet, is IDLE — a fact about the install, said once and
/// reported by health — never a crash loop. A spira.toml that does not parse is fatal: that is
/// a fault someone must fix.
fn queue_repos(cfg: Option<PathBuf>) -> Result<Vec<Repo>, NoRepos> {
    let Some(cfg) = cfg else { return Err(NoRepos::Idle("no spira.toml found".into())) };
    let doc = spira_config::load(&cfg).map_err(NoRepos::Fatal)?;
    // A repo may name its own forge script; otherwise SPIRA_FORGE, else the release's
    // `forge` binary by bare name on the launcher's PATH (sp-gypjk, sp-t4y60 — forge.sh is
    // retired; sp-yv4b3 — this default still named the deleted script).
    let default_forge = env::var_os("SPIRA_FORGE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("forge"));
    let repos: Vec<Repo> = doc
        .repo
        .iter()
        .filter(|(_, r)| {
            matches!(r.mode, spira_config::LandMode::Queue | spira_config::LandMode::QueueForge | spira_config::LandMode::QueueLocal)
        })
        .map(|(name, r)| Repo {
            name: name.clone(),
            path: PathBuf::from(&r.path),
            base: r.base.clone().unwrap_or_else(|| "origin/main".into()),
            forge: r.forge.as_ref().map(PathBuf::from).unwrap_or_else(|| default_forge.clone()),
            local: r.mode == spira_config::LandMode::QueueLocal,
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

/// The dedupe key and subject line for a head-stall incident. Keyed on (repo, PR), not on the
/// stall text: a stall still open next poll must bump incident.sh's own recurrence count on
/// the same bead, never file a second one for the same PR.
fn stall_incident(repo: &str, pr: &str, text: &str) -> (String, String) {
    (format!("incident:queue-watch-stall-{repo}-{pr}"), format!("queue-watch: {repo} PR {pr} stalled — {text}"))
}

/// Give a head-stall a delivery path that survives with no session attached. incident.sh's
/// write-ahead spool means a filing survives this process dying mid-call, and its ref-keyed
/// dedupe means a stall still open next poll bumps a recurrence count rather than piling up
/// duplicate beads — the head_warned latch above stops this being called again for the same
/// stall anyway, but a restarted queue-watch process has no memory of that latch.
fn file_stall_incident(incident_sh: &Path, db: Option<&Path>, repo: &str, pr: &str, text: &str) {
    let (ref_, subject) = stall_incident(repo, pr, text);
    let mut cmd = Command::new("bash");
    cmd.arg(incident_sh)
        .arg("file")
        .arg(&subject)
        .arg("-")
        .env("SPIRA_INCIDENT_REPO", repo)
        .env("SPIRA_INCIDENT_REF", &ref_)
        .env("SPIRA_INCIDENT_CAUSE", "queue-head-stall")
        .env("SPIRA_INCIDENT_ACTOR", "queue-watch")
        .env("SPIRA_INCIDENT_TYPE", "task")
        .env("SPIRA_INCIDENT_PRIORITY", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(db) = db {
        cmd.env("SPIRA_DB", db);
    }
    if let Ok(mut child) = cmd.spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

fn watch(o: &Opts) -> Result<(), String> {
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
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
        ci_queued_max_secs: env::var("SPIRA_CI_QUEUED_MAX_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(600),
    };
    let incident_sh = env::var_os("SPIRA_INCIDENT_SH").map(PathBuf::from).unwrap_or_else(|| home.join("incident.sh"));
    let mut states: BTreeMap<String, RepoState> = BTreeMap::new();
    let mut tick = 0u64;
    let stdout = std::io::stdout();
    // Idle is not a one-shot verdict: a queue-mode repo can appear in spira.toml after this
    // process started (or reappear after an unlanded branch gutted it), so every tick with
    // nothing to watch re-reads the config rather than latching idle forever
    // (law-absence-needs-a-positive-control — an idle watcher must keep proving it is still
    // looking, not just that it once found nothing).
    let mut repos: Vec<Repo> = Vec::new();
    let mut last_idle_why: Option<String> = None;
    loop {
        let t = now();
        if repos.is_empty() {
            match queue_repos(spira_config::discover(o.config.clone())) {
                Ok(r) => {
                    println!("{} - watching resumed: {} queue-mode repo(s) found", iso(t), r.len());
                    repos = r;
                    last_idle_why = None;
                }
                Err(NoRepos::Fatal(e)) => return Err(e),
                Err(NoRepos::Idle(why)) => {
                    if last_idle_why.as_deref() != Some(why.as_str()) {
                        println!("{} - idle: {why}; nothing to watch", iso(t));
                        last_idle_why = Some(why.clone());
                    }
                    write_idle(&run, t, o.interval, &why);
                    tick += 1;
                    if o.ticks.map(|n| tick >= n).unwrap_or(false) {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_secs(o.interval));
                    continue;
                }
            }
        }
        let mut blind = Vec::new();
        for r in &repos {
            let prev = states.remove(&r.name).unwrap_or_default();
            let snap = snapshot(&env_, r, &prev, t);
            if !snap.errors.is_empty() {
                blind.push(r.name.clone());
            }
            let (next, evs) = step(&prev, &snap, lim);
            {
                let mut out = stdout.lock();
                for e in &evs {
                    let _ = writeln!(out, "{}", render(t, &r.name, e, o.json));
                }
                let _ = out.flush();
            }
            // A head-stall is by definition loop-stopping — it is the ONLY batch slot this
            // repo's queue has — so it gets a delivery path that survives with no session
            // attached, not just a line in a log only a live Monitor happens to be tailing.
            // The idle-stall (no batch open at all) carries no `pr` and is excluded: an empty
            // queue is not occupying anything.
            for e in &evs {
                if e.kind == "stall" {
                    if let Some(pr) = &e.pr {
                        file_stall_incident(&incident_sh, env_.db.as_deref(), &r.name, pr, &e.text);
                    }
                }
            }
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
/// older than three intervals (it is hung or dead), when that poll was blind, or when it is
/// idle: an idle watcher is retrying every tick, but nothing proves the queue is actually
/// quiet rather than the config being wrong, so it reads DEGRADED and not healthy
/// (law-absence-needs-a-positive-control).
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
        "idle" => Err(format!("idle: {}", f.get(4).unwrap_or(&"?").replace('_', " "))),
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
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // Serialises the one test in this crate that touches the real process environment
    // (SPIRA_FORGE, PATH), and restores both on drop — same pattern as release's
    // `ENV_LOCK`/`PathGuard` (release/src/tests.rs).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        forge: Option<std::ffi::OsString>,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.forge {
                Some(f) => env::set_var("SPIRA_FORGE", f),
                None => env::remove_var("SPIRA_FORGE"),
            }
        }
    }

    // REGRESSION (sp-yv4b3): the default named the retired `forge.sh`, not the release's
    // `forge` binary. PATH goes to the spawn only; mutating the process PATH raced the
    // other tests' forks.
    #[test]
    fn queue_repos_default_forge_is_the_bare_release_binary_and_is_actually_reachable() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvGuard { forge: env::var_os("SPIRA_FORGE") };
        env::remove_var("SPIRA_FORGE");

        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = testkit::TempDir::new(&format!("queue-watch-default-forge-test-{n}"));
        testkit::write_exe(
            dir.join("forge"),
            "#!/bin/sh\ncase \"$1\" in check-status) echo green ;; *) exit 1 ;; esac\n",
        );
        let mut child_path = dir.path().as_os_str().to_os_string();
        child_path.push(":");
        child_path.push(env::var_os("PATH").unwrap_or_default());

        let cfg = dir.join("spira.toml");
        fs::write(&cfg, "[repo.q]\npath = \"/tmp/q\"\nmode = \"queue\"\n").unwrap();

        let repos = match queue_repos(Some(cfg)) {
            Ok(r) => r,
            Err(NoRepos::Idle(w)) => panic!("unexpectedly idle: {w}"),
            Err(NoRepos::Fatal(w)) => panic!("unexpectedly fatal: {w}"),
        };
        let q = repos.iter().find(|r| r.name == "q").unwrap();
        assert_eq!(q.forge, PathBuf::from("forge"), "default must name the bare release binary, not forge.sh");
        assert_eq!(crate::io::read_ci_on(q, "459", Some(&child_path)), Ok(crate::core::Ci::Green));

        let _ = fs::remove_dir_all(&dir);
    }

    // POSITIVE CONTROL: two stalls of the same PR must produce the identical ref, or
    // incident.sh's dedupe never finds the first bead and files a second every poll.
    #[test]
    fn stall_incident_ref_is_stable_across_repeated_text() {
        let (ref1, _) = stall_incident("q", "419", "PR 419 unchanged for 46m (CI running)");
        let (ref2, _) = stall_incident("q", "419", "PR 419 CI queued 90m with no runner ever assigned (threshold 10m)");
        assert_eq!(ref1, ref2, "the same repo+PR must dedupe to one incident regardless of wording");
        assert!(ref1.contains("q") && ref1.contains("419"), "{ref1}");
    }

    #[test]
    fn stall_incident_ref_distinguishes_repos_and_prs() {
        let (a, _) = stall_incident("q", "419", "x");
        let (b, _) = stall_incident("q", "420", "x");
        let (c, _) = stall_incident("other", "419", "x");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn iso_is_utc_civil_time() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_790_303_413), "2026-09-25T02:30:13Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
    }

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    // POSITIVE CONTROL: today's code filters on `LandMode::Queue` alone, so this fails on it —
    // "qf" and "ql" are silently dropped from the watched set, and the fixed code's `local`
    // flag (a compile error against the old, field-less `Repo`) cannot even be asserted.
    #[test]
    fn queue_repos_accepts_queue_queue_forge_and_queue_local() {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = testkit::TempDir::new(&format!("queue-watch-queue-repos-test-{n}"));
        let cfg = dir.join("spira.toml");
        fs::write(
            &cfg,
            r#"
[repo.q]
path = "/tmp/q"
mode = "queue"

[repo.qf]
path = "/tmp/qf"
mode = "queue.forge"

[repo.ql]
path = "/tmp/ql"
mode = "queue.local"
base = "local/main"

[repo.p]
path = "/tmp/p"
mode = "push"
"#,
        )
        .unwrap();

        let repos = match queue_repos(Some(cfg)) {
            Ok(r) => r,
            Err(NoRepos::Idle(w)) => panic!("unexpectedly idle: {w}"),
            Err(NoRepos::Fatal(w)) => panic!("unexpectedly fatal: {w}"),
        };
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"q"), "queue: {names:?}");
        assert!(names.contains(&"qf"), "queue.forge (alias, regression): {names:?}");
        assert!(names.contains(&"ql"), "queue.local: {names:?}");
        assert!(!names.contains(&"p"), "push must stay unwatched: {names:?}");

        let ql = repos.iter().find(|r| r.name == "ql").unwrap();
        assert!(ql.local, "queue.local repo must carry local=true");
        assert_eq!(ql.base, "local/main");

        let qf = repos.iter().find(|r| r.name == "qf").unwrap();
        assert!(!qf.local, "queue.forge is not local (regression)");

        let _ = fs::remove_dir_all(&dir);
    }
}
