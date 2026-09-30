//! `maechen-trigger` — DESIGN.md. Rust port of `spira/maechen-trigger.sh` (sp-0ekp7).

mod engine;
mod ports;
mod real;

use ports::World;
use real::Real;
use std::env;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

fn env_or(key: &str, default: &str) -> String {
    env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() {
    let spira_run = PathBuf::from(env_or("SPIRA_RUN", "."));
    let spira_home = PathBuf::from(env_or("SPIRA_HOME", "."));
    let db = env_or("SPIRA_DB", ".");
    let bd = env_or("SPIRA_BD", "bd");
    let repo_map = env::var("SPIRA_REPO_MAP").ok().filter(|v| !v.is_empty()).map(PathBuf::from);
    let world = Real::new(spira_home, spira_run.clone(), db, bd, repo_map);

    // MUTUAL EXCLUSION (gap G10) — non-blocking; a caller that loses the race skips this
    // tick rather than risking two overlapping list-then-create dedup checks (sp-uq55c,
    // sp-io5e). The lock file is leaked deliberately (never closed): closing it here would
    // release it before the process exits, and the kernel reclaims it at process exit anyway.
    let lock_path = spira_run.join("maechen-trigger.lock");
    match OpenOptions::new().create(true).write(true).open(&lock_path) {
        Ok(lock_file) => {
            if unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
                world.log("another instance holds the lock — skipping to avoid a duplicate trigger");
                std::mem::forget(lock_file);
                return;
            }
            std::mem::forget(lock_file);
        }
        Err(e) => {
            world.log(&format!("cannot open lock {}: {e} — proceeding unlocked", lock_path.display()));
        }
    }

    if !run(&world) {
        std::process::exit(1);
    }
}

/// Returns `false` only on a filing failure (bash exit 1); every other path — dedup skip,
/// lane skip, no-trigger — is success (bash exit 0).
fn run(world: &dyn World) -> bool {
    let scope_label = env::var("SPIRA_SCOPE_LABEL").unwrap_or_default();
    let maechen_label = env_or("SPIRA_MAECHEN_LABEL", "maechen-sweep");
    let labels = engine::trigger_labels(&scope_label, &maechen_label);

    // DEDUP — at most one open-or-in-progress trigger bead at a time.
    let open_count = world.open_trigger_count(&labels);
    if open_count > 0 {
        world.log(&format!(
            "trigger already open or in_progress ({open_count} bead(s) with labels [{labels}]) — skipping"
        ));
        return true;
    }

    // LANE CHECK — skip when no repository admits the maechen lane.
    if !world.lane_admitted(&maechen_label) {
        world.log(&format!("no repository admits lane {maechen_label} — skipping trigger"));
        return true;
    }

    let watermark_ts = world.read_watermark();
    let lastpass_ts = world.read_lastpass();
    let now_ts = world.now();
    let elapsed = now_ts - lastpass_ts;

    let max_gap = env_u64("SPIRA_MAECHEN_MAX_GAP_SECONDS", 10_800) as i64;
    let time_fired = engine::time_trigger(elapsed, max_gap);
    if time_fired {
        world.log(&format!("time trigger: {elapsed}s elapsed since last pass (threshold: {max_gap}s)"));
    }

    // LANDING TRIGGER — home repo always counted; additional repos from the repo-map,
    // skipping the home repo to avoid double-counting.
    let home_repo = world.home_repo();
    let mut landing_count: u64 = 0;
    if let Some(home_path) = world.repo_root(&home_repo) {
        if home_path.is_dir() {
            landing_count += count_landings(world, &home_repo, watermark_ts);
        }
    }
    for (name, path) in engine::parse_repo_map(&world.repo_map_text()) {
        if name == home_repo {
            continue;
        }
        if !std::path::Path::new(&path).is_dir() {
            continue;
        }
        landing_count += count_landings(world, &path, watermark_ts);
    }

    let landing_interval = env_u64("SPIRA_MAECHEN_LANDING_INTERVAL", 25);
    let landing_fired = engine::landing_trigger(landing_count, landing_interval);
    if landing_fired {
        world.log(&format!(
            "landing trigger: {landing_count} landings since watermark (threshold: {landing_interval})"
        ));
    }

    // INVALID-CLOSED TRIGGER.
    let ic_raw = world.detect_invalid_closed();
    let ic_rows = engine::invalid_closed_rows(&ic_raw);
    let ic_fired = !ic_rows.is_empty();
    if ic_fired {
        world.log(&format!("invalid-closed trigger: {} row(s) found", ic_rows.len()));
    }

    let reason = engine::TriggerReason { time: time_fired, landing: landing_fired, invalid_closed: ic_fired };
    if !reason.any() {
        world.log(&format!(
            "no trigger: {landing_count} landings (threshold: {landing_interval}), {elapsed}s since last pass (threshold: {max_gap}s), 0 invalid-closed rows"
        ));
        return true;
    }

    let reason_text = engine::reason_text(&reason, elapsed, landing_count, ic_rows.len());
    let max_beads = env_u64("SPIRA_MAECHEN_MAX_BEADS", 3);
    let ic_rows_text = if ic_fired { Some(ic_rows.join("\n")) } else { None };
    let description = engine::description(max_beads, &reason_text, lastpass_ts, watermark_ts, ic_rows_text.as_deref());

    let sop_ledger_labels = format!("{labels},delivers:note:{}/maechen.log", env_or("SPIRA_RUN", "."));
    let title = format!("Maechen pass — {reason_text}");
    match world.create_bead(&title, &sop_ledger_labels, &description) {
        Ok(()) => {
            world.log(&format!("Maechen trigger bead filed (labels: {sop_ledger_labels}, reason: {reason_text})"));
            true
        }
        Err(e) => {
            world.log(&format!("ERROR: failed to file Maechen trigger bead: {e}"));
            false
        }
    }
}

/// Landings on one repository (by name or absolute path) since `since_ts`. Prints nothing
/// and counts 0 when the base ref cannot be resolved — matches the bash's own log-and-skip.
fn count_landings(world: &dyn World, repo_name_or_path: &str, since_ts: i64) -> u64 {
    let Some(base_ref) = world.landref(repo_name_or_path) else {
        world.log(&format!("landing count: cannot resolve base ref for {repo_name_or_path} — skipped"));
        return 0;
    };
    let repo_path: PathBuf = if repo_name_or_path.contains('/') {
        PathBuf::from(repo_name_or_path)
    } else {
        match world.repo_root(repo_name_or_path) {
            Some(p) => p,
            None => return 0,
        }
    };
    let subjects = world.git_log_subjects(&repo_path, since_ts, &base_ref);
    engine::parse_landing_ids(&subjects).len() as u64
}
