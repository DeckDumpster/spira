//! `gate-diag [--home <spira-dir>] <results-root>` — see DESIGN.md.

mod engine;
mod ports;
mod real;

use ports::World;
use real::Real;
use std::path::PathBuf;

fn default_home() -> PathBuf {
    if let Some(h) = std::env::var_os("SPIRA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().and_then(|d| d.parent()).map(|r| r.join("spira")))
        .unwrap_or_else(|| PathBuf::from("spira"))
}

fn main() {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let mut home = None;
    if argv.first().map(String::as_str) == Some("--home") {
        if argv.len() < 2 {
            eprintln!("gate-diag: --home needs a directory");
            std::process::exit(1);
        }
        home = Some(PathBuf::from(argv.remove(1)));
        argv.remove(0);
    }
    let Some(root) = argv.first().filter(|r| !r.is_empty()) else {
        eprintln!("gate-diag.sh: 1: usage: gate-diag.sh <results-root>");
        std::process::exit(1);
    };
    let root = PathBuf::from(root);
    let home = home.unwrap_or_else(default_home);
    let world = Real;
    std::process::exit(run(&world, &home, &root));
}

fn run(world: &dyn World, home: &std::path::Path, root: &std::path::Path) -> i32 {
    let results = world.result_files(root);

    // results.jsonl and junit.xml — for EVERY suite, unconditionally, before the reds-only
    // report below (DESIGN.md: a reader of the machine-readable results needs the green and
    // skipped rows as much as the red ones).
    let jsonl_path = root.join("results.jsonl");
    world.write(&jsonl_path, "");
    for rf in &results {
        let (status, secs, _rc) = world.read_result(&rf.result_path);
        let fallback = if status.is_empty() { "red".to_string() } else { status };
        let secs_s = secs.map(|s| s.to_string()).unwrap_or_else(|| "0".to_string());
        let rows = world.tap_jsonl_rows(home, &rf.suite, &home.join(&rf.suite), &rf.out_path, &fallback, &secs_s);
        world.append(&jsonl_path, &rows);
    }
    if let Some(jsonl) = world.read(&jsonl_path) {
        let rows = engine::parse_tap_rows(&jsonl);
        world.write(&root.join("junit.xml"), &engine::junit_xml(&rows));
    }

    // Collect reds/timeouts, deduplicated on first occurrence — the same order the bash's
    // `case " ${reds} " in *" ${suite} "*` guard preserved.
    let mut reds: Vec<String> = Vec::new();
    for rf in &results {
        let (status, _, _) = world.read_result(&rf.result_path);
        if engine::is_red(&status) && !reds.contains(&rf.suite) {
            reds.push(rf.suite.clone());
        }
    }

    if reds.is_empty() {
        world.print("gate-diag: all suites passed or skipped\n");
        return 0;
    }

    let in_gha = world.github_actions();
    let tail_n = world.batch_tail_lines();
    let default_timeout = world.suite_timeout_default();

    let mut red_list: Vec<String> = Vec::new();
    let mut flaky_list: Vec<String> = Vec::new();
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

        let tail = engine::tail_n(&raw_out, tail_n);
        if in_gha {
            let annotation = engine::annotation_text(&first);
            world.print(&engine::render_gha_block(suite, verdict, &rc, &secs, &raw_out, &lines, &tail, tail_n, &annotation));
        } else {
            world.print(&engine::render_plain_block(suite, verdict, &rc, &secs, &raw_out, &lines, &tail, tail_n));
        }

        summary_rows.push(engine::summary_row(suite, &secs, &rc, verdict, &first));
    }

    world.write(&root.join("red-suites.json"), &engine::red_suites_json(&red_list, &flaky_list));
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
