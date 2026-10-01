//! landing-pass — the one landing pass for every land mode (DESIGN.md).
//!
//!   landing-pass --pass      pr mode (spira-landing-pass.timer)
//!   landing-pass land        push, hold, queue, queue.local (CHECK 6's worker)
//!   landing-pass halt [--reason T | --reason-file F|-] [--dry-run]
//!   landing-pass sweep-red
//!   landing-pass noverdict <id> <branch> <repo> <reason> <outcome>
//!                            `spira_land_noverdict` alone, gate output on stdin (sp-31hjr;
//!                            the real-sender suites' way in, no whole pass)
//!   landing-pass ask-rebase-loop <id> <branch> <repo> <n> <conflicts> <others> [<dir> <base>]
//!                            `spira_ask_rebase_loop` alone (sp-31hjr)
//!   landing-pass mark <id> <state> <tip> [reason] [extra]
//!                            `land_mark` alone (sp-cnnt6) — reads only $SPIRA_RUN, no
//!                            lib.sh seam, so every other crate's own land_mark call can
//!                            shell to this instead of sourcing bash.
//!   landing-pass state <id>  `land_state` alone (sp-cnnt6), same reasoning.

use landing_pass::cli::{self, Cmd, Reason};
use landing_pass::halt::{self, HaltArgs, HaltCtx, RealHalt};
use landing_pass::landstate;
use landing_pass::model::{RunRecord, StatusFile};
use landing_pass::pass::Pass;
use landing_pass::ports::Lib;
use landing_pass::pr::{PrPass, RealPrTools};
use landing_pass::real::{load_context, RealBeads, RealClock, RealGit, RealLib, RealProcs, RealTools, SeamRunner};
use landing_pass::records::Files;
use landing_pass::report::Reporter;
use landing_pass::lifecycle::{lifecycle_on, pin_for_children, RealLc};
use landing_pass::{signals, util};
use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match cli::parse(&args) {
        Ok(c) => c,
        Err((rc, msg)) => {
            eprintln!("{msg}");
            return ExitCode::from(rc as u8);
        }
    };
    let code = match cmd {
        Cmd::Help => {
            println!("{}", cli::USAGE);
            0
        }
        Cmd::Pr => pr(),
        Cmd::Land => land(),
        Cmd::Halt { reason, dry_run } => halt_cmd(reason, dry_run),
        Cmd::SweepRed => sweep_red(),
        Cmd::Noverdict { id, branch, repo, reason, outcome } => noverdict_cmd(&id, &branch, &repo, &reason, &outcome),
        Cmd::AskRebaseLoop(args) => ask_rebase_loop_cmd(&args),
        Cmd::Mark { id, state, tip, reason, extra } => mark_cmd(&id, &state, &tip, &reason, &extra),
        Cmd::State { id } => state_cmd(&id),
    };
    ExitCode::from(code as u8)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("SPIRA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Non-blocking exclusive flock; None when another holder has it.
fn try_lock(path: &Path) -> Result<Option<fs::File>, String> {
    if let Some(d) = path.parent() {
        let _ = fs::create_dir_all(d);
    }
    let f = OpenOptions::new().create(true).write(true).truncate(false).open(path).map_err(|e| format!("open lock {}: {e}", path.display()))?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(None);
    }
    Ok(Some(f))
}

fn pr() -> i32 {
    let out = Reporter::stdout(None);
    let Some(home) = home() else {
        eprintln!("landing-pass: SPIRA_HOME is unset");
        return 1;
    };
    let run = std::env::var_os("SPIRA_RUN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp/spira"));
    if run.join("world.halted").exists() {
        out.log("landing-pass: skipped — world is halted");
        return 0;
    }
    let _lock = match try_lock(&run.join("landing-pass.lock")) {
        Ok(Some(l)) => l,
        Ok(None) => {
            out.log("landing-pass: already running — skip");
            return 0;
        }
        Err(e) => {
            eprintln!("landing-pass: {e}");
            return 1;
        }
    };
    let (mut s, repos) = match load_context(&home, &out) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("landing-pass: {e}");
            return 1;
        }
    };
    s.lifecycle_enforce = lifecycle_on(s.toml.as_deref());
    pin_for_children(s.lifecycle_enforce);
    if !s.lifecycle_enforce {
        s.lc_bin = None;
    }
    let beads = RealBeads {
        home: s.home.clone(),
        db: s.db.clone(),
        bd: s.bd.clone(),
        timeout: s.bd_timeout,
        home_repo: s.home_repo.clone(),
        submitted_label: s.submitted_label.clone(),
        fixture: s.bdjson_fixture.clone(),
    };
    let tools = RealPrTools { s: &s, out: &out };
    let procs = RealProcs { run: s.run.clone() };
    let files = Files::new(&s.run);
    let lib = RealLib { seam: SeamRunner { home: s.home.clone(), out: &out }, incident: s.incident.clone(), s: s.clone(), beads: beads.clone() };
    let land_tools = RealTools::new(s.home.clone(), s.queue_bin.clone(), None, None);
    let p = PrPass {
        s: &s,
        repos: &repos,
        beads: &beads,
        git: &RealGit,
        lib: &lib,
        land_tools: &land_tools,
        procs: &procs,
        tools: &tools,
        files: &files,
        out: &out,
        loud: Default::default(),
    };
    p.run();
    0
}

