use crate::ports::{Bd, Clock, Proc};
use std::io::Write;
use std::process::{Command, Stdio};

/// Shells out to `lib.sh` exactly as `gate-check`'s Rust port already shells out to
/// `repo_root` — see `ports.rs`'s `Bd` doc comment for why this is the right boundary
/// rather than reimplementing `bdq`.
pub struct RealBd {
    pub spira_home: String,
}

const FORGET_SCRIPT: &str = "bdq forget \"$1\" >/dev/null";
const RECALL_SCRIPT: &str =
    "e=$(mktemp) || exit 1; bdq recall \"$1\" 2>\"$e\"; rc=$?; [ $rc -eq 0 ] || cat \"$e\" >&2; rm -f \"$e\"; exit $rc";

impl RealBd {
    /// `body` references its arguments as `"$1"`, `"$2"`, ... — passed as real argv
    /// entries, never interpolated into the script text, so a SOP's own text can never be
    /// read as shell syntax.
    fn seam(&self, body: &str, args: &[&str], stdin: Option<&[u8]>) -> (bool, Vec<u8>) {
        let script = format!(". \"$0\" >/dev/null || {{ echo \"sop: cannot source $0 (set SPIRA_HOME)\" >&2; exit 96; }}\n{body}");
        let mut cmd = spira_config::bounded::bounded("bash");
cmd.envs(spira_config::release_env::child_path_env_for_process());
        cmd.arg("-c").arg(script).arg(format!("{}/lib.sh", self.spira_home)).args(args);
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => return (false, Vec::new()),
        };
        if let Some(bytes) = stdin {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(bytes);
            }
        }
        match child.wait_with_output() {
            Ok(out) => {
                if out.status.code() == Some(96) {
                    eprintln!("sop: cannot source {}/lib.sh (set SPIRA_HOME) - shelf unreadable, NOT empty", self.spira_home);
                }
                (out.status.success(), out.stdout)
            }
            Err(_) => (false, Vec::new()),
        }
    }
}

impl Bd for RealBd {
    fn remember(&self, key: &str, text: &str) -> Result<(), String> {
        // stderr folded into stdout so the failure cause survives; exit status appended.
        let (ok, out) = self.seam(
            "o=$(bdq remember --key \"$1\" \"$2\" 2>&1 >/dev/null); rc=$?; printf '%s\nexit=%s' \"$o\" \"$rc\"; exit $rc",
            &[key, text],
            None,
        );
        if ok {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out).trim().to_string())
        }
    }

    fn recall(&self, key: &str) -> Option<String> {
        let (ok, out) = self.seam(
            RECALL_SCRIPT,
            &[key],
            None,
        );
        if !ok {
            return None;
        }
        let s = String::from_utf8_lossy(&out).into_owned();
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    }

    fn forget(&self, key: &str) -> bool {
        self.seam(FORGET_SCRIPT, &[key], None).0
    }

    fn memories_json(&self) -> Option<String> {
        // Retry with backoff: under host load bd times out transiently. A failed or empty
        // read is None, never an empty shelf (law-a-control-that-cannot-check-must-refuse).
        for attempt in 0..4u64 {
            let (ok, out) = self.seam("bdjson memories 2>/dev/null", &[], None);
            let s = String::from_utf8_lossy(&out).into_owned();
            if ok && !s.trim().is_empty() {
                return Some(s);
            }
            std::thread::sleep(std::time::Duration::from_secs(1 << attempt));
        }
        None
    }

    fn note(&self, bead: &str, text: &str) -> bool {
        self.seam("bdq note \"$1\" --stdin >/dev/null 2>&1", &[bead], Some(text.as_bytes())).0
    }
}

pub struct RealProc;

