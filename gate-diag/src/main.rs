//! `gate-diag [--home <spira-dir>] <results-root>` — see DESIGN.md.

mod engine;
mod ports;
mod real;

use ports::World;
use real::Real;
use std::path::PathBuf;

/// The declared harness home: `SPIRA_HOME`, else `$SPIRA_RELEASE/spira`; neither set is a refusal naming them.
fn default_home() -> Result<PathBuf, String> {
    spira_config::resolve::locate_home_for_process()
}

fn main() {
    let meter = spira_otel::metrics::Run::begin(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "gate-diag", "diagnose");
    let code = real_main(&meter);
    meter.end(spira_otel::metrics::outcome_of(code));
    std::process::exit(code);
}

#[derive(Default)]
struct Tally {
    suites: u64,
    wall_secs: f64,
    red: u64,
    flaky: u64,
    fault: u64,
}

fn push_tally(meter: &spira_otel::metrics::Run, t: &Tally) {
    use spira_otel::metrics::Point;
    meter.sink().push(&[Point::Count { name: "spira_gate_diag_suites", value: t.suites }, Point::Seconds { name: "spira_gate_diag_suites_seconds", value: t.wall_secs }], &[("component", "gate-diag")]);
    for (class, n) in [("real", t.red), ("flaky", t.flaky), ("harness", t.fault)] {
        if n > 0 {
            meter.sink().push(&[Point::Count { name: "spira_gate_diag_reds", value: n }], &[("component", "gate-diag"), ("class", class)]);
        }
    }
}

fn real_main(meter: &spira_otel::metrics::Run) -> i32 {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let mut home = None;
    if argv.first().map(String::as_str) == Some("--home") {
        if argv.len() < 2 {
            eprintln!("gate-diag: --home needs a directory");
            return 1;
        }
        home = Some(PathBuf::from(argv.remove(1)));
        argv.remove(0);
    }
    let Some(root) = argv.first().filter(|r| !r.is_empty()) else {
        eprintln!("gate-diag.sh: 1: usage: gate-diag.sh <results-root>");
        return 1;
    };
    let root = PathBuf::from(root);
    let home = match home.map(Ok).unwrap_or_else(default_home) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("gate-diag: {e}");
            return 1;
        }
    };
    // ONE SOURCE: the config file via spira_config::process::cfg_parse — no env fallback, no
    // literal default (per Ryan 2026-10-05: one source of config).
    let batch_tail_lines = match spira_config::process::cfg_parse::<usize>("SPIRA_BATCH_TAIL_LINES") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gate-diag: {e}");
            return 1;
        }
    };
    let suite_timeout_default = match spira_config::process::cfg_parse::<u64>("SPIRA_SUITE_TIMEOUT") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gate-diag: {e}");
            return 1;
        }
    };
    let world = Real { batch_tail_lines, suite_timeout_default };
    let mut tally = Tally::default();
    let code = run(&world, &home, &root, &mut tally);
    record_tests(&root, spira_config::process::cfg("SPIRA_RUN").ok().filter(|v| !v.is_empty()).map(PathBuf::from));
    push_tally(meter, &tally);
    code
}

/// Appends this round's `results.jsonl` to the `test-run` family as one row per test. Only a
/// caller that names `SPIRA_RUN` has a family to append to; a failed append is said, never fatal.
fn record_tests(root: &std::path::Path, run: Option<PathBuf>) {
    let Some(run) = run else { return };
    let Ok(jsonl) = std::fs::read_to_string(root.join("results.jsonl")) else { return };
    let (ts, host) = (tsd::now_iso(), tsd::default_host());
    let path = tsd::family_path(&run, tsd::testrun::FAMILY);
    let coverage = std::fs::read_to_string(root.join(tsd::testrun::COVERAGE_EXPORT)).unwrap_or_default();
    for row in tsd::testrun::rows_from_results(&jsonl, &coverage, &format!("{ts} {}", root.display()), &ts, &host) {
        if let Err(e) = tsd::append_line(&path, &row) {
            eprintln!("gate-diag: test-run rows not recorded: {e}");
            return;
        }
    }
}

