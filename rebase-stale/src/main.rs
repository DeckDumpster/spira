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

/// The environment conf.sh exports is what every caller runs under; spira-config's typed
/// `[spira]` section answers a key the environment leaves unset.
struct Keys {
    toml: Option<spira_config::SpiraSection>,
}

impl Keys {
    fn load() -> Keys {
        let toml = spira_config::discover(None)
            .and_then(|p| spira_config::load(&p).ok())
            .and_then(|d| d.spira);
        Keys { toml }
    }

    fn get(
        &self,
        env_key: &str,
        pick: impl Fn(&spira_config::SpiraSection) -> Option<String>,
    ) -> Option<String> {
        env::var(env_key)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| self.toml.as_ref().and_then(pick))
    }
}

fn die2(msg: &str) -> ExitCode {
    eprintln!("rebase-stale: {msg}");
    ExitCode::from(2)
}

/// The config `rebase-branch`/`recut-onto` need: just `SPIRA_RUN`, the identity and the
/// reaplog — no bd, no queue, no repository lookup of their own, because the caller (today,
/// lib.sh's own shim) has already resolved the repo path/name and, for `rebase-branch`, the
/// formatter command.
fn branch_config() -> Result<BranchConfig, String> {
    let keys = Keys::load();
    let run = keys.get("SPIRA_RUN", |s| s.run.clone()).map(PathBuf::from).ok_or_else(|| "SPIRA_RUN is not set".to_string())?;
    let reaplog = env::var("SPIRA_REAPLOG").ok().filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| run.join("reap.log"));
    let format_timeout = env::var("SPIRA_FORMAT_TIMEOUT").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    Ok(BranchConfig {
        run,
        reaplog,
        git_name: keys.get("SPIRA_GIT_NAME", |s| s.git_name.clone()).unwrap_or_else(|| "spira".into()),
        git_email: keys.get("SPIRA_GIT_EMAIL", |s| s.git_email.clone()).unwrap_or_else(|| "spira@spira.invalid".into()),
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

    let keys = Keys::load();
    let Some(home) = env::var_os("SPIRA_HOME").map(PathBuf::from) else {
        eprintln!("rebase-stale: SPIRA_HOME is not set (lib.sh lives there)");
        return ExitCode::from(3);
    };
    let Some(run) = keys.get("SPIRA_RUN", |s| s.run.clone()).map(PathBuf::from) else {
        eprintln!("rebase-stale: SPIRA_RUN is not set");
        return ExitCode::from(3);
    };
    let log = keys
        .get("SPIRA_REBASE_STALE_LOG", |s| s.rebase_stale_log.clone())
        .map(PathBuf::from)
        .unwrap_or_else(|| run.join("rebase-stale.log"));
    let cfg = Config {
        log,
        git_name: keys
            .get("SPIRA_GIT_NAME", |s| s.git_name.clone())
            .unwrap_or_else(|| "spira".into()),
        git_email: keys
            .get("SPIRA_GIT_EMAIL", |s| s.git_email.clone())
            .unwrap_or_else(|| "spira@spira.invalid".into()),
        lock_wait: Duration::from_secs(60),
        run: run.clone(),
    };
    let seam = LibSeam::new(
        home,
        keys.get("SPIRA_DB", |s| s.db.clone()).map(PathBuf::from),
        keys.get("SPIRA_BD", |s| s.bd.clone())
            .unwrap_or_else(|| "bd".into()),
    );
    let rules = Rules::standard(Some(run.join("rebase-stale.target")));
    let r = engine::run(&id, repo_arg.as_deref(), &cfg, &seam, &rules);
    if let Some(s) = r.stdout {
        println!("{s}");
    }
    if let Some(s) = r.stderr {
        eprintln!("{s}");
    }
    ExitCode::from(r.exit.code())
}
