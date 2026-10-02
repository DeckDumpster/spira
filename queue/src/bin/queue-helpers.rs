//! `queue-helpers` — the lib.sh queue-helpers family's leaf CLI (sp-hwjsq, "wave 4.32"):
//! `certified-list`, `sort-rows` and `git-push`. lib.sh's `queue_certified_list`,
//! `queue_sort_rows` and `spira_git_push` are one-line shims onto this binary.
//!
//! DELIBERATELY A SEPARATE BINARY FROM `queue`. The first cut named these subcommands
//! on `queue` itself; that broke production's `spira_git_push` the moment it ran under
//! any fixture that stubs the `queue` binary by NAME on PATH to isolate dispatch
//! behaviour (sp-gypjk's convention, used by `test-certify.sh` among others — see its
//! `queue-bin` stub, injected "ahead of the release's"). The stub swallowed `queue
//! git-push` silently: exit 0, nothing pushed, because the stub's job is to log argv and
//! return — it has no idea it was just asked to do a real push. A caller's config
//! problem with the bigger CLI must never take down a plain git/mail primitive these
//! three are, so they live on their own name, immune to any `queue` stub.
//!
//! No harness context needed — `queue-certified-list.sh` and `branch-sweep.sh` resolve
//! their own repo path and call straight in, and requiring a resolved harness context
//! first (bd, spira-lc, conf.sh) for a plain git/mail operation would make a config
//! problem elsewhere break a leaf call that never touches any of it.

use queue::ports::Lc;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: queue-helpers certified-list <repo-path> | queue-helpers sort-rows <repo-path> <base-sha> --prio-file <path> | queue-helpers git-push <repo-path> [push-args...]";

fn run_certified_list(args: &[String]) -> ExitCode {
    let Some(repo) = args.first().filter(|s| !s.is_empty()) else {
        eprintln!("queue-helpers certified-list: repo path required");
        return ExitCode::from(2);
    };
    for (id, tip, epoch) in queue::ops::helpers::certified_list(Path::new(repo)) {
        println!("{id} {tip} {epoch}");
    }
    ExitCode::SUCCESS
}

fn run_sort_rows(args: &[String]) -> ExitCode {
    let mut pos = Vec::new();
    let mut prio_file: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prio-file" => {
                i += 1;
                prio_file = args.get(i).cloned();
            }
            other => pos.push(other.to_string()),
        }
        i += 1;
    }
    let (Some(repo), Some(base)) = (pos.first(), pos.get(1)) else {
        eprintln!("queue-helpers sort-rows: repo and base sha required");
        return ExitCode::from(2);
    };
    let Some(prio_file) = prio_file else {
        eprintln!("queue-helpers sort-rows: --prio-file is required");
        return ExitCode::from(2);
    };
    use std::io::Read;
    let mut rows_text = String::new();
    let _ = std::io::stdin().read_to_string(&mut rows_text);
    let prio_json = std::fs::read_to_string(&prio_file).unwrap_or_else(|_| "[]".into());
    let repo_path = Path::new(repo);
    let suite_state = match queue::conf::nonempty("SPIRA_SUITE_STATE_FILE") {
        Ok(f) => f,
        Err(e) => {
            eprintln!("queue-helpers sort-rows: {e}");
            return ExitCode::from(1);
        }
    };
    let with_trans: Vec<(String, String, i64, bool)> = queue::ops::helpers::parse_rows(&rows_text)
        .into_iter()
        .map(|(id, tip, epoch)| {
            let is_trans = queue::ops::helpers::is_suite_transition(repo_path, &suite_state, &tip, base);
            (id, tip, epoch, is_trans)
        })
        .collect();
    let express: std::collections::HashSet<String> = match (queue::real::RealLc { bin: Some("spira-lc".into()) }).express_ids() {
        Ok(ids) => ids.into_iter().collect(),
        Err(e) => {
            eprintln!("queue-helpers sort-rows: express set unreadable ({e}) -- ranking without it");
            Default::default()
        }
    };
    let (ranked, warning) = queue::ops::helpers::sort_rows(&with_trans, &prio_json, &express);
    if let Some(w) = warning {
        eprintln!("{w}");
    }
    for r in &ranked {
        println!("{}", queue::ops::helpers::render_row(r));
    }
    ExitCode::SUCCESS
}

fn run_git_push(args: &[String]) -> ExitCode {
    let Some((repo, rest)) = args.split_first() else {
        eprintln!("queue-helpers git-push: repo path required");
        return ExitCode::from(2);
    };
    let rc = queue::ops::helpers::git_push_cmd(Path::new(repo), rest)
        .stdin(std::process::Stdio::null())
        .status()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(127);
    ExitCode::from(u8::try_from(rc.clamp(0, 255)).unwrap_or(1))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("certified-list") => run_certified_list(&argv[1..]),
        Some("sort-rows") => run_sort_rows(&argv[1..]),
        Some("git-push") => run_git_push(&argv[1..]),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