impl Proc for RealProc {
    fn inventory_scan(&self, text: &str) -> Result<Vec<String>, String> {
        let mut child = spira_config::bounded::bounded("spira-lint")
            .args(["--only", "inventory", "--scan", "/dev/stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "spira-lint is not on PATH — refusing to check operator infrastructure".to_string())?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(text.as_bytes());
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("spira-lint failed to run: {e}"))?;
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok(s.lines().filter(|l| !l.is_empty()).map(String::from).collect())
    }

    fn metric_probe(&self, bin: &str, bash_prefix: bool, subcmd: &str, timeout_secs: u64) -> Option<String> {
        let mut cmd = Command::new("timeout");
        cmd.arg(timeout_secs.to_string());
        if bash_prefix {
            // An override: a test's own fixture script, taking the subcmd directly —
            // unchanged from before cockpit-collect existed.
            cmd.arg("bash").arg(bin).arg(subcmd);
        } else {
            // The default target is cockpit-collect's own `probe` subcommand (sp-kt4l3;
            // formerly bare `cockpit.sh <subcmd>` off PATH).
            cmd.arg(bin).arg("probe").arg(subcmd);
        }
        let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// `tz` is `SPIRA_TZ`'s declared value (spira-config's one door), resolved once at the
/// process's top level and passed down here — never read from the environment in this
/// struct (per Ryan 2026-10-05: one source of config).
pub struct RealClock {
    pub tz: String,
}

impl Clock for RealClock {
    fn now(&self) -> (u64, String) {
        let epoch = spira_config::vtime::now_epoch();
        let iso = fmt_iso(epoch);
        (epoch, iso)
    }

    fn today(&self) -> String {
        let out = spira_config::bounded::bounded("date")
            .env("TZ", &self.tz)
            .args(["+%Y-%m-%d"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => fmt_iso(spira_config::vtime::now_epoch())[..10]
                .to_string(),
        }
    }
}

/// Minimal UTC `YYYY-MM-DDTHH:MM:SSZ` from a unix epoch — no chrono dependency for one
/// conversion; civil-from-days is the standard branch-free algorithm (Howard Hinnant).
fn fmt_iso(epoch: u64) -> String {
    let days = (epoch / 86400) as i64;
    let secs_of_day = epoch % 86400;
    let (h, m, s) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The `synth` OUT-path resolution: `SOP_PAGE` override, else `$SPIRA_WIKI/wiki/notes/...`
/// UNLESS the current directory is itself a worktree of `SPIRA_WIKI` — resolved by
/// git-common-dir, never by path string (a worktree and its origin checkout share one).
pub fn resolve_out_path(sop_page: Option<&str>, spira_wiki: Option<&str>) -> Option<String> {
    if let Some(p) = sop_page {
        if !p.is_empty() {
            return Some(p.to_string());
        }
    }
    let wiki = spira_wiki?;
    if wiki.is_empty() {
        return None;
    }
    let root = wiki_worktree_root(wiki).unwrap_or_else(|| wiki.to_string());
    Some(format!("{root}/wiki/notes/standard-operating-procedures.md"))
}

fn git_out(args: &[&str], cwd: Option<&str>) -> Option<String> {
    let mut cmd = spira_config::bounded::bounded("git");
    if let Some(d) = cwd {
        cmd.arg("-C").arg(d);
    }
    cmd.args(args).stdin(Stdio::null()).stderr(Stdio::null());
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn wiki_worktree_root(spira_wiki: &str) -> Option<String> {
    let cwd_top = git_out(&["rev-parse", "--show-toplevel"], None)?;
    let cwd_git = git_out(&["rev-parse", "--path-format=absolute", "--git-common-dir"], Some(&cwd_top))?;
    let wiki_git = git_out(&["rev-parse", "--path-format=absolute", "--git-common-dir"], Some(spira_wiki))?;
    if cwd_git == wiki_git {
        Some(cwd_top)
    } else {
        None
    }
}

/// Commits exactly the regenerated shelf page, then pulls --rebase and pushes when the
/// repo has an upstream. `Ok(None)` when the page is not in a git repo or did not change.
pub fn commit_page(out_path: &str, subject: &str) -> Result<Option<String>, String> {
    let path = std::path::Path::new(out_path);
    if !path.is_file() {
        return Ok(None);
    }
    let dir = path.parent().and_then(|d| d.to_str()).filter(|d| !d.is_empty()).unwrap_or(".");
    let Some(top) = git_out(&["rev-parse", "--show-toplevel"], Some(dir)) else {
        return Ok(None);
    };
    let run = |args: &[&str]| -> Result<String, String> {
        let out = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&top)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("git {}: {e}", args[0]))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!("git {} failed: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()))
        }
    };
    let abs = std::fs::canonicalize(path).map_err(|e| format!("{out_path}: {e}"))?;
    let abs = abs.to_string_lossy().into_owned();
    if run(&["status", "--porcelain", "--", &abs])?.trim().is_empty() {
        return Ok(None);
    }
    run(&["add", "--", &abs])?;
    run(&["commit", "--only", "-m", subject, "--", &abs])?;
    if run(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_ok() {
        run(&["pull", "--rebase", "--autostash"]).map_err(|e| format!("{e} (page committed locally, not pushed)"))?;
        run(&["push"]).map_err(|e| format!("{e} (page committed locally, not pushed)"))?;
        return Ok(Some(format!("sop: committed and pushed {out_path}")));
    }
    Ok(Some(format!("sop: committed {out_path}; no upstream configured")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_forget_and_recall_surface_bdq_stderr() {
        let dir = testkit::TempDir::new("sop-seam");
        std::fs::write(dir.join("lib.sh"), "bdq() { echo 'dolt timeout: stub detail' >&2; return 1; }\n").unwrap();
        for body in [FORGET_SCRIPT, RECALL_SCRIPT] {
            let out = Command::new("bash")
                .arg("-c")
                .arg(format!(". \"$0\"\n{body}"))
                .arg(dir.join("lib.sh"))
                .arg("k")
                .output()
                .unwrap();
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("dolt timeout: stub detail"), "{body}");
        }
    }

    #[test]
    fn iso_formatting_matches_a_known_instant() {
        // 2026-09-30T00:00:00Z
        assert_eq!(fmt_iso(1790726400), "2026-09-30T00:00:00Z");
    }

    #[test]
    fn iso_formatting_handles_a_mid_day_time() {
        // 2026-09-30T13:45:07Z = 1790726400 + 13*3600 + 45*60 + 7
        assert_eq!(fmt_iso(1790726400 + 13 * 3600 + 45 * 60 + 7), "2026-09-30T13:45:07Z");
    }

}

#[cfg(test)]
mod vtime_tests {
    use super::*;
    use crate::ports::Clock;

    #[test]
    fn real_clock_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, || RealClock { tz: "UTC".into() }.now().0);
        assert_eq!(got, 1_900_000_000);
    }

    fn g(dir: &std::path::Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    #[test]
    fn commit_page_commits_only_the_page_and_leaves_a_clean_page() {
        let t = testkit::TempDir::new("sop-commit-page");
        let repo = t.path().join("brain");
        std::fs::create_dir_all(repo.join("wiki/notes")).unwrap();
        g(&repo, &["init", "-q"]);
        g(&repo, &["config", "user.email", "t@t"]);
        g(&repo, &["config", "user.name", "t"]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let page = repo.join("wiki/notes/standard-operating-procedures.md");
        std::fs::write(&page, "page\n").unwrap();
        std::fs::write(repo.join("stray.txt"), "x").unwrap();
        let before = g(&repo, &["rev-list", "--count", "HEAD"]);
        let r = commit_page(page.to_str().unwrap(), "refactor: regenerate SOP shelf — sop-x").unwrap();
        assert!(r.is_some());
        let after: usize = g(&repo, &["rev-list", "--count", "HEAD"]).parse().unwrap();
        assert_eq!(after, before.parse::<usize>().unwrap() + 1);
        assert_eq!(g(&repo, &["show", "--name-only", "--format=", "HEAD"]), "wiki/notes/standard-operating-procedures.md");
        assert_eq!(g(&repo, &["status", "--porcelain"]), "?? stray.txt");
        assert!(commit_page(page.to_str().unwrap(), "refactor: regenerate SOP shelf — sop-x").unwrap().is_none());
    }

    #[test]
    fn commit_page_fails_loudly_when_the_upload_is_refused() {
        let t = testkit::TempDir::new("sop-commit-page-push");
        let remote = t.path().join("remote.git");
        let repo = t.path().join("brain");
        std::fs::create_dir_all(&repo).unwrap();
        g(t.path(), &["init", "-q", "--bare", remote.to_str().unwrap()]);
        g(&repo, &["init", "-q"]);
        g(&repo, &["config", "user.email", "t@t"]);
        g(&repo, &["config", "user.name", "t"]);
        g(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        g(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        g(&repo, &["push", "-q", "-u", "origin", "HEAD"]);
        let hook = remote.join("hooks/pre-receive");
        testkit::write_exe(&hook, "#!/bin/sh\nexit 1\n");
        let page = repo.join("p.md");
        std::fs::write(&page, "page\n").unwrap();
        let e = commit_page(page.to_str().unwrap(), "refactor: regenerate SOP shelf — sop-x").unwrap_err();
        assert!(e.contains("failed"), "{e}");
    }
}