fn run(world: &dyn World, home: &std::path::Path, root: &std::path::Path, tally: &mut Tally) -> i32 {
    let results = world.result_files(root);
    tally.suites = results.len() as u64;

    // results.jsonl and junit.xml — for EVERY suite, unconditionally, before the reds-only
    // report below (DESIGN.md: a reader of the machine-readable results needs the green and
    // skipped rows as much as the red ones).
    let jsonl_path = root.join("results.jsonl");
    world.write(&jsonl_path, "");
    for rf in &results {
        let (status, secs, _rc) = world.read_result(&rf.result_path);
        tally.wall_secs += secs.map_or(0.0, |s| s as f64);
        let fallback = if status.is_empty() { "red".to_string() } else { status };
        let secs_s = secs.map(|s| s.to_string()).unwrap_or_else(|| "0".to_string());
        let src = world.suite_source(home, &rf.suite).unwrap_or_default();
        let out = world.read(&rf.out_path);
        let rows = testenv::tap::jsonl_rows(&rf.suite, &src, out.as_deref(), &fallback, &secs_s);
        world.append(&jsonl_path, &rows);
    }
    if let Some(jsonl) = world.read(&jsonl_path) {
        let rows = engine::parse_tap_rows(&jsonl);
        world.write(&root.join("junit.xml"), &engine::junit_xml(&rows));
    }

    // Collect reds/timeouts, deduplicated on first occurrence — the same order the bash's
    // `case " ${reds} " in *" ${suite} "*` guard preserved. Faults (sp-3azqi: podman's own
    // exec lost the exit status) are collected separately — never a red, but also never
    // silently folded into "all suites passed or skipped" just because this list is empty.
    let mut reds: Vec<String> = Vec::new();
    let mut faulted: Vec<String> = Vec::new();
    for rf in &results {
        let (status, _, _) = world.read_result(&rf.result_path);
        if engine::is_red(&status) && !reds.contains(&rf.suite) {
            reds.push(rf.suite.clone());
        } else if engine::is_fault(&status) && !faulted.contains(&rf.suite) {
            faulted.push(rf.suite.clone());
        }
    }

    if reds.is_empty() && faulted.is_empty() {
        world.print("gate-diag: all suites passed or skipped\n");
        return 0;
    }

    let in_gha = world.github_actions();
    let tail_n = world.batch_tail_lines();
    let default_timeout = world.suite_timeout_default();

    let mut red_list: Vec<String> = Vec::new();
    let mut flaky_list: Vec<String> = Vec::new();
    let mut fault_list: Vec<String> = Vec::new();
    let mut summary_rows: Vec<String> = Vec::new();

    for suite in &reds {
        let rf = results.iter().find(|r| &r.suite == suite);
        let (secs, rc) = match rf {
            Some(rf) => {
                let (_, secs, rc) = world.read_result(&rf.result_path);
                (secs.map(|s| s.to_string()).unwrap_or_else(|| "?".into()), rc.map(|r| r.to_string()).unwrap_or_else(|| "-".into()))
            }
            None => ("?".into(), "-".into()),
        };
        let raw_out = rf.and_then(|rf| world.read(&rf.out_path)).unwrap_or_default();
        let lines = engine::fail_lines(&raw_out);
        let decl_to = engine::declared_timeout(world.suite_source(home, suite).as_deref(), default_timeout);
        let first = engine::first_fail(&raw_out, &lines, &rc, &secs, decl_to);

        let retry = world.retry_status(root, suite);
        let (class, verdict) = engine::classify_retry(retry.as_deref());
        match class {
            engine::RetryClass::Flaky => flaky_list.push(suite.clone()),
            _ => red_list.push(suite.clone()),
        }
        tally.red = red_list.len() as u64;
        tally.flaky = flaky_list.len() as u64;

        let tail = engine::tail_n(&raw_out, tail_n);
        if in_gha {
            let annotation = engine::annotation_text(&first);
            world.print(&engine::render_gha_block(suite, verdict, &rc, &secs, &raw_out, &lines, &tail, tail_n, &annotation));
        } else {
            world.print(&engine::render_plain_block(suite, verdict, &rc, &secs, &raw_out, &lines, &tail, tail_n));
        }

        summary_rows.push(engine::summary_row(suite, &secs, &rc, verdict, &first));
    }

    // Faults get their own block, labeled distinctly from red: no retry classification (a
    // podman infra race is not a flake to serially re-run) and no annotation title that
    // would read as a suite defect.
    for suite in &faulted {
        let rf = results.iter().find(|r| &r.suite == suite);
        let (secs, rc) = match rf {
            Some(rf) => {
                let (_, secs, rc) = world.read_result(&rf.result_path);
                (secs.map(|s| s.to_string()).unwrap_or_else(|| "?".into()), rc.map(|r| r.to_string()).unwrap_or_else(|| "-".into()))
            }
            None => ("?".into(), "-".into()),
        };
        let raw_out = rf.and_then(|rf| world.read(&rf.out_path)).unwrap_or_default();
        let lines = engine::fail_lines(&raw_out);
        let first = format!("(podman lost the exit status at {secs}s — not a suite defect, see spira/gate-retry.sh)");
        fault_list.push(suite.clone());
        tally.fault = fault_list.len() as u64;

        let tail = engine::tail_n(&raw_out, tail_n);
        if in_gha {
            let annotation = engine::annotation_text(&first);
            world.print(&engine::render_gha_block(suite, "fault", &rc, &secs, &raw_out, &lines, &tail, tail_n, &annotation));
        } else {
            world.print(&engine::render_plain_block(suite, "fault", &rc, &secs, &raw_out, &lines, &tail, tail_n));
        }

        summary_rows.push(engine::summary_row(suite, &secs, &rc, "fault", &first));
    }

    world.write(&root.join("red-suites.json"), &engine::red_suites_json(&red_list, &flaky_list, &fault_list));
    world.append(&jsonl_path, &engine::verdict_rows(&red_list, &flaky_list));

    let mut table = String::new();
    table += engine::summary_header();
    table.push('\n');
    for r in &summary_rows {
        table += r;
        table.push('\n');
    }
    world.print("\n");
    world.print(&table);

    if let Some(summary_path) = world.step_summary_path() {
        let mut doc = String::from("## Red suite summary\n\n");
        doc += engine::summary_header();
        doc.push('\n');
        for r in &summary_rows {
            doc += r;
            doc.push('\n');
        }
        world.append(&summary_path, &doc);
    }

    0
}