fn land() -> i32 {
    // Before any thread exists: TERM/INT are received by one thread with sigwait.
    signals::block();
    let boot = Reporter::stdout(None);
    let Some(home) = home() else {
        eprintln!("landing-pass: SPIRA_HOME is unset");
        return 1;
    };
    let (mut s, repos) = match load_context(&home, &boot) {
        Ok(x) => x,
        Err(e) => {
            boot.log(&format!("landing: {e} — no pass ran"));
            if let Some(run) = std::env::var_os("SPIRA_RUN").map(PathBuf::from) {
                Files::new(&run).write_status(&StatusFile { at: util::unix_now(), rc: 1, branches: 0, moved: 0 });
            }
            return 1;
        }
    };
    s.lifecycle_enforce = lifecycle_on(s.toml.as_deref());
    // Before the signal thread exists: the environment is only ever set single-threaded.
    pin_for_children(s.lifecycle_enforce);
    if !s.lifecycle_enforce {
        s.lc_bin = None;
    }
    let files = Files::new(&s.run);
    let _lock = match try_lock(&files.lock()) {
        Ok(Some(l)) => l,
        Ok(None) => {
            boot.log("landing: already running — skip");
            return 0;
        }
        Err(e) => {
            boot.log(&format!("landing: {e}"));
            return 1;
        }
    };
    signals::install(s.run.clone());
    let start = util::unix_now();
    let pid = std::process::id();
    files.write_run(&RunRecord { pid: pid.to_string(), started: start.to_string(), ..Default::default() });
    let _ = fs::write(files.containers(), "");

    let out = Reporter::stdout(Some(files.mailbox()));
    let beads = RealBeads {
        home: s.home.clone(),
        db: s.db.clone(),
        bd: s.bd.clone(),
        timeout: s.bd_timeout,
        home_repo: s.home_repo.clone(),
        submitted_label: s.submitted_label.clone(),
        fixture: s.bdjson_fixture.clone(),
    };
    let lib = RealLib { seam: SeamRunner { home: s.home.clone(), out: &out }, incident: s.incident.clone(), s: s.clone(), beads: beads.clone() };
    let tools = RealTools::new(s.home.clone(), s.queue_bin.clone(), Some(files.containers()), Some(s.run.join("gate-admission")));
    let procs = RealProcs { run: s.run.clone() };
    let lc = RealLc { bin: s.lc_bin.clone() };
    let p = Pass {
        s: &s,
        repos: &repos,
        beads: &beads,
        git: &RealGit,
        lib: &lib,
        tools: &tools,
        procs: &procs,
        clock: &RealClock,
        lc: &lc,
        lc_state: std::cell::OnceCell::new(),
        out: &out,
        files: Files::new(&s.run),
        start,
        pid,
        swept: Cell::new(0),
        swept_conflict: Cell::new(0),
    };
    p.run();
    files.write_status(&StatusFile { at: util::unix_now(), rc: 0, branches: out.branches(), moved: out.moved() });
    files.clear_run();
    0
}

fn halt_cmd(reason: Reason, dry_run: bool) -> i32 {
    let reason = match reason {
        Reason::None => String::new(),
        Reason::Text(t) => t,
        Reason::File(f) => {
            let mut s = String::new();
            let ok = if f == "-" {
                std::io::stdin().read_to_string(&mut s).is_ok()
            } else {
                fs::File::open(&f).and_then(|mut h| h.read_to_string(&mut s)).is_ok()
            };
            if !ok {
                eprintln!("landing halt: cannot read the reason from {f}");
                return 2;
            }
            s.trim().to_string()
        }
    };
    let quiet = Reporter::capture(None);
    let ctx = home().and_then(|h| load_context(&h, &quiet).ok());
    let run = match (&ctx, std::env::var_os("SPIRA_RUN")) {
        (Some((s, _)), _) => s.run.clone(),
        (None, Some(r)) => PathBuf::from(r),
        (None, None) => {
            eprintln!("landing halt: cannot resolve SPIRA_RUN");
            return 1;
        }
    };
    let grace = ctx.as_ref().map(|(s, _)| s.halt_grace).unwrap_or(30);
    let queue_dir = ctx.as_ref().map(|(s, _)| s.queue_dir.clone()).unwrap_or_else(|| run.join("queue"));
    let hc = HaltCtx {
        files: Files::new(&run),
        grace,
        self_pid: std::process::id(),
        repos: ctx.as_ref().map(|(_, r)| r.as_slice()),
        queue_dir,
    };
    let path = halt::child_path(
        ctx.as_ref().and_then(|(s, _)| s.path.as_deref()),
        std::env::var("SPIRA_PATH").ok().as_deref(),
        std::env::var("PATH").ok().as_deref(),
    );
    let (rc, out, err) = halt::halt(&hc, &HaltArgs { reason, dry_run }, &RealHalt { path }, &RealGit);
    for l in out {
        println!("{l}");
    }
    for l in err {
        eprintln!("{l}");
    }
    rc
}

