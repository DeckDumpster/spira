//! `queue` — see DESIGN.md §2.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use queue::cli;
use queue::ports::World;
use queue::real::*;

/// Where lib.sh and the harness scripts live: `$SPIRA_HOME`, else spira-config's
/// `spira.prod`, else beside this binary (`<release>/bin/queue` → `<release>/spira`,
/// `<workspace>/target/<profile>/queue` → `<workspace>/spira`).
fn harness_home() -> Option<PathBuf> {
    let has_lib = |p: &Path| p.join("lib.sh").is_file();
    if let Some(h) = std::env::var_os("SPIRA_HOME").map(PathBuf::from).filter(|p| has_lib(p)) {
        return Some(h);
    }
    if let Some(doc) = spira_config::discover(None).and_then(|p| spira_config::load(&p).ok()) {
        if let Some(p) = spira_config::get_path(&doc, "spira.prod").map(PathBuf::from).filter(|p| has_lib(p)) {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.ancestors().skip(1).take(4).map(|a| a.join("spira")).find(|p| has_lib(p))
}

/// Leaf utilities ported from lib.sh's queue-helpers family (sp-hwjsq, "wave 4.32"):
/// `queue_certified_list`, `queue_sort_rows` and `spira_git_push` are now one-line lib.sh
/// shims onto these. Handled before `cli::parse`/`harness_home` below, which they must
/// not depend on — `queue-certified-list.sh` and `branch-sweep.sh` resolve their own repo
/// path and call straight in, and requiring a resolved harness context first (bd, spira-lc,
/// conf.sh) for a plain git/mail operation would make a config problem elsewhere break a
/// leaf call that never touches any of it.
fn run_leaf(argv: &[String]) -> Option<ExitCode> {
    match argv.first().map(String::as_str) {
        Some("certified-list") => Some(run_certified_list(&argv[1..])),
        Some("sort-rows") => Some(run_sort_rows(&argv[1..])),
        Some("git-push") => Some(run_git_push(&argv[1..])),
        _ => None,
    }
}

fn run_certified_list(args: &[String]) -> ExitCode {
    let Some(repo) = args.first().filter(|s| !s.is_empty()) else {
        eprintln!("queue certified-list: repo path required");
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
    let mut express_label = "express".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prio-file" => {
                i += 1;
                prio_file = args.get(i).cloned();
            }
            "--express-label" => {
                i += 1;
                express_label = args.get(i).cloned().unwrap_or_default();
            }
            other => pos.push(other.to_string()),
        }
        i += 1;
    }
    let (Some(repo), Some(base)) = (pos.first(), pos.get(1)) else {
        eprintln!("queue sort-rows: repo and base sha required");
        return ExitCode::from(2);
    };
    let Some(prio_file) = prio_file else {
        eprintln!("queue sort-rows: --prio-file is required");
        return ExitCode::from(2);
    };
    use std::io::Read;
    let mut rows_text = String::new();
    let _ = std::io::stdin().read_to_string(&mut rows_text);
    let prio_json = std::fs::read_to_string(&prio_file).unwrap_or_else(|_| "[]".into());
    let repo_path = Path::new(repo);
    let with_trans: Vec<(String, String, i64, bool)> = queue::ops::helpers::parse_rows(&rows_text)
        .into_iter()
        .map(|(id, tip, epoch)| {
            let is_trans = queue::ops::helpers::is_suite_transition(repo_path, &tip, base);
            (id, tip, epoch, is_trans)
        })
        .collect();
    let (ranked, warning) = queue::ops::helpers::sort_rows(&with_trans, &prio_json, &express_label);
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
        eprintln!("queue git-push: repo path required");
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
    if let Some(rc) = run_leaf(&argv) {
        return rc;
    }
    let cmd = match cli::parse(&argv) {
        Ok(c) => c,
        Err(cli::Usage(m)) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };
    if cmd == cli::Cmd::Help {
        println!("{}", cli::USAGE);
        return ExitCode::SUCCESS;
    }
    let Some(home) = harness_home() else {
        eprintln!("queue.sh: cannot find the harness (lib.sh): set SPIRA_HOME or spira.prod");
        return ExitCode::from(1);
    };
    let lib = RealLib { home: home.clone() };
    // bd, spira-lc and the forge are configured by conf.sh; resolve once for them.
    let (bd, lc) = match queue::ports::Lib::context(&lib, None) {
        Ok((s, _)) => (RealBd { bd: s.bd.clone(), db: s.db.clone() }, RealLc { bin: s.lc_bin.clone() }),
        Err(e) => {
            eprintln!("queue.sh: cannot resolve the harness configuration: {e}");
            return ExitCode::from(1);
        }
    };
    let (git, scripts, forge, config, clock, env, io) = (RealGit, RealScripts { home }, RealForge, RealConfig, SysClock, SysEnv, StdEmit::new());
    let w = World { git: &git, bd: &bd, lib: &lib, scripts: &scripts, forge: &forge, lc: &lc, config: &config, clock: &clock, env: &env, io: &io };
    let rc = queue::dispatch(&w, &cmd);
    ExitCode::from(u8::try_from(rc.clamp(0, 255)).unwrap_or(1))
}
