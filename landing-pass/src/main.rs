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
use landing_pass::land_verify;
use landing_pass::landstate;
use landing_pass::model::{RunRecord, StatusFile};
use landing_pass::pass::Pass;
use landing_pass::ports::{Beads, Lib};
use landing_pass::pr::{PrPass, RealPrTools};
use landing_pass::real::{load_context, RealBeads, RealClock, RealGit, RealLib, RealProcs, RealTools, SeamRunner};
use landing_pass::records::Files;
use landing_pass::report::Reporter;
use landing_pass::lifecycle::{lifecycle_on, pin_for_children, RealLc};
use landing_pass::{signals, util};
use std::cell::Cell;
use std::collections::BTreeMap;
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
        Cmd::Landed { id, repo } => landed_cmd(&id, &repo),
        Cmd::LandSubject { id } => land_subject_cmd(&id),
        Cmd::PrMerged { repo, branch } => pr_merged_cmd(&repo, &branch),
        Cmd::ConflictNote(args) => conflict_note_cmd(&args),
        Cmd::OtherBeads { repo, branch, base, files } => other_beads_cmd(&repo, &branch, &base, &files),
        Cmd::IsWorkType { ty } => is_work_type_cmd(&ty),
        Cmd::CitedCommit { id, repo, base } => cited_commit_cmd(&id, &repo, &base),
        Cmd::CloseOnLand { id, sha } => close_on_land_cmd(&id, &sha),
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
    // sp-ivfu3: this used to default to the literal `/tmp/spira` whenever `$SPIRA_RUN`
    // itself was unset — `run_dir()` (below) is the same in-process `spira_config`
    // resolution `mark`/`state` already use instead, with a named refusal, never a
    // guessed path, when it cannot resolve at all.
    let run = match run_dir() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("landing-pass: {e}");
            return 1;
        }
    };
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
    if !s.repo_map_ok {
        // An unconfigured or unreadable map is a fault, not an empty work queue: exiting 0
        // here is what let this run 1704 times without enumerating a single repository
        // (law-fail-closed-at-the-source, law-a-control-that-cannot-check-must-refuse).
        out.log("landing-pass: repository map is not configured or unreadable — refusing to run");
        return 1;
    }
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
///
/// `$SPIRA_RUN` when it is exported; otherwise resolved in-process the way conf.sh itself
/// would, so a bare environment — a unit's own (`SPIRA_RELEASE` + `PATH` only), the shape
/// that broke this in production three times — still gets a real answer rather than a
/// refusal (law-a-binary-resolves-the-config-it-reads). The fallback still never shells to
/// bash: `spira_config::resolve` is the same in-process resolver `queue`/`aeon` already
/// call for this, not a lib.sh seam.
fn run_dir() -> Result<PathBuf, String> {
    if let Some(r) = std::env::var_os("SPIRA_RUN").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(r));
    }
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let home = harness_home(&env).ok_or_else(|| {
        "SPIRA_RUN is unset and SPIRA_HOME could not be resolved (no lib.sh found via \
         $SPIRA_HOME, spira.prod, or beside this binary)"
            .to_string()
    })?;
    let repo = spira_config::resolve::derive_repo_filesystem(&home, &env);
    let resolved = spira_config::resolve::resolve_for_process(&home, &repo, &env)
        .map_err(|e| format!("SPIRA_RUN is unset and resolving it failed: {e}"))?;
    let run = resolved.get("SPIRA_RUN");
    if run.is_empty() {
        return Err("SPIRA_RUN is unset and resolve() produced no SPIRA_RUN".to_string());
    }
    Ok(PathBuf::from(run))
}

