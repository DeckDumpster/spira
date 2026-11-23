//! `forge` as a library: a caller runs a verb in its own process and reads the answer the
//! binary would have printed — its exit code, its stdout and its stderr — with `gh` as the only
//! child. A caller holding a configured forge that is not the default (`forge`, by name) holds a
//! fixture's stand-in, and runs that program instead, so a suite's fake never reaches GitHub.

use crate::cmds::Out;
use crate::ports::{Gh, Proc};
use crate::real::{RealGh, RealProc};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The configured forge that means this library (spira/conf.d/SPIRA_FORGE's default).
pub const IN_PROCESS: &str = "forge";

/// One verb's answer, as the binary would have given it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Reply {
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    /// Stdout when the verb succeeded.
    pub fn stdout(&self) -> Option<String> {
        self.ok().then(|| self.out.clone())
    }

    fn of(out: Out, err: Vec<String>) -> Reply {
        Reply { code: out.code, out: out.lines.iter().map(|l| format!("{l}\n")).collect(), err: err.iter().map(|l| format!("{l}\n")).collect() }
    }
}

/// Where a configured forge's calls go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    InProcess,
    /// A fixture's stand-in program.
    Program(PathBuf),
}

pub fn route_of(configured: &Path) -> Route {
    let c = configured.to_string_lossy();
    if c.trim().is_empty() || c == IN_PROCESS {
        Route::InProcess
    } else {
        Route::Program(configured.to_path_buf())
    }
}

/// The seam a caller holds: one verb, answered.
pub trait Forge: Send + Sync {
    fn call(&self, cmd: &str, repo: &Path, args: &[&str], input: Option<&[u8]>) -> Reply;
}

/// The configured forge as a [`Forge`].
pub struct Configured {
    pub route: Route,
}

impl Configured {
    pub fn of(configured: &Path) -> Self {
        Configured { route: route_of(configured) }
    }
}

impl Forge for Configured {
    fn call(&self, cmd: &str, repo: &Path, args: &[&str], input: Option<&[u8]>) -> Reply {
        run(&self.route, cmd, repo, args, input)
    }
}

/// One verb on `route`.
pub fn run(route: &Route, cmd: &str, repo: &Path, args: &[&str], input: Option<&[u8]>) -> Reply {
    match route {
        Route::InProcess => call(cmd, repo, args, input),
        Route::Program(bin) => program(bin, cmd, repo, args, input),
    }
}

/// One verb in this process, counted as the binary counts a run.
pub fn call(cmd: &str, repo: &Path, args: &[&str], input: Option<&[u8]>) -> Reply {
    let mut reply = Reply::default();
    crate::telemetry::run(&crate::telemetry::sink(), Some(cmd), || {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let _ = crate::diag::take();
        let out = dispatch(&RealGh, &RealProc, cmd, repo, &args, input);
        reply = Reply::of(out, crate::diag::take());
        reply.code
    });
    reply
}

// FORK-EDGE: fixture stand-in — a declared SPIRA_FORGE script (suites only; production holds `forge`)
// batch-job: the stand-in fakes the whole forge binary, so it runs as long as the verb it fakes (a push, a PR create)
fn program(bin: &Path, cmd: &str, repo: &Path, args: &[&str], input: Option<&[u8]>) -> Reply {
    let mut c = Command::new(bin);
    c.arg(cmd).arg(repo).args(args);
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(e) => return Reply { code: 127, out: String::new(), err: format!("could not run the forge {}: {e}\n", bin.display()) },
    };
    if let (Some(bytes), Some(mut si)) = (input, child.stdin.take()) {
        let _ = si.write_all(bytes);
    }
    match child.wait_with_output() {
        Ok(o) => Reply { code: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() },
        Err(e) => Reply { code: -1, out: String::new(), err: format!("forge wait failed: {e}\n") },
    }
}

fn arg(args: &[String], i: usize) -> &str {
    args.get(i).map(String::as_str).unwrap_or("")
}

fn refused(e: String) -> Out {
    warn!("forge: {e}");
    Out { code: 2, lines: vec![] }
}

