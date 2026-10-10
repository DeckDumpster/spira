//! landing-pass — the one landing pass for every land mode (DESIGN.md).
//!
//!   landing-pass --pass      pr mode (spira-landing-pass.timer)
//!   landing-pass land        push, hold, queue, queue.local (CHECK 6's worker)
//!   landing-pass halt [--reason T | --reason-file F|-] [--dry-run]
//!   landing-pass noverdict <id> <branch> <repo> <reason> <outcome>
//!                            `spira_land_noverdict` alone, gate output on stdin (sp-31hjr;
//!                            the real-sender suites' way in, no whole pass)
//!   landing-pass ask-rebase-loop <id> <branch> <repo> <n> <conflicts> <others> [<dir> <base>]
//!                            `spira_ask_rebase_loop` alone (sp-31hjr)

use landing_pass::cli::{self, Cmd, Reason};
use landing_pass::halt::{self, HaltArgs, HaltCtx, RealHalt};
use landing_pass::land_verify;
use landing_pass::model::{RunRecord, StatusFile};
use landing_pass::pass::Pass;
use landing_pass::ports::{Beads, Lib};
use landing_pass::pr::{PrPass, RealPrTools};
use landing_pass::real::{load_context, RealBeads, RealClock, RealGit, RealLib, RealProcs, RealTools, SeamRunner};
use landing_pass::records::Files;
use landing_pass::report::Reporter;
use landing_pass::lifecycle::RealLc;
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
        Cmd::Noverdict { id, branch, repo, reason, outcome } => noverdict_cmd(&id, &branch, &repo, &reason, &outcome),
        Cmd::AskRebaseLoop(args) => ask_rebase_loop_cmd(&args),
        Cmd::LandSubject { id } => land_subject_cmd(&id),
        Cmd::PrMerged { repo, branch } => pr_merged_cmd(&repo, &branch),
        Cmd::ConflictNote(args) => conflict_note_cmd(&args),
        Cmd::OtherBeads { repo, branch, base, files } => other_beads_cmd(&repo, &branch, &base, &files),
        Cmd::IsWorkType { ty } => is_work_type_cmd(&ty),
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
    // resolution `run_dir` resolves instead, with a named refusal, never a
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
    let (s, repos) = match load_context(&home, &out) {
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
    let beads = RealBeads {
        home: s.home.clone(),
        db: s.db.clone(),
        bd: s.bd.clone(),
        timeout: s.bd_timeout,
        home_repo: s.home_repo.clone(),
        fixture: s.bdjson_fixture.clone(),
        lc_bin: s.lc_bin.clone(),
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
    let (s, repos) = match load_context(&home, &boot) {
        Ok(x) => x,
        Err(e) => {
            boot.log(&format!("landing: {e} — no pass ran"));
            // SPIRA_RUN via the one source of config (`run_dir`) — best-effort: if it too
            // cannot resolve, the status file is simply not written, same as before.
            if let Ok(run) = run_dir() {
                Files::new(&run).write_status(&StatusFile { at: util::unix_now(), rc: 1, branches: 0, moved: 0 });
            }
            return 1;
        }
    };
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
        fixture: s.bdjson_fixture.clone(),
        lc_bin: s.lc_bin.clone(),
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
    // SPIRA_RUN via the one source of config (`run_dir`) when the context itself did not
    // already resolve it.
    let run = match &ctx {
        Some((s, _)) => s.run.clone(),
        None => match run_dir() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("landing halt: cannot resolve SPIRA_RUN: {e}");
                return 1;
            }
        },
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
    // SPIRA_PATH is a registered config key (spira/conf.d), declared empty by default — an
    // empty resolved value is exactly the "no override" this call already treated an unset
    // env var as, so it is still passed through as `None` rather than `Some("")`.
    let spira_path = match spira_config::process::cfg("SPIRA_PATH") {
        Ok(v) if !v.is_empty() => Some(v),
        Ok(_) => None,
        Err(e) => {
            eprintln!("landing-pass: {e}");
            return 1;
        }
    };
    let path = halt::child_path(
        ctx.as_ref().and_then(|(s, _)| s.path.as_deref()),
        spira_path.as_deref(),
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
        fixture: s.bdjson_fixture.clone(),
        lc_bin: s.lc_bin.clone(),
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
        fixture: s.bdjson_fixture.clone(),
        lc_bin: s.lc_bin.clone(),
    };
    let lib = RealLib { seam: SeamRunner { home: s.home.clone(), out: &out }, incident: s.incident.clone(), s: s.clone(), beads };
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    lib.ask_rebase_loop(&refs);
    0
}

