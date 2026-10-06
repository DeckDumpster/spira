//! `release stage up|down` (DESIGN.md "stage"): stand up / tear down a fully isolated
//! Spira for testing — its own git repo and bare remote, its own beads database, its own
//! `SPIRA_RUN`, a minimal chamber, and the synthetic dispatch scripts that replace
//! `systemd-run` at the two harness seams `sentinel` needs (`SPIRA_SUMMON`, `SPIRA_LAUNCH`).
//! Nothing inside a stage touches the caller's `SPIRA_DB`, `SPIRA_RUN`, or any real git
//! repository. Replaces `spira/stage.sh`; `crate::canary` is the caller that most needs it.
//!
//! ISOLATION IS ASSERTED AT BUILD TIME: [`up`] refuses to return an env block where any of
//! `SPIRA_DB`, `SPIRA_RUN` or `SPIRA_HOME` — or the lifecycle credential, data dir or socket —
//! resolves outside the stage root, so a misconfiguration fails closed rather than silently
//! contaminating a real instance.
//!
//! THE LIFECYCLE MACHINE IS THE STAGE'S OWN (sp-880u4): a private Dolt sql-server under
//! `<root>/lc` holding `spira_lifecycle`, built by [`crate::stage_lc`] with the code
//! `spira-install` uses, and the `SPIRA_LC_*` env that sends every `spira-lc` call there.
//!
//! THE TWO HARNESS SEAMS:
//!
//! - `SPIRA_SUMMON` replaces `systemd-run` for sentinel's aeon-summon check. The stage
//!   writes `fake-summon.sh` here; it ignores every systemd flag and execs `release
//!   canary-worker` synchronously in the inherited stage env — the model is the one part
//!   canary cannot test, and this is the boundary.
//! - `SPIRA_LAUNCH` replaces `systemd-run` for sentinel's landing-dispatch check. The stage
//!   writes `fake-launch.sh` here; it records the dispatch timestamp (keeping sentinel's
//!   staleness accounting correct) and exits 0. `release canary` runs `landing-pass land`
//!   directly when it wants a landing pass.

use crate::fsutil;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The stage's own repo map file's basename. Deliberately not spelled the same as the real
/// config file `spira-config` owns (config-fence, DESIGN.md "stage") — `SPIRA_REPO_MAP` is
/// what every consumer actually reads, so the file's own name is otherwise arbitrary.
const STAGE_REPO_MAP_FILENAME: &str = "repo.map";

/// What `up` needs that isn't a pure function of the stage root.
pub struct StageOpts {
    /// An explicit root; `None` means a fresh `mktemp -d`.
    pub root: Option<PathBuf>,
    /// The harness's own `spira/` directory, whose non-test scripts this stage symlinks so
    /// it runs the actual code, never a copy frozen at setup time.
    pub harness_spira: PathBuf,
    /// Resolved path to the `bd-embedded` binary.
    pub bd_embedded: PathBuf,
    /// `TESTDB_SHARED=1` + `TESTDB_BASELINE`: copy that pre-built empty-database snapshot
    /// instead of running `bd-embedded init` (which costs ~10s of schema DDL per stage).
    pub testdb_baseline: Option<PathBuf>,
    /// The label sentinel and canary-worker scope beads to; empty means unrestricted
    /// (mirrors `SPIRA_SCOPE_LABEL`, which stage.sh deliberately leaves empty — the
    /// `spira.conf` default of `spira` would make sentinel query for `spira,plan` while the
    /// bead is created with `plan` only, and `plan_ready` stays 0).
    pub scope_label: String,
}

/// A stood-up stage: its root, and the environment `release canary` (or a caller) must
/// export into every process it launches against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub root: PathBuf,
    /// In the exact order `stage.sh up`'s env block printed them.
    pub env: Vec<(String, String)>,
}

impl Stage {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

fn write_exec(path: &Path, body: &str) -> Result<(), String> {
    fs::write(path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| format!("cannot chmod {}: {e}", path.display()))
}

/// `chamber/canary.fayth`'s content. `FAYTH_MAX_CONCURRENT=1` lets sentinel summon exactly
/// one worker; `FAYTH_LABELS` must include `SPIRA_SCOPE_LABEL` (when non-empty) to pass
/// `fayth_fenced`.
pub fn fayth_content() -> &'static str {
    "FAYTH_NAME=canary\nFAYTH_LABELS=\"${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}\"\nFAYTH_MAX_CONCURRENT=1\n"
}

