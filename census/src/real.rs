//! The production [`crate::ports::World`]: the `conf.sh`+`lib.sh` seam for the SQL runners
//! and the fold map (never re-derived — DESIGN.md §2), `bdq`/`git`/`date` calls, and
//! the in-process classification pipeline.

use crate::pipeline;
use crate::ports::World;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One bd query carries the recorded ids as a literal list, and argv is bounded.
const MAX_RECORDED_IDS: usize = 4000;

/// Three attempts, the delay doubling between them; all three failing surfaces the last driver error.
fn retry_sql(caller: &str, mut delay_s: u64, mut attempt: impl FnMut() -> (bool, String, String)) -> Result<String, String> {
    let mut last_stderr = String::new();
    for n in 1..=3 {
        let (ok, out, err) = attempt();
        if ok {
            return Ok(out);
        }
        last_stderr = err;
        if n < 3 {
            std::thread::sleep(std::time::Duration::from_secs(delay_s));
            delay_s *= 2;
        }
    }
    Err(format!("{caller}: query failed after 3 attempts: {last_stderr}"))
}

pub struct Real {
    pub home: PathBuf,   // spira/ — where conf.sh and lib.sh live
    pub bdq: String,
    /// `spira-lc`: `SPIRA_LC_BIN`, else the declared release's.
    pub lc: String,
    /// `PATH`/`SPIRA_RELEASE` every child runs under, from the declared release.
    pub child_env: Vec<(String, String)>,
}

