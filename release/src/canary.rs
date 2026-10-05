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
    if bead_status.as_deref() != Some("closed") {
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
    let scope = std::env::var("SPIRA_SCOPE_LABEL").unwrap_or_default();
    let label = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };

    let mut cmd = Command::new(&bd);
    if !db.is_empty() {
        cmd.arg("-C").arg(&db);
    }
    cmd.args(["ready", "--limit", "0", "--exclude-type", "epic,event", "-u", "--claim", "--label", &label, "--json"]);
    let out = cmd.output().map_err(|e| format!("cannot run bd ready --claim: {e}"))?;
    let id = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| {
            let first = if v.is_array() { v.get(0).cloned() } else { Some(v) };
            first.and_then(|o| o.get("id").and_then(|i| i.as_str()).map(str::to_string))
        })
        .filter(|s| !s.is_empty());
    let Some(id) = id else {
        log("nothing ready to claim");
        return Ok(());
    };
    log(&format!("claimed {id}"));

    let branch = format!("spira/{id}");
    let repo = std::env::var("SPIRA_REPO").unwrap_or_default();
    if repo.is_empty() {
        log("SPIRA_REPO not set");
        let _ = release_bead(&bd, &db, &id);
        return Err("SPIRA_REPO not set".into());
    }
    let checkout = |args: &[&str]| Command::new("git").current_dir(&repo).args(args).status().map(|s| s.success()).unwrap_or(false);
    if !checkout(&["checkout", "-q", "-b", &branch, "origin/main"]) && !checkout(&["checkout", "-q", &branch]) {
        log(&format!("could not create branch {branch}"));
        let _ = release_bead(&bd, &db, &id);
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

fn release_bead(bd: &str, db: &str, id: &str) -> Result<(), String> {
    let mut cmd = Command::new(bd);
    if !db.is_empty() {
        cmd.arg("-C").arg(db);
    }
    cmd.args(["release", id]);
    let _ = cmd.output();
    Ok(())
}
