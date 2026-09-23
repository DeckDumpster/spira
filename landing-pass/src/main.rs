// landing-pass: pr-mode repository landing on a short timer, no local gate.
//
// For every repository whose land mode is pr, for every spira/* branch whose bead is
// closed: rebase onto the base ref, run the confine check, force-push with a lease,
// open the pull request if none is open for that tip, record the result.
//
// The pull request's own CI is the gate. gate.sh is never called from this pass.
//
// landing-pass --pass

use serde_json::Value;
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
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) != Some("--pass") {
        eprintln!("usage: landing-pass --pass");
        return ExitCode::from(2);
    }
    match run_pass() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("landing-pass: {}", e);
            ExitCode::FAILURE
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────────────

struct Config {
    spira_home: String,
    spira_run: PathBuf,
    spira_db: String,
    repo_map: Option<PathBuf>,
    pr_pass_branch_sh: String,
    log_path: PathBuf,
    lock_path: PathBuf,
    landstate_dir: PathBuf,
    id_prefix: String,
    now_secs: u64,
}

impl Config {
    fn from_env() -> Config {
        let spira_run_str =
            env::var("SPIRA_RUN").unwrap_or_else(|_| "/tmp/spira".to_string());
        let spira_run = PathBuf::from(&spira_run_str);
        let spira_home = env::var("SPIRA_HOME").unwrap_or_default();
        let now = unix_now();
        Config {
            pr_pass_branch_sh: env::var("SPIRA_PR_PASS_BRANCH_SH")
                .unwrap_or_else(|_| format!("{}/pr-pass-branch.sh", spira_home)),
            log_path: env::var("SPIRA_LANDING_PASS_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|_| spira_run.join("landing-pass.log")),
            lock_path: spira_run.join("landing-pass.lock"),
            landstate_dir: spira_run.join("landstate"),
            repo_map: env::var("SPIRA_REPO_MAP").ok().map(PathBuf::from),
            spira_db: env::var("SPIRA_DB").unwrap_or_default(),
            id_prefix: env::var("SPIRA_ID_PREFIX").unwrap_or_else(|_| "sp".to_string()),
            now_secs: now,
            spira_run,
            spira_home,
        }
    }
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
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

fn log_line(log_path: &Path, msg: &str) {
    let ts = compute_now_iso();
    let line = format!("{} spira: {}\n", ts, msg);
    print!("{}", line);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = f.write_all(line.as_bytes());
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Repo-map parsing
// ──────────────────────────────────────────────────────────────────────────────

struct RepoEntry {
    name: String,
    path: String,
    land: String,
    base: String,
}

fn parse_repo_map(map_path: &Path) -> Vec<RepoEntry> {
    let content = match fs::read_to_string(map_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        let fields: Vec<&str> = t.splitn(8, '|').collect();
        if fields.len() < 3 {
            continue;
        }
        let name = fields[0].trim().to_string();
        let path = fields[1].trim().to_string();
        let land = fields[2].trim().to_string();
        // base is field 4 (index 3) only when NF >= 6
        let base = if fields.len() >= 6 {
            fields[3].trim().to_string()
        } else {
            String::new()
        };
        if name.is_empty() || path.is_empty() {
            continue;
        }
        out.push(RepoEntry { name, path, land, base });
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Git helpers
// ──────────────────────────────────────────────────────────────────────────────

fn git_for_each_ref(repo: &str) -> Vec<(String, String)> {
    let out = Command::new("git")
        .args(["-C", repo, "for-each-ref", "--format=%(refname:short) %(objectname)",
               "refs/heads/spira/*"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let mut refs = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(2, ' ');
        let refname = parts.next().unwrap_or("").to_string();
        let tip = parts.next().unwrap_or("").to_string();
        if !refname.is_empty() && !tip.is_empty() {
            refs.push((refname, tip));
        }
    }
    refs
}

// Resolve the base ref for a repo. Reads the declared base from the repo-map entry first,
// then falls back to asking the remote. The remote AND the base branch both come from here;
// nothing uses the literals "origin" or "main".
fn resolve_base(repo: &str, declared_base: &str) -> Option<String> {
    // 1 — declared in the map and verifiable in this checkout
    if !declared_base.is_empty() {
        let ok = Command::new("git")
            .args(["-C", repo, "rev-parse", "--verify", "-q", declared_base])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(declared_base.to_string());
        }
    }
    // 2 — symbolic ref of origin/HEAD (cached from previous fetch)
    let sym = Command::new("git")
        .args(["-C", repo, "symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(ref r) = sym {
        let ok = Command::new("git")
            .args(["-C", repo, "rev-parse", "--verify", "-q", r])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return sym;
        }
    }
    // 3 — ask the remote, write the symbolic ref so next time is free
    let remotes_out = Command::new("git")
        .args(["-C", repo, "remote"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let remotes: Vec<&str> = remotes_out.lines().collect();
    let remote = if remotes.iter().any(|r| *r == "origin") {
        Some("origin")
    } else if remotes.len() == 1 {
        Some(remotes[0])
    } else {
        None
    };
    if let Some(rem) = remote {
        let _ = Command::new("git")
            .args(["-C", repo, "remote", "set-head", rem, "--auto"])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status();
        let r = Command::new("git")
            .args(["-C", repo, "symbolic-ref", "-q", "--short",
                   &format!("refs/remotes/{}/HEAD", rem)])
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(ref base) = r {
            let ok = Command::new("git")
                .args(["-C", repo, "rev-parse", "--verify", "-q", base])
                .stderr(Stdio::null())
                .stdout(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return r;
            }
        }
    }
    None
}

fn git_fetch(repo: &str, base: &str) {
    // Extract the remote from a ref like "origin/main" -> "origin"
    if let Some(remote) = base.split('/').next().filter(|_| base.contains('/')) {
        let _ = Command::new("git")
            .args(["-C", repo, "fetch", "-q", "--no-write-fetch-head", remote])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status();
    }
}

// content_landed: true if the base already contains every change on the branch.
fn content_landed(repo: &str, branch: &str, base: &str) -> bool {
    // ancestor check first (cheap, local)
    let ahead = Command::new("git")
        .args(["-C", repo, "rev-list", "--count", &format!("{}..{}", base, branch)])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if ahead == 0 {
        return false; // zero commits ahead is not landed
    }
    if Command::new("git")
        .args(["-C", repo, "merge-base", "--is-ancestor", branch, base])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    // merge-tree check: would a merge change the tree?
    let merged = Command::new("git")
        .args(["-C", repo, "merge-tree", "--write-tree", base, branch])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().next().unwrap_or("").to_string())
        .unwrap_or_default();
    if merged.is_empty() {
        return false;
    }
    let basetree = Command::new("git")
        .args(["-C", repo, "rev-parse", &format!("{}^{{tree}}", base)])
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    !basetree.is_empty() && merged.trim() == basetree
}

// ──────────────────────────────────────────────────────────────────────────────
// Holder liveness
// ──────────────────────────────────────────────────────────────────────────────

fn pid_in_proc(pid: &str) -> bool {
    !pid.is_empty() && fs::metadata(format!("/proc/{}", pid.trim())).is_ok()
}

fn hold_alive(pidfile: &Path) -> bool {
    let pid = match fs::read_to_string(pidfile) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return false,
    };
    pid_in_proc(&pid)
}

fn aeon_alive(pidfile: &Path) -> bool {
    let pid = match fs::read_to_string(pidfile) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return false,
    };
    if !pid_in_proc(&pid) {
        return false;
    }
    let cmdline = fs::read_to_string(format!("/proc/{}/cmdline", pid.trim()))
        .unwrap_or_default()
        .replace('\0', " ");
    cmdline.contains("aeon.sh")
}

fn holder_alive(spira_run: &Path, id: &str) -> bool {
    // Check hold pidfiles
    let hold_pf = spira_run.join(format!("hold-{}.pid", id));
    if hold_pf.exists() && hold_alive(&hold_pf) {
        return true;
    }
    // Check aeon pidfiles: aeon-*-<id>.pid
    let entries = match fs::read_dir(spira_run) {
        Ok(e) => e,
        Err(_) => return false,
    };
    let pattern = format!("-{}.pid", id);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("aeon-") && name_str.ends_with(&pattern) {
            if aeon_alive(&entry.path()) {
                return true;
            }
        }
    }
    false
}

// ──────────────────────────────────────────────────────────────────────────────
// Bead status query
// ──────────────────────────────────────────────────────────────────────────────

struct BeadInfo {
    id: String,
    status: String,
    repo: String,
    superseded: bool,
    closed_at: String,
    priority: u64,
}

fn query_beads_json_array(spira_db: &str, spira_home: &str, ids: &[String], home_repo: &str) -> Vec<BeadInfo> {
    let raw_opt = {
        let mut cmd = Command::new("bd");
        cmd.arg("-C").arg(spira_db).arg("show");
        for id in ids {
            cmd.arg(id);
        }
        cmd.arg("--json");
        cmd.env("SPIRA_HOME", spira_home)
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
    };
    let raw = raw_opt.unwrap_or_default();
    let json_str = match raw.find(|c: char| c == '[' || c == '{') {
        Some(i) => &raw[i..],
        None => return Vec::new(),
    };
    let json: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let items: Vec<&Value> = if let Some(a) = json.as_array() {
        a.iter().collect()
    } else {
        vec![&json]
    };
    for item in items {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let labels: Vec<String> = item.get("labels")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect())
            .unwrap_or_default();
        let repo = labels.iter()
            .find(|l| l.starts_with("repo:"))
            .map(|l| l[5..].to_string())
            .unwrap_or_else(|| home_repo.to_string());
        let superseded = item.get("dependencies")
            .and_then(|v| v.as_array())
            .map(|deps| deps.iter().any(|d| {
                let ty = d.get("dependency_type").or_else(|| d.get("type"))
                    .and_then(|v| v.as_str()).unwrap_or("");
                ty == "supersedes"
            }))
            .unwrap_or(false);
        let closed_at = item.get("closed_at")
            .and_then(|v| v.as_str())
            .unwrap_or("9999-99-99")
            .to_string();
        let priority = item.get("priority")
            .and_then(|v| v.as_u64())
            .unwrap_or(9999);
        out.push(BeadInfo { id, status, repo, superseded, closed_at, priority });
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Main pass
// ──────────────────────────────────────────────────────────────────────────────

fn run_pass() -> Result<(), String> {
    let cfg = Config::from_env();

    if cfg.spira_run.join("world.halted").exists() {
        log_line(&cfg.log_path, "landing-pass: skipped — world is halted");
        return Ok(());
    }

    // Non-blocking flock: skip if another pass is already running
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&cfg.lock_path)
        .map_err(|e| format!("open lock {}: {}", cfg.lock_path.display(), e))?;
    if unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        log_line(&cfg.log_path, "landing-pass: already running — skip");
        return Ok(());
    }

    let map_path = match &cfg.repo_map {
        Some(p) => p.clone(),
        None => {
            log_line(&cfg.log_path, "landing-pass: SPIRA_REPO_MAP not set — nothing to do");
            return Ok(());
        }
    };

    let repos = parse_repo_map(&map_path);
    let pr_repos: Vec<&RepoEntry> = repos.iter().filter(|r| r.land == "pr").collect();

    if pr_repos.is_empty() {
        log_line(&cfg.log_path, "landing-pass: no pr-mode repositories in repo-map");
        drop(lock_file);
        return Ok(());
    }

    let home_repo = env::var("SPIRA_HOME_REPO")
        .unwrap_or_else(|_| {
            PathBuf::from(&cfg.spira_home).file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    let mut total_branches = 0u64;
    let mut total_acted = 0u64;

    for repo_entry in &pr_repos {
        let name = &repo_entry.name;
        let repo_path = &repo_entry.path;

        if !std::path::Path::new(repo_path).join(".git").exists()
            && !std::path::Path::new(repo_path).join(".git").is_file()
        {
            log_line(&cfg.log_path,
                &format!("landing-pass {}: {} is not a git checkout — skipped", name, repo_path));
            continue;
        }

        let base = match resolve_base(repo_path, &repo_entry.base) {
            Some(b) => b,
            None => {
                log_line(&cfg.log_path,
                    &format!("landing-pass {}: cannot resolve base ref — skipped. Give it a `base` in repo-map.", name));
                continue;
            }
        };

        git_fetch(repo_path, &base);

        let branches = git_for_each_ref(repo_path);
        if branches.is_empty() {
            continue;
        }
        total_branches += branches.len() as u64;

        // Bead ids are the branch name suffix after "spira/"
        let ids: Vec<String> = branches.iter()
            .map(|(br, _)| br.trim_start_matches("spira/").to_string())
            .collect();

        let mut bead_map = std::collections::HashMap::new();
        for b in query_beads_json_array(&cfg.spira_db, &cfg.spira_home, &ids, &home_repo) {
            bead_map.insert(b.id.clone(), b);
        }

        // Sort: closed oldest-first within priority tier (mirrors landing.sh sort)
        let mut sorted_branches = branches.clone();
        sorted_branches.sort_by(|(a_br, _), (b_br, _)| {
            let a_id = a_br.trim_start_matches("spira/");
            let b_id = b_br.trim_start_matches("spira/");
            let a = bead_map.get(a_id);
            let b = bead_map.get(b_id);
            let a_pri = a.map(|x| x.priority).unwrap_or(9999);
            let b_pri = b.map(|x| x.priority).unwrap_or(9999);
            let a_cat = a.map(|x| x.closed_at.as_str()).unwrap_or("9999-99-99");
            let b_cat = b.map(|x| x.closed_at.as_str()).unwrap_or("9999-99-99");
            a_pri.cmp(&b_pri).then(a_cat.cmp(b_cat))
        });

        for (br, tip) in &sorted_branches {
            let id = br.trim_start_matches("spira/");
            let bead = match bead_map.get(id) {
                Some(b) => b,
                None => {
                    log_line(&cfg.log_path,
                        &format!("landing-pass {}: {} not in bead db — skipped", name, id));
                    continue;
                }
            };

            if bead.status != "closed" {
                if !bead.status.is_empty() {
                    log_line(&cfg.log_path,
                        &format!("landing-pass {}: {} not landed — its bead is {}", name, id, bead.status));
                }
                continue;
            }

            // The bead's repo must match this repository
            if bead.repo != *name && bead.repo != home_repo {
                // Check if bead.repo matches this repo name
                if bead.repo != *name {
                    log_line(&cfg.log_path,
                        &format!("landing-pass {}: {} is in {} but the bead names repo:{} — not landing it here",
                            name, id, name, bead.repo));
                    continue;
                }
            }

            if bead.superseded {
                log_line(&cfg.log_path,
                    &format!("landing-pass {}: {} is superseded — leaving it for the Sending to reap", name, id));
                continue;
            }

            if content_landed(repo_path, br, &base) {
                log_line(&cfg.log_path,
                    &format!("landing-pass {}: {} already contains every change on {} — nothing to land",
                        name, base, br));
                write_landstate(&cfg.landstate_dir, id, "CONTENT", tip, "");
                continue;
            }

            if holder_alive(&cfg.spira_run, id) {
                log_line(&cfg.log_path,
                    &format!("landing-pass {}: a live aeon still holds {} — deferring", name, br));
                continue;
            }

            // Delegate per-branch work to the shell helper
            let rc = run_branch_helper(&cfg, name, repo_path, br, id, &base, tip);
            match rc {
                0 => {
                    total_acted += 1;
                }
                4 => {
                    // bead no longer closed — bead status changed while we worked
                }
                _ => {}
            }
        }
    }

    let elapsed = unix_now().saturating_sub(cfg.now_secs);
    log_line(&cfg.log_path,
        &format!("landing-pass: complete — {} branches seen, {} acted, {}s",
            total_branches, total_acted, elapsed));

    drop(lock_file);
    Ok(())
}

fn run_branch_helper(
    cfg: &Config,
    name: &str,
    repo: &str,
    branch: &str,
    id: &str,
    base: &str,
    tip: &str,
) -> i32 {
    let mut cmd = Command::new("bash");
    cmd.arg(&cfg.pr_pass_branch_sh)
        .arg(repo)
        .arg(branch)
        .arg(id)
        .arg(base)
        .arg(name)
        .arg(tip)
        .env("SPIRA_HOME", &cfg.spira_home)
        .env("SPIRA_RUN", &cfg.spira_run)
        .env("SPIRA_DB", &cfg.spira_db)
        .env("SPIRA_ID_PREFIX", &cfg.id_prefix);

    if let Some(map) = &cfg.repo_map {
        cmd.env("SPIRA_REPO_MAP", map);
    }

    let status = cmd.status().unwrap_or_else(|_| {
        std::process::exit(1);
    });
    status.code().unwrap_or(1)
}

fn write_landstate(landstate_dir: &Path, id: &str, state: &str, tip: &str, reason: &str) {
    let _ = fs::create_dir_all(landstate_dir);
    let tmp = landstate_dir.join(format!("{}.{}", id, std::process::id()));
    // Format matches land_mark in lib.sh: "<state> <tip> <at> [reason]" (no trailing newline).
    let content = if reason.is_empty() {
        format!("{} {} {}", state, tip, unix_now())
    } else {
        format!("{} {} {} {}", state, tip, unix_now(), reason)
    };
    if fs::write(&tmp, &content).is_ok() {
        let _ = fs::rename(&tmp, landstate_dir.join(id));
    }
}