/// Where lib.sh and the harness live, for [`run_dir`]'s fallback alone — never touches
/// [`home`] or any of the other commands above, which keep requiring `$SPIRA_HOME`
/// explicitly. The same three rungs queue's own `harness_home` climbs: `$SPIRA_HOME`, else
/// spira-config's `spira.prod`, else beside this binary (`<release>/bin/landing-pass` →
/// `<release>/spira`, `<workspace>/target/<profile>/landing-pass` → `<workspace>/spira`).
fn harness_home(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let has_lib = |p: &Path| p.join("lib.sh").is_file();
    if let Some(h) = env.get("SPIRA_HOME").map(PathBuf::from).filter(|p| has_lib(p)) {
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

fn mark_cmd(id: &str, state: &str, tip: &str, reason: &str, extra: &str) -> i32 {
    let run = match run_dir() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("landing-pass mark: {e}");
            return 2;
        }
    };
    if landstate::land_mark(&run, id, state, tip, reason, extra) {
        0
    } else {
        1
    }
}

fn state_cmd(id: &str) -> i32 {
    let run = match run_dir() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("landing-pass state: {e}");
            return 2;
        }
    };
    match landstate::land_state(&run, id) {
        Some(s) => {
            print!("{s}");
            0
        }
        None => 1,
    }
}

// ── family R (sp-81t4d, "wave 4.17" — landed verification) ──────────────────────────────
//
// None of these need the full pass context (repositories, the gate/queue tooling): each
// resolves only what it reads, the same `$SPIRA_HOME`-then-resolve-in-process rule
// `run_dir` already follows for `mark`/`state` (law-a-binary-resolves-the-config-it-reads).

/// `$SPIRA_HOME` when exported, else [`harness_home`]'s own three rungs — never a refusal
/// just because a unit's bare environment did not export it.
fn resolve_home() -> Result<PathBuf, String> {
    if let Some(h) = home() {
        return Ok(h);
    }
    let env: BTreeMap<String, String> = std::env::vars().collect();
    harness_home(&env).ok_or_else(|| "SPIRA_HOME is unset and could not be resolved".to_string())
}

/// bd's own connection facts, resolved in-process against `home` — conf.sh resolves
/// `SPIRA_DB`/`SPIRA_BD`/`BD_TIMEOUT`/`SPIRA_HOME_REPO`/`SPIRA_SUBMITTED_LABEL`/
/// `SPIRA_BDJSON_FIXTURE` but exports none of them (the same defect `run_dir`/`Registry::from_env`
/// already guard against), so this never reads them straight off `std::env`.
fn resolve_beads(home: &Path) -> Result<RealBeads, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let repo = spira_config::resolve::derive_repo_filesystem(home, &env);
    let resolved = spira_config::resolve::resolve_for_process(home, &repo, &env)?;
    let get = |k: &str, d: &str| {
        let v = resolved.get(k);
        if v.is_empty() { d.to_string() } else { v.to_string() }
    };
    // BD_TIMEOUT and SPIRA_BDJSON_FIXTURE carry no `spira/conf.d/<KEY>` entry — ad hoc
    // overrides `resolve_for_process` never produces (aeon's own seam doc names the same
    // split), so these two are read straight off the raw environment, never `resolved`.
    let ad_hoc = |k: &str, d: &str| env.get(k).filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| d.to_string());
    Ok(RealBeads {
        home: home.to_path_buf(),
        db: get("SPIRA_DB", ""),
        bd: get("SPIRA_BD", "bd"),
        timeout: ad_hoc("BD_TIMEOUT", "180").parse().unwrap_or(180),
        home_repo: get("SPIRA_HOME_REPO", "spira"),
        submitted_label: get("SPIRA_SUBMITTED_LABEL", "spira-submitted"),
        fixture: env.get("SPIRA_BDJSON_FIXTURE").filter(|s| !s.is_empty()).map(PathBuf::from),
    })
}

/// `landing-pass landed <id> <repo>`: lib.sh `landed`/`landed_sha` alone. Prints the
/// landing sha on a found exit; exit 1 not found; exit 2 cannot tell (an unresolvable land
/// ref, OR `$SPIRA_HOME` itself could not be resolved — both are the same named refusal,
/// never folded into "not landed").
fn landed_cmd(id: &str, repo: &str) -> i32 {
    let Ok(home) = resolve_home() else { return 2 };
    let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home);
    let Some((base, local)) = spira_config::repos::landrefs(&reg, repo) else { return 2 };
    let mut refs = vec![base];
    if let Some(l) = local {
        refs.push(l);
    }
    match land_verify::landed(&RealGit, Path::new(repo), id, &refs) {
        Some(sha) => {
            print!("{sha}");
            0
        }
        None => 1,
    }
}

