use crate::ports::{Bd, Clock, Proc};
use std::io::Write;
use spira_store::lc_call::{LcClient, Reply};
use std::process::{Command, Stdio};

/// The store, reached only through the content door.
pub struct RealBd {
    lc: LcClient,
}

impl RealBd {
    pub fn configured() -> Result<Self, String> {
        Ok(RealBd { lc: LcClient::configured()? })
    }

    pub fn over(lc: LcClient) -> Self {
        RealBd { lc }
    }

    fn content(&self, argv: &[&str], body: Option<&str>) -> Option<Reply> {
        let sent = match body {
            Some(b) => self.lc.call_stdin(argv, b),
            None => self.lc.call(argv),
        };
        match sent {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("sop: {e}");
                None
            }
        }
    }
}

fn json_only(s: &str) -> String {
    match s.lines().position(|l| l.starts_with('[') || l.starts_with('{')) {
        Some(n) => s.lines().skip(n).collect::<Vec<_>>().join("\n"),
        None => String::new(),
    }
}

impl Bd for RealBd {
    fn remember(&self, key: &str, text: &str) -> Result<(), String> {
        match self.content(&["content", "remember", "--key", key], Some(text)) {
            Some(r) if r.code == 0 => Ok(()),
            Some(r) => Err(format!("{}\nexit={}", r.err.trim(), r.code).trim().to_string()),
            None => Err("the content door did not answer".to_string()),
        }
    }

    fn recall(&self, key: &str) -> Option<String> {
        let r = self.content(&["content", "recall", key], None)?;
        if r.code != 0 {
            eprint!("{}", r.err);
            return None;
        }
        (!r.out.trim().is_empty()).then_some(r.out)
    }

    fn forget(&self, key: &str) -> bool {
        self.content(&["content", "forget", key], None).is_some_and(|r| r.code == 0)
    }

    fn memories_json(&self) -> Option<String> {
        // Retry with backoff: under host load the store times out transiently. A failed or empty
        // read is None, never an empty shelf (law-a-control-that-cannot-check-must-refuse).
        for attempt in 0..4u64 {
            if let Some(r) = self.content(&["content", "memories", "--json"], None) {
                let s = json_only(&r.out);
                if r.code == 0 && !s.trim().is_empty() {
                    return Some(s);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1 << attempt));
        }
        None
    }

    fn note(&self, bead: &str, text: &str) -> bool {
        self.content(&["content", "note", bead, "--stdin"], Some(text)).is_some_and(|r| r.code == 0)
    }
}

pub struct RealProc;

impl Proc for RealProc {
    fn inventory_scan(&self, text: &str) -> Result<Vec<String>, String> {
        let mut child = spira_config::bounded::bounded("spira-lint")
            .args(["--only", "inventory", "--scan", system_paths::DEV_STDIN])
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
}

#[cfg(test)]
mod door_tests {
    use super::*;
    use spira_store::lc_call::Transport;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Recorded(Mutex<Vec<(Vec<String>, Option<String>)>>);

    impl Transport for Recorded {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push((argv.to_vec(), stdin.map(String::from)));
            let out = if argv[1] == "memories" { "warning\n{\"sop-a\":\"x\"}" } else { "body" };
            Ok(Reply { code: 0, out: out.into(), err: String::new() })
        }
    }

    #[test]
    fn forget_recall_remember_note_and_memories_are_content_requests() {
        let t = Arc::new(Recorded(Mutex::default()));
        let bd = RealBd::over(LcClient::over(t.clone(), true));
        assert!(bd.forget("sop-a"));
        assert_eq!(bd.recall("sop-a").as_deref(), Some("body"));
        assert!(bd.remember("sop-a", "the text").is_ok());
        assert!(bd.note("sp-1", "a note"));
        assert_eq!(bd.memories_json().as_deref(), Some("{\"sop-a\":\"x\"}"));
        let got = t.0.lock().unwrap().clone();
        let v = |a: &[&str], s: Option<&str>| (a.iter().map(|x| x.to_string()).collect::<Vec<_>>(), s.map(String::from));
        assert_eq!(
            got,
            vec![
                v(&["content", "forget", "sop-a"], None),
                v(&["content", "recall", "sop-a"], None),
                v(&["content", "remember", "--key", "sop-a"], Some("the text")),
                v(&["content", "note", "sp-1", "--stdin"], Some("a note")),
                v(&["content", "memories", "--json"], None),
            ]
        );
    }

    #[test]
    fn a_refused_remember_carries_the_stores_text_and_the_exit() {
        struct Refuses;
        impl Transport for Refuses {
            fn exchange(&self, _a: &[String], _s: Option<&str>, _d: Duration) -> Result<Reply, String> {
                Ok(Reply { code: 3, out: String::new(), err: "refused: no\n".into() })
            }
        }
        let bd = RealBd::over(LcClient::over(Arc::new(Refuses), true));
        assert_eq!(bd.remember("k", "t"), Err("refused: no\nexit=3".to_string()));
        assert!(!bd.forget("k") && !bd.note("sp-1", "n") && bd.recall("k").is_none());
    }
}