/// The stage's own repo map file's one line (the file `SPIRA_REPO_MAP` names; not literally
/// named after that env var's own hyphenated spelling — config-fence reserves that spelling
/// for the real one, DESIGN.md "stage"). Six columns, as `doctor.sh` and `landing.sh`
/// require; the repo name must be `repo`'s own basename so `spira_home_repo()` resolves
/// without `SPIRA_HOME_REPO` set. An empty gate column means syntax-check only — right for a
/// synthetic repo.
pub fn repo_map_line(repo: &Path) -> String {
    let name = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    format!("{name} | {} | push | origin/main | |\n", repo.display())
}

const FAKE_SUMMON: &str = "#!/usr/bin/env bash\n# Replaces systemd-run for sentinel's aeon-summon check. Runs the canary worker synchronously.\nexec release canary-worker\n";

const FAKE_LAUNCH: &str = "#!/usr/bin/env bash\n# Replaces systemd-run for sentinel's landing-dispatch check. Records dispatch, exits 0.\n# release canary drives `landing-pass land` directly.\n[ -n \"${SPIRA_RUN:-}\" ] && date +%s > \"$SPIRA_RUN/landing.dispatched\"\nexit 0\n";

/// A fresh, empty directory under the system temp dir, for a `release stage up` with no
/// explicit root — the same job `mktemp -d` did in `stage.sh`, done natively so this crate
/// declares no dependency on it (`spira/deps.toml`; `mktemp` is not one of `deps-lint`'s
/// system-utility exceptions either).
fn mktemp_dir() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let base = std::env::temp_dir();
    for _ in 0..64 {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = base.join(format!("spira-stage-{}-{nanos}-{n}", std::process::id()));
        match fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot create {}: {e}", p.display())),
        }
    }
    Err(format!("could not create a unique temp directory under {}", base.display()))
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let st = cmd.status().map_err(|e| format!("cannot run {what}: {e}"))?;
    if !st.success() {
        return Err(format!("{what} failed ({st})"));
    }
    Ok(())
}

const GIT_LOCATION_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// A git command that cannot be redirected at the caller's repository: an inherited
/// `GIT_DIR` makes a dirless `git init --bare` or `git config` write into that repository's config.
fn git() -> Command {
    let mut c = Command::new("git");
    for v in GIT_LOCATION_VARS {
        c.env_remove(v);
    }
    c
}