/// `landing-pass land-subject <id>`: lib.sh `land_subject` alone. A bead that cannot be
/// read (bd unreachable, or `$SPIRA_HOME` itself unresolved) falls back to the bare form —
/// the same fallback the bash function's own `bdjson` failure took.
fn land_subject_cmd(id: &str) -> i32 {
    let title = resolve_home()
        .ok()
        .and_then(|home| resolve_beads(&home).ok())
        .and_then(|beads| beads.show(&[id.to_string()]).ok())
        .and_then(|rows| rows.into_iter().next())
        .map(|r| land_verify::collapse_title(&r.title))
        .unwrap_or_default();
    print!("{}", land_verify::land_subject(id, &title));
    0
}

/// `landing-pass pr-merged <repo> <branch>`: lib.sh `pr_merged` alone.
fn pr_merged_cmd(repo: &str, branch: &str) -> i32 {
    if land_verify::pr_merged(Path::new(repo), branch) {
        0
    } else {
        1
    }
}

/// `landing-pass conflict-note <repo> <branch> <base> <name> <conflicts> <actor> [rq_n]`:
/// lib.sh `conflict_reopen_note` alone.
fn conflict_note_cmd(args: &[String]) -> i32 {
    let get = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let rq_n = args.get(6).map(String::as_str).filter(|s| !s.is_empty()).unwrap_or("1");
    let note = land_verify::conflict_reopen_note(&RealGit, Path::new(get(0)), get(1), get(2), get(3), get(4), get(5), rq_n);
    print!("{note}");
    0
}

/// `landing-pass other-beads <repo> <branch> <base> <files>`: lib.sh
/// `other_beads_on_conflicts` alone.
fn other_beads_cmd(repo: &str, branch: &str, base: &str, files: &str) -> i32 {
    print!("{}", land_verify::other_beads_on_conflicts(&RealGit, Path::new(repo), branch, base, files));
    0
}

/// `landing-pass is-work-type <type>`: lib.sh `bead_is_work_type` alone.
/// `SPIRA_WORK_CLOSE_TYPES` is an ad hoc override with no `conf.d` entry (as it has always
/// been for the bash function), so this reads it straight off the environment, same as
/// every other such name this crate's seam used to snapshot.
fn is_work_type_cmd(ty: &str) -> i32 {
    let close_types = std::env::var("SPIRA_WORK_CLOSE_TYPES").unwrap_or_else(|_| "task bug feature".to_string());
    if land_verify::is_work_type(ty, &close_types) {
        0
    } else {
        1
    }
}

/// `landing-pass cited-commit <id> <repo> <base>`: lib.sh `bead_cited_commit_on_base`
/// alone. Prints "<sha> <rule>" on a found exit; exit 1 not found (including a bead or
/// `$SPIRA_HOME` that could not be read at all — there is nothing to cite without notes).
fn cited_commit_cmd(id: &str, repo: &str, base: &str) -> i32 {
    let Ok(home) = resolve_home() else { return 1 };
    let Ok(beads) = resolve_beads(&home) else { return 1 };
    let Some(row) = beads.show(&[id.to_string()]).ok().and_then(|rows| rows.into_iter().next()) else { return 1 };
    match land_verify::bead_cited_commit_on_base(&RealGit, Path::new(repo), base, id, &row.notes) {
        Some((sha, rule)) => {
            print!("{sha} {rule}");
            0
        }
        None => 1,
    }
}

