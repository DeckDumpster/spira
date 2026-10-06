//! `rebase-stale <bead-id> [repo]` — exit 0 ok/nothing to do, 1 real conflict, 2 red gate,
//! 3 not attempted. See DESIGN.md.
//!
//! Also reachable directly (wave 4.21, sp-07jcz) — lib.sh's `rebase_branch`/`recut_onto`
//! shims call these, so aeon/landing-pass/queue's existing bash-seam calls keep working
//! unchanged:
//!
//!   rebase-stale rebase-branch <branch> <onto> <repo> <name> [<format-cmd>]
//!   rebase-stale recut-onto    <branch> <onto> <repo> <name>
//!
//! See `src/branch.rs` for the contract (byte-for-byte lib.sh's own
//! `format_rebased`/`rebase_branch`/`recut_onto`).

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use rebase_stale::branch::{self, BranchConfig};
use rebase_stale::engine::{self, Config};
use rebase_stale::resolve::Rules;
use rebase_stale::seam::LibSeam;
use spira_config::process::cfg;

fn die2(msg: &str) -> ExitCode {
    eprintln!("rebase-stale: {msg}");
    ExitCode::from(2)
}

/// The config `rebase-branch`/`recut-onto` need: just `SPIRA_RUN`, the identity and the
/// reaplog — no bd, no queue, no repository lookup of their own, because the caller (today,
/// lib.sh's own shim) has already resolved the repo path/name and, for `rebase-branch`, the
/// formatter command.
///
/// `SPIRA_REAPLOG`/`SPIRA_FORMAT_TIMEOUT` are not registered config keys (no
/// `spira/conf.d/` entry) — they stay ambient env reads with their own defaults.
fn branch_config() -> Result<BranchConfig, String> {
    let run = PathBuf::from(cfg("SPIRA_RUN")?);
    let reaplog = env::var("SPIRA_REAPLOG").ok().filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| run.join("reap.log"));
    let format_timeout = env::var("SPIRA_FORMAT_TIMEOUT").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    Ok(BranchConfig {
        run,
        reaplog,
        git_name: cfg("SPIRA_GIT_NAME")?,
        git_email: cfg("SPIRA_GIT_EMAIL")?,
        format_timeout: Duration::from_secs(format_timeout),
    })
}

/// `rebase-branch <branch> <onto> <repo> <name> [<format-cmd>]` — lib.sh `rebase_branch`'s
/// shim target. On failure prints exactly 3 lines (failure, conflicts, refused_reason — any
/// may be empty) for the shim to fold back into `$REBASE_FAILURE`/`$REBASE_CONFLICTS`/
/// `$REBASE_REFUSED_REASON`; on success prints nothing. Exit 0 ok, 1 failure (named), 2 usage.
fn cmd_rebase_branch(args: Vec<String>) -> ExitCode {
    if args.len() != 4 && args.len() != 5 {
        return die2("rebase-branch <branch> <onto> <repo> <name> [<format-cmd>]");
    }
    let cfg = match branch_config() {
        Ok(c) => c,
        Err(e) => return die2(&e),
    };
    let repo = Path::new(&args[2]);
    let format_cmd = args.get(4).map(String::as_str).unwrap_or("");
    let r = branch::rebase_branch(&cfg, repo, &args[0], &args[1], &args[3], format_cmd);
    if r.ok {
        return ExitCode::from(0);
    }
    println!("{}", r.failure.map(|f| f.as_str()).unwrap_or(""));
    println!("{}", r.conflicts);
    println!("{}", r.refused_reason);
    ExitCode::from(1)
}

/// `recut-onto <branch> <onto> <repo> <name>` — lib.sh `recut_onto`'s shim target. Always
/// prints exactly 2 lines (applied count, conflicts — the latter is one of lib.sh's own
/// sentinel words, `no-base`/`no-branch`/`no-worktree`/`no-merge-base`/`no-checkout`, on an
/// early refusal). Exit 0 ok, 1 refused/conflict (still may have applied some commits), 2 usage.
fn cmd_recut_onto(args: Vec<String>) -> ExitCode {
    if args.len() != 4 {
        return die2("recut-onto <branch> <onto> <repo> <name>");
    }
    let cfg = match branch_config() {
        Ok(c) => c,
        Err(e) => return die2(&e),
    };
    let repo = Path::new(&args[2]);
    let r = branch::recut_onto(&cfg, repo, &args[0], &args[1], &args[3]);
    println!("{}", r.applied);
    println!("{}", r.conflicts);
    ExitCode::from(if r.ok { 0 } else { 1 })
}

fn main() -> ExitCode {
    let argv: Vec<String> = env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("rebase-branch") => return cmd_rebase_branch(argv[1..].to_vec()),
        Some("recut-onto") => return cmd_recut_onto(argv[1..].to_vec()),
        _ => {}
    }

    let mut args = argv.into_iter();
    let Some(id) = args.next().filter(|s| !s.is_empty()) else {
        eprintln!("rebase-stale: bead id required\nusage: rebase-stale <bead-id> [repo]");
        return ExitCode::from(3);
    };
    let repo_arg = args.next();

    // `SPIRA_HOME` is not a registered config key (no `spira/conf.d/` entry) — ambient env
    // read stays as is.
    let Some(home) = env::var_os("SPIRA_HOME").map(PathBuf::from) else {
        eprintln!("rebase-stale: SPIRA_HOME is not set (lib.sh lives there)");
        return ExitCode::from(3);
    };

    macro_rules! cfg_or_exit {
        ($key:expr) => {
            match cfg($key) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("rebase-stale: {e}");
                    return ExitCode::from(3);
                }
            }
        };
    }

    let run = PathBuf::from(cfg_or_exit!("SPIRA_RUN"));
    let log = PathBuf::from(cfg_or_exit!("SPIRA_REBASE_STALE_LOG"));
    let git_name = cfg_or_exit!("SPIRA_GIT_NAME");
    let git_email = cfg_or_exit!("SPIRA_GIT_EMAIL");
    // `SPIRA_DB` deliberately resolves to an empty string to mean "no database for this
    // run" (spira/conf.d/SPIRA_DB) — that emptiness is the declared value, not a missing one.
    let db = cfg_or_exit!("SPIRA_DB");
    let db = if db.is_empty() { None } else { Some(PathBuf::from(db)) };
    let bd = cfg_or_exit!("SPIRA_BD");

    let config = Config {
        log,
        git_name,
        git_email,
        lock_wait: Duration::from_secs(60),
        run: run.clone(),
    };
    let seam = LibSeam::new(home, db, bd, run.clone());
    let rules = Rules::standard(Some(run.join("rebase-stale.target")));
    let r = engine::run(&id, repo_arg.as_deref(), &config, &seam, &rules);
    if let Some(s) = r.stdout {
        println!("{s}");
    }
    if let Some(s) = r.stderr {
        eprintln!("{s}");
    }
    ExitCode::from(r.exit.code())
}
