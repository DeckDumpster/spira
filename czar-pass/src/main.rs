// Czar fast pass: deterministic queue-stall detection on a 30-second timer.
// Ported from czar.sh (sp-rpibz) per law-new-subsystems-are-rust (sp-54qsc).
//
// czar-pass --pass
//
// Reads only cheap sources: the landing.log tail since the last pass, the open batch
// record, the lifecycle machine (spira-lc), and at most 2 gh API calls (forge.sh batch-ci-status) for the
// open batch's run.

use czar_pass::{iso_of, parse_field, parse_iso_to_epoch};
use reconciler_engine::core::{last_remedy, record_remedy, step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{append_status, load_state, save_state, StateMap};
use reconciler_engine::paths;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

fn main() -> ExitCode {
    spira_otel::run::begin("czar-pass", spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref());
    let code = pass_main();
    spira_otel::run::finish(code);
    ExitCode::from(code as u8)
}

fn pass_main() -> i32 {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) == Some("--lint-default") {
        spira_otel::run::operation("lint-default");
        let text = args.get(2).map(String::as_str).unwrap_or("");
        if default_relaxes_a_check(text) {
            eprintln!("czar-pass: refused — a default may not disable or relax a check; restore the component, or escalate the restoration as the operator's order");
            return 1;
        }
        return 0;
    }
    if args.get(1).map(String::as_str) != Some("--pass") {
        eprintln!("usage: czar-pass --pass");
        return 2;
    }
    spira_otel::run::operation("pass");
    match run_pass() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("czar-pass: {}", e);
            1
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────────────

/// The programs the pass runs by name on the launcher's PATH (the queue is asked through
/// [`Config::batch_show`]). A test injects its own; nothing here is read from the environment.
#[derive(Clone)]
struct Seams {
    incident: String,
    forge: String,
    systemctl: String,
    spira_lc: String,
    sentinel: String,
    /// The unit manager's facts, through the `Units` port.
    units: std::sync::Arc<dyn runtime::units::Units>,
}

impl Seams {
    fn production() -> Seams {
        Seams {
            incident: "incident".to_string(),
            forge: must_cfg("SPIRA_FORGE"),
            systemctl: "systemctl".to_string(),
            spira_lc: "spira-lc".to_string(),
            sentinel: "sentinel".to_string(),
            units: std::sync::Arc::new(HostUnits(std::sync::OnceLock::new())),
        }
    }
}

/// The host's unit manager, reached on the first ask (most passes ask nothing). One it cannot
/// reach answers every call with that error, which a reader takes as it took a failed `systemctl`.
struct HostUnits(std::sync::OnceLock<Result<Box<dyn runtime::units::Units>, String>>);

impl HostUnits {
    fn get(&self) -> runtime::Result<&dyn runtime::units::Units> {
        self.0
            .get_or_init(|| {
                #[cfg(target_os = "linux")]
                let u = runtime::units::SystemdUnits::connect(std::sync::Arc::new(runtime::ports::WallClock))
                    .map(|u| Box::new(u) as Box<dyn runtime::units::Units>)
                    .map_err(|e| e.to_string());
                #[cfg(not(target_os = "linux"))]
                let u = Err("no unit manager backend on this platform".to_string());
                u
            })
            .as_ref()
            .map(|b| b.as_ref())
            .map_err(|e| runtime::RuntimeError::new(e.clone()))
    }
}

impl runtime::units::Units for HostUnits {
    fn start(&self, spec: &runtime::units::UnitSpec) -> runtime::Result<runtime::units::UnitHandle> {
        self.get()?.start(spec)
    }
    fn stop(&self, h: &runtime::units::UnitHandle) -> runtime::Result<()> {
        self.get()?.stop(h)
    }
    fn restart(&self, h: &runtime::units::UnitHandle) -> runtime::Result<()> {
        self.get()?.restart(h)
    }
    fn facts(&self, h: &runtime::units::UnitHandle) -> runtime::Result<runtime::units::UnitFacts> {
        self.get()?.facts(h)
    }
}

type BatchShow = std::sync::Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

#[derive(Clone)]
struct Config {
    spira_run: PathBuf,
    czar_log: PathBuf,
    qc_log: PathBuf,
    marker: PathBuf,
    seams: Seams,
    /// `queue batch show --repo <r>`: a forge repo's open batch record, wherever the queue
    /// keeps it; empty when none is open.
    batch_show: BatchShow,
    stall_secs: u64,
    ci_queued_max: u64,
    ci_red_max: u64,
    base_unreadable_grace: u64,
    lock_path: PathBuf,
    reconciler_state: PathBuf,
    store: std::sync::Arc<tsd::store::Pusher>,
    reconciler_stamp: PathBuf,
    reconciler_grace_secs: u64,
    reconciler_unit: String,
    round_min_n: u32,
    round_stall_secs: u64,
    spira_db: String,
    scope_label: String,
    czar_label: String,
    land_unit: String,
    repo_map: Option<PathBuf>,
    /// `SPIRA_CZAR_STAGE_<CLASS>` for every registered class (spira/conf.d), resolved once
    /// at construction via `cfg` — `stage()` below only ever looks this map up, it never
    /// reads config itself (per Ryan 2026-10-05: one source of config).
    stages: std::collections::BTreeMap<String, Stage>,
    now_secs: u64,
    now_iso: String,
}

/// Every `SPIRA_CZAR_STAGE_*` class this binary knows about (matches spira/conf.d's own
/// `SPIRA_CZAR_STAGE_*` registrations exactly — a class here with no registered key is a
/// config-registry bug, not a defaulting decision).
const CZAR_STAGE_CLASSES: &[&str] =
    &["ATTRIBUTION_FAILED", "BASE_RED", "CI_RED", "CI_STALLED", "DEADLOCK", "LOOP_STALLED", "POOL_IDLE", "RECONCILER_STALE", "SORT_FAILED", "STARVED"];

/// One source of config (per Ryan 2026-10-05): a registered key's value comes from
/// `spira_config::process::cfg`, resolved from `$SPIRA_TOML` — never from this process's
/// own environment, and never with a Rust-side default (every key read through here
/// already carries its default in spira/conf.d). A resolution failure is a named refusal,
/// not a silent fallback.
fn must_cfg(key: &str) -> String {
    match spira_config::process::cfg(key) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("czar-pass: {e}");
            std::process::exit(1);
        }
    }
}

fn must_cfg_parse<T: std::str::FromStr>(key: &str) -> T
where
    T::Err: std::fmt::Display,
{
    match spira_config::process::cfg_parse::<T>(key) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("czar-pass: {e}");
            std::process::exit(1);
        }
    }
}

impl Config {
    fn from_process(seams: Seams) -> Config {
        let spira_run_str = must_cfg("SPIRA_RUN");
        if spira_run_str.is_empty() {
            eprintln!("czar-pass: FATAL: cannot resolve spira.run (SPIRA_RUN resolved empty)");
            std::process::exit(1);
        }
        let spira_run = PathBuf::from(&spira_run_str);
        let now = unix_now();
        let iso = compute_now_iso();
        let stages = CZAR_STAGE_CLASSES
            .iter()
            .map(|class| {
                let key = format!("SPIRA_CZAR_STAGE_{class}");
                let stage = if must_cfg(&key) == "act" { Stage::Act } else { Stage::Shadow };
                (class.to_string(), stage)
            })
            .collect();
        let repo_map = must_cfg("SPIRA_REPO_MAP");
        Config {
            czar_log: spira_run.join("czar.log"),
            qc_log: spira_run.join("landing.log"),
            marker: spira_run.join("czar-pass.swept"),
            seams,
            batch_show: projection_batch_show(),
            stall_secs: must_cfg_parse("SPIRA_LOOP_STALL_SECS"),
            ci_queued_max: must_cfg_parse("SPIRA_CI_QUEUED_MAX_SECS"),
            ci_red_max: must_cfg_parse("SPIRA_CI_RED_MAX_SECS"),
            base_unreadable_grace: must_cfg_parse("SPIRA_BASE_CI_UNREADABLE_GRACE_SECS"),
            lock_path: spira_run.join("czar-pass.lock"),
            reconciler_state: paths::state(&spira_run),
            store: std::sync::Arc::new(tsd::store::Pusher::new(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().filter(|e| !e.trim().is_empty()))),
            reconciler_stamp: paths::pass_stamp(&spira_run),
            reconciler_grace_secs: must_cfg_parse("SPIRA_RECONCILER_GRACE_SECS"),
            reconciler_unit: format!("spira-runtime-{}.service", must_cfg("SPIRA_INSTANCE")),
            // Batcher's own adaptive_n floor (batcher/src/core.rs: raw.clamp(4.0, 30.0)) — a
            // pool below this can never be a PoolFull trigger, whatever the adaptive ceiling
            // turns out to be, so it is the one threshold this detector can check without
            // replaying batcher's own pool-history math.
            round_min_n: must_cfg_parse("SPIRA_QUEUE_ROUND_MIN_N"),
            round_stall_secs: must_cfg_parse("SPIRA_QUEUE_ROUND_STALL_SECS"),
            spira_db: must_cfg("SPIRA_DB"),
            scope_label: must_cfg("SPIRA_SCOPE_LABEL"),
            czar_label: must_cfg("SPIRA_CZAR_LABEL"),
            land_unit: "spira-landing".to_string(),
            repo_map: (!repo_map.is_empty()).then(|| PathBuf::from(repo_map)),
            stages,
            now_secs: now,
            now_iso: iso,
            spira_run,
        }
    }

