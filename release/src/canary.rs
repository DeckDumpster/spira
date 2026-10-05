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
//! claims, commits and closes the bead without invoking Claude. `SPIRA_LAUNCH` records the
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
    fn capture() -> ProdEnv {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        ProdEnv {
            db: get("SPIRA_DB"),
            run: get("SPIRA_RUN"),
            home: get("SPIRA_HOME"),
            notify: get("SPIRA_NOTIFY"),
            bd: get("SPIRA_BD").unwrap_or_else(|| "bd".into()),
            path: std::env::var("PATH").unwrap_or_default(),
            home_repo: get("SPIRA_HOME_REPO"),
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
            for k in ["SPIRA_HOME", "SPIRA_RUN", "SPIRA_DB", "SPIRA_BD", "SPIRA_REPO", "SPIRA_REPO_MAP", "SPIRA_FAYTHS", "SPIRA_MAX_AEONS", "SPIRA_NOTIFY", "SPIRA_SUMMON", "SPIRA_LAUNCH", "PATH", "SPIRA_SCOPE_LABEL"] {
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

    // Isolation check: every stage path resolves under STAGE_ROOT.
    for k in ["SPIRA_DB", "SPIRA_RUN", "SPIRA_HOME"] {
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

    // Run the real sentinel (one pass). SPIRA_SUMMON's fake-summon.sh runs `release
    // canary-worker` synchronously, claiming, committing and closing the bead.
    log("running sentinel (one pass)");
    let mut sentinel_cmd = Command::new("sentinel");
    for (k, v) in env {
        sentinel_cmd.env(k, v);
    }
    let _ = sentinel_cmd.status();

    let bead_status = bd_status(&bd, &db, &bead_id);
    if !spira_config::nonwork::is_closed(spira_config::nonwork::Kind::Canary, bead_status.as_deref().unwrap_or("")) {
        let msg = format!("sentinel pass did not close the bead: status={}", bead_status.as_deref().unwrap_or(""));
        log(&format!("FAIL: {msg}"));
        file_incident(prod, "canary: pipeline bead not closed", &format!("The sentinel pass completed but the bead {bead_id} is {}.\n\nStage: {}", bead_status.as_deref().unwrap_or("?"), stage_root.display()));
        return Err(msg);
    }
    log(&format!("bead closed: {bead_id}"));

    // Run the real landing pass.
    log("running landing pass");
    let mut land_cmd = Command::new("landing-pass");
    for (k, v) in env {
        land_cmd.env(k, v);
    }
    let _ = land_cmd.arg("land").status();

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

fn bd_status(bd: &str, db: &str, id: &str) -> Option<String> {
    let out = Command::new(bd).arg("-C").arg(db).args(["show", id, "--json"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let obj = if v.is_array() { v.get(0)?.clone() } else { v };
    obj.get("status")?.as_str().map(str::to_string)
}

/// `release canary-worker` (was `spira/canary-worker.sh`): the synthetic aeon. Invoked by
/// `fake-summon.sh`, synchronously, inheriting the full stage env (`SPIRA_HOME`,
/// `SPIRA_RUN`, `SPIRA_DB`, `SPIRA_REPO`). Claims the first ready bead in the canary
/// partition, commits to its branch, pushes, and closes it.
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

    let mut close = Command::new(&bd);
    if !db.is_empty() {
        close.arg("-C").arg(&db);
    }
    close.args(["close", &id, "--reason", &format!("canary-worker: committed on {branch}")]);
    let out = close.output().map_err(|e| format!("cannot run bd close: {e}"))?;
    if !out.status.success() {
        log(&format!("close failed for {id}"));
        return Err(format!("close failed for {id}"));
    }
    log(&format!("closed {id}"));
    Ok(())
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
