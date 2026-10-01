//! sending — the Sending (DESIGN.md). Delete the branch and the worktree of every bead whose
//! work has landed, and nothing else.
//!
//!   sending                     one pass over every repository
//!   sending --dry-run           print each branch's disposition, change nothing
//!   sending <bead-id|branch>    send exactly one bead's branch and worktree
//!   sending --status-from <f>   read `id<TAB>status` from a file instead of bd (tests)
//!   sending --skip-queue        every repository EXCEPT queue/queue.local ones (the sentinel)
//!   sending --queue-only        ONLY queue/queue.local repositories (the straggler sweep)
//!   sending --no-fetch          judge against the base refs as they stand
//!
//! Exit 0 when nothing failed, 1 when a deletion failed, 2 on a usage or context error.
//!
//! The destruction chokepoint itself (sp-9envm) is also reachable directly — the verbs
//! lib.sh's now-shimmed functions call, and the ones other bash callers (`held.sh`) use by
//! bare name instead of bypassing the chokepoint:
//!
//!   sending destroy-worktree    [--status-from <f>] <id> <path> <repo> <why>
//!   sending destroy-branch      [--status-from <f>] [--base <ref>] <id> <branch> <repo> <why> [<caller>]
//!   sending reap-landed-branch  [--status-from <f>] <id> <branch> <repo> <why> [<caller>]
//!   sending prune               <repo>
//!   sending salvage             <id> <worktree-path>
//!   sending witness             [--status-from <f>] <id>
//!
//! and the smaller plumbing the shims need. None of these touch bd, so none of them need
//! lib.sh's context at all — they read `$SPIRA_RUN`/`$SPIRA_REAPLOG` straight from the
//! environment, exactly as the bash functions they replace did:
//!
//!   sending hold-alive <pidfile>          sending holder-alive <id>
//!   sending worktree-of <branch> [<repo>] sending caller
//!   sending reaplog <verb> <id> [<detail>]
//!
//! Every subcommand above exits 0 on success, 1 on refusal/failure, 2 on a usage error —
//! except `reap-landed-branch`, which uses 2 for "certified-queued" (the one 3-way split
//! its own bash original made), printing the answer text `spira_destroy_branch`'s and
//! `spira_reap_landed_branch`'s shims capture into `$SPIRA_DESTROY_ERR`/`$SPIRA_REAP_ERR`.

use std::path::{Path, PathBuf};

use sending::ports::World;
use sending::real::Real;
use sending::sweep::Sweep;
use sending::{locate_home, parse, reap};

/// Pulls a leading `--status-from <f>` / `--base <r>` pair out of `args`, wherever it
/// appears, leaving the rest in order — the chokepoint subcommands accept these mixed in
/// with their positionals so a shim can build its argv with `${X:+--flag "$X"}`.
fn take_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    if i + 1 >= args.len() {
        return None;
    }
    args.remove(i);
    Some(args.remove(i))
}

fn die(msg: &str) -> ExitCodeLike {
    eprintln!("sending: {msg}");
    2
}

type ExitCodeLike = i32;

/// Builds the `Real` world the chokepoint subcommands that touch bd need (destroy-worktree,
/// destroy-branch, witness): the same lib.sh-locating, context-reading path the sweep uses.
/// `Real::minimal` — no context seam call, so a chokepoint subcommand given a repo PATH
/// directly never depends on the repo-NAME registry resolving (the full sweep's own
/// `Real::new` is a different, stricter, path — see its own doc comment).
fn real_world(status: Option<String>) -> Result<Real, String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sending"));
    let home = locate_home(std::env::var("SPIRA_HOME").ok().as_deref(), &exe).ok_or("cannot find lib.sh (set SPIRA_HOME)")?;
    Ok(Real::minimal(home, status))
}

fn cmd_destroy_worktree(mut args: Vec<String>) -> ExitCodeLike {
    let status = take_flag(&mut args, "--status-from");
    if args.len() != 4 {
        return die("destroy-worktree [--status-from <f>] <id> <path> <repo> <why>");
    }
    let w = match real_world(status) {
        Ok(w) => w,
        Err(e) => return die(&e),
    };
    let ok = w.destroy_worktree(&args[0], Path::new(&args[1]), Path::new(&args[2]), &args[3]);
    i32::from(!ok)
}

