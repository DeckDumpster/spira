//! The production [`crate::ports::World`]: the `conf.sh`+`lib.sh` seam for the SQL runners
//! and the fold map (never re-derived — DESIGN.md §2), direct `bd`/`git`/`date` calls, and
//! the unchanged `census/*.py` pipeline run as subprocesses with a scratch temp directory
//! for the two scripts that take file-path arguments (`merge.py`, `covers.py`,
//! `covers_closed.py`).

use crate::ports::World;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Real {
    pub home: PathBuf,   // spira/ — where conf.sh, lib.sh and census/*.py live
    pub scratch: PathBuf, // a process-lifetime scratch dir, removed on drop
}

impl Real {
    pub fn new(home: PathBuf) -> Real {
        let scratch = std::env::temp_dir().join(format!("census-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&scratch);
        Real { home, scratch }
    }

    fn census_py(&self) -> PathBuf {
        self.home.join("census")
    }

    /// Source conf.sh + lib.sh, then run `body` with `args` as `$1`, `$2`, ... Captures
    /// stdout, trimmed of a trailing newline; stderr discarded.
    ///
    /// THE "--" IS NOT DECORATION. `bash -c script arg0 arg1 arg2` assigns the FIRST
    /// argument after the script string to `$0` (bash's own command-name slot), and only
    /// the REST become `$1`, `$2`, ... Without a placeholder there, `args[0]` silently
    /// lands in `$0` and every real argument shifts down by one — `body`'s own `"$1"`
    /// reads what should have been `$2`, and the true `$1` is simply gone. Caught live by
    /// testenv's test-census.sh: the since-watermark argument landed in `$0`, so
    /// `census_events_run_sql "$1"` always ran unfiltered regardless of the real
    /// watermark (sp-yyk47). `skew`'s own seam avoids this by spending the `$0` slot on
    /// `lib.sh`'s own path (its `. "$0"` sourcing trick); `--` is the same fix without
    /// that trick.
    fn seam(&self, body: &str, args: &[&str]) -> String {
        let script = format!(
            ". \"{}/conf.sh\" >/dev/null 2>&1; . \"{}/lib.sh\" >/dev/null 2>&1; {body}",
            self.home.display(),
            self.home.display()
        );
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): `self.home` is always
        // `<release>/spira` (resolve_home's own contract), so its parent is this binary's
        // own release root.
        let envs = spira_config::release_env::child_path_env(self.home.parent(), std::env::var("PATH").ok().as_deref());
        let out = Command::new("bash").arg("-c").arg(script).arg("--").args(args).envs(envs).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }

    fn write_scratch(&self, name: &str, content: &str) -> PathBuf {
        let p = self.scratch.join(name);
        let _ = std::fs::write(&p, content);
        p
    }

    /// `<bd> -C <db> sql <query>` — stdout/stderr returned separately (never combined the
    /// way `seam`'s bash-pipe callers were), so a retrying caller can report the LAST
    /// attempt's stderr alone, exactly as `census_events_run_sql`'s own `2>"$_errtmp"`
    /// (truncated fresh each attempt) did.
    fn run_bd_sql(&self, query: &str) -> (bool, String, String) {
        let db = self.env("SPIRA_DB").unwrap_or_default();
        let bd = self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string());
        let out = Command::new(bd).arg("-C").arg(db).arg("sql").arg(query).stdin(Stdio::null()).output();
        match out {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string(),
                String::from_utf8_lossy(&o.stderr).trim_end_matches('\n').to_string(),
            ),
            Err(e) => (false, String::new(), e.to_string()),
        }
    }

    fn run_py(&self, script: &str, args: &[&Path], stdin: Option<&str>) -> String {
        let mut cmd = Command::new("python3");
        cmd.arg(self.census_py().join(script));
        for a in args {
            cmd.arg(a);
        }
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        if let (Some(s), Some(mut si)) = (stdin, child.stdin.take()) {
            use std::io::Write;
            let _ = si.write_all(s.as_bytes());
        }
        child.wait_with_output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }
}

impl Drop for Real {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

impl World for Real {
    fn env(&self, k: &str) -> Option<String> {
        std::env::var(k).ok()
    }

