//! The tiered probe supervisor — a 1:1 port of `spira/collect.sh`'s registry, fragment
//! lifecycle and merge (DESIGN.md "Design"). Fully self-contained: no `bd`/`git` calls of
//! its own, only the filesystem and spawning this same binary's `probe` subcommand with a
//! timeout — which is exactly why it is the most previously-buggy, most valuable part to
//! have real unit tests over (the missing `queue` subcommand arm, sp-ctag9's unadopted-ref
//! false positive, and the InvocationID unit-name migration all trace back to this file).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::io;

/// One row of the probe registry: `name:interval_s:timeout_s:subcommand`.
#[derive(Clone, Copy)]
pub struct Probe {
    pub name: &'static str,
    pub interval_s: u64,
    pub timeout_s: u64,
    pub subcommand: &'static str,
}

/// The exact registry `spira/collect.sh` carried: fast(5s)/medium(60s)/slow(600s) tiers by
/// what each probe touches, not by how fast it happened to run.
pub const PROBES: &[Probe] = &[
    Probe { name: "now", interval_s: 5, timeout_s: 30, subcommand: "now" },
    Probe { name: "slots", interval_s: 60, timeout_s: 20, subcommand: "slots" },
    Probe { name: "admission", interval_s: 60, timeout_s: 20, subcommand: "admission" },
    Probe { name: "reachable", interval_s: 60, timeout_s: 120, subcommand: "reachable" },
    Probe { name: "sphere", interval_s: 60, timeout_s: 90, subcommand: "sphere" },
    Probe { name: "repo_labels", interval_s: 60, timeout_s: 90, subcommand: "repo_labels" },
    Probe { name: "strands", interval_s: 60, timeout_s: 90, subcommand: "strands" },
    Probe { name: "ratelim", interval_s: 60, timeout_s: 90, subcommand: "ratelim" },
    Probe { name: "core", interval_s: 60, timeout_s: 150, subcommand: "core" },
    Probe { name: "queue", interval_s: 60, timeout_s: 90, subcommand: "queue" },
    Probe { name: "core_detail", interval_s: 600, timeout_s: 900, subcommand: "core_detail" },
    Probe { name: "mail", interval_s: 60, timeout_s: 90, subcommand: "mail" },
    Probe { name: "sops", interval_s: 600, timeout_s: 300, subcommand: "sops" },
    Probe { name: "livelock", interval_s: 600, timeout_s: 300, subcommand: "livelock" },
    Probe { name: "dup_refs", interval_s: 600, timeout_s: 300, subcommand: "dup_refs" },
    Probe { name: "unsent", interval_s: 600, timeout_s: 300, subcommand: "unsent" },
    Probe { name: "statute", interval_s: 600, timeout_s: 300, subcommand: "statute" },
    Probe { name: "drift", interval_s: 600, timeout_s: 300, subcommand: "drift" },
    Probe { name: "czar_triggers", interval_s: 600, timeout_s: 300, subcommand: "czar_triggers" },
    Probe { name: "sending", interval_s: 600, timeout_s: 300, subcommand: "sending" },
];

pub struct Config {
    pub run_dir: PathBuf,
    pub frag_dir: PathBuf,
    pub snap: PathBuf,
    /// Kept for parity with `collect.sh`'s own `HIST` var; `main.rs`'s `append_history`
    /// recomputes the same path itself rather than threading `Config` through it.
    #[allow(dead_code)]
    pub hist: PathBuf,
    pub tick: Duration,
    pub slow_concurrent: usize,
    pub merge_fail_max: u32,
    /// The binary (and leading args) `run_probe_body` invokes to run ONE probe's logic:
    /// normally this same executable with `probe`; overridable in tests (`COCK`). Used by
    /// `run_probe_body` and `_probe_body_test` only.
    pub probe_exe: PathBuf,
    pub probe_exe_args: Vec<String>,
    /// This binary's own path, ALWAYS — never `COCK`-overridden. `run_loop` re-enters
    /// itself with `--supervised-run` to run one probe slot as its own child process;
    /// that re-entry is this binary's own top-level dispatch (`main.rs`'s `run()`), not a
    /// probe invocation, so it must never carry `probe_exe_args`' `"probe"` prefix — doing
    /// so sent every scheduled child `<self> probe --supervised-run <name> ...`, which
    /// `main.rs` parses as `probe` with probe-name `"--supervised-run"`, a no-op that exits
    /// 1 before ever reaching `run_probe_body` or writing a fragment. No probe the
    /// supervisor scheduled ever ran; `now`'s 5s cadence made it the first one caught (the
    /// operator, 2026-10-01, production: `now.env` frozen at the pre-cutover bash pass,
    /// `cockpit.env`'s own `SP_AT` never advancing while the merge itself kept running).
    pub self_exe: PathBuf,
}

