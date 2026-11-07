//! heimdall — `heimdall [--home DIR] [--report]` | `heimdall classify` (observations on stdin). See DESIGN.md and each module's own doc
//! comment for the design this ports from (spira/heimdall.sh, retired in the same change).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use heimdall::conditions::LcConditions;
use heimdall::classify::{self, Mirror as ObsMirror, Observation, RestartAlert as ObsRestartAlert, Thresholds};
use heimdall::gather::{self, RealSystemctl, Systemctl};
use heimdall::pass::{self, PassInput, PassPaths};
use heimdall::state::State;
use heimdall::util::{self, Sink, StdSink};

fn env_or(env: &BTreeMap<String, String>, key: &str, default: &str) -> String {
    env.get(key).filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| default.to_string())
}
fn env_n(env: &BTreeMap<String, String>, key: &str, default: i64) -> i64 {
    env.get(key).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

fn resolve_home(flag: Option<&str>, env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let ok = |p: &Path| p.join("lib.sh").is_file();
    if let Some(f) = flag {
        let p = PathBuf::from(f);
        return ok(&p).then_some(p);
    }
    if let Some(h) = env.get("SPIRA_HOME").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(h);
        if ok(&p) {
            return Some(p);
        }
    }
    if let Some(r) = env.get("SPIRA_RELEASE").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(r).join("spira");
        if ok(&p) {
            return Some(p);
        }
    }
    None
}

static FIRING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn classify_main() -> i32 {
    let mut input = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
        eprintln!("heimdall classify: unreadable observations");
        return 2;
    }
    match classify::classify_stdin(&input) {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => {
            eprintln!("heimdall classify: {e}");
            2
        }
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("classify") {
        std::process::exit(classify_main());
    }
    let run = spira_otel::metrics::Run::begin(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "heimdall", "pass");
    let code = pass();
    run.end_with(spira_otel::metrics::outcome_of(code), &[spira_otel::metrics::Point::Count { name: "spira_heimdall_firing_alerts", value: FIRING.load(std::sync::atomic::Ordering::Relaxed) }]);
    std::process::exit(code);
}

