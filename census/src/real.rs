//! The production [`crate::ports::World`]: the `conf.sh`+`lib.sh` seam for the SQL runners
//! and the fold map (never re-derived — DESIGN.md §2), direct `bd`/`git`/`date` calls, and
//! the unchanged `census/*.py` pipeline run as subprocesses with a scratch temp directory
//! for the two scripts that take file-path arguments (`cluster_merge.py`, `covers.py`,
//! `covers_closed.py`).

use crate::ports::World;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One bd query carries the recorded ids as a literal list, and argv is bounded.
const MAX_RECORDED_IDS: usize = 4000;

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
        let out = spira_config::bounded::bounded("bash").arg("-c").arg(script).arg("--").args(args).envs(envs).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }

    /// SPIRA_LC_BIN (cockpit-collect's and queue-watch's own override) lets a suite pin the
    /// record; nothing sets it in production, where spira-lc is found on PATH.
    fn lc_bin(&self) -> String {
        std::env::var("SPIRA_LC_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira-lc".into())
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
        let out = spira_config::bounded::bounded(bd).arg("-C").arg(db).arg("sql").arg(query).stdin(Stdio::null()).output();
        match out {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string(),
                String::from_utf8_lossy(&o.stderr).trim_end_matches('\n').to_string(),
            ),
            Err(e) => (false, String::new(), e.to_string()),
        }
    }

    /// The same query over the attempt-history facts in the lifecycle event log, answered as
    /// bd's `sql` table so the one parser reads both.
    // batch-job: census report, bounded at 30 s.
    fn run_fact_sql(&self, query: &str) -> (bool, String, String) {
        let out = Command::new("timeout")
            .arg("30")
            .arg(spira_config::lifecycle_row::lc_bin())
            .arg("facts-query")
            .arg(query)
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string(),
                String::from_utf8_lossy(&o.stderr).trim_end_matches('\n').to_string(),
            ),
            Err(e) => (false, String::new(), e.to_string()),
        }
    }

    /// bd's events and the lifecycle facts, one table: the history written before the move
    /// lives in bd, everything since in the lifecycle log. Either leg failing fails the query.
    fn run_events_sql(&self, query: &str) -> (bool, String, String) {
        self.run_both_sql(query, query)
    }

    fn run_both_sql(&self, bd_query: &str, fact_query: &str) -> (bool, String, String) {
        let (bd_ok, bd_out, bd_err) = self.run_bd_sql(bd_query);
        if !bd_ok {
            return (false, bd_out, bd_err);
        }
        let (lc_ok, lc_out, lc_err) = self.run_fact_sql(fact_query);
        if !lc_ok {
            return (false, lc_out, format!("lifecycle facts: {lc_err}"));
        }
        (true, format!("{bd_out}\n{lc_out}"), String::new())
    }

    /// Beads whose reopen cause (a `reopen` fact, or a merge-conflict requeue) is in the
    /// lifecycle log since `since`: what bd's own `reopened` rows cannot see for themselves.
    // batch-job: census report, bounded at 30 s.
    fn recorded_cause_ids(&self, since: Option<i64>) -> Result<Vec<String>, String> {
        let mut cmd = Command::new("timeout");
        cmd.arg("30").arg(spira_config::lifecycle_row::lc_bin()).args(["facts", "--kinds", "reopen,requeued"]);
        if let Some(t) = since.filter(|&t| t > 0) {
            cmd.args(["--since", &t.to_string()]);
        }
        let out = cmd.stdin(Stdio::null()).output().map_err(|e| format!("spira-lc: {e}"))?;
        if !out.status.success() {
            return Err(format!("lifecycle facts: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).map_err(|e| format!("lifecycle facts: {e}"))?;
        let mut ids: Vec<String> = rows
            .iter()
            .filter(|r| {
                let kind = r.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
                let cause = r.get("new_value").and_then(|v| v.as_str()).unwrap_or("");
                kind == "reopen" || (kind == "requeued" && cause == "merge-conflict")
            })
            .filter_map(|r| r.get("issue_id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        ids.sort();
        ids.dedup();
        if ids.len() > MAX_RECORDED_IDS {
            return Err(format!("lifecycle facts: {} beads with a recorded cause is more than one query can carry", ids.len()));
        }
        Ok(ids)
    }

    fn run_py(&self, script: &str, args: &[&Path], stdin: Option<&str>) -> String {
        let mut cmd = spira_config::bounded::bounded("python3");
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

// Registered config keys (spira/conf.d) this crate's generic `World::env` seam also
// serves — $SPIRA_TOML only, never the raw environment, never a fallback (per Ryan
// 2026-10-05). Every other key `env` is called with (SPIRA_NOW, CENSUS_RETRY_DELAY_S) is
// not a registered key and stays a plain environment read.
const RESOLVED_KEYS: &[&str] = &["SPIRA_DB", "SPIRA_BD", "SPIRA_RUN", "SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S"];

impl World for Real {
    fn env(&self, k: &str) -> Option<String> {
        if RESOLVED_KEYS.contains(&k) {
            return spira_config::process::cfg(k).ok();
        }
        std::env::var(k).ok()
    }

    /// Ported in-process (wave 4.35, sp-kelr2, row M): the query itself is `crate::sql`'s
    /// (checked byte-for-byte against the live bash — see the module doc); the retry loop
    /// below is `census_events_run_sql`'s own (3 attempts, `CENSUS_RETRY_DELAY_S` doubling
    /// each retry, default 2s) — the one piece of row M that was never pure SQL text, so it
    /// moves here rather than into `crate::sql`.
    fn census_events_run_sql(&self, since: Option<i64>) -> Result<String, String> {
        let since_formatted = since.filter(|&s| s > 0).map(|s| self.format_epoch_utc(s));
        let causes = self.deliberate_cause_names();
        let fact_query = crate::sql::events_sql(since_formatted.as_deref(), &causes);
        let recorded = self.recorded_cause_ids(since.filter(|&s| s > 0))?;
        let bd_query = crate::sql::events_sql_with(since_formatted.as_deref(), &causes, &recorded);
        let mut delay: u64 = self.env("CENSUS_RETRY_DELAY_S").and_then(|v| v.parse().ok()).unwrap_or(2);
        let mut last_stderr = String::new();
        for attempt in 1..=3 {
            let (ok, out, err) = self.run_both_sql(&bd_query, &fact_query);
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
    fn census_event_rows_run_sql(&self, since: Option<i64>) -> Result<String, String> {
        let since_formatted = since.filter(|&s| s > 0).map(|s| self.format_epoch_utc(s));
        let query = crate::sql::event_rows_sql(since_formatted.as_deref(), &self.deliberate_cause_names());
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
        Err(format!("census_event_rows_run_sql: query failed after 3 attempts: {last_stderr}"))
    }
    fn census_handwritten_run_sql(&self) -> String {
        self.run_events_sql(&crate::sql::handwritten_sql()).1
    }
    fn census_deliberate_run_sql(&self, since: Option<i64>) -> String {
        let since_formatted = since.filter(|&s| s > 0).map(|s| self.format_epoch_utc(s));
        self.run_events_sql(&crate::sql::deliberate_sql(since_formatted.as_deref(), &self.deliberate_cause_names())).1
    }
    fn census_class_fold_map(&self) -> String {
        crate::sql::class_fold_map().trim_end_matches('\n').to_string()
    }
    fn deliberate_cause_names(&self) -> Vec<String> {
        let out = spira_config::bounded::bounded("spira-claim").arg("deliberate-causes").stdin(Stdio::null()).stderr(Stdio::null()).output();
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
    fn lc_landed(&self, id: &str) -> i32 {
        let envs = spira_config::release_env::child_path_env(self.home.parent(), std::env::var("PATH").ok().as_deref());
        let bin = self.lc_bin();
        let out = spira_config::bounded::bounded(bin).args(["state", id]).envs(envs).stdin(Stdio::null()).stderr(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => i32::from(String::from_utf8_lossy(&o.stdout).trim() != "LANDED"),
            // rc 1 is spira-lc's NO_ROW: the record holds no row, so nothing says it landed.
            Ok(o) if o.status.code() == Some(1) => 1,
            _ => 2,
        }
    }

    fn cluster_py(&self, tabular: &str) -> Result<String, String> {
        Ok(self.run_py("cluster.py", &[], Some(tabular)))
    }
    fn cluster_merge_py(&self, all_time: &str, since_wm: &str) -> String {
        let a = self.write_scratch("all_time.txt", all_time);
        let b = self.write_scratch("since_wm.txt", since_wm);
        self.run_py("cluster_cluster_merge.py", &[&a, &b], None)
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

    fn bd_list_all_json(&self, label_pattern: &str) -> String {
        let db = self.env("SPIRA_DB").unwrap_or_default();
        let bd = self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string());
        spira_config::bounded::bounded(bd)
            .arg("-C")
            .arg(db)
            .args(["list", "--all", "--label-pattern", label_pattern, "--limit", "0", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn lc_rows(&self) -> Result<Vec<spira_config::lc_state::Row>, String> {
        let envs = spira_config::release_env::child_path_env(self.home.parent(), std::env::var("PATH").ok().as_deref());
        let bin = self.lc_bin();
        // call-deadline: 5 s, like every other caller of the machine.
        let o = Command::new("timeout")
            .args(["5", &bin, "list"])
            .envs(envs)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run {bin}: {e}"))?;
        if !o.status.success() {
            let why = String::from_utf8_lossy(&o.stderr);
            let why = why.lines().find(|l| !l.trim().is_empty()).unwrap_or("no message").to_string();
            return Err(format!("{bin} list exited {}: {why}", o.status.code().unwrap_or(-1)));
        }
        spira_config::lc_state::parse_rows(&String::from_utf8_lossy(&o.stdout))
    }

    fn git_branch_exists_matching(&self, repo: &str, pattern: &str) -> bool {
        spira_config::bounded::bounded("git")
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
        spira_config::bounded::bounded("date")
            .args(["-u", "+%s"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    }
    fn bd_sql_utc_now_row(&self) -> Result<String, String> {
        let db = self.env("SPIRA_DB").unwrap_or_default();
        let bd = self.env("SPIRA_BD").unwrap_or_else(|| "bd".to_string());
        let out = spira_config::bounded::bounded(bd)
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
        let out = spira_config::bounded::bounded("date").args(["-u", "-d", s, "+%s"]).output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    fn format_epoch_utc(&self, epoch: i64) -> String {
        spira_config::bounded::bounded("date")
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