/// `release stage up`.
pub fn up(o: &StageOpts) -> Result<Stage, String> {
    let root = match &o.root {
        Some(r) => {
            if r.exists() {
                return Err(format!("up: {} already exists — release stage down it first", r.display()));
            }
            fs::create_dir_all(r).map_err(|e| format!("cannot create {}: {e}", r.display()))?;
            fs::canonicalize(r).map_err(|e| format!("cannot resolve {}: {e}", r.display()))?
        }
        None => mktemp_dir()?,
    };

    // ---- bd-embedded: a private bin dir so `bd` resolves without touching the caller's PATH.
    let bin = root.join("bin");
    fs::create_dir_all(&bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
    std::os::unix::fs::symlink(&o.bd_embedded, bin.join("bd")).map_err(|e| format!("cannot symlink bd: {e}"))?;

    // ---- beads database.
    let db = root.join("db");
    fs::create_dir_all(&db).map_err(|e| format!("cannot create {}: {e}", db.display()))?;
    match &o.testdb_baseline {
        Some(baseline) if baseline.join(".beads").is_dir() => {
            copy_dir(&baseline.join(".beads"), &db.join(".beads")).map_err(|e| {
                let _ = fsutil::remove_tree(&root);
                format!("stage: baseline copy failed: {e}")
            })?;
        }
        _ => {
            let mut cmd = Command::new("bd-embedded");
            cmd.current_dir(&db)
                .env_clear()
                .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
                .env("HOME", std::env::var("HOME").unwrap_or_default())
                .env("TERM", "dumb")
                .env("BD_NON_INTERACTIVE", "1")
                .args(["init", "--non-interactive", "--prefix", "sp", "--skip-agents", "--skip-hooks", "-q"]);
            if let Err(e) = run(&mut cmd, "bd-embedded init") {
                let _ = fsutil::remove_tree(&root);
                return Err(format!("stage: {e}"));
            }
        }
    }

    // ---- git: bare remote + working checkout.
    let remote = root.join("remote.git");
    let repo = root.join("repo");
    run(git().args(["init", "-q", "--bare", "-b", "main"]).arg(&remote), "git init --bare")?;
    run(git().args(["init", "-q", "-b", "main"]).arg(&repo), "git init")?;
    run(
        git()
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "stage")
            .env("GIT_AUTHOR_EMAIL", "stage@example.invalid")
            .env("GIT_COMMITTER_NAME", "stage")
            .env("GIT_COMMITTER_EMAIL", "stage@example.invalid")
            .args(["commit", "-q", "--allow-empty", "-m", "stage: initial"]),
        "git commit",
    )?;
    run(git().current_dir(&repo).args(["remote", "add", "origin"]).arg(&remote), "git remote add")?;
    run(git().current_dir(&repo).args(["push", "-q", "origin", "main"]), "git push")?;
    run(git().current_dir(&repo).args(["fetch", "-q", "origin"]), "git fetch")?;

    // ---- SPIRA_RUN.
    fs::create_dir_all(root.join("run/worktree")).map_err(|e| format!("cannot create run/worktree: {e}"))?;

    // ---- harness directory: SPIRA_HOME = <root>/spira.
    let sh = root.join("spira");
    fs::create_dir_all(sh.join("chamber")).map_err(|e| format!("cannot create chamber: {e}"))?;
    for entry in fs::read_dir(&o.harness_spira).map_err(|e| format!("cannot read {}: {e}", o.harness_spira.display()))?.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if p.is_dir() && name == "conf.d" {
            let link = sh.join(&name);
            let _ = fs::remove_file(&link);
            std::os::unix::fs::symlink(&p, &link).map_err(|e| format!("cannot symlink {name}: {e}"))?;
            continue;
        }
        if !p.is_file() {
            continue;
        }
        let is_script = name.ends_with(".sh") || name.ends_with(".py");
        if !is_script || name.starts_with("test-") || name == "stage.sh" || name == "canary.sh" {
            continue;
        }
        let link = sh.join(&name);
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&p, &link).map_err(|e| format!("cannot symlink {name}: {e}"))?;
    }

    write_exec(&sh.join("fake-summon.sh"), FAKE_SUMMON)?;
    write_exec(&sh.join("fake-launch.sh"), FAKE_LAUNCH)?;

    fs::write(sh.join("chamber/canary.fayth"), fayth_content()).map_err(|e| format!("cannot write canary.fayth: {e}"))?;
    let stage_repomap_path = sh.join(STAGE_REPO_MAP_FILENAME);
    fs::write(&stage_repomap_path, repo_map_line(&repo)).map_err(|e| format!("cannot write the stage's repo map: {e}"))?;

    // ---- lifecycle store (sp-880u4): every claim goes through the lifecycle machine, so the
    // stage gets a machine of its own, built from the release's lifecycle/ the way install
    // builds one. The release root is the parent of its spira/ directory.
    let path_env = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let lifecycle_dir = o.harness_spira.parent().map(|r| r.join("lifecycle")).unwrap_or_default();
    let store = match crate::stage_lc::up(&root, &lifecycle_dir, &path_env) {
        Ok(s) => s,
        Err(e) => {
            let _ = fsutil::remove_tree(&root);
            return Err(format!("stage: lifecycle store: {e}"));
        }
    };
    let lc_env = store.env();

    // ---- isolation check: the stores, the run dir, the harness, and the lifecycle
    // credential and socket every spira-lc call in the stage resolves.
    let lc_paths: Vec<PathBuf> =
        lc_env.iter().filter(|(k, _)| matches!(k.as_str(), "SPIRA_LC_PASSWORD_FILE" | "SPIRA_LC_SOCKET" | "SPIRA_LC_DATA_DIR")).map(|(_, v)| PathBuf::from(v)).collect();
    for p in [&db, &root.join("run"), &sh].into_iter().chain(lc_paths.iter()) {
        if !p.starts_with(&root) {
            crate::stage_lc::stop(&root);
            let _ = fsutil::remove_tree(&root);
            return Err(format!("isolation violated: {} is outside {}", p.display(), root.display()));
        }
    }

    let mut env = vec![
        ("STAGE_ROOT".to_string(), root.display().to_string()),
        ("SPIRA_HOME".to_string(), sh.display().to_string()),
        ("SPIRA_RUN".to_string(), root.join("run").display().to_string()),
        ("SPIRA_DB".to_string(), db.display().to_string()),
        ("SPIRA_BD".to_string(), "bd-embedded".to_string()),
        ("SPIRA_REPO".to_string(), repo.display().to_string()),
        ("SPIRA_REPO_MAP".to_string(), stage_repomap_path.display().to_string()),
        ("SPIRA_FAYTHS".to_string(), "canary".to_string()),
        ("SPIRA_MAX_AEONS".to_string(), "1".to_string()),
        ("SPIRA_NOTIFY".to_string(), "/bin/true".to_string()),
        ("SPIRA_SUMMON".to_string(), sh.join("fake-summon.sh").display().to_string()),
        ("SPIRA_LAUNCH".to_string(), sh.join("fake-launch.sh").display().to_string()),
        ("PATH".to_string(), path_env),
        ("SPIRA_PATH".to_string(), bin.display().to_string()),
        ("SPIRA_SCOPE_LABEL".to_string(), o.scope_label.clone()),
    ];
    env.extend(lc_env);
    // THE ONE SOURCE (per Ryan 2026-10-05): every process in the stage reads config only from
    // the spec SPIRA_TOML names, never the environment. So the stage declares its own values
    // in a layer of its own over the parent's complete spec, and runs everything under that.
    let layer = root.join(spira_config::FILE_NAME);
    let declared: Vec<(String, String)> = env
        .iter()
        .filter(|(k, _)| sh.join("conf.d").join(k).is_file())
        .map(|(k, v)| {
            let path = format!("spira.{}", k.trim_start_matches("SPIRA_").to_ascii_lowercase());
            // A list key is declared as a list (SPIRA_FAYTHS is the one the stage sets).
            let v = if k == "SPIRA_FAYTHS" { serde_json::json!(v.split_whitespace().collect::<Vec<_>>()).to_string() } else { v.clone() };
            (path, v)
        })
        .collect();
    let write = || -> Result<String, String> {
        fs::write(&layer, "[spira]\n").map_err(|e| format!("{}: {e}", layer.display()))?;
        let pairs: Vec<(&str, &str)> = declared.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        spira_config::set_paths_in_file(&layer, &pairs)?;
        Ok(format!("{}:{}", spira_config::process::spec()?, layer.display()))
    };
    match write() {
        Ok(spec) => env.push(("SPIRA_TOML".to_string(), spec)),
        Err(e) => {
            crate::stage_lc::stop(&root);
            let _ = fsutil::remove_tree(&root);
            return Err(format!("stage: cannot declare the stage's own config: {e}"));
        }
    }
    Ok(Stage { root, env })
}

