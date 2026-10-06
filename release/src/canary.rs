//! `release canary` and `release canary-worker` (DESIGN.md "canary"): an end-to-end
//! pipeline canary on an isolated [`crate::stage`]. Files a synthetic bead, runs the real
//! `sentinel` (which dispatches the canary worker via `SPIRA_SUMMON`), runs the real
//! landing pass directly, and asserts that a commit naming the bead id appears on
//! `origin/main` inside a deadline. On failure it files a `spira,incident` bead in the
//! PRODUCTION database (the caller's own `SPIRA_DB`, saved before the stage's env
//! replaces it), then exits non-zero. Replaces `spira/canary.sh` and, for the worker,
//! `spira/canary-worker.sh`.
//!
//! THE ONE THING THAT IS FAKE IS THE MODEL. `SPIRA_SUMMON` points to `fake-summon.sh`,
//! which execs `release canary-worker` instead of `aeon.sh` — a scripted worker that
//! claims, commits and submits the bead through the stage's own lifecycle machine
//! (`crate::stage_lc`) without invoking Claude. `SPIRA_LAUNCH` records the
//! landing dispatch and exits 0; `release canary` drives `landing-pass land` directly so it
//! controls the timing.
//!
//! NOTHING TOUCHES THE REAL INSTANCE: every path this module writes to comes from the
//! stage's own env, asserted to resolve under `STAGE_ROOT` before anything runs.

use crate::stage::{self, Stage, StageOpts};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Every key `release stage up` exports — the lifecycle store's included, so an external
/// stage's `spira-lc` calls reach the stage's machine and never the operator's (sp-880u4).
const STAGE_KEYS: &[&str] = &[
    "SPIRA_HOME",
    "SPIRA_RUN",
    "SPIRA_DB",
    "SPIRA_BD",
    "SPIRA_REPO",
    "SPIRA_REPO_MAP",
    "SPIRA_FAYTHS",
    "SPIRA_MAX_AEONS",
    "SPIRA_NOTIFY",
    "SPIRA_SUMMON",
    "SPIRA_LAUNCH",
    "PATH",
    "SPIRA_PATH",
    "SPIRA_SCOPE_LABEL",
    "SPIRA_LC_HOST",
    "SPIRA_LC_PORT",
    "SPIRA_LC_DB",
    "SPIRA_LC_DATA_DIR",
    "SPIRA_LC_USER",
    "SPIRA_LC_PASSWORD_FILE",
    "SPIRA_LC_PASSWORD",
    "SPIRA_LC_SOCKET",
];

pub struct CanaryOpts {
    /// Use an already-running stage (its env, read from the caller's own process
    /// environment — the equivalent of `eval "$(stage.sh up)"` having already run) instead
    /// of standing one up and tearing it down here.
    pub external_stage: Option<PathBuf>,
    pub stage_opts: StageOpts,
    pub deadline: Duration,
    /// How many commit subjects on `origin/main` to search for the bead id (default 400).
    pub verdict_window: usize,
}

pub struct CanaryResult {
    pub stage_root: PathBuf,
    pub elapsed: Duration,
    pub commit: String,
}

fn log(s: &str) {
    eprintln!("{} canary: {s}", crate::fsutil::now_rfc3339());
}

fn run(cmd: &mut Command, what: &str) -> Result<std::process::Output, String> {
    cmd.output().map_err(|e| format!("cannot run {what}: {e}"))
}