fn pass() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let report = args.iter().any(|a| a == "--report");
    let mut home_flag = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--home" {
            home_flag = it.next().cloned();
        }
    }

    let original: BTreeMap<String, String> = std::env::vars().collect();
    let Some(home) = resolve_home(home_flag.as_deref(), &original) else {
        eprintln!("heimdall: cannot find the harness's spira/ directory (pass --home, or set SPIRA_HOME/SPIRA_RELEASE)");
        return 1;
    };

    // THE STORE GETS TEN SECONDS: a store that has not answered in ten seconds IS
    // "unreachable" as far as a watchdog is concerned.
    let store_timeout = env_n(&original, "SPIRA_HEIMDALL_BD_TIMEOUT", 10).max(1);
    // The store's door, in-process over its socket; nothing is launched to reach it.
    let lc = match spira_config::lc_call::LcClient::configured() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("heimdall: {e}");
            return 1;
        }
    };

    // `SPIRA_REPO` (the home repo path), derived in-process the same way `cfg()` itself
    // derives it internally — not a config-key read, so it stays this direct call.
    let repo_path = spira_config::resolve::derive_home_repo(&home, &original);

    // Every key below (SPIRA_RUN/SPIRA_DB/SPIRA_EXPORTER/SPIRA_INSTANCE/SPIRA_TZ/
    // SPIRA_HEIMDALL_RESTARTS/SPIRA_HEIMDALL_RESTART_WINDOW) is registered in `spira/conf.d` —
    // the one source of config (per Ryan 2026-10-05), through
    // `spira_config::process::cfg`/`cfg_parse`. No Rust-side literal default stands in for
    // an unresolved value; a key that fails to resolve exits named, exactly as the
    // config-resolution failure just above already does.
    macro_rules! cfg_or_die {
        ($key:expr) => {
            match spira_config::process::cfg($key) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("heimdall: {e}");
                    return 1;
                }
            }
        };
    }
    macro_rules! cfg_num_or_die {
        ($key:expr) => {
            match spira_config::process::cfg_parse($key) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("heimdall: {e}");
                    return 1;
                }
            }
        };
    }

    let now = util::now_epoch();
    let sink = StdSink;

    // ---- config: registered keys through `cfg`/`cfg_parse`; SPIRA_HEIMDALL_* (besides the
    // two above) carries no conf.sh key, so it stays an ad hoc raw-environment read -----
    let run_dir = PathBuf::from(cfg_or_die!("SPIRA_RUN"));
    let db = cfg_or_die!("SPIRA_DB");
    let repo = repo_path.to_string_lossy().into_owned();
    let wiki = cfg_or_die!("SPIRA_WIKI");
    let exporter = match cfg_or_die!("SPIRA_EXPORTER") {
        e if e.is_empty() && !wiki.is_empty() => home.join("export-beads.sh").display().to_string(),
        e => e,
    };
    let systemctl_bin = env_or(&original, "SPIRA_SYSTEMCTL", "systemctl");
    let instance = cfg_or_die!("SPIRA_INSTANCE");
    let tz = cfg_or_die!("SPIRA_TZ");

    let state_path = run_dir.join("heimdall.state");
    let status_path = run_dir.join("heimdall.status");
    let fallback_path = PathBuf::from(env_or(&original, "SPIRA_HEIMDALL_FALLBACK", &run_dir.join("heimdall.alerts.json").display().to_string()));
    let sentinel_log_path = PathBuf::from(env_or(&original, "SPIRA_HEIMDALL_SENTINEL_LOG", &run_dir.join("sentinel.log").display().to_string()));
    let strands_path = PathBuf::from(env_or(&original, "SPIRA_HEIMDALL_STRANDS", &run_dir.join("strands.json").display().to_string()));
    let mirror_path = PathBuf::from(env_or(&original, "SPIRA_HEIMDALL_MIRROR", &format!("{}/raw/spira-beads/spira.jsonl", if wiki.is_empty() { &repo } else { &wiki })));
    let tail_bytes = env_n(&original, "SPIRA_HEIMDALL_TAIL_BYTES", 262_144).max(0) as u64;
    let confirm = env_n(&original, "SPIRA_HEIMDALL_CONFIRM", 2);
    let clear_n = env_n(&original, "SPIRA_HEIMDALL_CLEAR", 2);
    let refresh = env_n(&original, "SPIRA_HEIMDALL_REFRESH", 3600);
    let restarts_threshold: i64 = cfg_num_or_die!("SPIRA_HEIMDALL_RESTARTS");
    let restart_window: i64 = cfg_num_or_die!("SPIRA_HEIMDALL_RESTART_WINDOW");
    let thresholds = Thresholds {
        pass_stale: env_n(&original, "SPIRA_HEIMDALL_PASS_STALE", 600),
        starve_passes: env_n(&original, "SPIRA_HEIMDALL_STARVE_PASSES", 5),
        mirror_stale: env_n(&original, "SPIRA_HEIMDALL_MIRROR_STALE", 90_000),
        ghost_stale: env_n(&original, "SPIRA_HEIMDALL_GHOST_STALE", 1800),
        restart_threshold: restarts_threshold,
        restart_window,
    };

    // ---- state -----------------------------------------------------------------------
    let state_text = std::fs::read_to_string(&state_path).unwrap_or_default();
    let mut state = State::parse(&state_text);
    // ---- gather ------------------------------------------------------------------------
    let log_tail = gather::read_log_tail(&sentinel_log_path, tail_bytes);
    let sc = RealSystemctl { bin: systemctl_bin.clone(), timeout: Duration::from_secs(5), units: host_units() };
    let sentinel_timer = gather::sentinel_timer_status(&sc, &instance);
    let mirror = gather::check_mirror(&exporter, &mirror_path);
    let world = gather::world_state(&run_dir);
    let strands: serde_json::Value = std::fs::read_to_string(&strands_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::json!({}));

    let mut restart_alerts: Vec<ObsRestartAlert> = Vec::new();
    let units = sc.spira_services();
    if !units.is_empty() {
        let current = sc.nrestarts(&units);
        for (unit, cur) in &current {
            let (base, delta) = heimdall::restart::update(state.restarts.get(unit), *cur, now, restart_window);
            state.restarts.insert(unit.clone(), base);
            if delta > restarts_threshold {
                restart_alerts.push(ObsRestartAlert { unit: unit.clone(), current: *cur, baseline: state.restarts[unit].baseline, delta, window: restart_window });
            }
        }
    }

    let ops = LcConditions { client: lc.within(Duration::from_secs(store_timeout as u64)) };
    let obs = Observation {
        now,
        heimdall_first: state.first_run,
        sentinel_log: log_tail.text,
        sentinel_log_readable: log_tail.readable,
        sentinel_log_error: log_tail.error,
        sentinel_log_mtime: log_tail.mtime,
        sentinel_log_path: sentinel_log_path.display().to_string(),
        sentinel_timer,
        db_reachable: false,
        db_error: String::new(),
        db_path: db.clone(),
        fallback_path: fallback_path.display().to_string(),
        mirror: ObsMirror { configured: mirror.configured, exists: mirror.exists, mtime: mirror.mtime, path: mirror_path.display().to_string(), exporter: exporter.clone() },
        strands,
        restart_alerts,
        world_halted: world.halted,
        world_halt_at: world.halt_at,
        world_draining: world.draining,
        world_drain_at: world.drain_at,
        thresholds,
    };
    let inp = PassInput { obs, confirm, clear_n, refresh, tz, render_time: util::iso_display, report_only: report };
    let paths = PassPaths { fallback: fallback_path, heartbeat: status_path };
    let rep = pass::run(&inp, &ops, &mut state, &paths, now);
    for l in &rep.out {
        sink.out(l);
    }
    for l in &rep.err {
        sink.err(l);
    }

    if report {
        if rep.firing.is_empty() {
            println!(
                "heimdall: nothing firing (sentinel timer {}, db read: {} write: {})",
                inp.obs.sentinel_timer,
                if rep.db_reachable { "ok" } else { "UNREACHABLE" },
                match rep.probe.write_ok {
                    Some(true) => "ok",
                    Some(false) => "FAILED",
                    None => "unknown",
                }
            );
        } else {
            for f in &rep.firing {
                println!("\n=== {} ===\n{}\n\n{}\n", f.key, f.title, f.evidence);
            }
        }
        return 0;
    }

    let _ = std::fs::write(&state_path, state.render());
    FIRING.store(rep.firing.len() as u64, std::sync::atomic::Ordering::Relaxed);
    0
}