/// `release stage down <root>`. Refuses a path that does not look like a stage (no
/// `spira/chamber/canary.fayth`) — the same safety check `stage.sh down` makes before an
/// `rm -rf`. Stops the stage's lifecycle server first.
pub fn down(root: &Path) -> Result<(), String> {
    let canon = fs::canonicalize(root).map_err(|_| format!("down: {} does not exist", root.display()))?;
    if !canon.join("spira/chamber/canary.fayth").is_file() {
        return Err(format!("down: {} does not look like a stage (no canary.fayth) — refusing rm -rf", canon.display()));
    }
    crate::stage_lc::stop(&canon);
    fsutil::remove_tree(&canon)
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    let st = Command::new("cp").args(["-rp"]).arg(from).arg(to).stdout(Stdio::null()).status().map_err(|e| format!("cannot run cp: {e}"))?;
    if !st.success() {
        return Err(format!("cp -rp {} {} failed ({st})", from.display(), to.display()));
    }
    Ok(())
}

#[cfg(test)]
mod git_env_tests {
    use super::*;

    #[test]
    fn git_command_strips_every_location_var() {
        let c = git();
        let removed: Vec<String> =
            c.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_string_lossy().to_string()).collect();
        for v in GIT_LOCATION_VARS {
            assert!(removed.iter().any(|r| r == v), "{v} not stripped");
        }
    }
}