fn run_ok(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = run(cmd, what)?;
    if !out.status.success() {
        return Err(format!("{what} failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Save the caller's own (production) identity before a stage's env replaces it, for
/// [`file_incident`].
struct ProdEnv {
    db: Option<String>,
    run: Option<String>,
    home: Option<String>,
    notify: Option<String>,
    bd: String,
    path: String,
    home_repo: Option<String>,
}

impl ProdEnv {
    /// `SPIRA_DB`, `SPIRA_RUN`, `SPIRA_NOTIFY`, `SPIRA_BD` and `SPIRA_HOME_REPO` are
    /// registered keys (spira/conf.d) — read through `cfg`, never a raw environment lookup
    /// (per Ryan 2026-10-05: one source of config). A `cfg` that cannot resolve at all (no
    /// `$SPIRA_TOML`) is treated the same as the key resolving empty: this is the best-effort
    /// production identity for [`file_incident`]'s own side channel, which already no-ops
    /// without a `db` — never a reason to fail `canary` itself. `SPIRA_HOME` is not a
    /// registered key, so it keeps reading the raw environment.
    fn capture() -> ProdEnv {
        let reg = |k: &str| spira_config::process::cfg(k).ok().filter(|v| !v.is_empty());
        ProdEnv {
            db: reg("SPIRA_DB"),
            run: reg("SPIRA_RUN"),
            home: std::env::var("SPIRA_HOME").ok().filter(|v| !v.is_empty()),
            notify: reg("SPIRA_NOTIFY"),
            bd: reg("SPIRA_BD").unwrap_or_default(),
            path: std::env::var("PATH").unwrap_or_default(),
            home_repo: reg("SPIRA_HOME_REPO"),
        }
    }
}

/// Write a `spira,incident` bead to the PRODUCTION database (never the stage's). Best
/// effort: a failure here must never change canary's own exit status.
fn file_incident(prod: &ProdEnv, title: &str, body: &str) {
    let Some(db) = &prod.db else { return };
    let mut cmd = Command::new("incident.sh");
    cmd.env("SPIRA_DB", db)
        .env("SPIRA_RUN", prod.run.as_deref().unwrap_or("/tmp/canary-inc"))
        .env("SPIRA_BD", &prod.bd)
        .env("PATH", &prod.path)
        .env("SPIRA_INCIDENT_TYPE", "bug")
        .env("SPIRA_INCIDENT_PRIORITY", "1")
        .env("SPIRA_INCIDENT_LABELS", "spira,incident")
        .env("SPIRA_INCIDENT_REPO", prod.home_repo.as_deref().unwrap_or("spira"))
        .env("SPIRA_INCIDENT_REF", "canary:pipeline")
        .env("SPIRA_INCIDENT_CAUSE", "canary-fail")
        .args(["file", title, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(h) = &prod.home {
        cmd.env("SPIRA_HOME", h);
    }
    if let Some(n) = &prod.notify {
        cmd.env("SPIRA_NOTIFY", n);
    } else {
        cmd.env("SPIRA_NOTIFY", "/bin/true");
    }
    if let Ok(mut child) = cmd.spawn() {
        use std::io::Write;
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(body.as_bytes());
        }
        let _ = child.wait();
    }
}

/// `release canary [--stage ROOT]`.
pub fn canary(o: &CanaryOpts) -> Result<CanaryResult, String> {
    let prod = ProdEnv::capture();
    let t0 = Instant::now();

    let (stage_root, env, own_stage) = match &o.external_stage {
        Some(root) => {
            if !root.join("spira/chamber/canary.fayth").is_file() {
                return Err(format!("--stage {} does not look like a stage", root.display()));
            }
            // Honour the caller's own environment for the rest (mirrors `eval "$(stage.sh
            // up)"` already having run in the calling shell).
            let mut env = Vec::new();
            for k in STAGE_KEYS {
                if let Ok(v) = std::env::var(k) {
                    env.push((k.to_string(), v));
                }
            }
            (root.clone(), env, false)
        }
        None => {
            let s: Stage = stage::up(&o.stage_opts)?;
            (s.root.clone(), s.env.clone(), true)
        }
    };
    log(&format!("stage: STAGE_ROOT={}", stage_root.display()));

    let result = run_canary_in(&stage_root, &env, &prod, o, t0);

    if own_stage {
        let _ = stage::down(&stage_root);
    }
    result
}

fn envmap(env: &[(String, String)]) -> BTreeMap<String, String> {
    env.iter().cloned().collect()
}

fn run_canary_in(stage_root: &Path, env: &[(String, String)], prod: &ProdEnv, o: &CanaryOpts, t0: Instant) -> Result<CanaryResult, String> {
    let e = envmap(env);
    let get = |k: &str| e.get(k).cloned().unwrap_or_default();

    // Isolation check: every stage path resolves under STAGE_ROOT — the lifecycle credential
    // and socket too, or a spira-lc call would reach the operator's machine.
    for k in ["SPIRA_DB", "SPIRA_RUN", "SPIRA_HOME", "SPIRA_LC_PASSWORD_FILE", "SPIRA_LC_SOCKET"] {
        let v = get(k);
        if !Path::new(&v).starts_with(stage_root) {
            return Err(format!("isolation check failed: {v} is outside STAGE_ROOT={}", stage_root.display()));
        }
    }

    let db = get("SPIRA_DB");
    let bd = if get("SPIRA_BD").is_empty() { "bd".to_string() } else { get("SPIRA_BD") };

    // One plan bead in the STAGE database. Unparented: Spira works the whole plan backlog,
    // there is no goal epic to hang it under (sp-k6m1m).
    let scope = get("SPIRA_SCOPE_LABEL");
    let labels = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let bead_out = run(
        Command::new(&bd).arg("-C").arg(&db).args(["create", "canary: synthetic pipeline test", "--type", "task", "--labels", &labels, "--silent"]),
        "bd create (bead)",
    )?;
    let bead_id = String::from_utf8_lossy(&bead_out.stdout).trim().to_string();
    if bead_id.is_empty() {
        return Err("could not create plan bead".into());
    }
    log(&format!("bead: {bead_id}"));

    // Its lifecycle row, READY, in the stage's machine: a work bead is the machine's to move.
    let created = run(staged(Command::new("timeout").args(LC_DEADLINE).args(["spira-lc", "create-bead", &bead_id]), env), "spira-lc create-bead")?;
    if !created.status.success() {
        return Err(format!("spira-lc create-bead {bead_id} failed ({}): {}", created.status, String::from_utf8_lossy(&created.stderr).trim()));
    }

    // Run the real sentinel (one pass). SPIRA_SUMMON's fake-summon.sh runs `release
    // canary-worker` synchronously: it claims the bead through the machine, commits, and
    // submits the branch's tip.
    log("running sentinel (one pass)");
    let _ = staged(&mut Command::new("sentinel"), env).status();

    let state = lc_state(env, &bead_id);
    if !spira_config::lc_state::past_builder(state.as_deref().unwrap_or("")) {
        let msg = format!("sentinel pass did not hand the bead on: lifecycle state={}", state.as_deref().unwrap_or("(unreadable)"));
        log(&format!("FAIL: {msg}"));
        file_incident(prod, "canary: pipeline bead not submitted", &format!("The sentinel pass completed but the bead {bead_id}'s lifecycle row is {}.\n\nStage: {}", state.as_deref().unwrap_or("unreadable"), stage_root.display()));
        return Err(msg);
    }
    log(&format!("bead submitted: {bead_id} ({})", state.as_deref().unwrap_or("")));

    // Run the real landing pass.
    log("running landing pass");
    let _ = staged(Command::new("landing-pass").arg("land"), env).status();

    // Assert a commit naming the bead id on origin/main — read from the bare remote
    // directly, no fetch needed, no stale tracking ref.
    let remote = stage_root.join("remote.git");
    let out = run(Command::new("git").arg("-C").arg(&remote).args(["log", "main", "--format=%s", "-n"]).arg(o.verdict_window.to_string()), "git log")?;
    let log_text = String::from_utf8_lossy(&out.stdout);
    let found = log_text.lines().find(|l| l.contains(bead_id.as_str())).map(str::to_string);

    let elapsed = t0.elapsed();
    match found {
        None => {
            let msg = format!("commit naming {bead_id} not found on origin/main after {}s", elapsed.as_secs());
            log(&format!("FAIL: {msg}"));
            let landing_status = std::fs::read_to_string(Path::new(&get("SPIRA_RUN")).join("landing.status")).unwrap_or_else(|_| "(none)".into());
            file_incident(prod, "canary: commit not found on base branch", &format!("Stage canary failed: {msg}\n\nbead_id: {bead_id}\nstage: {}\n\nlanding.status:\n{landing_status}", stage_root.display()));
            Err(msg)
        }
        Some(commit) => {
            log(&format!("PASS — commit '{commit}' on origin/main in {}s", elapsed.as_secs()));
            Ok(CanaryResult { stage_root: stage_root.to_path_buf(), elapsed, commit })
        }
    }
}

/// Every spira-lc call's bound (`timeout` argv): one row read or one event against the
/// stage's own server.
const LC_DEADLINE: &[&str] = &["5"];

/// `cmd` with the stage's env laid over the caller's.
fn staged<'c>(cmd: &'c mut Command, env: &[(String, String)]) -> &'c mut Command {
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

/// The bead's lifecycle state in the stage's machine (`spira-lc show`); `None` when there
/// is no row or the machine cannot be read.
fn lc_state(env: &[(String, String)], id: &str) -> Option<String> {
    let out = staged(Command::new("timeout").args(LC_DEADLINE).args(["spira-lc", "show", id]), env).stdin(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    spira_config::lc_state::parse_show(&String::from_utf8_lossy(&out.stdout)).ok()?.map(|r| r.state)
}

/// `release canary-worker` (was `spira/canary-worker.sh`): the synthetic aeon. Invoked by
/// `fake-summon.sh`, synchronously, inheriting the full stage env (`SPIRA_HOME`,
/// `SPIRA_RUN`, `SPIRA_DB`, `SPIRA_REPO`, `SPIRA_LC_*`). Claims the first ready bead in the
/// canary partition through the stage's lifecycle machine, commits to its branch, pushes, and
/// submits the branch's tip.
pub fn canary_worker() -> Result<(), String> {
    let log = |s: &str| eprintln!("{} canary-worker: {s}", crate::fsutil::now_rfc3339());
    let bd = std::env::var("SPIRA_BD").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "bd".into());
    let db = std::env::var("SPIRA_DB").unwrap_or_default();
    let holder = std::env::var("BEADS_ACTOR").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| CANARY_HOLDER.into());
    let lease_until = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) + CANARY_LEASE_SECS;
    // batch-job: the stage's synthetic aeon claiming its bead, bounded by the canary's own --deadline.
    let mut tools = |prog: &str, args: &[String]| -> (i32, String) {
        match Command::new(prog).args(args).stdin(Stdio::null()).stderr(Stdio::inherit()).output() {
            Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned()),
            Err(e) => (127, format!("{prog}: {e}")),
        }
    };
    let Some(id) = claim_canary_bead(&holder, lease_until, &mut tools)? else {
        log("nothing ready to claim");
        return Ok(());
    };
    log(&format!("claimed {id}"));

    let branch = format!("spira/{id}");
    let repo = std::env::var("SPIRA_REPO").unwrap_or_default();
    if repo.is_empty() {
        log("SPIRA_REPO not set");
        release_bead(&id, &holder);
        return Err("SPIRA_REPO not set".into());
    }
    let checkout = |args: &[&str]| Command::new("git").current_dir(&repo).args(args).status().map(|s| s.success()).unwrap_or(false);
    if !checkout(&["checkout", "-q", "-b", &branch, "origin/main"]) && !checkout(&["checkout", "-q", &branch]) {
        log(&format!("could not create branch {branch}"));
        release_bead(&id, &holder);
        return Err(format!("could not create branch {branch}"));
    }

    std::fs::write(Path::new(&repo).join("canary.txt"), format!("{id}\n")).map_err(|e| format!("cannot write canary.txt: {e}"))?;
    let git_env = |c: &mut Command| {
        c.env("GIT_AUTHOR_NAME", "canary").env("GIT_AUTHOR_EMAIL", "canary@example.invalid").env("GIT_COMMITTER_NAME", "canary").env("GIT_COMMITTER_EMAIL", "canary@example.invalid");
    };
    let mut add = Command::new("git");
    add.current_dir(&repo).args(["add", "canary.txt"]);
    git_env(&mut add);
    run_ok(&mut add, "git add")?;
    let mut commit = Command::new("git");
    commit.current_dir(&repo).args(["commit", "-q", "-m", &format!("feat: {id} — canary synthetic commit")]);
    git_env(&mut commit);
    run_ok(&mut commit, "git commit")?;
    if !Command::new("git").current_dir(&repo).args(["push", "-q", "origin", &branch]).status().map(|s| s.success()).unwrap_or(false) {
        log(&format!("push failed for {branch}"));
        return Err(format!("push failed for {branch}"));
    }
    log(&format!("committed and pushed {branch}"));

    let mut set_state = Command::new(&bd);
    if !db.is_empty() {
        set_state.arg("-C").arg(&db);
    }
    set_state.args(["set-state", &id, &format!("branch={branch}")]);
    let _ = set_state.output();

    // Hand the bead on the way an aeon does: the machine's Submit event with the branch's
    // tip (WORKING -> SUBMITTED), never a bd close — bd status is inert for a work bead.
    let tip = Command::new("git").current_dir(&repo).args(["rev-parse", "HEAD"]).output().map_err(|e| format!("cannot run git rev-parse: {e}"))?;
    let tip = String::from_utf8_lossy(&tip.stdout).trim().to_string();
    let (rc, out) = tools("timeout", &submit_args(&id, &tip, &holder));
    if rc != 0 {
        log(&format!("submit refused for {id} (rc={rc}): {}", out.trim()));
        release_bead(&id, &holder);
        return Err(format!("spira-lc work {id} submit failed (rc={rc})"));
    }
    log(&format!("submitted {id} at {tip}"));
    Ok(())
}

/// `timeout`'s argv for the worker's Submit: `spira-lc work <id> submit --tip <tip> --actor
/// <holder>`, the verb `work submit` sends for an aeon.
pub(crate) fn submit_args(id: &str, tip: &str, holder: &str) -> Vec<String> {
    ["5", "spira-lc", "work", id, "submit", "--tip", tip, "--actor", holder].iter().map(|s| s.to_string()).collect()
}

/// The stage's one fayth (stage.rs `fayth_content`): the worker's ready set is this fayth's.
const CANARY_FAYTH: &str = "canary";
/// The worker's claim holder when the stage sets no `BEADS_ACTOR`.
const CANARY_HOLDER: &str = "canary-worker";
const CANARY_LEASE_SECS: u64 = 3600;
/// lib.sh's `lc_claim_bead`, the aeon's own claim (sp-860zj): the Claim event is the claim.
const LC_CLAIM: &str = r#". "$SPIRA_HOME/lib.sh" || exit 2; lc_claim_bead "$1" "$2" "$3""#;

/// The worker's ready set and claim, through the one ready set an aeon claims from (sp-7g5q6):
/// `spira-claim fayth-ready canary --json` — the machine's READY/REWORK rows — never a
/// `bd ready --claim` of its own. Each candidate in order is claimed the way an aeon claims
/// it: the lifecycle Claim event (`lc_claim_bead`; a refusal means another holder won it, so
/// the next is tried). `Ok(None)` is nothing ready; a ready set that cannot
/// be read is an `Err`, never "nothing ready".
/// Runs a program with its argv, answering (exit code, stdout): the worker's real tools, or a
/// test's script.
pub(crate) type Tools<'a> = dyn FnMut(&str, &[String]) -> (i32, String) + 'a;

pub(crate) fn claim_canary_bead(
    holder: &str,
    lease_until: u64,
    run: &mut Tools,
) -> Result<Option<String>, String> {
    let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    let (rc, out) = run("spira-claim", &own(&["fayth-ready", CANARY_FAYTH, "--json"]));
    if rc != 0 {
        return Err(format!("spira-claim fayth-ready {CANARY_FAYTH} --json failed (rc={rc})"));
    }
    let rows: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("spira-claim fayth-ready {CANARY_FAYTH}: not JSON: {e}"))?;
    let ids: Vec<String> = rows.as_array().into_iter().flatten().filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect();
    for id in ids {
        let rc = run("bash", &own(&["-c", LC_CLAIM, "lc-claim", &id, holder, &lease_until.to_string()])).0;
        if rc == 0 {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Let go of a claim the worker cannot finish: `spira-lc unclaim` (lib.sh
/// `release_own_claim`). Best-effort, as before.
fn release_bead(id: &str, holder: &str) {
    let _ = Command::new("timeout").args(["5", "spira-lc", "unclaim", id, holder]).stdin(Stdio::null()).output();
}

#[cfg(test)]
mod claim_tests {
    use super::*;

    /// A scripted toolbox: answers by program, records every call.
    fn tools<'a>(calls: &'a mut Vec<String>, ready: &'a str, claim_rc: &'a dyn Fn(&str) -> i32) -> impl FnMut(&str, &[String]) -> (i32, String) + 'a {
        move |prog, args| {
            calls.push(format!("{prog} {}", args.join(" ")));
            match prog {
                "spira-claim" => (0, ready.to_string()),
                "bash" => (claim_rc(&args[3]), String::new()),
                _ => (claim_rc(args.iter().find(|a| a.starts_with("sp-")).map(String::as_str).unwrap_or("")), String::new()),
            }
        }
    }

    // sp-7g5q6: the canary worker claimed with its own `bd ready --claim`. Under the machine
    // its ready set is spira-claim's and its claim the lifecycle Claim event; bd is not asked.
    #[test]
    fn the_worker_claims_the_machines_ready_bead_through_lc_claim_bead() {
        let mut calls = Vec::new();
        let refuse_first = |id: &str| if id == "sp-won-elsewhere" { 3 } else { 0 };
        let mut t = tools(&mut calls, r#"[{"id":"sp-won-elsewhere"},{"id":"sp-canary"}]"#, &refuse_first);
        let got = claim_canary_bead("canary-worker", 99, &mut t).unwrap();
        drop(t);
        assert_eq!(got.as_deref(), Some("sp-canary"));
        assert_eq!(calls[0], "spira-claim fayth-ready canary --json");
        assert!(calls[1].starts_with("bash -c ") && calls[1].ends_with("lc-claim sp-won-elsewhere canary-worker 99"), "{calls:?}");
        assert!(calls[2].ends_with("lc-claim sp-canary canary-worker 99"), "{calls:?}");
        assert!(!calls.iter().any(|c| c.starts_with("bd ") || c.contains(" ready ") || c.contains("--claim")), "{calls:?}");
    }

    #[test]
    fn an_empty_set_is_nothing_ready_and_an_unreadable_set_is_an_error() {
        let mut none = |_: &str, _: &[String]| (0, "[]".to_string());
        assert_eq!(claim_canary_bead("h", 1, &mut none).unwrap(), None, "an empty set is nothing ready");
        let mut down = |_: &str, _: &[String]| (1, String::new());
        assert!(claim_canary_bead("h", 1, &mut down).is_err(), "a refused ready set is not nothing ready");
    }
}
