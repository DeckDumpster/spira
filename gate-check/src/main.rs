//! `gate-check [--home <spira-dir>]` — see DESIGN.md.

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
            eprintln!("gate-check: --home needs a directory");
            std::process::exit(1);
        }
        home = Some(PathBuf::from(argv.remove(1)));
        argv.remove(0);
    }
    let home = home.unwrap_or_else(default_home);
    let db = std::env::var("SPIRA_DB").ok().filter(|v| !v.is_empty());
    let world = Real::new(home.clone(), db);
    run(&world, &home);
}

fn run(world: &dyn World, home: &std::path::Path) {
    discover(world);
    let check_out = world.bd_gate_check();
    world.print(&check_out);
    world.print("\n");

    let stuck = engine::stuck_count(&check_out);
    if stuck > 0 {
        world.print(&format!("{stuck} stuck: no await_id — cannot resolve\n"));
    }

    for esc in engine::escalate_lines(&check_out) {
        resolve_escalated(world, esc);
    }

    if let Some(repo) = world.flaky_repo() {
        flaky_beads(world, &repo);
        red_twice_beads(world, &repo);
        tsd_ingest_recent(world, home, &repo);
    }
}

/// STEP 1: DISCOVER — pr-mode repositories only (push merges directly, hold leaves the
/// branch to a human; in both, the landing gate is the only gate and no CI run exists).
fn discover(world: &dyn World) {
    let gate_list = world.bd_gate_list_json();
    let branches = engine::discover_branches(&gate_list);
    if branches.is_empty() {
        return;
    }
    for name in world.repo_names() {
        if world.repo_land(&name) != "pr" {
            continue;
        }
        let Some(repo) = world.repo_root(&name) else { continue };
        if !repo.join(".git").exists() {
            continue;
        }
        for branch in &branches {
            world.bd_gate_discover(&repo, branch);
        }
    }
}

/// STEP 2b: for each `ESCALATE` line, resolve the gate so the bead re-enters the queue and
/// emit `ci.failed`.
fn resolve_escalated(world: &dyn World, esc: &str) {
    let Some(gate_id) = engine::gate_id_in(esc) else { return };
    let show = world.bd_show_json(&gate_id);
    let Some(blocked) = engine::blocked_bead(&show) else { return };
    world.bd_gate_resolve(&gate_id);
    world.spira_event("ci.failed", &blocked, &format!("CI red — {blocked} returned to queue"), esc);
}

/// The lifecycle rows a filing step dedups against; without them the step files nothing this
/// pass rather than risk a duplicate it could not check for.
fn lc_or_skip(world: &dyn World, step: &str) -> Option<engine::Lc> {
    match world.lc_rows() {
        Ok(lc) => Some(lc),
        Err(e) => {
            world.print(&format!("gate-check: {step} skipped — lifecycle state unreadable ({e})"));
            None
        }
    }
}

/// STEP 3: FLAKY SUITE BEADS.
fn flaky_beads(world: &dyn World, repo: &str) {
    let home_repo = world.home_repo();
    let Some(lc) = lc_or_skip(world, "flaky suite beads") else { return };
    for run_id in world.gh_recent_run_ids(repo, 20) {
        for job_id in engine::job_ids(&world.gh_jobs_json(repo, &run_id)) {
            let annotations = world.gh_annotations_json(repo, &job_id.to_string());
            for suite in engine::flaky_suites(&annotations) {
                let title = engine::flaky_title(&suite);
                if engine::has_open_bead(&world.bd_list_json(), &title, &lc) {
                    continue;
                }
                world.file_bead(&title, &home_repo, 2, &engine::flaky_body(&run_id, &suite));
            }
        }
    }
}

/// STEP 4: RED-TWICE SUITE BEADS — only push-to-main runs carry the full corpus, so this is
/// scoped to `--branch main` (a PR run sharing the repo must never trigger it).
fn red_twice_beads(world: &dyn World, repo: &str) {
    let home_repo = world.home_repo();
    let Some(lc) = lc_or_skip(world, "red-twice suite beads") else { return };
    let repo_root = world.repo_root(&home_repo);
    let last_green = world.gh_last_green_main_sha(repo).unwrap_or_default();
    let mut seen: Vec<String> = Vec::new();
    for (run_id, sha) in world.gh_failed_main_runs(repo, 10) {
        for job_id in engine::job_ids(&world.gh_jobs_json(repo, &run_id)) {
            let annotations = world.gh_annotations_json(repo, &job_id.to_string());
            for suite in engine::red_twice_suites(&annotations) {
                if seen.iter().any(|s| s == &suite) {
                    continue;
                }
                seen.push(suite.clone());
                let title = engine::red_twice_title(&suite);
                let list = world.bd_list_json();
                if let Some((id, pri)) = engine::find_open_bead(&list, &title, &lc) {
                    if pri <= 1 {
                        continue;
                    }
                    world.bd_priority(&id, 1);
                    world.bd_note(&id, &format!("Raised to P1: {suite} still red on main (run {run_id}, commit {})", if sha.is_empty() { "?" } else { &sha }));
                    continue;
                }
                let fail_lines = world.gh_fail_lines(repo, &run_id);
                let commits = match (&repo_root, last_green.is_empty(), sha.is_empty(), last_green == sha) {
                    (Some(root), false, false, false) => world.git_log_range(root, &last_green, &sha),
                    _ => String::new(),
                };
                let body = engine::red_twice_body(&suite, &run_id, &sha, &last_green, &fail_lines, &commits);
                world.file_bead(&title, &home_repo, 1, &body);
            }
        }
    }
}

/// STEP 5: TSD INGEST.
fn tsd_ingest_recent(world: &dyn World, home: &std::path::Path, repo: &str) {
    for run_id in world.gh_recent_run_ids(repo, 20) {
        world.tsd_ingest(home, repo, &run_id);
    }
}