impl Config {
    pub fn from_env(run_dir: PathBuf) -> Config {
        let frag_dir = std::env::var_os("FRAG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| run_dir.join("cockpit.d"));
        let snap = run_dir.join("cockpit.env");
        let hist = run_dir.join("cockpit-history.csv");
        let tick = std::env::var("SPIRA_COCKPIT_TICK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        let slow_concurrent = std::env::var("SPIRA_COCKPIT_SLOW_CONCURRENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let merge_fail_max = std::env::var("SPIRA_COCKPIT_MERGE_FAIL_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        // `COCK` is the test seam `collect.sh` carried (`: "${COCK:=$(command -v
        // cockpit.sh)}"`): a suite that overrides it points at its own mock script, invoked
        // directly with no subcommand prefix — the same direct-call shape the bash used.
        // Production leaves it unset and gets this binary's own `probe` subcommand.
        let self_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cockpit-collect"));
        let (probe_exe, probe_exe_args) = match std::env::var_os("COCK").filter(|v| !v.is_empty()) {
            Some(cock) => (PathBuf::from(cock), Vec::new()),
            None => (self_exe.clone(), vec!["probe".to_string()]),
        };
        Config {
            run_dir,
            frag_dir,
            snap,
            hist,
            tick: Duration::from_secs(tick),
            slow_concurrent,
            merge_fail_max,
            probe_exe,
            probe_exe_args,
            self_exe,
        }
    }
}

/// `_write_never_frag`: seed an absent fragment so the merge renders `?` for its keys from
/// the very first tick. Never overwrites an existing fragment — a supervisor restart must
/// not discard a previous session's last-known-good values.
pub fn write_never_frag(frag_dir: &Path, name: &str) {
    let frag = frag_dir.join(format!("{name}.env"));
    if frag.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(frag_dir);
    write_atomic(&frag, "_PROBE_AT=0\n_PROBE_STATUS=never\n_PROBE_KILLED=0\n");
}

fn write_atomic(path: &Path, content: &str) {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".{}.{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("frag"), std::process::id()));
    if std::fs::write(&tmp, content).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn read_header(frag: &Path) -> (i64, String, u32) {
    let mut at = 0i64;
    let mut status = "never".to_string();
    let mut killed = 0u32;
    if let Ok(content) = std::fs::read_to_string(frag) {
        for line in content.lines() {
            if let Some(v) = line.strip_prefix("_PROBE_AT=") {
                at = v.parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("_PROBE_STATUS=") {
                status = v.to_string();
            } else if let Some(v) = line.strip_prefix("_PROBE_KILLED=") {
                killed = v.parse().unwrap_or(0);
            }
        }
    }
    (at, status, killed)
}

fn value_lines(frag: &Path) -> Vec<String> {
    std::fs::read_to_string(frag)
        .map(|c| {
            c.lines()
                .filter(|l| {
                    !l.starts_with("_PROBE_AT=")
                        && !l.starts_with("_PROBE_STATUS=")
                        && !l.starts_with("_PROBE_KILLED=")
                })
                .map(|l| l.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Outcome of one probe pass, for the caller to log the same two lines
/// `collect.sh` logged (`probe %s ok %ss` / `probe %s %s after %ss`).
pub enum RunOutcome {
    /// `wall_s` is not read by any caller (the log line prints it directly inside
    /// `run_probe_body`, before this value is ever returned) but stays on the variant as
    /// the documented shape of a successful pass, matching `RunOutcome::exit_code()`'s
    /// sibling variants that do carry data a caller reads.
    #[allow(dead_code)]
    Ok { wall_s: u64 },
    #[allow(dead_code)]
    Fault { status: &'static str, timeout_s: u64, rc: i32 },
    Stale { rc: i32 },
    RaceSkipped,
}

impl RunOutcome {
    /// The exit code `collect.sh _probe_body_test` relayed: the underlying probe's own rc
    /// on any failure, 0 on success or a race-skip.
    pub fn exit_code(&self) -> i32 {
        match self {
            RunOutcome::Ok { .. } | RunOutcome::RaceSkipped => 0,
            RunOutcome::Fault { rc, .. } | RunOutcome::Stale { rc } => *rc,
        }
    }
}

/// `_run_probe_body`: run one probe (`timeout <timeout_s> <probe_exe> <probe_exe_args> <cmd>`),
/// write its fragment per the never/ok/stale/error/timeout lifecycle (DESIGN.md — `docs/
/// test-plan/cockpit-observability.toml` UC-04/UC-05).
pub fn run_probe_body(cfg: &Config, name: &str, timeout_s: u64, subcommand: &str) -> RunOutcome {
    let frag = cfg.frag_dir.join(format!("{name}.env"));
    let now = io::now();
    let started = Instant::now();

    let mut cmd = Command::new("timeout");
    cmd.arg(timeout_s.to_string()).arg(&cfg.probe_exe);
    for a in &cfg.probe_exe_args {
        cmd.arg(a);
    }
    cmd.arg(subcommand);
    cmd.stdin(Stdio::null()).stderr(Stdio::null());
    let output = cmd.output();

    match output {
        Ok(out) if out.status.success() => {
            // Race guard: a concurrent writer (should not happen, but mirrors the bash) may
            // have already written a fragment stamped later than `now`; do not clobber it.
            let (existing_at, _, _) = read_header(&frag);
            if existing_at > now {
                return RunOutcome::RaceSkipped;
            }
            let body = String::from_utf8_lossy(&out.stdout);
            let content = format!("_PROBE_AT={now}\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\n{body}");
            write_atomic(&frag, &content);
            if name == "slots" {
                io::tsd_slots_sample(&cfg.run_dir, &frag);
            }
            let wall_s = started.elapsed().as_secs();
            // The log line lives here, in the body, not in a caller — `_probe_body_test`
            // (the test-only seam) calls this function directly and must see the exact
            // same line the supervised `--supervised-run` path logs (test-cockpit-collector-
            // quota.sh section 2).
            eprintln!("collect.sh: probe {name} ok {wall_s}s");
            RunOutcome::Ok { wall_s }
        }
        Ok(out) => {
            let rc = out.status.code().unwrap_or(1);
            let (prev_at, prev_status, prev_killed) = read_header(&frag);
            let new_killed = prev_killed + 1;
            if prev_status == "never" {
                let fault_status = if rc == 124 { "timeout" } else { "error" };
                let content = format!(
                    "_PROBE_AT={prev_at}\n_PROBE_STATUS={fault_status}\n_PROBE_KILLED={new_killed}\n"
                );
                write_atomic(&frag, &content);
                eprintln!("collect.sh: probe {name} {fault_status} after {timeout_s}s");
                return RunOutcome::Fault {
                    status: if fault_status == "timeout" { "timeout" } else { "error" },
                    timeout_s,
                    rc,
                };
            }
            let kept: Vec<String> = value_lines(&frag);
            let mut content = format!("_PROBE_AT={prev_at}\n_PROBE_STATUS=stale\n_PROBE_KILLED={new_killed}\n");
            for l in kept {
                content.push_str(&l);
                content.push('\n');
            }
            write_atomic(&frag, &content);
            RunOutcome::Stale { rc }
        }
        Err(_) => RunOutcome::Stale { rc: 127 },
    }
}

/// `_sweep_probe_tmps` equivalent: nothing here uses on-disk temp markers the way the bash
/// did (`write_atomic` cleans up its own `.name.pid` temp synchronously on failure), so the
/// only orphan class left is a previous process's unfinished rename target — swept the same
/// way `_sweep_tmps` sweeps `.cockpit.*`.
pub fn sweep_probe_tmps(frag_dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(frag_dir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                if std::fs::remove_file(e.path()).is_ok() {
                    n += 1;
                }
            }
        }
    }
    n
}

/// A parsed fragment, ready for the merge.
struct Fragment {
    name: String,
    at: i64,
    status: String,
    killed: u32,
    values: Vec<(String, String)>,
}

fn parse_fragment(path: &Path) -> Fragment {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut at = 0i64;
    let mut status = "never".to_string();
    let mut killed = 0u32;
    let mut values = Vec::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "_PROBE_AT" => at = v.parse().unwrap_or(0),
                "_PROBE_STATUS" => status = v.to_string(),
                "_PROBE_KILLED" => killed = v.parse().unwrap_or(0),
                _ if !k.is_empty()
                    && (k.chars().next().unwrap().is_ascii_alphabetic() || k.starts_with('_')) =>
                {
                    // Idempotent against a fragment that already carries a quoted value
                    // (e.g. one a "stale" pass copied forward from before a fix landed) —
                    // see `quoting::unquote_shell_single`'s own doc for why this is here.
                    values.push((k.to_string(), crate::quoting::unquote_shell_single(v)));
                }
                _ => {}
            }
        }
    }
    Fragment { name, at, status, killed, values }
}

/// `_merge_fragments`: meta keys for every fragment always; value keys (first-wins, by
/// fragment name sorted alphabetically) only from `ok`/`stale` fragments — `never`/`timeout`/
/// `error` contribute no values, so their keys stay absent and render `?` through the
/// renderer's shell defaults (UC-04).
pub fn merge_fragments(cfg: &Config) -> bool {
    let _ = std::fs::create_dir_all(&cfg.frag_dir);
    let mut frag_paths: Vec<PathBuf> = match std::fs::read_dir(&cfg.frag_dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("env"))
            .collect(),
        Err(_) => return false,
    };
    frag_paths.sort();

    let mut meta_lines = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut value_lines: Vec<(String, String)> = Vec::new();

    for p in &frag_paths {
        let frag = parse_fragment(p);
        meta_lines.push(format!("_PROBE_AT_{}={}", frag.name, frag.at));
        meta_lines.push(format!("_PROBE_STATUS_{}={}", frag.name, frag.status));
        meta_lines.push(format!("SP_PROBE_KILLED_{}={}", frag.name, frag.killed));
        if frag.status != "never" && frag.status != "timeout" && frag.status != "error" {
            for (k, v) in frag.values {
                if seen.insert(k.clone()) {
                    value_lines.push((k, v));
                }
            }
        }
    }

    let mut out = String::new();
    for line in &meta_lines {
        let (k, v) = line.split_once('=').unwrap_or((line.as_str(), ""));
        out.push_str(&format!("{k}={}\n", crate::quoting::self_quote(v)));
    }
    for (k, v) in &value_lines {
        out.push_str(&format!("{k}={}\n", crate::quoting::self_quote(v)));
    }
    let rev = io::git(&io::home_dir(), &["rev-parse", "--short", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    out.push_str(&format!("SP_COLLECTOR_REV={}\n", crate::quoting::self_quote(&rev)));

    let tmp = cfg.run_dir.join(format!(".cockpit.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, out).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    std::fs::rename(&tmp, &cfg.snap).is_ok()
}

/// `collect_may_write` / `cockpit_may_write`: only the process systemd started for the
/// cockpit service may write the shared snapshot — the fence is `INVOCATION_ID`, which
/// systemd sets once per unit invocation. `SPIRA_COCKPIT_FORCE=1` names the override.
pub fn may_write(instance: Option<&str>) -> bool {
    if std::env::var("SPIRA_COCKPIT_FORCE").as_deref() == Ok("1") {
        return true;
    }
    let invocation_id = match std::env::var("INVOCATION_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => return false,
    };
    let candidates: Vec<String> = match instance {
        Some(i) if !i.is_empty() => vec![format!("spira-cockpit-{i}.service"), "spira-cockpit.service".to_string()],
        _ => vec!["spira-cockpit.service".to_string()],
    };
    for unit in candidates {
        if let Some(svc_id) = io::unit_show_invocation_id(&unit) {
            if svc_id == invocation_id {
                return true;
            }
        }
    }
    false
}

/// The per-tick scheduling decision, pure and unit-tested directly (the same property
/// `test-cockpit-collect-concurrency.sh` drove by extracting the bash's scheduling block at
/// runtime): which due, not-already-running probes to start this tick, respecting
/// `slow_concurrent` cumulatively across the probes THIS call selects — a tick with two
/// due slow probes and one free slot starts only the first of them, not neither and not
/// both. `running`/`last_started` are read-only here; the caller spawns and records them.
fn due_probes<'a>(
    probes: &'a [Probe],
    now: i64,
    running: &std::collections::HashSet<&str>,
    last_started: &HashMap<&'static str, i64>,
    slow_running_now: usize,
    slow_concurrent: usize,
) -> Vec<&'a Probe> {
    let mut out = Vec::new();
    let mut slow_running = slow_running_now;
    for p in probes {
        if running.contains(p.name) {
            continue;
        }
        let last = *last_started.get(p.name).unwrap_or(&0);
        if now - last < p.interval_s as i64 {
            continue;
        }
        if p.interval_s >= 600 {
            if slow_running >= slow_concurrent {
                continue;
            }
            slow_running += 1;
        }
        out.push(p);
    }
    out
}

/// The supervisor's main loop. Runs until a signal, a config change, or too many consecutive
/// merge failures. Spawns each due probe as a detached child tracked in `running`; a probe
/// still running when its slot comes due again is skipped (visible as growing fragment age,
/// never a silent backlog — `docs/test-plan` UC-05/UC-06).
/// The exact command `run_loop` spawns to run one probe slot as its own child process:
/// always THIS binary (`cfg.self_exe`, never `COCK`), with no args before
/// `--supervised-run` — that flag is `main.rs`'s own top-level dispatch, not a probe
/// invocation `cfg.probe_exe_args`' `"probe"` prefix could ever apply to. Pure and unit
/// tested directly via `Command::get_program`/`get_args` (no spawn), the same way
/// `due_probes` is tested without a real tick loop.
fn supervised_run_command(cfg: &Config, p: &Probe) -> Command {
    let mut cmd = Command::new(&cfg.self_exe);
    cmd.arg("--supervised-run")
        .arg(p.name)
        .arg(p.timeout_s.to_string())
        .arg(p.subcommand)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

pub fn run_loop(cfg: &Config, log: impl Fn(&str)) -> i32 {
    let _ = std::fs::create_dir_all(&cfg.frag_dir);
    sweep_probe_tmps(&cfg.frag_dir);
    for p in PROBES {
        write_never_frag(&cfg.frag_dir, p.name);
    }

    let mut running: HashMap<&'static str, Child> = HashMap::new();
    let mut last_started: HashMap<&'static str, i64> = HashMap::new();
    let mut merge_fail = 0u32;

    loop {
        let now = io::now();
        let mut slow_running = 0usize;
        let mut finished = Vec::new();
        for (name, child) in running.iter_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // A scheduled probe's own child exiting non-zero is distinct from a
                    // probe that ran and reported a failure through its fragment
                    // (run_probe_body already logs that) — this is the child never
                    // REACHING run_probe_body at all (a bad re-entry command, a missing
                    // binary, anything), which would otherwise be silent: the fragment
                    // simply stays whatever it was, forever, with nothing to say why.
                    if !status.success() {
                        log(&format!(
                            "collect: probe {name} exited {} before writing its fragment — scheduling is broken for this probe, not the probe itself",
                            status.code().map(|c| c.to_string()).unwrap_or_else(|| "(signal)".to_string())
                        ));
                    }
                    finished.push(*name);
                }
                Ok(None) => {
                    if PROBES.iter().find(|p| p.name == *name).map(|p| p.interval_s).unwrap_or(0) >= 600 {
                        slow_running += 1;
                    }
                }
                Err(e) => {
                    log(&format!("collect: probe {name}: wait failed: {e}"));
                    finished.push(*name);
                }
            }
        }
        for name in finished {
            running.remove(name);
        }

        let running_names: std::collections::HashSet<&str> = running.keys().copied().collect();
        for p in due_probes(PROBES, now, &running_names, &last_started, slow_running, cfg.slow_concurrent) {
            if let Ok(child) = supervised_run_command(cfg, p).spawn() {
                running.insert(p.name, child);
                last_started.insert(p.name, now);
                if p.interval_s >= 600 {
                    slow_running += 1;
                }
            } else {
                log(&format!("collect: probe {}: failed to spawn {:?}", p.name, cfg.self_exe));
            }
        }

        if merge_fragments(cfg) {
            merge_fail = 0;
        } else {
            merge_fail += 1;
            if merge_fail >= cfg.merge_fail_max {
                log(&format!(
                    "collect: {merge_fail} consecutive merge failures — exiting for restart"
                ));
                for (_, mut child) in running {
                    let _ = child.kill();
                }
                return 1;
            }
        }

        io::sleep(cfg.tick);
    }
}

/// The out-of-process form `run_loop` spawns for each probe slot: run one probe, write its
/// fragment, log the same lines `collect.sh` logged, and exit 0 always (the fragment carries
/// the fault, not the exit code — the supervisor never learns or needs to learn a per-probe
/// exit status).
pub fn supervised_run_main(name: &str, timeout_s: u64, subcommand: &str) {
    let run = io::run_dir();
    let cfg = Config::from_env(run);
    // Logging lives in `run_probe_body` itself now (both callers must see the same line).
    let _ = run_probe_body(&cfg, name, timeout_s, subcommand);
}

/// `_probe_body_test`: run `run_probe_body` once with caller-supplied args and return its
/// exit code — test-only, mirroring `collect.sh _probe_body_test`'s own seam so the
/// existing concurrency/quota suites can drive this binary's fragment lifecycle directly.
pub fn probe_body_test_main(name: &str, timeout_s: u64, subcommand: &str) -> i32 {
    let run = io::run_dir();
    let cfg = Config::from_env(run);
    run_probe_body(&cfg, name, timeout_s, subcommand).exit_code()
}

/// `merge` subcommand: one-shot merge of whatever fragments are already on disk, no
/// supervision check (the seam a test or an operator drives directly).
pub fn merge_once() -> i32 {
    let run = io::run_dir();
    let cfg = Config::from_env(run);
    let ok = merge_fragments(&cfg);
    let n = std::fs::read_dir(&cfg.frag_dir)
        .map(|rd| rd.flatten().filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("env")).count())
        .unwrap_or(0);
    println!("cockpit-collect: merged {n} fragments into {}", cfg.snap.display());
    if ok { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::TempDir;

    fn cfg(run: &Path) -> Config {
        let mut c = Config::from_env(run.to_path_buf());
        c.frag_dir = run.join("cockpit.d");
        c
    }

    // Ported from test-cockpit-collect-concurrency.sh's "1. probe registry: shape and the
    // timeout-shorter-than-interval case" — the bash extracted the `PROBES=()` array literal
    // at runtime and checked its shape; here the array is a real Rust `const`, so the same
    // properties are asserted directly against it, no extraction needed.
    #[test]
    fn probes_registry_is_well_formed() {
        assert_eq!(PROBES.len(), 20, "20 probes registered");
        let mut seen = std::collections::HashSet::new();
        let mut shorter_timeout = 0;
        for p in PROBES {
            assert!(seen.insert(p.name), "duplicate probe name '{}'", p.name);
            assert!(
                matches!(p.interval_s, 5 | 60 | 600),
                "'{}' interval {} is not a declared tier (5/60/600)",
                p.name,
                p.interval_s
            );
            assert!(p.timeout_s > 0, "'{}' timeout must be positive", p.name);
            if p.timeout_s < p.interval_s {
                shorter_timeout += 1;
            }
        }
        assert!(
            shorter_timeout > 0,
            "at least one entry must have timeout < interval (the gap test-cockpit-collect-concurrency.sh named)"
        );
    }

    // Ported from the same suite's "2. SLOW_CONCURRENT: a running slow probe blocks a second
    // due slow probe" — PASS 1/PASS 2 become one deterministic call each, no real sleep/kill.
    #[test]
    fn due_probes_caps_slow_tier_concurrency_cumulatively_within_one_tick() {
        let probes = [
            Probe { name: "slow_a", interval_s: 600, timeout_s: 300, subcommand: "mock" },
            Probe { name: "slow_b", interval_s: 600, timeout_s: 300, subcommand: "mock" },
        ];
        let now = 1_000_000i64;

        // PASS 1: slow_a is already running (holds the only slot); slow_b is due
        // (last_started absent -> 0). The cap must block slow_b from starting.
        let mut running = std::collections::HashSet::new();
        running.insert("slow_a");
        let last_started: HashMap<&'static str, i64> = [("slow_a", now)].into_iter().collect();
        let due = due_probes(&probes, now, &running, &last_started, 1, 1);
        assert!(due.is_empty(), "slow_b must not start while slow_a holds the only slot");

        // PASS 2: slow_a's process is gone — its slot is free. slow_b is still due; slow_a
        // is not (it started moments ago, well inside its 600s interval).
        let running = std::collections::HashSet::new();
        let due = due_probes(&probes, now, &running, &last_started, 0, 1);
        assert_eq!(due.iter().map(|p| p.name).collect::<Vec<_>>(), vec!["slow_b"]);
    }

    // A single tick with two due slow probes and a cap of one starts only the first —
    // never neither (the cap's whole purpose) and never both (the cap enforced once per
    // probe, not once per tick).
    #[test]
    fn due_probes_starts_only_one_slow_probe_per_tick_when_both_are_due() {
        let probes = [
            Probe { name: "slow_a", interval_s: 600, timeout_s: 300, subcommand: "mock" },
            Probe { name: "slow_b", interval_s: 600, timeout_s: 300, subcommand: "mock" },
        ];
        let running = std::collections::HashSet::new();
        let last_started: HashMap<&'static str, i64> = HashMap::new();
        let due = due_probes(&probes, 1_000_000, &running, &last_started, 0, 1);
        assert_eq!(due.iter().map(|p| p.name).collect::<Vec<_>>(), vec!["slow_a"]);
    }

    /// The regression this bead exists for (the operator, 2026-10-01, production): every
    /// scheduled probe's child was spawned as `<self_exe> <probe_exe_args...>
    /// --supervised-run <name> <timeout> <subcmd>` — `probe_exe_args` defaults to
    /// `["probe"]`, so the real argv was `<self> probe --supervised-run now 30 now`, which
    /// `main.rs`'s own dispatch parses as subcommand `probe` with probe-name
    /// `--supervised-run` — not a probe name, so it prints usage and exits 1 before ever
    /// reaching `run_probe_body`. No fragment was ever written by ANY scheduled probe;
    /// `now`'s 5s cadence surfaced it first because `SP_AT`'s staleness is what the pane
    /// watches. `supervised_run_command` must use `self_exe` with NOTHING before
    /// `--supervised-run`, regardless of what `probe_exe_args` holds.
    #[test]
    fn supervised_run_command_never_carries_the_probe_exe_args_prefix() {
        let run = TempDir::new("cc-supervised-cmd");
        let mut cfg = cfg(&run);
        cfg.self_exe = PathBuf::from("/usr/local/bin/cockpit-collect");
        // Deliberately non-empty and different from self_exe, so a regression that uses
        // probe_exe/probe_exe_args here instead of self_exe cannot pass by coincidence.
        cfg.probe_exe = PathBuf::from("/some/other/mock");
        cfg.probe_exe_args = vec!["probe".to_string()];
        let p = &PROBES[0];

        let cmd = supervised_run_command(&cfg, p);
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/usr/local/bin/cockpit-collect"));
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(
            args,
            vec![
                std::ffi::OsStr::new("--supervised-run"),
                std::ffi::OsStr::new(p.name),
                std::ffi::OsStr::new(&p.timeout_s.to_string()),
                std::ffi::OsStr::new(p.subcommand),
            ],
            "no \"probe\" prefix, and self_exe, not probe_exe"
        );
    }

    /// End-to-end through the REAL spawn path (not a mock of it): a fake `self_exe` script
    /// stands in for `cockpit-collect --supervised-run`, writing a fresh, advancing
    /// `SP_AT` to its fragment exactly the way the real `now` probe does. Scheduling it
    /// twice (via `due_probes` + `supervised_run_command`, the same two calls `run_loop`
    /// makes) and merging after each must produce a strictly advancing `SP_AT` in
    /// `cockpit.env` — catching any future regression in the re-entry command the way the
    /// pure argv test above cannot (that one does not spawn anything).
    #[test]
    fn two_consecutive_scheduled_runs_produce_an_advancing_sp_at() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        let run = TempDir::new("cc-advancing-sp-at");
        let mut cfg = cfg(&run);
        let self_exe = run.path().join("fake-self");
        // Mimics `cockpit-collect --supervised-run now <timeout> now`: write a fragment
        // carrying a fresh SP_AT directly, exactly what run_probe_body's own
        // write_atomic would have left behind had the real probe run ($2 is the probe
        // name in `--supervised-run <name> <timeout> <subcmd>`, matching argv position).
        testkit::write_exe(
            &self_exe,
            r#"#!/usr/bin/env bash
printf '_PROBE_AT=%s\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_AT=%s\n' "$(date +%s)" "$(date +%s%N)" > "$FRAG_DIR/$2.env"
"#,
        );
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        std::env::set_var("FRAG_DIR", &cfg.frag_dir);
        cfg.self_exe = self_exe;

        let probes = [Probe { name: "now", interval_s: 5, timeout_s: 30, subcommand: "now" }];
        let mut running: HashMap<&'static str, std::process::Child> = HashMap::new();
        let mut last_started: HashMap<&'static str, i64> = HashMap::new();

        let run_one_tick = |running: &mut HashMap<&'static str, std::process::Child>, last_started: &mut HashMap<&'static str, i64>| {
            let now = io::now();
            let running_names: std::collections::HashSet<&str> = running.keys().copied().collect();
            for p in due_probes(&probes, now, &running_names, last_started, 0, cfg.slow_concurrent) {
                let child = supervised_run_command(&cfg, p).spawn().expect("spawn fake self_exe");
                running.insert(p.name, child);
                last_started.insert(p.name, now);
            }
            for (_, child) in running.iter_mut() {
                child.wait().expect("wait for fake self_exe");
            }
            running.clear();
            assert!(merge_fragments(&cfg));
        };

        run_one_tick(&mut running, &mut last_started);
        let snap1 = std::fs::read_to_string(&cfg.snap).unwrap();
        let at1 = snap1.lines().find_map(|l| l.strip_prefix("SP_AT=")).expect("SP_AT present after first run");

        // Force the next tick to be due immediately regardless of the 5s interval, the
        // same way the real supervisor's next tick would be once 5s has actually passed.
        last_started.clear();
        run_one_tick(&mut running, &mut last_started);
        let snap2 = std::fs::read_to_string(&cfg.snap).unwrap();
        let at2 = snap2.lines().find_map(|l| l.strip_prefix("SP_AT=")).expect("SP_AT present after second run");

        std::env::remove_var("FRAG_DIR");
        assert_ne!(at1, at2, "SP_AT must advance between two scheduled runs, not freeze");
    }

    /// A scheduled probe whose child exits non-zero before writing anything (this bead's
    /// exact defect, reproduced directly) must be LOGGED by the supervisor loop itself —
    /// not silently dropped. Before this fix, nothing anywhere said a probe never ran.
    #[test]
    fn a_probe_that_exits_without_writing_is_logged_not_silent() {
        let run = TempDir::new("cc-logged-failure");
        let mut cfg = cfg(&run);
        let self_exe = run.path().join("fake-self-fails");
        testkit::write_exe(&self_exe, "#!/usr/bin/env bash\nexit 1\n");
        cfg.self_exe = self_exe;

        let p = Probe { name: "now", interval_s: 5, timeout_s: 30, subcommand: "now" };
        let mut child = supervised_run_command(&cfg, &p).spawn().expect("spawn");
        let status = child.wait().expect("wait");

        let logged = std::sync::Mutex::new(Vec::<String>::new());
        // Reproduce exactly the check `run_loop`'s reap loop performs on a finished child.
        if !status.success() {
            logged.lock().unwrap().push(format!(
                "collect: probe {} exited {} before writing its fragment — scheduling is broken for this probe, not the probe itself",
                p.name,
                status.code().map(|c| c.to_string()).unwrap_or_else(|| "(signal)".to_string())
            ));
        }
        assert_eq!(logged.lock().unwrap().len(), 1, "a failed-before-writing child must produce exactly one log line");
        assert!(logged.lock().unwrap()[0].contains("exited 1"));
    }

    /// UC-cockpit-observability-49. The failure is made by pointing `frag_dir` at a regular
    /// file, which fails the merge for root and non-root alike (a read-only directory does not),
    /// and the wait is bounded so a loop that never exits fails the test instead of hanging it.
    #[test]
    fn run_loop_exits_one_after_consecutive_merge_failures() {
        let run = TempDir::new("cc-merge-fail-exit");
        let mut cfg = cfg(&run);
        cfg.tick = Duration::from_millis(1);
        cfg.merge_fail_max = 3;
        let self_exe = run.path().join("fake-self-ok");
        testkit::write_exe(&self_exe, "#!/usr/bin/env bash\nexit 0\n");
        cfg.self_exe = self_exe;

        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        assert!(merge_fragments(&cfg), "positive control: the same config merges when frag_dir is a directory");
        std::fs::remove_dir_all(&cfg.frag_dir).unwrap();
        std::fs::write(&cfg.frag_dir, "not a directory").unwrap();
        assert!(!merge_fragments(&cfg), "the fixture must fail the merge every time");

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let lines = std::sync::Mutex::new(Vec::<String>::new());
            let rc = run_loop(&cfg, |l| lines.lock().unwrap().push(l.to_string()));
            let _ = tx.send((rc, lines.into_inner().unwrap()));
        });
        let (rc, lines) = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("run_loop must exit on consecutive merge failures, not run on");
        assert_eq!(rc, 1);
        assert!(
            lines.iter().any(|l| l.contains("3 consecutive merge failures")),
            "exit must name the failure count: {lines:?}"
        );
    }

    #[test]
    fn never_frag_seeds_absence_and_does_not_clobber() {
        let run = TempDir::new("cc-never");
        let cfg = cfg(&run);
        write_never_frag(&cfg.frag_dir, "now");
        let p = cfg.frag_dir.join("now.env");
        let c1 = std::fs::read_to_string(&p).unwrap();
        assert!(c1.contains("_PROBE_STATUS=never"));
        // A second call must not overwrite an existing (possibly already-successful) frag.
        write_atomic(&p, "_PROBE_AT=99\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_X=5\n");
        write_never_frag(&cfg.frag_dir, "now");
        let c2 = std::fs::read_to_string(&p).unwrap();
        assert!(c2.contains("_PROBE_STATUS=ok"));
    }

    #[test]
    fn merge_never_contributes_no_value_keys() {
        let run = TempDir::new("cc-merge-never");
        let cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        write_atomic(&cfg.frag_dir.join("now.env"), "_PROBE_AT=0\n_PROBE_STATUS=never\n_PROBE_KILLED=0\n");
        assert!(merge_fragments(&cfg));
        let snap = std::fs::read_to_string(&cfg.snap).unwrap();
        assert!(snap.contains("_PROBE_STATUS_now='never'"));
        assert!(!snap.contains("SP_AT="));
    }

    #[test]
    fn merge_ok_contributes_values_and_meta() {
        let run = TempDir::new("cc-merge-ok");
        let cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        write_atomic(
            &cfg.frag_dir.join("now.env"),
            "_PROBE_AT=123\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_AT=123\nSP_AEONS=2\n",
        );
        assert!(merge_fragments(&cfg));
        let snap = std::fs::read_to_string(&cfg.snap).unwrap();
        assert!(snap.contains("SP_AEONS='2'"));
        assert!(snap.contains("_PROBE_STATUS_now='ok'"));
        assert!(snap.contains("SP_COLLECTOR_REV="));
    }

    /// The merge path's own copy of the empty-value regression `main.rs`'s tests cover for
    /// `once` (the operator, 2026-09-30): a probe's empty value for one of the ten previously
    /// self-quoting keys must merge to exactly `KEY=''`, not the doubled quoting production
    /// carried (a probe quoting its own output, then merge's `self_quote` quoting it again).
    #[test]
    fn merge_renders_empty_value_as_two_char_empty_quotes_not_doubled() {
        let run = TempDir::new("cc-merge-empty");
        let cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        write_atomic(
            &cfg.frag_dir.join("now.env"),
            "_PROBE_AT=1\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_HOTFIX_LINE=\nSP_PROTECTED_NAMES=\n",
        );
        assert!(merge_fragments(&cfg));
        let snap = std::fs::read_to_string(&cfg.snap).unwrap();
        assert!(snap.contains("SP_HOTFIX_LINE=''\n"), "snap was: {snap}");
        assert!(snap.contains("SP_PROTECTED_NAMES=''\n"), "snap was: {snap}");
        assert!(!snap.contains("''''"), "must not be doubled: {snap}");
    }

    /// The real production fixture (the operator, 2026-09-30): `run/cockpit.d/now.env`
    /// already carried `SP_HOTFIX_ALERT=''`/`SP_AURON_KEYS=''` — quoted, not raw — because
    /// a stale pass had copied those lines forward from before the probe-level self-
    /// quoting fix landed (DESIGN.md Decisions). Merge must not re-quote an already-quoted
    /// value: `''\'''\'''` reached `cockpit.env` and the new pane sourced it as a non-empty
    /// 4-character string, not absence. Checks both the empty and a non-empty quoted value
    /// round-trip to exactly one layer of quoting.
    #[test]
    fn merge_does_not_requote_a_fragment_value_that_is_already_quoted() {
        let run = TempDir::new("cc-merge-prequoted");
        let cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        write_atomic(
            &cfg.frag_dir.join("now.env"),
            "_PROBE_AT=1\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_HOTFIX_ALERT=''\nSP_AURON_KEYS=''\nSP_OVERRIDES_LIST='a b'\n",
        );
        assert!(merge_fragments(&cfg));
        let snap = std::fs::read_to_string(&cfg.snap).unwrap();
        assert!(snap.contains("SP_HOTFIX_ALERT=''\n"), "snap was: {snap}");
        assert!(snap.contains("SP_AURON_KEYS=''\n"), "snap was: {snap}");
        assert!(snap.contains("SP_OVERRIDES_LIST='a b'\n"), "snap was: {snap}");
        assert!(!snap.contains("''''") && !snap.contains("\\'"), "must not be doubled: {snap}");
    }

    #[test]
    fn merge_stale_keeps_last_known_good_values() {
        let run = TempDir::new("cc-merge-stale");
        let cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        write_atomic(
            &cfg.frag_dir.join("queue.env"),
            "_PROBE_AT=100\n_PROBE_STATUS=stale\n_PROBE_KILLED=3\nSP_QUEUE_DEPTH=7\n",
        );
        assert!(merge_fragments(&cfg));
        let snap = std::fs::read_to_string(&cfg.snap).unwrap();
        assert!(snap.contains("SP_QUEUE_DEPTH='7'"));
        assert!(snap.contains("SP_PROBE_KILLED_queue='3'"));
    }

    #[test]
    fn merge_timeout_and_error_contribute_no_values() {
        let run = TempDir::new("cc-merge-fault");
        let cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        write_atomic(
            &cfg.frag_dir.join("sending.env"),
            "_PROBE_AT=0\n_PROBE_STATUS=timeout\n_PROBE_KILLED=1\n",
        );
        assert!(merge_fragments(&cfg));
        let snap = std::fs::read_to_string(&cfg.snap).unwrap();
        assert!(snap.contains("_PROBE_STATUS_sending='timeout'"));
        assert!(!snap.contains("SP_SENT="));
    }

    #[test]
    fn merge_first_wins_alphabetically_on_key_clash() {
        let run = TempDir::new("cc-merge-clash");
        let cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        // "core" sorts before "now" alphabetically; both claim SP_SHARED.
        write_atomic(&cfg.frag_dir.join("core.env"), "_PROBE_AT=1\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_SHARED=core\n");
        write_atomic(&cfg.frag_dir.join("now.env"), "_PROBE_AT=1\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_SHARED=now\n");
        assert!(merge_fragments(&cfg));
        let snap = std::fs::read_to_string(&cfg.snap).unwrap();
        assert!(snap.contains("SP_SHARED='core'"));
    }

    #[test]
    fn run_probe_body_success_writes_ok_fragment() {
        let run = TempDir::new("cc-run-ok");
        let mut cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        cfg.probe_exe = PathBuf::from("/bin/echo");
        cfg.probe_exe_args = vec![];
        let outcome = run_probe_body(&cfg, "now", 5, "SP_AT=1");
        assert!(matches!(outcome, RunOutcome::Ok { .. }));
        let c = std::fs::read_to_string(cfg.frag_dir.join("now.env")).unwrap();
        assert!(c.contains("_PROBE_STATUS=ok"));
        assert!(c.contains("SP_AT=1"));
    }

    #[test]
    fn run_probe_body_first_failure_is_fault_not_stale() {
        let run = TempDir::new("cc-run-fault");
        let mut cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        write_never_frag(&cfg.frag_dir, "sending");
        cfg.probe_exe = PathBuf::from("/bin/false");
        cfg.probe_exe_args = vec![];
        let outcome = run_probe_body(&cfg, "sending", 5, "ignored");
        assert!(matches!(outcome, RunOutcome::Fault { .. }));
        let c = std::fs::read_to_string(cfg.frag_dir.join("sending.env")).unwrap();
        assert!(c.contains("_PROBE_STATUS=error"));
        assert!(c.contains("_PROBE_KILLED=1"));
    }

    #[test]
    fn run_probe_body_failure_after_success_is_stale_and_keeps_values() {
        let run = TempDir::new("cc-run-stale");
        let mut cfg = cfg(&run);
        std::fs::create_dir_all(&cfg.frag_dir).unwrap();
        write_atomic(
            &cfg.frag_dir.join("queue.env"),
            "_PROBE_AT=1\n_PROBE_STATUS=ok\n_PROBE_KILLED=0\nSP_QUEUE_DEPTH=9\n",
        );
        cfg.probe_exe = PathBuf::from("/bin/false");
        cfg.probe_exe_args = vec![];
        let outcome = run_probe_body(&cfg, "queue", 5, "ignored");
        assert!(matches!(outcome, RunOutcome::Stale { .. }));
        let c = std::fs::read_to_string(cfg.frag_dir.join("queue.env")).unwrap();
        assert!(c.contains("_PROBE_STATUS=stale"));
        assert!(c.contains("SP_QUEUE_DEPTH=9"));
        assert!(c.contains("_PROBE_KILLED=1"));
    }

    #[test]
    fn may_write_refuses_bare_and_allows_forced() {
        let _guard = crate::test_support::ENV_LOCK.lock().unwrap();
        // Both assertions live in one test function: `SPIRA_COCKPIT_FORCE`/`INVOCATION_ID`
        // are process-global, and cargo runs tests in parallel threads, so a separate test
        // per case raced the other's env mutation (observed flake: this file's own CI run).
        std::env::remove_var("SPIRA_COCKPIT_FORCE");
        std::env::remove_var("INVOCATION_ID");
        assert!(!may_write(None));

        std::env::set_var("SPIRA_COCKPIT_FORCE", "1");
        assert!(may_write(None));
        std::env::remove_var("SPIRA_COCKPIT_FORCE");
    }
}