/// The user manager through the `Units` port; `None` (every state read "cannot tell") when it
/// cannot be reached, or on a platform with no systemd backend.
fn host_units() -> Option<std::sync::Arc<dyn runtime::units::Units>> {
    #[cfg(target_os = "linux")]
    {
        match runtime::units::SystemdUnits::connect(std::sync::Arc::new(runtime::ports::WallClock)) {
            Ok(u) => return Some(std::sync::Arc::new(u)),
            Err(e) => eprintln!("heimdall: the unit manager: {}", e.0),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_home_prefers_the_flag() {
        let dir = testkit::TempDir::new("heimdall-main");
        std::fs::write(dir.join("lib.sh"), "").unwrap();
        let home = resolve_home(Some(dir.to_str().unwrap()), &BTreeMap::new());
        assert_eq!(home, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn resolve_home_falls_back_to_spira_home_env() {
        let dir = testkit::TempDir::new("heimdall-main");
        std::fs::write(dir.join("lib.sh"), "").unwrap();
        let mut env = BTreeMap::new();
        env.insert("SPIRA_HOME".to_string(), dir.display().to_string());
        assert_eq!(resolve_home(None, &env), Some(dir.path().to_path_buf()));
    }

    #[test]
    fn resolve_home_falls_back_to_spira_release_spira() {
        let dir = testkit::TempDir::new("heimdall-main");
        std::fs::create_dir_all(dir.join("spira")).unwrap();
        std::fs::write(dir.join("spira").join("lib.sh"), "").unwrap();
        let mut env = BTreeMap::new();
        env.insert("SPIRA_RELEASE".to_string(), dir.display().to_string());
        assert_eq!(resolve_home(None, &env), Some(dir.join("spira")));
    }

    #[test]
    fn resolve_home_none_when_nothing_resolves() {
        assert_eq!(resolve_home(None, &BTreeMap::new()), None);
    }
}