fn cmd_destroy_branch(mut args: Vec<String>) -> ExitCodeLike {
    let status = take_flag(&mut args, "--status-from");
    let explicit_base = take_flag(&mut args, "--base");
    if args.len() != 4 && args.len() != 5 {
        return die("destroy-branch [--status-from <f>] [--base <ref>] <id> <branch> <repo> <why> [<caller>]");
    }
    let w = match real_world(status) {
        Ok(w) => w,
        Err(e) => return die(&e),
    };
    let caller = args.get(4).map(String::as_str).unwrap_or("");
    let repo = Path::new(&args[2]);
    // `--base` overrides for a caller that already resolved it; otherwise resolve it the
    // same way the retired bash body did (`spira_landref "$repo"`, through the unchanged
    // family-W seam) — an unresolvable base is not an error here, it is the same "skip the
    // fence rather than refuse" the bash `&&` chain fell through to.
    let resolved = explicit_base.or_else(|| w.base(repo).map(|b| b.landref));
    match w.destroy_branch(&args[0], &args[1], repo, &args[3], caller, resolved.as_deref()) {
        Ok(()) => 0,
        // The message goes to stdout, unadorned, for `spira_destroy_branch`'s shim to
        // capture into `$SPIRA_DESTROY_ERR` exactly as the retired bash set that variable —
        // `"certified-queued"` included, since that exact string is the signal
        // `spira_reap_landed_branch`'s own (now also shimmed) caller branches on.
        Err(e) => {
            let msg = match e {
                reap::DestroyBranchErr::CertifiedQueued => "certified-queued".to_string(),
                reap::DestroyBranchErr::Refused(m) | reap::DestroyBranchErr::Failed(m) => m,
            };
            print!("{msg}");
            1
        }
    }
}

fn cmd_prune(args: Vec<String>) -> ExitCodeLike {
    if args.len() != 1 {
        return die("prune <repo>");
    }
    let reaplog_path = PathBuf::from(std::env::var("SPIRA_REAPLOG").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        format!("{}/reap.log", std::env::var("SPIRA_RUN").unwrap_or_default())
    }));
    i32::from(!reap::prune_worktrees(&reaplog_path, Path::new(&args[0])))
}

fn cmd_salvage(args: Vec<String>) -> ExitCodeLike {
    if args.len() != 2 {
        return die("salvage <id> <worktree-path>");
    }
    let run = PathBuf::from(std::env::var("SPIRA_RUN").unwrap_or_default());
    let reaplog_path = PathBuf::from(std::env::var("SPIRA_REAPLOG").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| run.join("reap.log").to_string_lossy().into_owned()));
    i32::from(reap::salvage(&run, &reaplog_path, &args[0], Path::new(&args[1])).is_err())
}

fn cmd_hold_alive(args: Vec<String>) -> ExitCodeLike {
    if args.len() != 1 {
        return die("hold-alive <pidfile>");
    }
    i32::from(!reap::hold_alive(Path::new(&args[0])))
}

fn cmd_holder_alive(args: Vec<String>) -> ExitCodeLike {
    if args.len() != 1 {
        return die("holder-alive <id>");
    }
    let run = PathBuf::from(std::env::var("SPIRA_RUN").unwrap_or_default());
    i32::from(!reap::holder_alive(&run, &args[0]))
}

fn cmd_worktree_of(args: Vec<String>) -> ExitCodeLike {
    if args.is_empty() || args.len() > 2 {
        return die("worktree-of <branch> [<repo>]");
    }
    let repo = args.get(1).cloned().unwrap_or_else(|| ".".to_string());
    if let Some(p) = reap::worktree_of(Path::new(&repo), &args[0]) {
        println!("{}", p.display());
    }
    0
}

fn cmd_caller(args: Vec<String>) -> ExitCodeLike {
    if !args.is_empty() {
        return die("caller");
    }
    println!("{}", reap::spira_caller());
    0
}

