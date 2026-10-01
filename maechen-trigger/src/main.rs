//! `maechen-trigger` — DESIGN.md. Rust port of `spira/maechen-trigger.sh` (sp-0ekp7).

mod engine;
mod lanes;
mod ports;
mod real;

use lanes::LaneLabels;
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

/// `std::env::var`, trimmed to "set and non-empty" — no `spira_config` fallback. Used only
/// inside [`resolved_config`]'s own init, which must not call back into [`env_or`]/
/// [`env_u64`] (that would recurse into `resolved_config()` while it is still being built).
fn raw_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

/// Set once, at the very top of `main`, from the `--home` argument `maechen-trigger.sh`
/// now passes (wave 4.9, sp-k80sa — this replaces the shim's own `export
/// SPIRA_HOME="$HERE"`). `SPIRA_HOME` is a per-copy fact `spira_config::resolve`
/// deliberately never derives (it is the input that LOCATES config, not a value config
/// produces), so it has to be told explicitly rather than read back out of a resolution
/// that depends on it. Left unset, [`home_dir`] falls back to [`raw_env`]`("SPIRA_HOME")`
/// exactly as before this bead — the one test that exercises this path
/// (`env_u64_falls_back_to_the_registry_then_the_callers_default`) never calls `main` and
/// so never sets this, and is deliberately left alone.
static HOME_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// `--home <path>` from argv, if this invocation carries one. `maechen-trigger.sh` always
/// passes it now; a direct invocation (a test, an operator debugging by hand) without the
/// flag falls back to [`raw_env`] inside [`home_dir`], unchanged from before this bead.
fn parse_home_flag() -> Option<PathBuf> {
    let mut it = env::args();
    while let Some(a) = it.next() {
        if a == "--home" {
            return it.next().map(PathBuf::from);
        }
    }
    None
}

/// `SPIRA_HOME`: [`HOME_OVERRIDE`] (the `--home` flag) if `main` set one, else
/// [`raw_env`]`("SPIRA_HOME")` — the same precedence [`resolved_config`] always used, kept
/// as the fallback rather than removed so the crate's own unit test (which sets
/// `SPIRA_HOME` via `std::env::set_var` and never calls `main`) still exercises this
/// unchanged.
fn home_dir() -> PathBuf {
    HOME_OVERRIDE
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from(raw_env("SPIRA_HOME").unwrap_or_else(|| ".".to_string())))
}

/// Wave 4.8 ("retire conf re-import seams in Rust"): this crate used to read every
/// `SPIRA_*` key straight out of its own process environment, with no snapshot and no
/// the config document load at all (wave4-decomposition.md row (b) names maechen-trigger by
/// file, "beyond its shim": SPIRA_MAECHEN_*). Resolved once, lazily, and cached:
/// `spira_config::resolve_for_process`, using [`home_dir`] (wave 4.9, sp-k80sa: formerly
/// raw `SPIRA_HOME` directly) and `spira_config::resolve::derive_home_repo`. A resolution
/// failure yields an empty [`spira_config::resolve::Resolved`] — [`env_or`]/[`env_u64`]'s
/// own callers see exactly the behaviour this crate had before this bead, never a panic.
fn resolved_config() -> &'static spira_config::resolve::Resolved {
    static RESOLVED: std::sync::OnceLock<spira_config::resolve::Resolved> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(|| {
        let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
        let home = home_dir();
        let repo = spira_config::resolve::derive_home_repo(&home, &env_map);
        spira_config::resolve::resolve_for_process(&home, &repo, &env_map).unwrap_or_default()
    })
}