/// `landing-pass noverdict <id> <branch> <repo> <reason> <outcome>`, gate output on
/// stdin: `Lib::noverdict` alone (sp-31hjr), for the real-sender suites that used to
/// source lib.sh directly and call `spira_land_noverdict`.
fn noverdict_cmd(id: &str, branch: &str, repo: &str, reason: &str, outcome: &str) -> i32 {
    let Some(home) = home() else {
        eprintln!("landing-pass: SPIRA_HOME is unset");
        return 1;
    };
    let out = Reporter::stdout(None);
    let (s, _repos) = match load_context(&home, &out) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("landing-pass: {e}");
            return 1;
        }
    };
    let beads = RealBeads {
        home: s.home.clone(),
        db: s.db.clone(),
        bd: s.bd.clone(),
        timeout: s.bd_timeout,
        home_repo: s.home_repo.clone(),
        submitted_label: s.submitted_label.clone(),
        fixture: s.bdjson_fixture.clone(),
    };
    let lib = RealLib { seam: SeamRunner { home: s.home.clone(), out: &out }, incident: s.incident.clone(), s: s.clone(), beads };
    let mut gate_out = String::new();
    let _ = std::io::stdin().read_to_string(&mut gate_out);
    lib.noverdict(id, branch, repo, reason, outcome, &gate_out);
    0
}

/// `landing-pass ask-rebase-loop <id> <branch> <repo> <n> <conflicts> <others> [<repo-dir>
/// <base>]`: `Lib::ask_rebase_loop` alone (sp-31hjr), for the real-sender suites that used
/// to source lib.sh directly and call `spira_ask_rebase_loop`.
fn ask_rebase_loop_cmd(args: &[String]) -> i32 {
    let Some(home) = home() else {
        eprintln!("landing-pass: SPIRA_HOME is unset");
        return 1;
    };
    let out = Reporter::stdout(None);
    let (s, _repos) = match load_context(&home, &out) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("landing-pass: {e}");
            return 1;
        }
    };
    let beads = RealBeads {
        home: s.home.clone(),
        db: s.db.clone(),
        bd: s.bd.clone(),
        timeout: s.bd_timeout,
        home_repo: s.home_repo.clone(),
        submitted_label: s.submitted_label.clone(),
        fixture: s.bdjson_fixture.clone(),
    };
    let lib = RealLib { seam: SeamRunner { home: s.home.clone(), out: &out }, incident: s.incident.clone(), s: s.clone(), beads };
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    lib.ask_rebase_loop(&refs);
    0
}

fn sweep_red() -> i32 {
    let quiet = Reporter::capture(None);
    let run = home()
        .and_then(|h| load_context(&h, &quiet).ok())
        .map(|(s, _)| s.run)
        .or_else(|| std::env::var_os("SPIRA_RUN").map(PathBuf::from));
    let Some(run) = run else {
        eprintln!("sweep-red: cannot resolve SPIRA_RUN");
        return 1;
    };
    let (rc, out, err) = halt::sweep_red(&run.join("landstate"));
    for l in out {
        println!("{l}");
    }
    for l in err {
        eprintln!("{l}");
    }
    rc
}

/// `$SPIRA_RUN` alone — never the lib.sh seam. `mark`/`state` are the hot path every other
/// crate's own land_mark/land_state call becomes (sp-cnnt6): shelling to bash just to read
/// one already-exported variable would reintroduce the per-call cost this wave exists to
/// cut (wave4-decomposition.md's own cost note).
fn run_dir() -> Option<PathBuf> {
    std::env::var_os("SPIRA_RUN").filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn mark_cmd(id: &str, state: &str, tip: &str, reason: &str, extra: &str) -> i32 {
    let Some(run) = run_dir() else {
        eprintln!("landing-pass mark: SPIRA_RUN is unset");
        return 2;
    };
    if landstate::land_mark(&run, id, state, tip, reason, extra) {
        0
    } else {
        1
    }
}

fn state_cmd(id: &str) -> i32 {
    let Some(run) = run_dir() else {
        eprintln!("landing-pass state: SPIRA_RUN is unset");
        return 2;
    };
    match landstate::land_state(&run, id) {
        Some(s) => {
            print!("{s}");
            0
        }
        None => 1,
    }
}