fn cmd_reaplog(args: Vec<String>) -> ExitCodeLike {
    if args.len() != 2 && args.len() != 3 {
        return die("reaplog <verb> <id> [<detail>]");
    }
    let run = std::env::var("SPIRA_RUN").unwrap_or_default();
    let reaplog_path = PathBuf::from(std::env::var("SPIRA_REAPLOG").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{run}/reap.log")));
    reap::reaplog(&reaplog_path, &args[0], &args[1], args.get(2).map(String::as_str).unwrap_or(""));
    0
}

/// `spira_holder_witnesses`'s own shim target: prints the reason on exit 0 (somebody may be
/// home), prints nothing on exit 1 (nobody is).
fn cmd_witness(mut args: Vec<String>) -> ExitCodeLike {
    let status = take_flag(&mut args, "--status-from");
    if args.len() != 1 {
        return die("witness [--status-from <f>] <id>");
    }
    let w = match real_world(status) {
        Ok(w) => w,
        Err(e) => return die(&e),
    };
    match reap::holder_witnesses(&w.run(), &args[0], &w) {
        Some(why) => {
            print!("{why}");
            0
        }
        None => 1,
    }
}

/// `spira_reap_landed_branch`'s own shim target. Resolves base/remote itself (through the
/// unchanged family-W `Base` seam) exactly as the bash original re-derived them, so
/// `bead_close_on_land` — still bash, still the production hot path for every landing —
/// keeps working unchanged. The push-delete's own diagnostic line goes to STDERR, not
/// stdout: stdout here is the shim's captured answer (`$SPIRA_REAP_ERR`), and mixing a log
/// line into it would both corrupt that answer and swallow the line (never printed at all).
fn cmd_reap_landed_branch(mut args: Vec<String>) -> ExitCodeLike {
    let status = take_flag(&mut args, "--status-from");
    if args.len() != 4 && args.len() != 5 {
        return die("reap-landed-branch [--status-from <f>] <id> <branch> <repo> <why> [<caller>]");
    }
    let w = match real_world(status) {
        Ok(w) => w,
        Err(e) => return die(&e),
    };
    let repo = Path::new(&args[2]);
    let caller = args.get(4).map(String::as_str).unwrap_or("");
    let base_info = w.base(repo);
    let base_ref = base_info.as_ref().map(|b| b.landref.as_str());
    let remote = base_info.as_ref().and_then(|b| b.remote.as_deref());
    let (run, reaplog_path) = (w.run(), w.reaplog_path());
    let log = |m: &str| eprintln!("{} spira: {m}", reap::utc_now());
    let label_remove = |id: &str, label: &str| w.label_remove(id, label);
    match reap::reap_landed_branch(&run, &reaplog_path, &args[0], &args[1], repo, &args[3], caller, base_ref, remote, &w, &log, &label_remove) {
        reap::ReapOutcome::Done => 0,
        reap::ReapOutcome::Queued => {
            print!("certified-queued");
            2
        }
        reap::ReapOutcome::Failed(m) => {
            print!("{m}");
            1
        }
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(verb) = args.first().cloned() {
        let mut rest = || args.split_off(1);
        let rc = match verb.as_str() {
            "destroy-worktree" => Some(cmd_destroy_worktree(rest())),
            "destroy-branch" => Some(cmd_destroy_branch(rest())),
            "reap-landed-branch" => Some(cmd_reap_landed_branch(rest())),
            "prune" => Some(cmd_prune(rest())),
            "salvage" => Some(cmd_salvage(rest())),
            "witness" => Some(cmd_witness(rest())),
            "hold-alive" => Some(cmd_hold_alive(rest())),
            "holder-alive" => Some(cmd_holder_alive(rest())),
            "worktree-of" => Some(cmd_worktree_of(rest())),
            "caller" => Some(cmd_caller(rest())),
            "reaplog" => Some(cmd_reaplog(rest())),
            _ => None,
        };
        if let Some(rc) = rc {
            std::process::exit(rc);
        }
    }

    let (opts, status) = match parse(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sending: {e}");
            std::process::exit(2);
        }
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sending"));
    let Some(home) = locate_home(std::env::var("SPIRA_HOME").ok().as_deref(), &exe) else {
        eprintln!("sending: cannot find lib.sh (set SPIRA_HOME)");
        std::process::exit(2);
    };
    let (w, repos) = match Real::new(home, status) {
        Ok(x) => x,
        Err(e) => {
            // FAIL CLOSED: a sweep that cannot read which repositories exist deletes nothing
            // and says so, rather than reporting a clean pass over none of them.
            eprintln!("sending: cannot read the harness context: {e}");
            std::process::exit(2);
        }
    };
    let label = w.submitted_label.clone();
    let mut s = Sweep { w: &w, opts, submitted_label: label, tally: Default::default() };
    std::process::exit(s.run(&repos));
}