/// `$SPIRA_RUN` alone — never the lib.sh seam: shelling to bash just to read one
/// already-exported variable would reintroduce the per-call cost wave 4 existed to cut
/// (wave4-decomposition.md's own cost note).
///
/// SPIRA_RUN is a registered config key (spira/conf.d) — the one source of config
/// (`$SPIRA_TOML`, per Ryan 2026-10-05), resolved once per process. No second,
/// process-environment read behind it, and no crate-local derivation of `$SPIRA_HOME` to
/// feed one: an unresolvable config is a refusal naming the key, never a guessed path.
fn run_dir() -> Result<PathBuf, String> {
    let run = spira_config::process::cfg("SPIRA_RUN")?;
    if run.is_empty() {
        return Err("SPIRA_RUN resolved empty".to_string());
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

// ── family R (sp-81t4d, "wave 4.17" — landed verification) ──────────────────────────────
//
// None of these need the full pass context (repositories, the gate/queue tooling): each
// resolves only what it reads, the same `$SPIRA_HOME`-then-resolve-in-process rule
// `run_dir` already follows (law-a-binary-resolves-the-config-it-reads).

/// `$SPIRA_HOME` when exported, else [`harness_home`]'s own three rungs — never a refusal
/// just because a unit's bare environment did not export it.
fn resolve_home() -> Result<PathBuf, String> {
    if let Some(h) = home() {
        return Ok(h);
    }
    let env: BTreeMap<String, String> = std::env::vars().collect();
    harness_home(&env).ok_or_else(|| "SPIRA_HOME is unset and could not be resolved".to_string())
}

/// bd's own connection facts: `SPIRA_DB`/`SPIRA_BD`/`SPIRA_HOME_REPO` are registered keys
/// (`spira/conf.d`), read through `spira_config::process::cfg` — the one source of config
/// (per Ryan 2026-10-05) — never a literal default standing in for an unresolved value.
/// `BD_TIMEOUT`/`SPIRA_BDJSON_FIXTURE` carry no `spira/conf.d/<KEY>` entry — ad hoc
/// overrides `cfg` never produces (aeon's own seam doc names the same split), so these two
/// are still read straight off the raw environment.
fn resolve_beads(home: &Path) -> Result<RealBeads, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let ad_hoc = |k: &str, d: &str| env.get(k).filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| d.to_string());
    Ok(RealBeads {
        home: home.to_path_buf(),
        db: spira_config::process::cfg("SPIRA_DB")?,
        bd: spira_config::process::cfg("SPIRA_BD")?,
        timeout: ad_hoc("BD_TIMEOUT", "180").parse().unwrap_or(180),
        home_repo: spira_config::process::cfg("SPIRA_HOME_REPO")?,
        fixture: env.get("SPIRA_BDJSON_FIXTURE").filter(|s| !s.is_empty()).map(PathBuf::from),
        // Content only (titles): nothing here decides on a bead's state.
        lc_bin: None,
    })
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
/// `SPIRA_WORK_CLOSE_TYPES` is a registered config key (spira/conf.d) — the one source of
/// config; its own declared default is `"task bug feature"`, so this no longer repeats
/// that literal here.
fn is_work_type_cmd(ty: &str) -> i32 {
    let close_types = match spira_config::process::cfg("SPIRA_WORK_CLOSE_TYPES") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("landing-pass: {e}");
            return 1;
        }
    };
    if land_verify::is_work_type(ty, &close_types) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // law-a-binary-resolves-the-config-it-reads: `run_dir` must get a real answer from the
    // one source of config, not a crash, whenever $SPIRA_TOML names a real config file — the
    // production defect this originally guarded against was a landing-pass call silently
    // doing nothing when its process environment lacked $SPIRA_RUN. Per Ryan 2026-10-05
    // ("one source of config"), a bare environment with no $SPIRA_TOML is now a refusal by
    // design, not a derived guess, so this proves resolution through the fixture toml
    // instead of through a raw $SPIRA_RUN env var.
    //
    // This is the ONLY test in this binary that resolves `spira_config::process::cfg` —
    // that door caches its answer for the lifetime of the process (OnceLock), so a second
    // such test in this same test binary would not get an independent answer.
    #[test]
    fn run_dir_resolves_through_the_one_source_of_config() {
        let dir = testkit::TempDir::new("landing-pass-run-dir");
        let home = dir.join("home");
        fs::create_dir_all(home.join("conf.d")).unwrap();
        let run_path = dir.join("the-run-dir");
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_RUN", run_path.to_str().unwrap())]);

        let env = testkit::env(&[
            ("SPIRA_RUN", None),
            ("SPIRA_HOME", home.to_str()),
            ("SPIRA_TOML", toml.to_str()),
        ]);

        let got = run_dir();
        drop(env);

        assert_eq!(got.expect("run_dir must resolve through $SPIRA_TOML"), run_path);
    }
}
