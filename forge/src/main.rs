//! `forge <verb> <repo-dir> [args...]` — see DESIGN.md §3.1. Argv shape matches forge.sh's
//! own `case` dispatch exactly, so every caller (`SPIRA_FORGE`-named, bare, on PATH) needs
//! no change beyond conf.sh's default (`forge.sh` → `forge`).

use forge::cmds::Out;
use forge::ports::{Gh, Proc};
use forge::real::{RealGh, RealProc};
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        eprintln!("forge: usage: forge <verb> <repo-dir> [args...]");
        return ExitCode::from(1);
    }
    let cmd = argv.remove(0);
    let repo = if !argv.is_empty() { argv.remove(0) } else { String::new() };
    let repo = Path::new(&repo);
    let gh = RealGh;
    let proc = RealProc;
    let out = dispatch(&gh, &proc, &cmd, repo, &argv);
    for l in &out.lines {
        println!("{l}");
    }
    ExitCode::from(out.code.clamp(0, 255) as u8)
}

fn arg(args: &[String], i: usize) -> &str {
    args.get(i).map(String::as_str).unwrap_or("")
}

fn stdin_bytes() -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);
    buf
}

fn dispatch(gh: &dyn Gh, proc: &dyn Proc, cmd: &str, repo: &Path, args: &[String]) -> Out {
    use forge::cmds::*;
    match cmd {
        "pr-create" => pr_create(gh, repo, arg(args, 0), arg(args, 1), arg(args, 2), &stdin_bytes()),
        "pr-number" => pr_number(gh, repo, arg(args, 0)),
        "pr-list-queue" => pr_list_queue(gh, repo),
        "pr-list-open" => pr_list_open(gh, repo),
        "pr-mergeability" => pr_mergeability(gh, repo, arg(args, 0)),
        "pr-state" => pr_state(gh, repo, arg(args, 0)),
        "pr-red" => pr_red(gh, repo, arg(args, 0)),
        "pr-automerge" => pr_automerge(gh, repo, arg(args, 0)),
        "check-status" => check_status(gh, proc, repo, arg(args, 1)),
        "run-id" => run_id(gh, repo, arg(args, 0)),
        "runs-for-branch" => runs_for_branch(gh, repo, arg(args, 0)),
        "runs-queue-branches" => runs_queue_branches(gh, repo),
        "batch-ci-status" => batch_ci_status(gh, proc, repo, arg(args, 0)),
        "queued-since" => queued_since(gh, repo, arg(args, 0)),
        "runs-active" => runs_active(gh, repo),
        "stranded-runners" => {
            // SPIRA_STRANDED_RUNNER_MIN_AGE is a registered config key (spira/conf.d) — the
            // one source of config, through `spira_config::process::cfg_parse`, never a
            // competing environment override or a crate-local default (per Ryan
            // 2026-10-05).
            let mut min_age: u64 = match spira_config::process::cfg_parse("SPIRA_STRANDED_RUNNER_MIN_AGE") {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("forge: {e}");
                    return Out { code: 2, lines: vec![] };
                }
            };
            let mut it = args.iter();
            while let Some(a) = it.next() {
                if a == "--min-age" {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        min_age = v;
                    }
                }
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            stranded_runners(gh, repo, now, min_age)
        }
        "run-metadata" => run_metadata(gh, repo, arg(args, 0)),
        "run-cancel" => run_cancel(gh, repo, arg(args, 0)),
        "force-cancel" => force_cancel(gh, repo, arg(args, 0)),
        "workflow-rerun" => workflow_rerun(gh, repo, arg(args, 0)),
        "pr-close" => pr_close(gh, repo, arg(args, 0)),
        "pr-comment" => pr_comment(gh, repo, arg(args, 0), arg(args, 1)),
        "dispatch" => dispatch_cmd(gh, repo, arg(args, 0), arg(args, 1)),
        "fail-lines" => fail_lines(gh, proc, repo, arg(args, 0), arg(args, 1)),
        "branch-protect" => {
            // SPIRA_QUEUE_ACTIONS_APP_ID is a registered config key — the one source of
            // config, never a crate-local literal default on top of it.
            let app_id = match spira_config::process::cfg("SPIRA_QUEUE_ACTIONS_APP_ID") {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("forge: {e}");
                    return Out { code: 2, lines: vec![] };
                }
            };
            branch_protect(gh, repo, arg(args, 0), &app_id)
        }
        "branch-protection-status" => branch_protection_status(gh, repo, arg(args, 0)),
        other => {
            eprintln!("forge: unknown command: {other}");
            Out { code: 1, lines: vec![] }
        }
    }
}

// `dispatch` is also a module-level fn name above; the verb needs its own name to call
// `cmds::dispatch` without shadowing.
fn dispatch_cmd(gh: &dyn Gh, repo: &Path, ref_: &str, suites: &str) -> Out {
    forge::cmds::dispatch(gh, repo, ref_, suites)
}
