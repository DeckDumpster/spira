//! auron — `auron [--home DIR] [--report]`. See DESIGN.md and each module's own doc
//! comment for the design this ports from (spira/auron.sh, retired in the same change).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use auron::alerts::{self, ProbeOutcome};
use auron::bdops::{BdOps, SeamBdOps};
use auron::classify::{self, Mirror as ObsMirror, Observation, RestartAlert as ObsRestartAlert, Thresholds};
use auron::fallback;
use auron::gather::{self, Journal, RealJournal, RealSystemctl, Systemctl};
use auron::heartbeat::{self, Heartbeat};
use auron::reconcile::{self, Trigger};
use auron::seam::BashSeam;
use auron::state::State;
use auron::util::{self, Sink, StdSink};

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

fn main() {
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
        eprintln!("auron: cannot find the harness's spira/ directory (pass --home, or set SPIRA_HOME/SPIRA_RELEASE)");
        std::process::exit(1);
    };

    // THE DATABASE GETS TEN SECONDS, NOT bdq's usual 180 (auron.sh's own reasoning,
    // kept): a bd that has not answered in ten seconds IS "unreachable" as far as a
    // watchdog is concerned. bdq reads BD_TIMEOUT from the seam subprocess's OWN
    // environment (it is a fresh process, not a subshell of this one), so the override
    // is set here rather than relying on it merely being in scope.
    let bd_timeout = env_n(&original, "SPIRA_AURON_BD_TIMEOUT", 10).max(1);
    let mut seam_env = original.clone();
    seam_env.insert("BD_TIMEOUT".to_string(), bd_timeout.to_string());

    let seam = BashSeam { lib: home.join("lib.sh"), env: &seam_env };

    // Wave 4.8 ("retire conf re-import seams in Rust"): `_auron_snapshot` used to be
    // `env -0` alone, which misses every SPIRA_* key conf.sh/lib.sh set but did not
    // export — SPIRA_REPO chief among them, so `repo` below was ALWAYS "" and the
    // mirror_path default silently became the filesystem-root-relative
    // "/raw/spira-beads/spira.jsonl" instead of "<repo>/raw/spira-beads/spira.jsonl"
    // (wave4-decomposition.md row (b): "unverified; check" — confirmed live on this box,
    // no SPIRA_AURON_MIRROR override in force). SPIRA_AURON_RESTARTS/_WINDOW are conf
    // keys too, but were read from the raw environment below (bypassing `snap`
    // entirely), so a toml override of either was silently ignored — the same row's
    // other named hazard. `_auron_snapshot`/`seam::snapshot` had exactly this one
    // caller, so it is retired outright rather than ported: `derive_home_repo` plus
    // `resolve_for_process`, in-process, replace both the seam call and the bug.
    let repo_path = spira_config::resolve::derive_home_repo(&home, &original);
    let resolved = match spira_config::resolve::resolve_for_process(&home, &repo_path, &original) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("auron: could not resolve config: {e}");
            std::process::exit(1);
        }
    };
    let rget = |k: &str, d: &str| {
        let v = resolved.get(k);
        if v.is_empty() { d.to_string() } else { v.to_string() }
    };
    let rnum = |k: &str, d: i64| resolved.get(k).trim().parse().unwrap_or(d);

    let now = util::now_epoch();
    let sink = StdSink;

    // ---- config, exactly auron.sh's own env reads (SPIRA_AURON_* from the raw
    // environment — auron-specific, no conf.sh key; everything else resolved in-process) ---
    let run_dir = PathBuf::from(rget("SPIRA_RUN", "/run/spira"));
    let db = rget("SPIRA_DB", "");
    let repo = repo_path.to_string_lossy().into_owned();
    let exporter = rget("SPIRA_EXPORTER", "");
    let systemctl_bin = env_or(&original, "SPIRA_SYSTEMCTL", "systemctl");
    let instance = rget("SPIRA_INSTANCE", "");
    let tz = rget("SPIRA_TZ", "UTC");

    let state_path = run_dir.join("auron.state");
    let status_path = run_dir.join("auron.status");
    let fallback_path = PathBuf::from(env_or(&original, "SPIRA_AURON_FALLBACK", &run_dir.join("auron.alerts.json").display().to_string()));
    let sentinel_log_path = PathBuf::from(env_or(&original, "SPIRA_AURON_SENTINEL_LOG", &run_dir.join("sentinel.log").display().to_string()));
    let strands_path = PathBuf::from(env_or(&original, "SPIRA_AURON_STRANDS", &run_dir.join("strands.json").display().to_string()));
    let mirror_path = PathBuf::from(env_or(&original, "SPIRA_AURON_MIRROR", &format!("{repo}/raw/spira-beads/spira.jsonl")));
    let tail_bytes = env_n(&original, "SPIRA_AURON_TAIL_BYTES", 262_144).max(0) as u64;
    let confirm = env_n(&original, "SPIRA_AURON_CONFIRM", 2);
    let clear_n = env_n(&original, "SPIRA_AURON_CLEAR", 2);
    let refresh = env_n(&original, "SPIRA_AURON_REFRESH", 3600);
    let restarts_threshold = rnum("SPIRA_AURON_RESTARTS", 5);
    let restart_window = rnum("SPIRA_AURON_RESTART_WINDOW", 3600);
    let drain_ttl = env_n(&original, "SPIRA_DRAIN_TTL", 1800);
    let thresholds = Thresholds {
        pass_stale: env_n(&original, "SPIRA_AURON_PASS_STALE", 600),
        starve_passes: env_n(&original, "SPIRA_AURON_STARVE_PASSES", 5),
        mirror_stale: env_n(&original, "SPIRA_AURON_MIRROR_STALE", 90_000),
        ghost_stale: env_n(&original, "SPIRA_AURON_GHOST_STALE", 1800),
        restart_threshold: restarts_threshold,
        restart_window,
    };

    // ---- state -----------------------------------------------------------------------
    let state_text = std::fs::read_to_string(&state_path).unwrap_or_default();
    let mut state = State::parse(&state_text);
    if state.first_run == 0 {
        state.first_run = now;
    }

    // ---- gather ------------------------------------------------------------------------
    let log_tail = gather::read_log_tail(&sentinel_log_path, tail_bytes);
    let sc = RealSystemctl { bin: systemctl_bin.clone(), timeout: Duration::from_secs(5) };
    let journal = RealJournal { bin: "journalctl".to_string(), timeout: Duration::from_secs(5) };
    let sentinel_timer = gather::sentinel_timer_status(&sc, &instance);
    let mirror = gather::check_mirror(&exporter, &mirror_path);
    let world = gather::world_state(&run_dir, now, drain_ttl);
    let strands: serde_json::Value = std::fs::read_to_string(&strands_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::json!({}));

    let mut restart_alerts: Vec<ObsRestartAlert> = Vec::new();
    let units = sc.spira_services();
    if !units.is_empty() {
        let current = sc.nrestarts(&units);
        for (unit, cur) in &current {
            let (base, delta) = auron::restart::update(state.restarts.get(unit), *cur, now, restart_window);
            state.restarts.insert(unit.clone(), base);
            if delta > restarts_threshold {
                restart_alerts.push(ObsRestartAlert { unit: unit.clone(), current: *cur, baseline: state.restarts[unit].baseline, delta, window: restart_window, journal: journal.tail(unit, 10) });
            }
        }
    }

    // THE ALERT QUERY IS ALSO THE READ PROBE (auron.sh's own reasoning, kept): asking a
    // second, separate "is the database readable" question would be a second thing to get
    // out of step with the first.
    let ops = SeamBdOps { seam: &seam };
    let mut db_saturated = false;
    let (db_reachable, db_error, alert_rows) = match ops.list_by_label("alert") {
        Ok(rows) => (true, String::new(), rows),
        Err(f) => {
            if f == auron::bdops::Failure::Saturated {
                db_saturated = true;
            }
            (false, "bd list --label alert did not answer".to_string(), Vec::new())
        }
    };

    // THE WRITE PROBE, unconditional on every pass (§ alerts.rs's own doc).
    let probe = if db_reachable { alerts::maintain_probe(&ops, &state.probe_id) } else { ProbeOutcome { id: None, write_ok: None, saturated: false } };
    state.probe_id = probe.id.clone().unwrap_or_default();
    if probe.saturated {
        db_saturated = true;
    }

    // ---- classify ------------------------------------------------------------------------
    let obs = Observation {
        now,
        auron_first: state.first_run,
        sentinel_log: log_tail.text,
        sentinel_log_readable: log_tail.readable,
        sentinel_log_error: log_tail.error,
        sentinel_log_mtime: log_tail.mtime,
        sentinel_log_path: sentinel_log_path.display().to_string(),
        sentinel_timer,
        db_reachable,
        db_error,
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
    let firing = classify::run(&obs);
    let firing_keys: BTreeSet<String> = firing.iter().map(|f| f.key.clone()).collect();
    let titles: BTreeMap<String, String> = firing.iter().map(|f| (f.key.clone(), f.title.clone())).collect();
    let evidence_of = |k: &str| firing.iter().find(|f| f.key == k).map(|f| f.evidence.clone()).unwrap_or_default();

    if report {
        if firing.is_empty() {
            println!(
                "auron: nothing firing (sentinel timer {}, db read: {} write: {})",
                obs.sentinel_timer,
                if db_reachable { "ok" } else { "UNREACHABLE" },
                match probe.write_ok {
                    Some(true) => "ok",
                    Some(false) => "FAILED",
                    None => "unknown",
                }
            );
        } else {
            for f in &firing {
                println!("\n=== {} ===\n{}\n\n{}\n", f.key, f.title, f.evidence);
            }
        }
        std::process::exit(0);
    }

    // ---- reconcile -------------------------------------------------------------------
    let steps = reconcile::step_keys(now, confirm.max(1), clear_n.max(1), refresh.max(1), db_reachable, &firing_keys, &state.keys);
    let mut beads_ok = true;
    let mut acted: i64 = 0;
    state.keys.clear();
    for step in &steps {
        let key = &step.key;
        let existing = alerts::find_by_key(&alert_rows, key);
        match step.trigger {
            Trigger::None => {
                state.keys.insert(key.clone(), step.counters.clone());
            }
            Trigger::ConfirmRaise => {
                let title = titles.get(key).cloned().unwrap_or_else(|| key.clone());
                let body = alerts::body_of(key, step.counters.first, step.counters.flaps, &evidence_of(key), &tz);
                match alerts::alert_write(&ops, key, &title, &body, step.counters.flaps, existing) {
                    Ok(id) => {
                        sink.out(&util::log_line(now, &format!("AURON raised {key} as {id}")));
                        let mut k = step.on_success(now);
                        k.bead = id;
                        state.keys.insert(key.clone(), k);
                        acted += 1;
                    }
                    Err(_) => {
                        sink.err(&util::log_line(now, &format!("AURON could not create the alert bead for {key}")));
                        state.keys.insert(key.clone(), step.on_failure());
                        beads_ok = false;
                    }
                }
            }
            Trigger::ConfirmClear => match alerts::alert_clear(&ops, existing, step.counters.first, step.counters.flaps, clear_n, &tz) {
                Ok(()) => {
                    if let Some(row) = existing {
                        sink.out(&util::log_line(now, &format!("AURON cleared {key} ({})", row.id)));
                    }
                    state.keys.insert(key.clone(), step.on_success(now));
                    acted += 1;
                }
                Err(_) => {
                    state.keys.insert(key.clone(), step.on_failure());
                    beads_ok = false;
                }
            },
            Trigger::Refresh => {
                let closed_by_hand = existing.map(|r| r.status == "closed").unwrap_or(false);
                if closed_by_hand {
                    state.keys.insert(key.clone(), step.on_refresh_skip_closed(now));
                } else {
                    let title = titles.get(key).cloned().unwrap_or_else(|| key.clone());
                    let body = alerts::body_of(key, step.counters.first, step.counters.flaps, &evidence_of(key), &tz);
                    match alerts::alert_write(&ops, key, &title, &body, step.counters.flaps, existing) {
                        Ok(id) => {
                            sink.out(&util::log_line(now, &format!("AURON re-raised {key} on {id}")));
                            let mut k = step.on_success(now);
                            k.bead = id;
                            state.keys.insert(key.clone(), k);
                        }
                        Err(_) => {
                            state.keys.insert(key.clone(), step.counters.clone());
                            beads_ok = false;
                        }
                    }
                }
            }
        }
    }

    // ---- the fallback file -------------------------------------------------------------
    let fallback_needed = !db_reachable || !beads_ok || probe.write_ok == Some(false);
    if fallback_needed {
        let doc = fallback::build(now, db_reachable, probe.write_ok, &firing);
        match fallback::write(&fallback_path, &doc) {
            Ok(()) => sink.out(&util::log_line(
                now,
                &format!("AURON wrote the fallback channel — db_read={} db_write={:?}", if db_reachable { "ok" } else { "DOWN" }, probe.write_ok),
            )),
            Err(e) => sink.err(&util::log_line(now, &format!("AURON could not write the fallback channel: {e}"))),
        }
    } else {
        fallback::remove(&fallback_path);
    }

    // ---- state, then the heartbeat (written on every run, including a failing one) -----
    let _ = std::fs::write(&state_path, state.render());

    let db_read_status = heartbeat::db_read_status(db_reachable, db_saturated);
    let db_write_status = heartbeat::db_write_status(probe.write_ok, db_saturated);
    let firing_keys_vec: Vec<String> = firing_keys.iter().cloned().collect();
    let hb = Heartbeat { at: now, firing_keys: &firing_keys_vec, db_read: db_read_status, db_write: db_write_status, db_saturated, fallback_written: fallback_needed, acted };
    let _ = heartbeat::write(&status_path, &hb.render());

    sink.out(&util::log_line(
        now,
        &format!(
            "auron: {} firing [{}], {} change(s), db_read={} db_write={}",
            firing_keys_vec.len(),
            firing_keys_vec.join(","),
            acted,
            db_read_status.as_str(),
            db_write_status.as_str()
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_home_prefers_the_flag() {
        let dir = testkit::TempDir::new("auron-main");
        std::fs::write(dir.join("lib.sh"), "").unwrap();
        let home = resolve_home(Some(dir.to_str().unwrap()), &BTreeMap::new());
        assert_eq!(home, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn resolve_home_falls_back_to_spira_home_env() {
        let dir = testkit::TempDir::new("auron-main");
        std::fs::write(dir.join("lib.sh"), "").unwrap();
        let mut env = BTreeMap::new();
        env.insert("SPIRA_HOME".to_string(), dir.display().to_string());
        assert_eq!(resolve_home(None, &env), Some(dir.path().to_path_buf()));
    }

    #[test]
    fn resolve_home_falls_back_to_spira_release_spira() {
        let dir = testkit::TempDir::new("auron-main");
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