/// The verb table: `forge <cmd> <repo> [args...]`, with `input` as the verb's stdin.
pub fn dispatch(gh: &dyn Gh, proc: &dyn Proc, cmd: &str, repo: &Path, args: &[String], input: Option<&[u8]>) -> Out {
    use crate::cmds::*;
    let workflow = match workflow_of(repo) {
        Ok(w) => w,
        Err(e) => return refused(e),
    };
    let workflow = workflow.as_str();
    let second_pr = std::env::var("QUEUE_SECOND_PR_CONSIDERED").unwrap_or_default();
    match cmd {
        "pr-create" => pr_create(gh, repo, arg(args, 0), arg(args, 1), arg(args, 2), input.unwrap_or_default(), &second_pr),
        "pr-preflight" => pr_preflight(gh, repo, arg(args, 0), &second_pr),
        "pr-number" => pr_number(gh, repo, arg(args, 0)),
        "pr-list-queue" => pr_list_queue(gh, repo),
        "pr-list-open" => pr_list_open(gh, repo),
        "pr-mergeability" => pr_mergeability(gh, repo, arg(args, 0)),
        "pr-state" => pr_state(gh, repo, arg(args, 0)),
        "pr-checks" => pr_checks(gh, repo, arg(args, 0)),
        "pr-merge-commit" => pr_merge_commit(gh, repo, arg(args, 0)),
        "pr-red" => pr_red(gh, repo, arg(args, 0)),
        "pr-automerge" => pr_automerge(gh, repo, arg(args, 0), arg(args, 1)),
        "pr-arm-queue" => pr_arm_queue(gh, repo, arg(args, 0)),
        "check-status" => check_status(gh, proc, repo, workflow, arg(args, 1)),
        "run-id" => run_id(gh, repo, workflow, arg(args, 0)),
        "runs-for-branch" => runs_for_branch(gh, repo, workflow, arg(args, 0)),
        "runs-queue-branches" => runs_queue_branches(gh, repo, workflow),
        "batch-ci-status" => batch_ci_status(gh, proc, repo, workflow, arg(args, 0)),
        "queued-since" => queued_since(gh, repo, arg(args, 0)),
        "runs-active" => runs_active(gh, repo),
        "stranded-runners" => {
            // SPIRA_STRANDED_RUNNER_MIN_AGE is a registered config key (spira/conf.d) — the
            // one source of config, through `spira_config::process::cfg_parse`, never a
            // competing environment override or a crate-local default (per Ryan
            // 2026-10-05).
            let mut min_age: u64 = match spira_config::process::cfg_parse("SPIRA_STRANDED_RUNNER_MIN_AGE") {
                Ok(v) => v,
                Err(e) => return refused(e),
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
        "dispatch" => crate::cmds::dispatch(gh, repo, workflow, arg(args, 0), arg(args, 1)),
        "ami-runs" => ami_runs(gh, repo),
        "ami-result" => ami_result(gh, repo, arg(args, 0)),
        "ami-log" => ami_log(gh, repo, arg(args, 0), arg(args, 1).parse().unwrap_or(40)),
        "ami-dispatch" => ami_dispatch(gh, repo, arg(args, 0)),
        "fail-lines" => fail_lines(gh, proc, repo, arg(args, 0), arg(args, 1)),
        "branch-protect" => {
            // SPIRA_QUEUE_ACTIONS_APP_ID is a registered config key — the one source of
            // config, never a crate-local literal default on top of it.
            let app_id = match spira_config::process::cfg("SPIRA_QUEUE_ACTIONS_APP_ID") {
                Ok(v) => v,
                Err(e) => return refused(e),
            };
            branch_protect(gh, repo, arg(args, 0), &app_id)
        }
        "branch-protection-status" => branch_protection_status(gh, repo, arg(args, 0)),
        other => {
            warn!("forge: unknown command: {other}");
            Out { code: 1, lines: vec![] }
        }
    }
}

fn workflow_of(repo: &Path) -> Result<String, String> {
    let default = spira_config::process::cfg("SPIRA_FORGE_WORKFLOW")?;
    let table = spira_config::process::cfg("SPIRA_FORGE_WORKFLOWS")?;
    if table.trim().is_empty() {
        return Ok(default);
    }
    let home = spira_config::resolve::locate_home_for_process()?;
    let env = std::env::vars().collect();
    let name = spira_config::repos::Registry::from_env(env, &home).name_at(&repo.to_string_lossy());
    Ok(crate::cmds::workflow_for(&default, &table, name.as_deref()).to_string())
}