impl Real {
    /// Every tool and the child PATH from the declared release; a refusal names what is missing.
    pub fn new(home: PathBuf) -> Result<Real, String> {
        Ok(Real {
            home,
            bdq: spira_config::release_env::tool("bdq")?,
            lc: spira_store::lifecycle_row::lc_bin()?,
            child_env: spira_config::release_env::child_path_env_for_process()?,
        })
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
        let out = spira_config::bounded::bounded("bash").arg("-c").arg(script).arg("--").args(args).envs(self.child_env.iter().cloned()).stdin(Stdio::null()).stderr(Stdio::null()).output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()).unwrap_or_default()
    }

    /// `bdq sql <query>` — stdout/stderr returned separately (never combined the
    /// way `seam`'s bash-pipe callers were), so a retrying caller can report the LAST
    /// attempt's stderr alone, exactly as `census_events_run_sql`'s own `2>"$_errtmp"`
    /// (truncated fresh each attempt) did.
    fn run_bd_sql(&self, query: &str) -> (bool, String, String) {
        let out = spira_config::bounded::bounded(&self.bdq).arg("sql").arg(query).stdin(Stdio::null()).output();
        match out {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string(),
                String::from_utf8_lossy(&o.stderr).trim_end_matches('\n').to_string(),
            ),
            Err(e) => (false, String::new(), e.to_string()),
        }
    }

    /// The attempt-history facts the folds read, since `since`, from the lifecycle log.
    // batch-job: census report, bounded at 30 s.
    fn fact_rows(&self, since: Option<i64>) -> Result<Vec<crate::facts::FactRow>, String> {
        let mut cmd = Command::new("timeout");
        cmd.arg("30").arg(&self.lc).args(["fact", "list", "--kinds", crate::facts::KINDS]);
        if let Some(t) = since.filter(|&t| t > 0) {
            cmd.args(["--since", &t.to_string()]);
        }
        let out = cmd.stdin(Stdio::null()).output().map_err(|e| format!("spira-lc: {e}"))?;
        if !out.status.success() {
            return Err(format!("lifecycle facts: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        crate::facts::parse(&String::from_utf8_lossy(&out.stdout)).map_err(|e| format!("lifecycle facts: {e}"))
    }

    /// bd's table and the lifecycle facts' table, one text: the history written before the move
    /// lives in bd, everything since in the lifecycle log. Either leg failing fails the query.
    fn run_both_sql(&self, bd_query: &str, facts: impl Fn() -> Result<String, String>) -> (bool, String, String) {
        let (bd_ok, bd_out, bd_err) = self.run_bd_sql(bd_query);
        if !bd_ok {
            return (false, bd_out, bd_err);
        }
        match facts() {
            Ok(lc_out) => (true, format!("{bd_out}\n{lc_out}"), String::new()),
            Err(e) => (false, String::new(), e),
        }
    }

    /// The reopen causes the facts record, for bd's `reopened` rows that cannot see them.
    fn recorded_cause_ids(rows: &[crate::facts::FactRow]) -> Result<Vec<String>, String> {
        let ids = crate::facts::recorded_cause_ids(rows);
        if ids.len() > MAX_RECORDED_IDS {
            return Err(format!("lifecycle facts: {} beads with a recorded cause is more than one query can carry", ids.len()));
        }
        Ok(ids)
    }
}

// Registered config keys (spira/conf.d) this crate's generic `World::env` seam also
// serves — $SPIRA_TOML only, never the raw environment, never a fallback (per Ryan
// 2026-10-05). Every other key `env` is called with (SPIRA_NOW, CENSUS_RETRY_DELAY_S) is
// not a registered key and stays a plain environment read.
const RESOLVED_KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S"];

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
        let since = since.filter(|&s| s > 0);
        let since_formatted = since.map(|s| self.format_epoch_utc(s));
        let causes = self.deliberate_cause_names();
        let rows = self.fact_rows(since)?;
        let bd_query = crate::sql::events_sql_with(since_formatted.as_deref(), &causes, &Self::recorded_cause_ids(&rows)?);
        let delay = self.env("CENSUS_RETRY_DELAY_S").and_then(|v| v.parse().ok()).unwrap_or(2);
        retry_sql("census_events_run_sql", delay, || self.run_both_sql(&bd_query, || Ok(crate::facts::events(&rows, since, &causes))))
    }
    fn census_event_rows_run_sql(&self, since: Option<i64>) -> Result<String, String> {
        let since = since.filter(|&s| s > 0);
        let since_formatted = since.map(|s| self.format_epoch_utc(s));
        let causes = self.deliberate_cause_names();
        let rows = self.fact_rows(since)?;
        let bd_query = crate::sql::event_rows_sql(since_formatted.as_deref(), &causes, &Self::recorded_cause_ids(&rows)?);
        let delay = self.env("CENSUS_RETRY_DELAY_S").and_then(|v| v.parse().ok()).unwrap_or(2);
        retry_sql("census_event_rows_run_sql", delay, || self.run_both_sql(&bd_query, || Ok(crate::facts::event_rows(&rows, since, &causes))))
    }
    fn census_handwritten_run_sql(&self) -> String {
        self.run_both_sql(&crate::sql::handwritten_sql(), || self.fact_rows(None).map(|rows| crate::facts::handwritten(&rows))).1
    }
    fn census_deliberate_run_sql(&self, since: Option<i64>) -> String {
        let since = since.filter(|&s| s > 0);
        let since_formatted = since.map(|s| self.format_epoch_utc(s));
        let causes = self.deliberate_cause_names();
        let bd_query = crate::sql::deliberate_sql(since_formatted.as_deref(), &causes);
        self.run_both_sql(&bd_query, || self.fact_rows(since).map(|rows| crate::facts::deliberate(&rows, since, &causes))).1
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
        let out = spira_config::bounded::bounded(&self.lc).args(["state", id]).envs(self.child_env.iter().cloned()).stdin(Stdio::null()).stderr(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => i32::from(String::from_utf8_lossy(&o.stdout).trim() != "LANDED"),
            // rc 1 is spira-lc's NO_ROW: the record holds no row, so nothing says it landed.
            Ok(o) if o.status.code() == Some(1) => 1,
            _ => 2,
        }
    }

    fn cluster(&self, tabular: &str) -> Result<String, String> {
        let gap = self.env("SPIRA_CENSUS_CLUSTER_GAP_S").and_then(|v| v.trim().parse::<i64>().ok()).filter(|g| *g != 0).unwrap_or(300);
        Ok(pipeline::cluster(tabular, gap))
    }
    fn cluster_merge(&self, all_time: &str, since_wm: &str) -> String {
        pipeline::cluster_merge(all_time, since_wm)
    }
    fn covers(&self, bdq_json: &str, fold_map: &str) -> String {
        pipeline::covers(bdq_json, fold_map)
    }
    fn covers_closed(&self, bdq_json: &str, fold_map: &str) -> String {
        let window = self.env("SPIRA_REMEDY_WINDOW").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(30);
        pipeline::covers_closed(bdq_json, fold_map, window, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
    }
    fn handwritten(&self, tabular: &str) -> String {
        pipeline::handwritten(tabular)
    }
    fn deliberate(&self, tabular: &str) -> String {
        pipeline::deliberate(tabular)
    }

    fn bd_list_all_json(&self, label_pattern: &str) -> String {
        spira_config::bounded::bounded(&self.bdq)
            .args(["list", "--all", "--label-pattern", label_pattern, "--limit", "0", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn lc_rows(&self, ids: &[String]) -> Result<Vec<spira_store::lc_state::Row>, String> {
        let bin = &self.lc;
        let mut rows = Vec::new();
        for chunk in ids.chunks(100) {
            // call-deadline: 5 s, like every other caller of the machine.
            let o = Command::new("timeout")
                .args(["5", bin, "list", "--ids", &chunk.join(",")])
                .envs(self.child_env.iter().cloned())
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("cannot run {bin}: {e}"))?;
            if !o.status.success() {
                let why = String::from_utf8_lossy(&o.stderr);
                let why = why.lines().find(|l| !l.trim().is_empty()).unwrap_or("no message").to_string();
                return Err(format!("{bin} list exited {}: {why}", o.status.code().unwrap_or(-1)));
            }
            rows.extend(spira_store::lc_state::parse_rows(&String::from_utf8_lossy(&o.stdout))?);
        }
        Ok(rows)
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
        let out = spira_config::bounded::bounded(&self.bdq)
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

#[cfg(test)]
mod tests {
    use super::{retry_sql, Real};
    use crate::ports::World;

    fn failing_then_ok(fail_first: usize, calls: &std::cell::Cell<usize>) -> (bool, String, String) {
        calls.set(calls.get() + 1);
        if calls.get() <= fail_first {
            (false, String::new(), "read tcp: i/o timeout".into())
        } else {
            (true, "rows".into(), String::new())
        }
    }

    // covers: UC-ops-detection-remediation-15
    #[test]
    fn the_census_sql_call_succeeds_after_two_transient_failures_in_three_attempts() {
        let calls = std::cell::Cell::new(0);
        assert_eq!(retry_sql("census_events_run_sql", 0, || failing_then_ok(2, &calls)), Ok("rows".into()));
        assert_eq!(calls.get(), 3);
    }

    // covers: UC-ops-detection-remediation-15
    #[test]
    fn the_census_sql_call_surfaces_the_driver_error_when_all_three_attempts_fail() {
        let calls = std::cell::Cell::new(0);
        let err = retry_sql("census_event_rows_run_sql", 0, || failing_then_ok(usize::MAX, &calls)).unwrap_err();
        assert_eq!(calls.get(), 3);
        assert!(err.contains("i/o timeout") && err.contains("census_event_rows_run_sql"), "{err}");
    }

    // covers: UC-ops-detection-remediation-15
    #[test]
    fn a_first_success_is_not_retried() {
        let calls = std::cell::Cell::new(0);
        assert!(retry_sql("c", 0, || failing_then_ok(0, &calls)).is_ok());
        assert_eq!(calls.get(), 1);
    }

    fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
        let p = dir.join(name);
        testkit::write_exe(&p, &format!("#!/bin/sh\n{body}\n"));
        p.to_string_lossy().into_owned()
    }

    fn real_over(dir: &std::path::Path, fact_list_out: &str) -> (Real, std::path::PathBuf) {
        let calls = dir.join("lc-calls");
        let lc = script(dir, "lc", &format!("echo \"$*\" >> '{}'\ncat <<'EOF'\n{fact_list_out}\nEOF", calls.display()));
        let bd = script(dir, "bd-stand-in", "echo '| bd | row |'");
        (Real { home: dir.to_path_buf(), bdq: bd, lc, child_env: Vec::new() }, calls)
    }

    #[test]
    fn the_fact_leg_reads_fact_list_since_the_watermark_and_never_facts_query() {
        let tmp = testkit::TempDir::new("census-argv");
        let dir = tmp.path().to_path_buf();
        let row = r#"[{"issue_id":"sp-a","event_type":"requeued","new_value":"gate-red","actor":"harness","created_at":"2023-11-14T22:13:30Z"}]"#;
        let (real, calls) = real_over(&dir, row);
        let out = real.census_events_run_sql(Some(1_700_000_000)).unwrap();
        let argv = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(argv.trim(), "fact list --kinds requeued,reclaimed,recurred,lapsed,reopen --since 1700000000");
        assert!(out.contains("| bd | row |") && out.contains("| requeued | gate-red | 1 | 1 | 1 |"), "{out}");
    }

    #[test]
    fn a_report_with_no_watermark_reads_the_whole_history_in_one_fetch() {
        let tmp = testkit::TempDir::new("census-whole");
        let dir = tmp.path().to_path_buf();
        let (real, calls) = real_over(&dir, "[]");
        real.census_event_rows_run_sql(None).unwrap();
        real.census_handwritten_run_sql();
        let argv = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(argv.lines().collect::<Vec<_>>(), ["fact list --kinds requeued,reclaimed,recurred,lapsed,reopen"; 2]);
    }
}