#[cfg(test)]
mod record_tests_tests {
    use super::record_tests;

    #[test]
    fn a_round_appends_one_test_run_row_per_case_and_none_without_a_run_dir() {
        let tmp = testkit::TempDir::new("gate-diag-rec");
        let d = tmp.path().to_path_buf();
        std::fs::write(
            d.join("results.jsonl"),
            "{\"suite\":\"a.sh\",\"case\":\"x\",\"status\":\"pass\",\"seconds\":1}\n{\"suite\":\"a.sh\",\"case\":\"(verdict)\",\"status\":\"red-red\",\"seconds\":0}\n",
        )
        .unwrap();
        record_tests(&d, None);
        assert!(!d.join("tsd").exists());
        record_tests(&d, Some(d.clone()));
        let text = std::fs::read_to_string(d.join("tsd/test-run.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("\"test\":\"a.sh::x\""), "{text}");
    }

    #[test]
    fn a_round_with_a_per_case_export_writes_the_covered_lines_onto_that_cases_row() {
        let tmp = testkit::TempDir::new("gate-diag-cov");
        let d = tmp.path().to_path_buf();
        std::fs::write(d.join("results.jsonl"), "{\"suite\":\"a.sh\",\"case\":\"x\",\"status\":\"pass\",\"seconds\":1}\n").unwrap();
        std::fs::write(d.join(tsd::testrun::COVERAGE_EXPORT), "{\"suite\":\"a.sh\",\"case\":\"x\",\"covered\":[\"src/b.rs:7\"]}\n").unwrap();
        record_tests(&d, Some(d.clone()));
        let text = std::fs::read_to_string(d.join("tsd/test-run.jsonl")).unwrap();
        assert!(text.contains("\"covered\":[\"src/b.rs:7\"]"), "{text}");
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;
    use crate::ports::ResultFile;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct Fake {
        statuses: HashMap<String, (String, u64, i32)>,
        outputs: HashMap<String, String>,
        retries: HashMap<String, String>,
        printed: RefCell<String>,
        summary: RefCell<String>,
    }

    impl Fake {
        fn suite(mut self, name: &str, status: &str, rc: i32, output: Option<&str>, retry: Option<&str>) -> Fake {
            self.statuses.insert(name.into(), (status.into(), 3, rc));
            if let Some(o) = output {
                self.outputs.insert(name.into(), o.into());
            }
            if let Some(r) = retry {
                self.retries.insert(name.into(), r.into());
            }
            self
        }
        fn names(&self) -> Vec<String> {
            let mut n: Vec<String> = self.statuses.keys().cloned().collect();
            n.sort();
            n
        }
    }

    impl World for Fake {
        fn result_files(&self, _: &Path) -> Vec<ResultFile> {
            self.names()
                .into_iter()
                .map(|s| ResultFile { result_path: PathBuf::from(format!("{s}.result")), out_path: PathBuf::from(format!("{s}.out")), suite: s })
                .collect()
        }
        fn read_result(&self, p: &Path) -> (String, Option<u64>, Option<i32>) {
            let name = p.to_string_lossy().trim_end_matches(".result").to_string();
            let (s, secs, rc) = &self.statuses[&name];
            (s.clone(), Some(*secs), Some(*rc))
        }
        fn read(&self, p: &Path) -> Option<String> {
            self.outputs.get(p.to_string_lossy().trim_end_matches(".out")).cloned()
        }
        fn suite_source(&self, _: &Path, _: &str) -> Option<String> {
            None
        }
        fn retry_status(&self, _: &Path, suite: &str) -> Option<String> {
            self.retries.get(suite).cloned()
        }
        fn write(&self, _: &Path, _: &str) {}
        fn append(&self, p: &Path, content: &str) {
            if p.ends_with("step-summary.md") {
                self.summary.borrow_mut().push_str(content);
            }
        }
        fn github_actions(&self) -> bool {
            true
        }
        fn step_summary_path(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/x/step-summary.md"))
        }
        fn batch_tail_lines(&self) -> usize {
            2
        }
        fn suite_timeout_default(&self) -> u64 {
            600
        }
        fn print(&self, s: &str) {
            self.printed.borrow_mut().push_str(s);
        }
    }

    fn diagnose(w: &Fake) -> i32 {
        run(w, Path::new("/home"), Path::new("/root"), &mut Tally::default())
    }

    // covers: UC-test-infrastructure-36
    #[test]
    fn each_red_suite_is_diagnosed_and_summarised_and_an_all_green_batch_diagnoses_nothing() {
        let w = Fake::default()
            .suite("test-green.sh", "ok", 0, Some("fine\n"), None)
            .suite("test-flaky.sh", "red", 1, Some("setup\n  FAIL  the thing broke\nt1\nt2\nt3\n"), Some("ok"))
            .suite("test-silent.sh", "red", 137, None, Some("red"));
        assert_eq!(diagnose(&w), 0);
        let out = w.printed.borrow().clone();
        assert!(out.contains("::group::test-flaky.sh  red-green (flake)  rc=1"), "{out}");
        assert!(out.contains("--- FAIL lines ---\n  FAIL  the thing broke\n"), "{out}");
        assert!(out.contains("--- last 2 lines ---\nt2\nt3\n"), "{out}");
        assert!(out.contains("::error file=spira/test-flaky.sh::FAIL  the thing broke"), "{out}");
        assert!(out.contains("::group::test-silent.sh  red-red  rc=137"), "{out}");
        assert!(out.contains("(no output)"), "{out}");
        assert!(out.contains("::error file=spira/test-silent.sh::"), "{out}");
        assert!(!out.contains("test-green.sh"), "a passing suite is not diagnosed: {out}");
        let summary = w.summary.borrow().clone();
        assert!(summary.contains("| test-flaky.sh | 3s (rc=1) | red-green (flake) |"), "{summary}");
        assert!(summary.contains("| test-silent.sh | 3s (rc=137) | red-red |"), "{summary}");
        assert!(!summary.contains("test-green.sh"), "the summary lists reds only: {summary}");

        let green = Fake::default().suite("test-a.sh", "ok", 0, None, None).suite("test-b.sh", "skip", 0, None, None);
        assert_eq!(diagnose(&green), 0);
        let out = green.printed.borrow().clone();
        assert!(!out.contains("::group::") && !out.contains("::error"), "{out}");
        assert!(green.summary.borrow().is_empty());
    }
}