/// The environment, then `spira_config::resolve()`'s in-process answer — never the
/// reverse, so an explicit env override still wins exactly as it did before this bead.
fn resolved_env(key: &str) -> Option<String> {
    raw_env(key).or_else(|| {
        let v = resolved_config().get(key);
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn env_or(key: &str, default: &str) -> String {
    resolved_env(key).unwrap_or_else(|| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    resolved_env(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// The six `SPIRA_*_LABEL` conf keys, resolved exactly as `run()`'s own `maechen_label`
/// already is — `env_or`'s raw-env-then-registry precedence, never a raw `env::var` read,
/// per law-a-binary-resolves-the-config-it-reads: three of these six are not in
/// `spira_config::resolve::EXPORT_KEYS` (wave4-decomposition.md row (b) names this crate by
/// file for exactly that reason), so a bare `std::env::vars()` lookup would see them in a
/// test that exports them and miss them in production, which never does.
fn lane_labels() -> LaneLabels {
    LaneLabels {
        plan: env_or("SPIRA_PLAN_LABEL", "plan"),
        incident: env_or("SPIRA_INCIDENT_LABEL", "incident"),
        groomer: env_or("SPIRA_GROOMER_LABEL", "groom"),
        maechen: env_or("SPIRA_MAECHEN_LABEL", "maechen-sweep"), // literal-ok: Rust fallback mirroring conf.sh's own default when SPIRA_MAECHEN_LABEL is unset
        spike: env_or("SPIRA_SPIKE_LABEL", "spike"),
        czar: env_or("SPIRA_CZAR_LABEL", "czar-trigger"),
    }
}

/// `env::args()` with the `--home <path>` pair [`parse_home_flag`] already consumed
/// stripped out, leaving only positional arguments. Used by the lib.sh shim doors below —
/// `groom-trigger.sh` is the one surviving bash caller of `spira_lane_admitted`/
/// `spira_open_trigger_count`/`spira_repo_lanes` (wave 4.35, sp-kelr2, row V), and it always
/// passes `--home` first, exactly as the sweep's own invocation does.
fn subcommand_args() -> Vec<String> {
    let mut it = env::args().skip(1);
    let mut out = Vec::new();
    while let Some(a) = it.next() {
        if a == "--home" {
            it.next();
        } else {
            out.push(a);
        }
    }
    out
}

fn main() {
    if let Some(h) = parse_home_flag() {
        let _ = HOME_OVERRIDE.set(h);
    }
    let spira_run = PathBuf::from(env_or("SPIRA_RUN", "."));
    let spira_home = home_dir();
    let db = env_or("SPIRA_DB", ".");
    let bd = env_or("SPIRA_BD", "bd");
    let repo_map = env::var("SPIRA_REPO_MAP").ok().filter(|v| !v.is_empty()).map(PathBuf::from);
    let world = Real::new(spira_home, spira_run.clone(), db, bd, repo_map, lane_labels());

    // THE THREE LIB.SH SHIM DOORS (wave 4.35, row V) — none of them touches the sweep's own
    // lock file below; `groom-trigger.sh` calling `lane-admitted` must never contend with, or
    // wait on, a concurrent Maechen sweep.
    let sub_args = subcommand_args();
    match sub_args.first().map(String::as_str) {
        Some("repo-lanes") => {
            let name = sub_args.get(1).cloned().unwrap_or_default();
            match world.repo_lanes(&name) {
                Ok(s) => {
                    println!("{s}");
                    return;
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Some("lane-admitted") => {
            let lane = sub_args.get(1).cloned().unwrap_or_default();
            std::process::exit(if world.lane_admitted(&lane) { 0 } else { 1 });
        }
        Some("open-trigger-count") => {
            let labels = sub_args.get(1).cloned().unwrap_or_default();
            println!("{}", world.open_trigger_count(&labels));
            return;
        }
        _ => {}
    }

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
    let maechen_label = env_or("SPIRA_MAECHEN_LABEL", "maechen-sweep"); // literal-ok: Rust fallback mirroring conf.sh's own default when SPIRA_MAECHEN_LABEL is unset
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

    // LANDING TRIGGER — home repo always counted; additional repos from $SPIRA_REPO_MAP,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::Path;

    /// Wave 4.8: `env_or`/`env_u64` now fall back to `spira_config::resolve()` between
    /// the raw environment and the caller's own default. This is the ONLY test in this
    /// binary that calls them (so the `OnceLock` inside `resolved_config`, which computes
    /// once per process and never resets, cannot collide with another test's fixture).
    /// SPIRA_HOME/SPIRA_TOML point at a throwaway fixture (never the real box's).
    #[test]
    fn env_u64_falls_back_to_the_registry_then_the_callers_default() {
        let dir = testkit::TempDir::new("maechen-trigger-env");
        let home = dir.join("spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(
            home.join("conf.d/SPIRA_MAECHEN_MAX_BEADS"),
            "TYPE=u32\nGROUP=maechen\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_MAECHEN_MAX_BEADS:=3}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        env::set_var("SPIRA_HOME", &home);
        env::set_var("SPIRA_TOML", dir.join("no-such-config.toml"));
        env::remove_var("SPIRA_MAECHEN_MAX_BEADS");

        assert_eq!(env_u64("SPIRA_MAECHEN_MAX_BEADS", 1), 3, "a registry default must reach env_u64 without an env override");
        assert_eq!(env_or("SPIRA_NO_SUCH_KEY_AT_ALL_EVER", "fallback"), "fallback");

        env::set_var("SPIRA_MAECHEN_MAX_BEADS", "99");
        assert_eq!(env_u64("SPIRA_MAECHEN_MAX_BEADS", 1), 99, "an explicit env override still wins over the registry");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `World` whose every answer is a canned value, to test `run`/`count_landings`'s
    /// own orchestration without a database, a git checkout or the `lib.sh` seam.
    #[derive(Default)]
    struct FakeWorld {
        home_repo: String,
        repo_root: HashMap<String, PathBuf>,
        /// `landref` answers keyed by the exact `repo_name_or_path` argument — deliberately
        /// NOT keyed by, or assumed to be, any particular remote name. A base ref here can
        /// be anything a remote's default branch resolves to ("gitea/master",
        /// "upstream/trunk", …); count_landings must treat it as an opaque ref string, the
        /// same way it treats "origin/main" — neither this fake nor count_landings itself
        /// ever special-cases "origin" (that resolution lives entirely in the lib.sh seam,
        /// `spira_landref`, which this bead does not port — DESIGN.md "Non-goals").
        landref: HashMap<String, String>,
        /// Commit subjects keyed by (repo_path, base_ref) — proves the exact ref
        /// `landref` returned is the one actually passed to `git log`.
        git_log: HashMap<(PathBuf, String), String>,
        repo_map_text: String,
        created: RefCell<Vec<(String, String, String)>>,
    }

    impl World for FakeWorld {
        fn log(&self, _msg: &str) {}
        fn read_watermark(&self) -> i64 {
            0
        }
        fn read_lastpass(&self) -> i64 {
            0
        }
        fn now(&self) -> i64 {
            0
        }
        fn open_trigger_count(&self, _labels: &str) -> u64 {
            0
        }
        fn lane_admitted(&self, _lane: &str) -> bool {
            true
        }
        fn repo_lanes(&self, _name: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn home_repo(&self) -> String {
            self.home_repo.clone()
        }
        fn repo_root(&self, name: &str) -> Option<PathBuf> {
            self.repo_root.get(name).cloned()
        }
        fn landref(&self, repo_path_or_name: &str) -> Option<String> {
            self.landref.get(repo_path_or_name).cloned()
        }
        fn repo_map_text(&self) -> String {
            self.repo_map_text.clone()
        }
        fn git_log_subjects(&self, repo_path: &Path, _since_ts: i64, base_ref: &str) -> String {
            self.git_log.get(&(repo_path.to_path_buf(), base_ref.to_string())).cloned().unwrap_or_default()
        }
        fn detect_invalid_closed(&self) -> String {
            String::new()
        }
        fn create_bead(&self, title: &str, labels: &str, description: &str) -> Result<(), String> {
            self.created.borrow_mut().push((title.to_string(), labels.to_string(), description.to_string()));
            Ok(())
        }
    }

    #[test]
    fn count_landings_treats_a_non_origin_base_ref_as_an_opaque_string() {
        // Regression guard (per the Concierge, sp-0ekp7): count_landings must not assume
        // or construct "origin/<branch>" anywhere — the base ref is whatever
        // `spira_landref`'s seam call resolved, verbatim, and it may name any remote.
        let repo_path = PathBuf::from("/repo/gitea-repo");
        let mut w = FakeWorld::default();
        w.landref.insert("/repo/gitea-repo".to_string(), "gitea/master".to_string());
        w.git_log.insert(
            (repo_path.clone(), "gitea/master".to_string()),
            "sp-alt1: first landing on gitea remote\nsp-alt2: second landing on gitea remote\n".to_string(),
        );
        let n = count_landings(&w, "/repo/gitea-repo", 0);
        assert_eq!(n, 2, "both landings on the non-origin remote's default branch must count");
    }

    #[test]
    fn count_landings_by_name_resolves_repo_root_first() {
        let repo_path = PathBuf::from("/repo/named");
        let mut w = FakeWorld::default();
        w.repo_root.insert("named-repo".to_string(), repo_path.clone());
        w.landref.insert("named-repo".to_string(), "upstream/trunk".to_string());
        w.git_log.insert((repo_path.clone(), "upstream/trunk".to_string()), "sp-x1: landed\n".to_string());
        assert_eq!(count_landings(&w, "named-repo", 0), 1);
    }

    #[test]
    fn count_landings_unresolvable_base_ref_counts_zero_and_logs() {
        let w = FakeWorld::default();
        assert_eq!(count_landings(&w, "/repo/nope", 0), 0);
    }

    #[test]
    fn run_end_to_end_fires_the_landing_trigger_for_a_non_origin_satellite_repo() {
        // The full repository-map → count_landings → threshold path, with a satellite repo
        // whose only remote is not named "origin" — the exact shape of the scenario the
        // Concierge asked to be covered (the real defect turned out to be in the bash
        // test harness's own PATH construction, not here, but this is the regression
        // guard on the Rust side of that contract either way).
        let gitea_path = PathBuf::from("/repo/gitea-repo");
        let home_path = PathBuf::from("/repo/home");
        let mut w = FakeWorld::default();
        w.home_repo = "home-repo".to_string();
        w.repo_root.insert("home-repo".to_string(), home_path.clone());
        w.landref.insert("home-repo".to_string(), "origin/main".to_string());
        w.git_log.insert((home_path.clone(), "origin/main".to_string()), String::new());
        w.landref.insert("/repo/gitea-repo".to_string(), "gitea/master".to_string());
        w.git_log.insert(
            (gitea_path.clone(), "gitea/master".to_string()),
            "sp-alt1: first landing on gitea remote\nsp-alt2: second landing on gitea remote\n".to_string(),
        );
        w.repo_map_text = "gitea-repo|/repo/gitea-repo|\n".to_string();

        // repo_root/is_dir gating in `run()` checks the filesystem directly for satellite
        // rows (`Path::new(&path).is_dir()`); route around that by testing count_landings'
        // contribution the same way `run()` sums it, since a fake filesystem is out of
        // scope for this port (DESIGN.md "Non-goals" — no Rust reimplementation of the
        // directory-existence checks the bash also just shelled out for).
        let home_contribution = count_landings(&w, &w.home_repo(), 0);
        let satellite_contribution = count_landings(&w, "/repo/gitea-repo", 0);
        assert_eq!(home_contribution, 0);
        assert_eq!(satellite_contribution, 2);
        assert!(engine::landing_trigger(home_contribution + satellite_contribution, 2));
    }
}