    /// Looked up from `self.stages`, resolved once in `from_env` — never a config read of
    /// its own, so this stays callable from pure detector logic.
    fn stage(&self, class: &str) -> Stage {
        let key = class.to_uppercase().replace('-', "_");
        self.stages.get(&key).cloned().unwrap_or(Stage::Shadow)
    }
}

#[derive(Clone, PartialEq)]
enum Stage {
    Shadow,
    Act,
}

// ──────────────────────────────────────────────────────────────────────────────
// Utilities
// ──────────────────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn compute_now_iso() -> String {
    iso_of(unix_now())
}

fn log_print(msg: &str) {
    println!("{} spira: {}", compute_now_iso(), msg);
}

fn append_czar_log(path: &Path, content: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(content.as_bytes());
    }
}

fn telem(cfg: &Config, class: &str, verdict: &Verdict, remedy: &str, tier: &str) {
    let detected = if verdict.is_gap { "yes" } else { "no" };
    let lat = verdict.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
    let line = format!(
        "{} CLASS={} DETECTED={} STATUS={} REMEDY={} TIER={} LATENCY={}s\n",
        cfg.now_iso, class, detected, status_word(verdict), remedy, tier, lat
    );
    append_czar_log(&cfg.czar_log, &line);
    if verdict.is_gap {
        spira_otel::run::event("detected", &[("class", class), ("remedy", remedy), ("tier", tier)]);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// The reconciler invariant engine seam: every detector's hysteresis and remedy-
// verification runs through here instead of its own hand-rolled first-seen marker file
// (sp-pu7v6). `evaluate` does exactly three things: advance `key`'s HysteresisState by one
// pass, record the raw reading to the time series (every pass, not only when it changes —
// UNOBSERVABLE IS NEVER SATISFIED, so a forge call that failed is never indistinguishable
// from "nothing going on"), and return the Verdict a detector decides its action from.
// ──────────────────────────────────────────────────────────────────────────────

fn evaluate(cfg: &Config, state: &mut StateMap, key: &str, raw: RawStatus, grace_secs: u64) -> Verdict {
    let prev = state.remove(key).unwrap_or_default();
    let (verdict, next) = step(cfg.now_secs, raw, grace_secs, prev);
    append_status(&cfg.store, &cfg.now_iso, key, &verdict);
    if next != HysteresisState::default() {
        state.insert(key.to_string(), next);
    }
    verdict
}

fn status_word(v: &Verdict) -> &'static str {
    match &v.status {
        RawStatus::Satisfied => "satisfied",
        RawStatus::Gap { .. } => "gap",
        RawStatus::Unobservable { .. } => "unobservable",
        RawStatus::Deliberate { .. } => "deliberate",
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Shadow / act helpers
// ──────────────────────────────────────────────────────────────────────────────

fn czar_would_log(cfg: &Config, class: &str, kind: &str, desc: &str) {
    append_czar_log(
        &cfg.czar_log,
        &format!(
            "{} CZAR-WOULD: {} {} — {}\n",
            cfg.now_iso, class, kind, desc
        ),
    );
}

fn infer(cfg: &Config, class: &str, ref_: &str, subj: &str, body: &str) {
    match cfg.stage(class) {
        Stage::Shadow => czar_would_log(cfg, class, "inference", subj),
        Stage::Act => {
            let labels = if cfg.scope_label.is_empty() {
                cfg.czar_label.clone()
            } else {
                format!("{},{}", cfg.scope_label, cfg.czar_label)
            };
            // batch-job: the incident binary's store write has no short bound
            // FORK-EDGE: needs an incident library filing entry that takes the per-call SPIRA_INCIDENT_* fields (labels, type, express, actor, repo, ref, cause, fenrir-exempt); intake::file_payload reads them from the process environment only
            let mut child = Command::new(&cfg.seams.incident)
                .arg("file")
                .arg(subj)
                .arg("-")
                .env("SPIRA_DB", &cfg.spira_db)
                .env("SPIRA_INCIDENT_LABELS", labels)
                .env("SPIRA_INCIDENT_TYPE", "task")
                .env("SPIRA_INCIDENT_PRIORITY", "1")
                .env("SPIRA_INCIDENT_ACTOR", "czar-pass")
                .env("SPIRA_FENRIR_EXEMPT", "1")
                .env("SPIRA_INCIDENT_REPO", "spira")
                .env("SPIRA_INCIDENT_REF", format!("incident:queue-{}", ref_))
                .env("SPIRA_INCIDENT_CAUSE", class)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(ref mut child) = child {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(body.as_bytes());
                }
                let _ = child.wait();
            }
            log_print(&format!(
                "czar pass: {} → inference (filed, calling czar)",
                class
            ));
            call_rune_czar(cfg);
        }
    }
}

// A red base blocks every branch of its own repository, not one batch's members, so it is
// filed at P0 + express against the REPOSITORY THAT IS RED rather than at czar-pass's usual
// P1 against "spira" — a red main.py needs a builder in the failing repo, at the front of
// the queue, not a routine queue-health ticket. Priority does not imply express — it is asked
// for explicitly, as lifecycle state, through SPIRA_INCIDENT_EXPRESS.
fn infer_urgent(cfg: &Config, class: &str, ref_: &str, subj: &str, body: &str, repo: &str) {
    match cfg.stage(class) {
        Stage::Shadow => czar_would_log(cfg, class, "inference", subj),
        Stage::Act => {
            let mut labels = "plan".to_string();
            if !cfg.scope_label.is_empty() {
                labels = format!("{},{}", cfg.scope_label, labels);
            }
            // batch-job: the incident binary's store write has no short bound
            // FORK-EDGE: needs an incident library filing entry that takes the per-call SPIRA_INCIDENT_* fields (labels, type, express, actor, repo, ref, cause, fenrir-exempt); intake::file_payload reads them from the process environment only
            let mut child = Command::new(&cfg.seams.incident)
                .arg("file")
                .arg(subj)
                .arg("-")
                .env("SPIRA_DB", &cfg.spira_db)
                .env("SPIRA_INCIDENT_LABELS", labels)
                .env("SPIRA_INCIDENT_TYPE", "bug")
                .env("SPIRA_INCIDENT_PRIORITY", "0")
                .env("SPIRA_INCIDENT_EXPRESS", "1")
                .env("SPIRA_INCIDENT_ACTOR", "czar-pass")
                .env("SPIRA_FENRIR_EXEMPT", "1")
                .env("SPIRA_INCIDENT_REPO", repo)
                .env("SPIRA_INCIDENT_REF", format!("incident:base-red:{}", ref_))
                .env("SPIRA_INCIDENT_CAUSE", class)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(ref mut child) = child {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(body.as_bytes());
                }
                let _ = child.wait();
            }
            log_print(&format!(
                "czar pass: {} → inference (filed, calling czar)",
                class
            ));
            call_rune_czar(cfg);
        }
    }
}

fn det_action(cfg: &Config, class: &str, desc: &str, action: impl FnOnce()) {
    match cfg.stage(class) {
        Stage::Shadow => czar_would_log(cfg, class, "det", desc),
        Stage::Act => {
            log_print(&format!("czar pass: {} → det: {}", class, desc));
            action();
        }
    }
}

/// `sentinel --call czar` directly — no lib.sh sourcing at all (wave 4.27, family G,
/// sp-gzmd2: `call_rune` moved in-process into the sentinel crate). The own
/// `world.halted` pre-check this used to need is gone too: `sentinel --call` runs the
/// SAME `world_gate` sentinel's own pass does, which also catches a live DRAIN this
/// hand-rolled check never did.
fn call_rune_czar(cfg: &Config) {
    // batch-job: this runs whatever its caller names, as long as that takes
    // FORK-EDGE: machine leg sentinel (--call czar)
    let _ = Command::new(&cfg.seams.sentinel)
        .arg("--call")
        .arg("czar")
        .stderr(Stdio::null())
        .status();
}

// ──────────────────────────────────────────────────────────────────────────────
// Repo map: the base ref
// ──────────────────────────────────────────────────────────────────────────────

// The base ref a repository's branches are cut from and judged against (repo-map column
// 4), reduced to a bare branch name: `gh run list --branch` wants "main", not "origin/main".
// Empty in repo-map means "let spira_landref resolve it" (doctor.sh's own leave-empty
// convention) — czar-pass has no such resolver, so an empty or absent base skips base-red
// for that repo rather than guessing.
fn repo_base(name: &str, repo_map: &Option<PathBuf>) -> Option<String> {
    let map_path = repo_map.as_ref()?;
    let content = fs::read_to_string(map_path).ok()?;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        let parts: Vec<&str> = t.splitn(6, '|').collect();
        if parts.len() >= 4 && parts[0].trim() == name {
            let base = parts[3].trim();
            if base.is_empty() {
                return None;
            }
            return Some(base.rsplit('/').next().unwrap_or(base).to_string());
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────────
// Queue / forge helpers
// ──────────────────────────────────────────────────────────────────────────────

/// The open batch of each repo from the store's own projection, its batch records (and no
/// lifecycle row) loaded once on the first ask (one pass is one process): what `queue batch
/// show --repo` printed, with no queue process. A view the store cannot vouch for — not loaded,
/// or stale — could not say; it is never read as "no batch".
fn projection_batch_show() -> BatchShow {
    let views: std::sync::OnceLock<Result<store_view::Projection, String>> = std::sync::OnceLock::new();
    std::sync::Arc::new(move |repo: &str| {
        let p = views
            .get_or_init(|| {
                let p = store_view::Projection::configured(spira_config::lc_call::LcClient::configured()?)?;
                p.refresh_batches().map_err(String::from)?;
                Ok(p)
            })
            .as_ref()
            .map_err(|e| format!("could not say: {e}"))?;
        shown_of(p.batch_view(repo).map(|r| r.value))
    })
}

/// `batch show`'s stdout from a view's answer; a refusal is "could not say".
fn shown_of(read: Result<store_view::BatchView, spira_config::lc_call::LcError>) -> Result<String, String> {
    read.map(|v| v.shown()).map_err(|e| format!("could not say: {e}"))
}

/// A forge repo's open batch as the queue shows it.
#[derive(Debug, PartialEq, Eq)]
struct OpenBatch {
    branch: Option<String>,
}

/// `repos` with each one's open batch: `Ok(None)` when none is open, `Err` when the queue could
/// not say — never read as no batch.
fn open_batches(cfg: &Config, repos: Vec<(String, PathBuf)>) -> Vec<(String, PathBuf, Result<Option<OpenBatch>, String>)> {
    repos
        .into_iter()
        .map(|(name, path)| {
            let shown = (cfg.batch_show)(&name).map(|text| {
                (!text.trim().is_empty()).then(|| OpenBatch { branch: text.lines().find_map(|l| l.strip_prefix("branch=")).map(str::to_string) })
            });
            (name, path, shown)
        })
        .collect()
}

// A failed forge call and "nothing going on" must stay distinguishable (sp-pu7v6): a
// spawn failure, a non-zero exit or invalid UTF-8 all report Err, so a caller can report
// its invariant unobservable instead of silently reading the failure as "satisfied".
/// One forge verb (`args`: verb, repo, the rest) through the forge library — `gh` is its only
/// child — or a fixture's stand-in when the configured forge is one.
fn run_forge(forge: &str, args: &[&str]) -> Result<String, String> {
    let r = forge_reply(forge, args);
    if !r.ok() {
        return Err(format!("forge exited {}", r.code));
    }
    Ok(r.out)
}

fn forge_reply(forge: &str, args: &[&str]) -> forge::client::Reply {
    let (verb, repo, rest) = match args {
        [] => ("", "", &[][..]),
        [v] => (*v, "", &[][..]),
        [v, r, rest @ ..] => (*v, *r, rest),
    };
    forge::client::run(&forge::client::route_of(Path::new(forge)), verb, Path::new(repo), rest, None)
}

/// A forge verb whose output went to this process's own stdout and stderr.
fn forge_shown(forge: &str, args: &[&str]) {
    let r = forge_reply(forge, args);
    print!("{}", r.out);
    eprint!("{}", r.err);
}

// ──────────────────────────────────────────────────────────────────────────────
// Main pass
// ──────────────────────────────────────────────────────────────────────────────

const CZAR_LANE: &str = "czar";

/// The line a pass logs when it skips because the czar lane is stopped on every repo.
fn czar_lane_stopped(stop: Option<spira_config::scoped_stop::RepoStop>) -> Option<String> {
    stop.map(|s| spira_config::scoped_stop::skip_line("czar pass", CZAR_LANE, &s))
}

fn run_pass() -> Result<(), String> {
    let cfg = Config::from_process(Seams::production());

    // World halted
    if cfg.spira_run.join("world.halted").exists() {
        log_print("czar pass: skipped — world is halted");
        return Ok(());
    }
    if let Some(why) = czar_lane_stopped(spira_config::scoped_stop::stop_everywhere_registered(&cfg.spira_run, CZAR_LANE)) {
        log_print(&why);
        return Ok(());
    }

    // Flock — non-blocking: if another pass is running, skip.
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&cfg.lock_path)
        .map_err(|e| format!("open lock {}: {}", cfg.lock_path.display(), e))?;
    if unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        log_print("czar pass: already running — skip");
        return Ok(());
    }

    // New log entries since last pass marker.
    let prev = fs::read_to_string(&cfg.marker)
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let new_lines = read_new_lines(&cfg.qc_log, &prev);

    // Every invariant's hysteresis lives in one persisted state map, loaded once per pass
    // and saved once per pass (sp-pu7v6) — replacing a flat first-seen marker file per class.
    let mut state = load_state(&cfg.reconciler_state);

    // ── DETECTOR: deadlock ────────────────────────────────────────────────────
    let (dl_v, dl_rem, dl_tier) = detect_deadlock(&cfg, &mut state, &new_lines);

    // ── DETECTOR: attribution-failed ──────────────────────────────────────────
    let (af_v, af_rem, af_tier) = detect_attribution_failed(&cfg, &mut state, &new_lines);

    // ── DETECTOR: sort-failed ─────────────────────────────────────────────────
    let (sf_v, sf_rem, sf_tier) = detect_sort_failed(&cfg, &mut state, &new_lines);

    // ── DETECTOR: loop-stalled ────────────────────────────────────────────────
    let (ls_v, ls_rem, ls_tier) = detect_loop_stalled(&cfg, &mut state);

    // ── DETECTORS: ci-stalled + ci-red (shared forge calls) ──────────────────
    let (cis_v, cis_rem, cis_tier, cir_v, cir_rem, cir_tier) = detect_ci(&cfg, &mut state);

    // ── DETECTOR: base-red (the base ref's own gate run, not a batch's) ──────
    let (br_v, br_rem, br_tier) = detect_base_red(&cfg, &mut state);

    // ── DETECTOR: pool-idle (certified pool full, no batch open, no round cut) ──
    let (pi_v, pi_rem, pi_tier) = detect_pool_idle(&cfg, &mut state);

    // ── DETECTOR: reconciler-stale (the reconciler's own pass stamp) ─────────
    let (rs_v, rs_rem, rs_tier) = detect_reconciler_stale(&cfg, &mut state);

    // Telemetry — one line per class per pass.
    telem(&cfg, "deadlock",           &dl_v,  dl_rem,  dl_tier);
    telem(&cfg, "attribution-failed", &af_v,  af_rem,  af_tier);
    telem(&cfg, "sort-failed",        &sf_v,  sf_rem,  sf_tier);
    telem(&cfg, "loop-stalled",       &ls_v,  ls_rem,  ls_tier);
    telem(&cfg, "ci-stalled",         &cis_v, cis_rem, cis_tier);
    telem(&cfg, "ci-red",             &cir_v, cir_rem, cir_tier);
    telem(&cfg, "base-red",           &br_v,  br_rem,  br_tier);
    telem(&cfg, "pool-idle",          &pi_v,  pi_rem,  pi_tier);
    telem(&cfg, "reconciler-stale",   &rs_v,  rs_rem,  rs_tier);

    let _ = save_state(&cfg.reconciler_state, &state);

    // Marker
    let _ = fs::write(&cfg.marker, format!("{}\n", cfg.now_iso));

    let elapsed = unix_now().saturating_sub(cfg.now_secs);
    log_print(&format!("czar pass: complete ({}s)", elapsed));

    drop(lock_file);
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Log reading
// ──────────────────────────────────────────────────────────────────────────────

fn read_new_lines(qc_log: &Path, prev: &str) -> String {
    if !qc_log.exists() {
        return String::new();
    }
    let content = fs::read_to_string(qc_log).unwrap_or_default();
    if prev.is_empty() {
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(200);
        lines[start..].join("\n")
    } else {
        content
            .lines()
            .filter(|l| {
                l.split_whitespace()
                    .next()
                    .map(|ts| ts > prev)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Individual detectors — return (detected_str, remedy_str, tier_str)
// ──────────────────────────────────────────────────────────────────────────────

fn detect_deadlock(cfg: &Config, state: &mut StateMap, new_lines: &str) -> (Verdict, &'static str, &'static str) {
    let trigger = czar_pass::DEADLOCK_LINE;
    let raw = czar_pass::deadlock(new_lines.contains(trigger));
    let verdict = evaluate(cfg, state, "deadlock", raw, 0);
    if !verdict.is_gap {
        return (verdict, "none", "det");
    }

    let ctx: String = new_lines
        .lines()
        .filter(|l| l.contains(trigger))
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    // A deterministic remedy is attempted once per streak; if it is still a gap on the
    // next pass, remedy_failed says so and this pass escalates instead of retrying blind.
    let mut run_id = String::new();
    let mut repo_path = String::new();
    if !verdict.remedy_failed {
        for (_, rp, shown) in open_batches(cfg, mapped_repos(&cfg.repo_map)) {
            let Ok(Some(OpenBatch { branch: Some(branch) })) = shown else { continue };
            let rp_str = rp.to_string_lossy().to_string();
            let out = run_forge(&cfg.seams.forge, &["run-id", &rp_str, &branch]);
            let rid = out.map(|s| s.trim().to_string()).unwrap_or_default();
            if !rid.is_empty() {
                run_id = rid;
                repo_path = rp_str;
                break;
            }
        }
    }

    if !run_id.is_empty() {
        let desc = format!("workflow-rerun {}", run_id);
        let forge = cfg.seams.forge.clone();
        let rid = run_id.clone();
        let rp = repo_path.clone();
        det_action(cfg, "deadlock", &desc, move || {
            forge_shown(&forge, &["workflow-rerun", &rp, &rid]);
        });
        if let Some(st) = state.get_mut("deadlock") {
            record_remedy(st, cfg.now_secs, &desc);
        }
        (verdict, "det-rerun", "det")
    } else {
        let tried = last_remedy(state.get("deadlock").unwrap_or(&HysteresisState::default()))
            .map(|d| format!("\nLast remedy tried: {}\n", d))
            .unwrap_or_default();
        let body = format!(
            "verdict left the batch PR open after a red with no attributable suite.\n\
             The queue cannot advance until the batch is closed or requeued by hand.\n\
             {}\nRecent log lines:\n{}\n",
            tried, ctx
        );
        infer(
            cfg,
            "deadlock",
            "deadlock-batch-open",
            "QUEUE: batch open — no suites identified (DEADLOCK)",
            &body,
        );
        (verdict, "inference", "inf")
    }
}

fn detect_attribution_failed(cfg: &Config, state: &mut StateMap, new_lines: &str) -> (Verdict, &'static str, &'static str) {
    // grep -qE 'ejected 0, requeued [1-9]'
    let raw = czar_pass::attribution_failed(czar_pass::requeued_all(new_lines));
    let verdict = evaluate(cfg, state, "attribution-failed", raw, 0);
    if !verdict.is_gap {
        return (verdict, "none", "det");
    }
    let ctx: String = new_lines
        .lines()
        .filter(|l| l.contains("ejected 0, requeued"))
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let body = format!(
        "Attribution found no branch to isolate the offender.\n\
         The entire batch was requeued with the failing member still in it; this loops.\n\n\
         Recent log lines:\n{}\n",
        ctx
    );
    infer(
        cfg,
        "attribution-failed",
        "attribution-failed-requeue",
        "QUEUE: attribution ejected 0, requeued whole batch",
        &body,
    );
    (verdict, "inference", "inf")
}

fn detect_sort_failed(cfg: &Config, state: &mut StateMap, new_lines: &str) -> (Verdict, &'static str, &'static str) {
    let raw = czar_pass::sort_failed(new_lines.contains(czar_pass::SORT_FAILED_LINE));
    let verdict = evaluate(cfg, state, "sort-failed", raw, 0);
    if !verdict.is_gap {
        return (verdict, "none", "det");
    }
    let ctx: String = new_lines
        .lines()
        .filter(|l| l.contains("queue_sort_rows: ranking failed"))
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let body = format!(
        "The queue sorter failed and fell back to unranked order.\n\
         Priority ordering is suspended until the sorter recovers.\n\n\
         Recent log lines:\n{}\n",
        ctx
    );
    infer(
        cfg,
        "sort-failed",
        "sort-failed-ranking",
        "QUEUE: queue_sort_rows ranking failed",
        &body,
    );
    (verdict, "inference", "inf")
}

fn detect_loop_stalled(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str) {
    let last_ts: Option<String> = if cfg.qc_log.exists() {
        fs::read_to_string(&cfg.qc_log)
            .ok()
            .and_then(|c| {
                c.lines().rfind(|l| l.contains("landing: pass complete"))
                    .and_then(|l| l.get(..20))
                    .map(|s| s.to_string())
            })
    } else {
        None
    };

    // No log yet, or no pass-complete line yet, is a legitimate fresh-install absence —
    // not a failure to observe. An unparseable timestamp IS a failure to observe: the old
    // code silently read that as "satisfied", which is exactly the bug this engine closes
    // (law-a-control-that-cannot-check-must-refuse).
    let raw = match &last_ts {
        None => RawStatus::Satisfied,
        Some(ts) => match parse_iso_to_epoch(ts) {
            None => RawStatus::Unobservable { reason: format!("landing.log timestamp unparseable: {:?}", ts) },
            Some(ts_epoch) => czar_pass::loop_stalled(cfg.now_secs, Some(ts_epoch), cfg.stall_secs),
        },
    };

    let verdict = evaluate(cfg, state, "loop-stalled", raw, 0);
    if !verdict.is_gap {
        return (verdict, "none", "det");
    }
    if matches!(verdict.status, RawStatus::Unobservable { .. }) {
        return (verdict, "none", "det");
    }

    let age = verdict.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
    let unit_service = format!("{}.service", cfg.land_unit);
    let is_failed = cfg.seams.units.facts(&runtime::units::UnitHandle::named(&unit_service)).is_ok_and(|f| f.state == runtime::units::UnitState::Failed);

    if is_failed && !verdict.remedy_failed {
        let sc = cfg.seams.systemctl.clone();
        let svc = unit_service.clone();
        let desc = format!("reset-failed + start {}", cfg.land_unit);
        det_action(cfg, "loop-stalled", &desc, move || {
            // FORK-EDGE: needs Units::reset_failed and a start of an installed unit (the port starts transient units only)
            let _ = spira_config::bounded::bounded(&sc).args(["--user", "reset-failed", &svc]).status();
            let _ = spira_config::bounded::bounded(&sc).args(["--user", "start", &svc]).status();
        });
        if let Some(st) = state.get_mut("loop-stalled") {
            record_remedy(st, cfg.now_secs, &desc);
        }
        (verdict, "det-restart", "det")
    } else {
        let body = format!(
            "No landing: pass complete in the last {}s (threshold: {}s).\n\nSee: {}\n",
            age,
            cfg.stall_secs,
            cfg.qc_log.display()
        );
        infer(
            cfg,
            "loop-stalled",
            "loop-stalled",
            &format!("QUEUE: landing loop stalled — no pass for {}s", age),
            &body,
        );
        (verdict, "inference", "inf")
    }
}

use czar_pass::rollup;

fn detect_ci(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str, Verdict, &'static str, &'static str) {
    let mut cis_remedy: &'static str = "none";
    let mut cis_tier: &'static str = "det";
    let mut cir_remedy: &'static str = "none";
    let mut cir_tier: &'static str = "det";
    let mut cis_verdicts: Vec<Verdict> = Vec::new();
    let mut cir_verdicts: Vec<Verdict> = Vec::new();

    {
        for (repo_name, repo_path, shown) in open_batches(cfg, mapped_repos(&cfg.repo_map)) {
            let repo_safe = repo_name.replace(',', "-");
            let cis_key = format!("ci-stalled:{}", repo_safe);
            let cir_key = format!("ci-red:{}", repo_safe);
            let branch = match shown {
                Ok(Some(OpenBatch { branch: Some(b) })) => b,
                Ok(_) => continue,
                Err(e) => {
                    let reason = format!("{}: whether a batch is open: {}", repo_name, e);
                    cis_verdicts.push(evaluate(cfg, state, &cis_key, RawStatus::Unobservable { reason: reason.clone() }, 0));
                    cir_verdicts.push(evaluate(cfg, state, &cir_key, RawStatus::Unobservable { reason }, 0));
                    continue;
                }
            };
            let repo_path_str = repo_path.to_string_lossy().to_string();

            let ci_out = match run_forge(&cfg.seams.forge, &["batch-ci-status", &repo_path_str, &branch]) {
                Ok(out) => out,
                Err(e) => {
                    // A failed forge call is not "no CI activity": both invariants for this
                    // repo are unobservable this pass, never silently read as satisfied.
                    let reason = format!("{}: batch-ci-status: {}", repo_name, e);
                    let v1 = evaluate(cfg, state, &cis_key, RawStatus::Unobservable { reason: reason.clone() }, 0);
                    cis_verdicts.push(v1);
                    let v2 = evaluate(cfg, state, &cir_key, RawStatus::Unobservable { reason }, 0);
                    cir_verdicts.push(v2);
                    continue;
                }
            };
            let run_id = parse_field(&ci_out, "run-id").unwrap_or_default();
            let queued_since = parse_field(&ci_out, "queued-since").and_then(|s| s.parse::<u64>().ok());

            // ci-stalled: job queued with no runner > threshold
            let cis_raw = czar_pass::ci_stalled(cfg.now_secs, &repo_name, queued_since, cfg.ci_queued_max);
            let cis_v = evaluate(cfg, state, &cis_key, cis_raw, 0);
            cis_verdicts.push(cis_v.clone());
            if cis_v.is_gap {
                let stalled_s = cis_v.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
                if !run_id.is_empty() && !cis_v.remedy_failed {
                    let desc = format!("workflow-rerun {} ({}, queued {}s)", run_id, repo_name, stalled_s);
                    let forge = cfg.seams.forge.clone();
                    let rp = repo_path_str.clone();
                    let rid = run_id.clone();
                    det_action(cfg, "ci-stalled", &desc, move || {
                        forge_shown(&forge, &["workflow-rerun", &rp, &rid]);
                    });
                    if let Some(st) = state.get_mut(&cis_key) {
                        record_remedy(st, cfg.now_secs, &desc);
                    }
                    cis_remedy = "det-rerun";
                } else {
                    let body = format!(
                        "A CI job for the open batch in {} has been queued for {}s \
                         (threshold: {}s).\nBranch: {}\n\n\
                         Cancel the stuck run and re-dispatch the whole workflow.\n\
                         Never rerun --failed: that strands the run on the torn-down VM label.\n",
                        repo_name, stalled_s, cfg.ci_queued_max, branch
                    );
                    infer(
                        cfg,
                        "ci-stalled",
                        &format!("ci-stalled-{}", repo_safe),
                        &format!("QUEUE: CI job queued with no runner for {}s ({})", stalled_s, repo_name),
                        &body,
                    );
                    cis_remedy = "inference";
                    cis_tier = "inf";
                }
            }

            // ci-red: run completed failure, verdict not acting.
            // Measures from run-completed-at (CI completion), not from batch open mtime.
            // If run-completed-at is absent, that reading is Satisfied: a batch's own gate
            // hasn't reported a conclusion, distinct from base-red's "no run at all" case
            // below, which is a repo's base ref, not a batch's PR, and unobservable there.
            let conclusion = parse_field(&ci_out, "run-conclusion");
            let completed_secs = parse_field(&ci_out, "run-completed-at").and_then(|s| s.trim().parse::<u64>().ok());
            let cir_raw = czar_pass::ci_red(cfg.now_secs, &repo_name, conclusion.as_deref(), completed_secs, cfg.ci_red_max);
            let cir_v = evaluate(cfg, state, &cir_key, cir_raw, 0);
            cir_verdicts.push(cir_v.clone());
            if cir_v.is_gap {
                let red_age = cir_v.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
                let body = format!(
                    "CI run completed red for the open batch in {}.\n\
                     Red for {}s (threshold {}s); verdict has not acted.\n\
                     Branch: {}\nInvestigate the red and act.\n",
                    repo_name, red_age, cfg.ci_red_max, branch
                );
                infer(
                    cfg,
                    "ci-red",
                    &format!("ci-red-{}", repo_safe),
                    &format!("QUEUE: CI run completed red, verdict not acting for {}s ({})", red_age, repo_name),
                    &body,
                );
                cir_remedy = "inference";
                cir_tier = "inf";
            }
        }
    }

    (rollup(&cis_verdicts), cis_remedy, cis_tier, rollup(&cir_verdicts), cir_remedy, cir_tier)
}

// The base ref's OWN gate run, as opposed to ci-red above (a batch's PR run). A batch PR
// runs diff-selected suites; the push that lands it on the base ref runs the whole corpus,
// so a batch can go green and land red with nothing else watching for that. Fires at once
// on the first red pass — no age threshold — because the entire point is to beat "cut by
// hand two hours later" (sp-eve0i), and infer_urgent files at P0 + express so it is not
// merely first in a P0 queue that is itself hours deep.
//
// UNREADABLE IS NOT GREEN (law-a-control-that-cannot-check-must-refuse): when the forge
// seam returns no run at all for the base branch, that is filed too, distinctly, once it
// has persisted past base_unreadable_grace — long enough that it is not just GitHub not
// yet having created the run row for a push that landed seconds ago.
fn detect_base_red(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str) {
    let mut remedy: &'static str = "none";
    let mut tier: &'static str = "det";
    let mut verdicts: Vec<Verdict> = Vec::new();

    {
        for (repo_name, repo_path, shown) in open_batches(cfg, mapped_repos(&cfg.repo_map)) {
            let repo_safe = repo_name.replace(',', "-");
            let key = format!("base-red:{}", repo_safe);
            match shown {
                Ok(Some(_)) => {}
                Ok(None) => continue,
                Err(e) => {
                    let reason = format!("{}: whether a batch is open: {}", repo_name, e);
                    verdicts.push(evaluate(cfg, state, &key, RawStatus::Unobservable { reason }, 0));
                    continue;
                }
            }
            let base_branch = match repo_base(&repo_name, &cfg.repo_map) {
                Some(b) => b,
                None => continue,
            };
            let repo_path_str = repo_path.to_string_lossy().to_string();

            let ci_out = run_forge(&cfg.seams.forge, &["batch-ci-status", &repo_path_str, &base_branch]);
            let run_id = ci_out.as_ref().ok().and_then(|out| parse_field(out, "run-id"));

            if run_id.is_none() {
                // No run at all for the base branch — the same grace period the old code
                // gave a fresh marker file applies here as the engine's own grace_secs, so
                // an ordinary push whose run GitHub has not created yet stays quiet.
                let reason = match &ci_out {
                    Err(e) => format!("{}'s base ({}): {}", repo_name, base_branch, e),
                    Ok(_) => format!("{}'s base ({}) CI status returned no run information", repo_name, base_branch),
                };
                let v = evaluate(cfg, state, &key, RawStatus::Unobservable { reason }, cfg.base_unreadable_grace);
                verdicts.push(v.clone());
                if v.is_gap {
                    let age = v.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
                    let body = format!(
                        "{}'s base ({}) CI status could not be read for {}s — the forge \
                         seam returned no run information for that branch.\n\
                         A base whose status cannot be read is treated as failed, not \
                         green: nothing here can tell you it is safe to land on.\n",
                        repo_name, base_branch, age
                    );
                    infer_urgent(
                        cfg,
                        "base-red",
                        &format!("{}:unreadable", repo_name),
                        &format!("QUEUE: {}'s base CI status could not be read", repo_name),
                        &body,
                        &repo_name,
                    );
                    remedy = "inference";
                    tier = "inf";
                }
                continue;
            }
            let ci_out = ci_out.expect("run_id.is_some() implies ci_out was Ok");

            let conclusion = parse_field(&ci_out, "run-conclusion");
            let raw = czar_pass::base_red(&repo_name, &base_branch, conclusion.as_deref());
            let v = evaluate(cfg, state, &key, raw, 0);
            verdicts.push(v.clone());
            if v.is_gap {
                let sha = parse_field(&ci_out, "head-sha").unwrap_or_else(|| "unknown".to_string());
                let run_url = parse_field(&ci_out, "run-url").unwrap_or_else(|| "(no run url)".to_string());
                let mut suites: Vec<String> = ci_out
                    .lines()
                    .filter_map(|l| l.strip_prefix("red-suite: ").map(|s| s.to_string()))
                    .collect();
                let suite_key = czar_pass::suite_key(&suites);
                suites.sort();
                suites.dedup();
                let suite_list = if suites.is_empty() { "(suite names unavailable)".to_string() } else { suites.join(", ") };

                let body = format!(
                    "{}'s own base ({}) gate run completed red.\n\n\
                     failing suites   {}\n\
                     base sha         {}\n\
                     run url          {}\n\n\
                     Nothing else stops the next batch landing on this red base. Post-hoc \
                     attribution is acceptable — naming who caused it is not required —\
                     but this bead does not close until the base is green again.\n",
                    repo_name, base_branch, suite_list, sha, run_url
                );
                infer_urgent(
                    cfg,
                    "base-red",
                    &format!("{}:{}", repo_name, suite_key),
                    &format!("QUEUE: {}'s base is red — {}", repo_name, suite_list),
                    &body,
                    &repo_name,
                );
                remedy = "inference";
                tier = "inf";
            }
        }
    }

    (rollup(&verdicts), remedy, tier)
}

/// Which of `repo_path`'s own `refs/heads/spira/*` branches exist — the same authority
/// `certified_pool` (batcher-cut) and `queue_certified_list` (lib.sh) use to scope the machine's
/// beads to one repository.
fn repo_branch_ids(repo_path: &Path) -> Vec<String> {
    let Ok(out) = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(repo_path)
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads/spira/*"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("spira/").map(str::to_string))
        .collect()
}

/// This repo's own CERTIFIED depth — czar-pass's read of the same count
/// `watchtower.sh --throttle-check` computes, without the title/priority/express enrichment
/// `certified_pool` (batcher-cut) does: a stall detector needs only the number. An unreachable
/// or unparseable machine reads as depth 0, said on stderr: a stall detector must not invent a
/// pool it could not see.
fn certified_depth(cfg: &Config, repo_path: &Path) -> u32 {
    let what = "czar-pass: spira-lc list --state CERTIFIED";
    let answer = spira_config::lc_call::LcClient::for_bin(&cfg.seams.spira_lc).map_err(spira_config::lc_call::LcError::Fault).and_then(|c| c.call(&["list", "--state", "CERTIFIED", "--snapshot"]));
    let out = match answer {
        Ok(r) if r.code == 0 => r.out.into_bytes(),
        Ok(r) => {
            eprintln!("{what} exited {}", r.code);
            return 0;
        }
        Err(e) => {
            eprintln!("{what}: {e}");
            return 0;
        }
    };
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_slice(&out) else {
        eprintln!("czar-pass: spira-lc list --state CERTIFIED: unparsed reply");
        return 0;
    };
    let certified: Vec<&str> = rows.iter().filter_map(|r| r.get("bead_id").and_then(|b| b.as_str())).collect();
    repo_branch_ids(repo_path).iter().filter(|id| certified.contains(&id.as_str())).count() as u32
}

/// The queue-mode rows of the repo-map (`land` column `queue` or `queue.forge`, the same
/// alias `repo_land` in lib.sh normalizes). It is not gated on an
/// open batch existing — a repo whose pool has filled with NO batch open is exactly the
/// failure `detect_pool_idle` exists to catch.
fn queue_repo_names(repo_map: &Option<PathBuf>) -> Vec<(String, PathBuf)> {
    repo_map_rows(repo_map, true)
}

/// Every row of the repo-map, whatever its land: the CI and deadlock detectors judge whichever
/// repo the queue shows an open batch for, as they judged every queue directory holding one.
fn mapped_repos(repo_map: &Option<PathBuf>) -> Vec<(String, PathBuf)> {
    repo_map_rows(repo_map, false)
}

fn repo_map_rows(repo_map: &Option<PathBuf>, queue_only: bool) -> Vec<(String, PathBuf)> {
    let Some(map_path) = repo_map else { return Vec::new() };
    let Ok(content) = fs::read_to_string(map_path) else { return Vec::new() };
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        let parts: Vec<&str> = t.splitn(6, '|').collect();
        if parts.len() < 3 {
            continue;
        }
        let (name, path, land) = (parts[0].trim(), parts[1].trim(), parts[2].trim());
        if name.is_empty() || path.is_empty() {
            continue;
        }
        if queue_only && land != "queue" && land != "queue.forge" {
            continue;
        }
        out.push((name.to_string(), PathBuf::from(path)));
    }
    out
}

/// The certified-pool trigger (sp-ji62y/sp-forah): today a chat-session while-true loop or a
/// re-armed Monitor dies with the session that started it — this detector is the harness-
/// owned replacement, on czar-pass's own 30s timer. Fires once a repo's certified pool has
/// sat at or past `round_min_n` (batcher's adaptive_n floor: at that depth the batcher would
/// already have cut, whatever its ceiling turns out to be) with no batch open, for longer
/// than `round_stall_secs`. A batch already open is deliberate back-pressure
/// (law-queue-back-pressure-is-an-open-pr), not a stall: silent there, whatever the depth.
fn detect_reconciler_stale(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str) {
    let stamp = fs::read_to_string(&cfg.reconciler_stamp).ok().map(|s| s.trim().to_string());
    let raw = match stamp.as_deref().map(str::parse::<u64>) {
        None => RawStatus::Gap {
            desired: format!("a reconciler pass within {}s", cfg.reconciler_grace_secs),
            observed: format!("no pass stamp at {}", cfg.reconciler_stamp.display()),
            since_hint: None,
        },
        Some(Err(_)) => RawStatus::Unobservable { reason: format!("{} unparseable", cfg.reconciler_stamp.display()) },
        Some(Ok(at)) => {
            let age = cfg.now_secs.saturating_sub(at);
            if age > cfg.reconciler_grace_secs {
                RawStatus::Gap {
                    desired: format!("a reconciler pass within {}s", cfg.reconciler_grace_secs),
                    observed: format!("last pass {}s ago", age),
                    since_hint: Some(at),
                }
            } else {
                RawStatus::Satisfied
            }
        }
    };
    let verdict = evaluate(cfg, state, "reconciler-stale", raw, cfg.reconciler_grace_secs);
    if !verdict.is_gap || matches!(verdict.status, RawStatus::Unobservable { .. }) {
        return (verdict, "none", "det");
    }
    let body = format!(
        "The reconciler has not completed a pass within {}s.\n\nHost: {}\nCheck: the runtime is enabled and active, \
         and {} is being written.\n",
        cfg.reconciler_grace_secs,
        timer_state(&cfg.seams.systemctl, &cfg.reconciler_unit),
        cfg.reconciler_stamp.display()
    );
    infer(cfg, "reconciler-stale", "reconciler-stale", "RECONCILER: no pass completed — reconciler is not running", &body);
    (verdict, "inference", "inf")
}

const RELAXING_VERBS: [&str; 17] = [
    "disable", "disarm", "silence", "mute", "suppress", "relax", "loosen", "lower", "raise", "bypass", "skip",
    "turn off", "switch off", "stop running", "remove", "delete", "drop",
];
const CHECK_NOUNS: [&str; 9] = ["check", "detector", "alarm", "alert", "watcher", "gate", "threshold", "grace", "guard"];

/// A czar default that disarms or relaxes a check (law-never-disarm-a-check-to-proceed).
fn default_relaxes_a_check(text: &str) -> bool {
    let t = text.to_lowercase();
    RELAXING_VERBS.iter().any(|v| {
        t.match_indices(v).any(|(i, _)| {
            let rest: String = t[i + v.len()..].chars().take_while(|c| !".;".contains(*c)).take(40).collect();
            CHECK_NOUNS.iter().any(|n| rest.contains(n))
        })
    })
}

/// What `systemctl show` says about a unit, as the advice the czar acts on. "not installed"
/// is reserved for a unit systemd has no file for.
fn timer_advice(unit: &str, load_state: &str, unit_file_state: &str, active_state: &str) -> String {
    if load_state == "not-found" || load_state.is_empty() {
        return format!("{unit}: not installed (no unit file)");
    }
    if load_state == "masked" {
        return format!("{unit}: masked — unmask and enable {unit}");
    }
    if unit_file_state != "enabled" {
        return format!("{unit}: disabled: enable {unit}");
    }
    if active_state != "active" {
        return format!("{unit}: enabled but {active_state}: start {unit}");
    }
    format!("{unit}: enabled and active")
}

fn show_value<'a>(out: &'a str, key: &str) -> &'a str {
    out.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('='))).unwrap_or("").trim()
}

fn timer_state(systemctl: &str, unit: &str) -> String {
    // FORK-EDGE: needs UnitFacts to carry LoadState and UnitFileState (enablement); the port reports the active state only
    let out = spira_config::bounded::bounded(systemctl)
        .args(["--user", "show", unit, "-p", "LoadState", "-p", "UnitFileState", "-p", "ActiveState"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok());
    let Some(out) = out else {
        return format!("{unit}: state unobservable (systemctl failed)");
    };
    timer_advice(
        unit,
        show_value(&out, "LoadState"),
        show_value(&out, "UnitFileState"),
        show_value(&out, "ActiveState"),
    )
}

fn detect_pool_idle(cfg: &Config, state: &mut StateMap) -> (Verdict, &'static str, &'static str) {
    let mut remedy: &'static str = "none";
    let mut tier: &'static str = "det";
    let mut verdicts: Vec<Verdict> = Vec::new();

    for (repo_name, repo_path, shown) in open_batches(cfg, queue_repo_names(&cfg.repo_map)) {
        let repo_safe = repo_name.replace(',', "-");
        let key = format!("pool-idle:{}", repo_safe);
        let batch_open = match shown {
            Ok(b) => b.is_some(),
            Err(e) => {
                let reason = format!("{}: whether a batch is open: {}", repo_name, e);
                verdicts.push(evaluate(cfg, state, &key, RawStatus::Unobservable { reason }, cfg.round_stall_secs));
                continue;
            }
        };
        let depth = certified_depth(cfg, &repo_path);

        let raw = czar_pass::pool_idle(&repo_name, batch_open, u64::from(depth), u64::from(cfg.round_min_n));
        let v = evaluate(cfg, state, &key, raw, cfg.round_stall_secs);
        verdicts.push(v.clone());
        if v.is_gap {
            let age = v.since.map(|s| cfg.now_secs.saturating_sub(s)).unwrap_or(0);
            let body = format!(
                "{}'s certified pool has sat at {} member(s) (round threshold: {}) for {}s \
                 with no batch open — a round should have cut by now.\n\n\
                 Check: is spira-verdict.timer active, is SPIRA_BATCHER_BIN built and \
                 executable, and is queue/{}'s own lock stuck.\n",
                repo_name, depth, cfg.round_min_n, age, repo_name
            );
            infer(
                cfg,
                "pool-idle",
                &format!("pool-idle-{}", repo_safe),
                &format!("QUEUE: {}'s certified pool ({}) idle {}s with no round cut", repo_name, depth, age),
                &body,
            );
            remedy = "inference";
            tier = "inf";
        }
    }
    (rollup(&verdicts), remedy, tier)
}

#[cfg(test)]
fn file_mtime(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};


    // A private scratch directory per test, so parallel `cargo test` threads never collide
    // on the same path (the real callers always get SPIRA_RUN handed to them by the caller,
    // never a shared ambient default).
    fn scratch_dir(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("czar-pass-test-{}-{}", name, SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()))
    }

    #[test]
    fn parse_field_reads_the_named_key() {
        let out = "run-id: 42\nrun-conclusion: failure\nhead-sha: abc123\n";
        assert_eq!(parse_field(out, "run-id"), Some("42".to_string()));
        assert_eq!(parse_field(out, "run-conclusion"), Some("failure".to_string()));
    }

    #[test]
    fn parse_field_is_anchored_to_the_prefix_not_a_substring() {
        // "run-id" must not match inside "run-id-extra: x" — the prefix carries ": ".
        let out = "run-id-extra: x\n";
        assert_eq!(parse_field(out, "run-id"), None);
    }

    #[test]
    fn parse_field_missing_key_is_none() {
        assert_eq!(parse_field("run-id: 42\n", "queued-since"), None);
    }

    #[test]
    fn repo_base_reduces_a_full_ref_to_a_bare_branch_name() {
        let dir = scratch_dir("repo-base");
        let map = dir.join("repo-map");
        fs::write(&map, "spira | /srv/checkouts/spira | queue | origin/main | | \n").unwrap();
        assert_eq!(repo_base("spira", &Some(map)), Some("main".to_string()));
    }

    #[test]
    fn repo_base_empty_column_means_let_the_caller_resolve_it() {
        let dir = scratch_dir("repo-base-empty");
        let map = dir.join("repo-map");
        fs::write(&map, "spira | /srv/checkouts/spira | queue | | | \n").unwrap();
        assert_eq!(repo_base("spira", &Some(map)), None);
    }

    #[test]
    fn a_forge_batch_on_the_store_is_the_record_the_queue_shows() {
        let dir = scratch_dir("open-batches-shown");
        let map = dir.join("repo-map");
        fs::write(&map, "deck | /srv/deck | queue.forge | main | | \nidle | /srv/idle | queue | main | | \nlost | /srv/lost | queue | main | | \npushy | /srv/p | push | | | \n").unwrap();
        let mut cfg = test_config(&dir, 1000);
        cfg.repo_map = Some(map);
        cfg.batch_show = std::sync::Arc::new(|repo: &str| match repo {
            "deck" => Ok("batch_id=deck-1\nforge.phase=armed\npr=12\nbranch=spira/queue/deck-1\nmembers=sp-a:ta\n".into()),
            "idle" => Ok(String::new()),
            _ => Err("queue batch show exited 1".into()),
        });
        let got: Vec<_> = open_batches(&cfg, queue_repo_names(&cfg.repo_map)).into_iter().map(|(n, _, s)| (n, s)).collect();
        assert_eq!(
            got,
            [
                ("deck".to_string(), Ok(Some(OpenBatch { branch: Some("spira/queue/deck-1".into()) }))),
                ("idle".to_string(), Ok(None)),
                ("lost".to_string(), Err("queue batch show exited 1".to_string())),
            ]
        );
        let lost = dir.join("repo-map-lost");
        fs::write(&lost, "lost | /srv/lost | queue | main | | \n").unwrap();
        cfg.repo_map = Some(lost);
        let mut state = StateMap::new();
        let (v, _, _) = detect_pool_idle(&cfg, &mut state);
        assert!(matches!(v.status, RawStatus::Unobservable { .. }), "cannot tell is not no batch: {:?}", v.status);
    }

    #[test]
    fn file_mtime_is_none_for_a_missing_file() {
        let dir = scratch_dir("file-mtime-missing");
        assert_eq!(file_mtime(&dir.join("does-not-exist")), None);
    }

    #[test]
    fn file_mtime_is_recent_for_a_freshly_written_file() {
        let dir = scratch_dir("file-mtime-fresh");
        let f = dir.join("f");
        fs::write(&f, "x").unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mtime = file_mtime(&f).expect("freshly written file must have an mtime");
        assert!(mtime <= now && now - mtime < 30, "mtime {} vs now {}", mtime, now);
    }

    #[test]
    fn read_new_lines_with_no_marker_returns_the_tail() {
        let dir = scratch_dir("read-new-lines-no-marker");
        let log = dir.join("landing.log");
        fs::write(&log, "2026-01-01T00:00:00Z spira: verdict\n").unwrap();
        assert!(read_new_lines(&log, "").contains("verdict"));
    }

    #[test]
    fn read_new_lines_excludes_lines_at_or_before_the_marker() {
        // The two-pass sequence run_pass() actually performs: pass 1 sees the line with
        // no marker, then writes cfg.now_iso as the marker; pass 2 must not see the same
        // line again, so a marker at or after the line's own timestamp excludes it.
        let dir = scratch_dir("read-new-lines-marker");
        let log = dir.join("landing.log");
        let line_ts = "2026-01-01T00:00:00Z";
        fs::write(&log, format!("{} spira: verdict\n", line_ts)).unwrap();

        assert!(read_new_lines(&log, "").contains("verdict"));

        let marker_after_first_pass = "2026-01-01T00:00:01Z";
        let second = read_new_lines(&log, marker_after_first_pass);
        assert!(
            !second.contains("verdict"),
            "same line must not re-fire once the marker has advanced past it: got {:?}",
            second
        );
    }

    #[test]
    fn read_new_lines_includes_lines_strictly_after_the_marker() {
        let dir = scratch_dir("read-new-lines-after-marker");
        let log = dir.join("landing.log");
        fs::write(
            &log,
            "2026-01-01T00:00:00Z spira: old\n2026-01-01T00:00:02Z spira: new\n",
        )
        .unwrap();
        let out = read_new_lines(&log, "2026-01-01T00:00:01Z");
        assert!(!out.contains("old"), "line before the marker must be excluded: {:?}", out);
        assert!(out.contains("new"), "line after the marker must be included: {:?}", out);
    }

    // ────────────────────────────────────────────────────────────────────────────────
    // Detector fire/silent boundaries (UC-ops-detection-remediation-23).
    //
    // Every case runs in Shadow stage (the default with no SPIRA_CZAR_STAGE_* set):
    // det_action/infer only append a CZAR-WOULD line there, so a "fire" case never spawns
    // a real forge workflow-rerun or files a real incident — exactly what a unit test
    // wants from a detector whose whole job is to trigger a side effect.
    // ────────────────────────────────────────────────────────────────────────────────

    const KNOWN_EPOCH: u64 = 1_609_459_200; // 2021-01-01T00:00:00Z, for parse_iso_to_epoch

    fn test_config(dir: &Path, now: u64) -> Config {
        Config {
            spira_run: dir.to_path_buf(),
            czar_log: dir.join("czar.log"),
            qc_log: dir.join("landing.log"),
            marker: dir.join("czar-pass.swept"),
            seams: Seams {
                incident: dir.join("incident").to_string_lossy().to_string(),
                forge: dir.join("forge").to_string_lossy().to_string(),
                // A path that cannot exist, so `systemctl is-failed` always fails to spawn
                // (is_failed = false) instead of depending on the test host's real systemd.
                systemctl: dir.join("no-such-systemctl").to_string_lossy().to_string(),
                spira_lc: dir.join("spira-lc").to_string_lossy().to_string(),
                sentinel: dir.join("sentinel").to_string_lossy().to_string(),
                units: std::sync::Arc::new(runtime::units::ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0)))),
            },
            batch_show: std::sync::Arc::new(|_| Ok(String::new())),
            stall_secs: 3000,
            ci_queued_max: 600,
            ci_red_max: 600,
            base_unreadable_grace: 120,
            lock_path: dir.join("czar-pass.lock"),
            reconciler_state: dir.join("reconciler-state.json"),
            store: std::sync::Arc::new(tsd::store::Pusher::new(None)),
            reconciler_stamp: dir.join("reconciler-pass.stamp"),
            reconciler_grace_secs: 300,
            reconciler_unit: "spira-runtime-prod.service".to_string(),
            round_min_n: 4,
            round_stall_secs: 900,
            spira_db: String::new(),
            scope_label: String::new(),
            czar_label: "czar-trigger".to_string(),
            land_unit: "spira-landing".to_string(),
            repo_map: None,
            // Every case here runs in Shadow stage (the suite's own doc comment above) —
            // an empty map's lookup already defaults to Shadow, matching that intent.
            stages: std::collections::BTreeMap::new(),
            now_secs: now,
            now_iso: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn detect_deadlock_silent_without_the_trigger_line() {
        let dir = scratch_dir("deadlock-silent");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, remedy, tier) = detect_deadlock(&cfg, &mut state, "landing: pass complete\n");
        assert!(!v.is_gap);
        assert_eq!(remedy, "none");
        assert_eq!(tier, "det");
    }

    #[test]
    fn detect_deadlock_fires_on_the_trigger_line() {
        let dir = scratch_dir("deadlock-fire");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, remedy, tier) =
            detect_deadlock(&cfg, &mut state, "no suites identified; leaving batch open\n");
        assert!(v.is_gap);
        assert_eq!(remedy, "inference");
        assert_eq!(tier, "inf");
    }

    #[test]
    fn detect_attribution_failed_silent_with_no_matching_line() {
        let dir = scratch_dir("attribution-silent-absent");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, _remedy, _tier) =
            detect_attribution_failed(&cfg, &mut state, "landing: verdict PASS\n");
        assert!(!v.is_gap);
    }

    #[test]
    fn detect_attribution_failed_silent_when_requeued_is_zero() {
        let dir = scratch_dir("attribution-silent-zero");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, _remedy, _tier) =
            detect_attribution_failed(&cfg, &mut state, "ejected 0, requeued 0\n");
        assert!(!v.is_gap, "requeued 0 is a no-op, not an attribution failure");
    }

    #[test]
    fn detect_attribution_failed_fires_when_requeued_is_nonzero() {
        let dir = scratch_dir("attribution-fire");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, remedy, tier) =
            detect_attribution_failed(&cfg, &mut state, "ejected 0, requeued 3\n");
        assert!(v.is_gap);
        assert_eq!(remedy, "inference");
        assert_eq!(tier, "inf");
    }

    #[test]
    fn detect_sort_failed_silent_without_the_trigger() {
        let dir = scratch_dir("sort-silent");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, _remedy, _tier) = detect_sort_failed(&cfg, &mut state, "landing: verdict PASS\n");
        assert!(!v.is_gap);
    }

    #[test]
    fn detect_sort_failed_fires_on_the_ranking_failure_line() {
        let dir = scratch_dir("sort-fire");
        let cfg = test_config(&dir, 1000);
        let mut state = StateMap::new();
        let (v, remedy, tier) =
            detect_sort_failed(&cfg, &mut state, "queue_sort_rows: ranking failed\n");
        assert!(v.is_gap);
        assert_eq!(remedy, "inference");
        assert_eq!(tier, "inf");
    }

    #[test]
    fn detect_loop_stalled_silent_with_no_landing_log() {
        let dir = scratch_dir("loop-silent-no-log");
        let cfg = test_config(&dir, KNOWN_EPOCH + 100);
        let mut state = StateMap::new();
        let (v, remedy, tier) = detect_loop_stalled(&cfg, &mut state);
        assert!(!v.is_gap);
        assert_eq!(remedy, "none");
        assert_eq!(tier, "det");
    }

    #[test]
    fn detect_ci_silent_with_no_queue_dir() {
        let dir = scratch_dir("ci-no-queue");
        let cfg = test_config(&dir, 2_000_000_000);
        let mut state = StateMap::new();
        let (cis_v, cis_remedy, _t1, cir_v, cir_remedy, _t2) = detect_ci(&cfg, &mut state);
        assert!(!cis_v.is_gap);
        assert!(!cir_v.is_gap);
        assert_eq!(cis_remedy, "none");
        assert_eq!(cir_remedy, "none");
    }

    // ────────────────────────────────────────────────────────────────────────────────
    // pool-idle: the certified-pool trigger (sp-ji62y/sp-forah), harness-owned.
    // ────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn a_default_that_disables_or_relaxes_a_check_is_refused_and_a_restoring_one_is_not() {
        for bad in [
            "disable the queue-reconciler-stale check until spira-reconciler.timer is installed",
            "relax the grace threshold to 900s",
            "Silence the alarm",
        ] {
            assert!(default_relaxes_a_check(bad), "{bad}");
        }
        assert!(!default_relaxes_a_check("enable spira-reconciler-prod.timer"));
    }

    #[test]
    fn a_disabled_scoped_timer_is_reported_disabled_with_its_scoped_name_never_not_installed() {
        let a = timer_advice("spira-reconciler-prod.timer", "loaded", "disabled", "inactive");
        assert_eq!(a, "spira-reconciler-prod.timer: disabled: enable spira-reconciler-prod.timer");
        assert!(!a.contains("not installed"));
        assert!(timer_advice("spira-reconciler-prod.timer", "not-found", "", "inactive").contains("not installed"));
        assert!(timer_advice("t.timer", "loaded", "enabled", "inactive").contains("start t.timer"));
        assert!(timer_advice("t.timer", "loaded", "enabled", "active").ends_with("enabled and active"));
    }

    #[test]
    fn detect_reconciler_stale_missing_stamp_is_a_gap() {
        let dir = scratch_dir("reconciler-stale-missing");
        let cfg = test_config(&dir, KNOWN_EPOCH);
        let (v, _, _) = detect_reconciler_stale(&cfg, &mut StateMap::new());
        assert!(matches!(v.status, RawStatus::Gap { .. }));
    }

    #[test]
    fn the_clock_and_the_landing_log_stamp_read_without_date() {
        assert_eq!(iso_of(KNOWN_EPOCH), "2021-01-01T00:00:00Z");
        assert_eq!(iso_of(951_782_400 + 3_661), "2000-02-29T01:01:01Z");
        assert_eq!(parse_iso_to_epoch("2021-01-01T00:00:00Z"), Some(KNOWN_EPOCH));
        assert_eq!(parse_iso_to_epoch("2000-02-29 01:01:01"), Some(951_782_400 + 3_661));
        for t in [1_700_000_000u64, 0, 4_102_444_800] {
            assert_eq!(parse_iso_to_epoch(&iso_of(t)), Some(t));
        }
        assert_eq!(parse_iso_to_epoch("2021-02-30T00:00:00Z"), None);
        assert_eq!(parse_iso_to_epoch("garbage-not-a-stamp!"), None);
    }

    #[test]
    fn a_batch_view_the_store_cannot_vouch_for_could_not_say() {
        use spira_config::lc_call::LcError;
        use store_view::{BatchView, Record};
        let err = shown_of(Err(LcError::Refused("stale past 3000ms".into()))).unwrap_err();
        assert!(err.starts_with("could not say") && err.contains("stale"), "{err}");
        assert_eq!(shown_of(Ok(BatchView { repo: "r".into(), open: None })), Ok(String::new()));
        let open = Record { id: "b1".into(), state: "OPEN".into(), fields: vec![("batch_id".into(), "b1".into()), ("branch".into(), "spira/queue/b1".into())] };
        assert_eq!(shown_of(Ok(BatchView { repo: "r".into(), open: Some(open) })), Ok("batch_id=b1\nbranch=spira/queue/b1\n".into()));
    }

    #[test]
    fn a_stalled_loop_restarts_only_a_unit_the_unit_manager_reports_failed() {
        use runtime::units::{ScriptedUnits, UnitFacts, UnitState};
        let dir = scratch_dir("loop-stalled-units");
        fs::write(dir.join("landing.log"), "2021-01-01T00:00:00Z spira: landing: pass complete\n").unwrap();
        let mut cfg = test_config(&dir, KNOWN_EPOCH + 10_000);
        let units = ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0)));
        units.install("spira-landing.service", UnitFacts { state: UnitState::Failed, main_pid: None, exit: None, since_ms: None });
        cfg.seams.units = std::sync::Arc::new(units);
        let (_, remedy, _) = detect_loop_stalled(&cfg, &mut StateMap::new());
        assert_eq!(remedy, "det-restart");
        let cfg = test_config(&dir, KNOWN_EPOCH + 10_000);
        let (_, remedy, _) = detect_loop_stalled(&cfg, &mut StateMap::new());
        assert_eq!(remedy, "inference");
    }
}

#[cfg(test)]
mod fork_fence {
    #[test]
    fn czar_pass_starts_no_spira_child_that_is_not_a_named_fork_edge() {
        let got = forge::fork_scan::scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(got.is_empty(), "unmarked children:\n{}", got.join("\n"));
    }
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    #[test]
    fn a_czar_lane_stopped_on_every_repo_skips_the_pass() {
        let d = testkit::TempDir::new("czar-pass-lane");
        let repos = ["spira".to_string(), "other".to_string()];
        spira_config::scoped_stop::pause_repo(d.path(), "spira", &["czar".to_string()], "cutover-mode", "cutover", "world stop", 1).unwrap();
        assert_eq!(czar_lane_stopped(spira_config::scoped_stop::stop_everywhere(d.path(), &repos, CZAR_LANE)), None, "the other repo still runs");
        spira_config::scoped_stop::pause_repo(d.path(), "other", &["czar".to_string()], "cutover-mode", "cutover", "world stop", 1).unwrap();
        let why = czar_lane_stopped(spira_config::scoped_stop::stop_everywhere(d.path(), &repos, CZAR_LANE)).expect("stopped everywhere");
        assert!(why.starts_with("czar pass: skipped") && why.contains("world stop"), "{why}");
    }
}