    /// Ported in-process (wave 4.35, sp-kelr2, row M): the query itself is `crate::sql`'s
    /// (checked byte-for-byte against the live bash — see the module doc); the retry loop
    /// below is `census_events_run_sql`'s own (3 attempts, `CENSUS_RETRY_DELAY_S` doubling
    /// each retry, default 2s) — the one piece of row M that was never pure SQL text, so it
    /// moves here rather than into `crate::sql`.
    fn census_events_run_sql(&self, since: Option<i64>) -> Result<String, String> {
        let since_formatted = since.filter(|&s| s > 0).map(|s| self.format_epoch_utc(s));
        let query = crate::sql::events_sql(since_formatted.as_deref(), &self.deliberate_cause_names());
        let mut delay: u64 = self.env("CENSUS_RETRY_DELAY_S").and_then(|v| v.parse().ok()).unwrap_or(2);
        let mut last_stderr = String::new();
        for attempt in 1..=3 {
            let (ok, out, err) = self.run_bd_sql(&query);
            if ok {
                return Ok(out);
            }
            last_stderr = err;
            if attempt < 3 {
                std::thread::sleep(std::time::Duration::from_secs(delay));
                delay *= 2;
            }
        }
        Err(format!("census_events_run_sql: query failed after 3 attempts: {last_stderr}"))
    }
    fn census_handwritten_run_sql(&self) -> String {
        self.run_bd_sql(&crate::sql::handwritten_sql()).1
    }
    fn census_deliberate_run_sql(&self, since: Option<i64>) -> String {
        let since_formatted = since.filter(|&s| s > 0).map(|s| self.format_epoch_utc(s));
        self.run_bd_sql(&crate::sql::deliberate_sql(since_formatted.as_deref(), &self.deliberate_cause_names())).1
    }
    fn census_class_fold_map(&self) -> String {
        crate::sql::class_fold_map().trim_end_matches('\n').to_string()
    }
    fn deliberate_cause_names(&self) -> Vec<String> {
        let out = Command::new("spira-claim").arg("deliberate-causes").stdin(Stdio::null()).stderr(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()
            }
            _ => Vec::new(),
        }
    }
    fn repo_root(&self) -> Option<String> {
        let out = self.seam("repo_root 2>/dev/null", &[]);
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
    fn landed(&self, id: &str) -> i32 {
        let envs = spira_config::release_env::child_path_env(self.home.parent(), std::env::var("PATH").ok().as_deref());
        let out = Command::new("spira-lc").args(["state", id]).envs(envs).stdin(Stdio::null()).stderr(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => i32::from(String::from_utf8_lossy(&o.stdout).trim() != "LANDED"),
            _ => 2,
        }
    }

    fn count_py(&self, tabular: &str) -> Result<String, String> {
        Ok(self.run_py("count.py", &[], Some(tabular)))
    }
    fn merge_py(&self, all_time: &str, since_wm: &str) -> String {
        let a = self.write_scratch("all_time.txt", all_time);
        let b = self.write_scratch("since_wm.txt", since_wm);
        self.run_py("merge.py", &[&a, &b], None)
    }
    fn covers_py(&self, bdq_json: &str, fold_map: &str) -> String {
        let f = self.write_scratch("fold_map.txt", fold_map);
        self.run_py("covers.py", &[&f], Some(bdq_json))
    }
    fn covers_closed_py(&self, bdq_json: &str, fold_map: &str) -> String {
        let f = self.write_scratch("fold_map.txt", fold_map);
        self.run_py("covers_closed.py", &[&f], Some(bdq_json))
    }
    fn handwritten_py(&self, tabular: &str) -> String {
        self.run_py("handwritten.py", &[], Some(tabular))
    }
    fn deliberate_py(&self, tabular: &str) -> String {
        self.run_py("deliberate.py", &[], Some(tabular))
    }

    fn bd_list_json(&self, status: &str, label_pattern: &str) -> String {
        let db = self.env("SPIRA_DB").unwrap_or_default();
        let bd = self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string());
        Command::new(bd)
            .arg("-C")
            .arg(db)
            .args(["list", "--status", status, "--label-pattern", label_pattern, "--limit", "0", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn git_branch_exists_matching(&self, repo: &str, pattern: &str) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["branch", "-a", "--list", pattern])
            .stdin(Stdio::null())
            .output()
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false)
    }

    fn host_utc_epoch(&self) -> i64 {
        if let Some(n) = self.env("SPIRA_NOW").and_then(|v| v.parse().ok()) {
            return n;
        }
        Command::new("date")
            .args(["-u", "+%s"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    }
    fn bd_sql_utc_now_row(&self) -> Result<String, String> {
        let db = self.env("SPIRA_DB").unwrap_or_default();
        let bd = self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string());
        let out = Command::new(bd)
            .arg("-C")
            .arg(db)
            .args(["sql", "SELECT DATE_FORMAT(UTC_TIMESTAMP(), '%Y-%m-%d %H:%i:%s') AS utc_fn"])
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) => {
                let combined = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                if o.status.success() {
                    Ok(combined.lines().nth(2).unwrap_or("").to_string())
                } else {
                    Err(combined)
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }
    fn parse_utc_to_epoch(&self, s: &str) -> Option<i64> {
        if s.is_empty() {
            return None;
        }
        let out = Command::new("date").args(["-u", "-d", s, "+%s"]).output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    fn format_epoch_utc(&self, epoch: i64) -> String {
        Command::new("date")
            .args(["-u", "-d", &format!("@{epoch}"), "+%Y-%m-%d %H:%M:%S"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    fn read_to_string(&self, p: &Path) -> Option<String> {
        std::fs::read_to_string(p).ok()
    }

    fn out(&self, s: &str) {
        println!("{s}");
    }
    fn err(&self, s: &str) {
        eprintln!("{s}");
    }
}