/// `landing-pass close-on-land <id> [sha]`: lib.sh `bead_close_on_land` alone. Always
/// exits 0 — every caller (including lib.sh's own shim) already discards this function's
/// exit code (`|| true`), so there is no exit-code contract to preserve beyond "ran".
fn close_on_land_cmd(id: &str, sha: &str) -> i32 {
    let Ok(home) = resolve_home() else { return 0 };
    let Ok(run) = run_dir() else { return 0 };
    let Ok(beads) = resolve_beads(&home) else { return 0 };
    let row = beads.show(&[id.to_string()]).ok().and_then(|rows| rows.into_iter().next());
    let out = Reporter::stdout(None);
    land_verify::close_on_land(&RealGit, &out, &run, &home, &beads.submitted_label, row.as_ref(), id, sha);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises this file's env-mutating tests against each other and against anything
    /// else in this binary that might read these same names — same reasoning as
    /// `landing_pass::testutil::serial` (not reusable here: it is private to the lib
    /// crate, and this test lives in the bin crate).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn run_dir_resolves_in_process_when_spira_run_is_unset() {
        // law-a-binary-resolves-the-config-it-reads: a bare environment — a unit's own
        // (SPIRA_RELEASE + PATH only) — must still get a real answer from `run_dir`, not a
        // refusal. The production defect this guards: aeon's own `landing-pass mark` call
        // silently did nothing when its process environment lacked $SPIRA_RUN.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = testkit::TempDir::new("landing-pass-run-dir");
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("lib.sh"), "# fixture\n").unwrap();
        // sp-1cdgq-2: a missing conf.d is now a named registry error, so any fixture
        // whose SPIRA_HOME runs real config resolution needs the directory to exist.
        fs::create_dir_all(home.join("conf.d")).unwrap();
        let xdg_data = dir.join("xdg-data");
        let xdg_config = dir.join("xdg-config"); // empty: no config file for discover() to pick up
        fs::create_dir_all(&xdg_config).unwrap();

        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            ["SPIRA_RUN", "SPIRA_HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "SPIRA_TOML", "HOME"].iter().map(|k| (*k, std::env::var_os(k))).collect();
        std::env::remove_var("SPIRA_RUN");
        std::env::set_var("SPIRA_HOME", &home);
        std::env::set_var("XDG_DATA_HOME", &xdg_data);
        std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
        std::env::remove_var("SPIRA_TOML");
        std::env::set_var("HOME", dir.join("userhome"));

        let got = run_dir();

        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }

        let run = got.expect("run_dir must resolve a real answer without $SPIRA_RUN");
        // The exact directory spira_config::resolve derives from XDG_DATA_HOME with no toml
        // override and the default ("prod") instance — asserted exactly, so this proves
        // resolution ran, not a lucky guess at some other path.
        assert_eq!(run, xdg_data.join("spira").join("run"));
    }

    #[test]
    fn state_reads_back_a_record_through_the_resolved_run_dir() {
        // The same fallback, exercised through state_cmd end to end: with $SPIRA_RUN
        // unset, a record planted at the path `run_dir` resolves to is still read back.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = testkit::TempDir::new("landing-pass-run-dir-state");
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("lib.sh"), "# fixture\n").unwrap();
        // sp-1cdgq-2: a missing conf.d is now a named registry error, so any fixture
        // whose SPIRA_HOME runs real config resolution needs the directory to exist.
        fs::create_dir_all(home.join("conf.d")).unwrap();
        let xdg_data = dir.join("xdg-data");
        let xdg_config = dir.join("xdg-config");
        fs::create_dir_all(&xdg_config).unwrap();
        let run = xdg_data.join("spira").join("run");
        fs::create_dir_all(run.join("landstate")).unwrap();
        fs::write(run.join("landstate").join("sp-x"), "LANDED deadbeef 1700000000 spira").unwrap();

        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            ["SPIRA_RUN", "SPIRA_HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "SPIRA_TOML", "HOME"].iter().map(|k| (*k, std::env::var_os(k))).collect();
        std::env::remove_var("SPIRA_RUN");
        std::env::set_var("SPIRA_HOME", &home);
        std::env::set_var("XDG_DATA_HOME", &xdg_data);
        std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
        std::env::remove_var("SPIRA_TOML");
        std::env::set_var("HOME", dir.join("userhome"));

        let rc = state_cmd("sp-x");

        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }

        assert_eq!(rc, 0);
    }
}
