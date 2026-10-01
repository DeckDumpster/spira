//! sentinel — the loop's heartbeat (DESIGN.md). Replaces spira/sentinel.sh.
//!
//!   sentinel                 one full pass                     (spira-sentinel.service)
//!   sentinel --report        print the open plan beads, change nothing
//!   sentinel --summon-only   CHECK 7 alone                     (spira-summon.service, and
//!                            every aeon unit's ExecStopPost)
//!   sentinel --audit         the decoupled audit worker        (the spira-audit unit)
//!   sentinel --open-children [--dry-run]
//!                            CHECK 3c alone over a fresh snapshot (suites; a read-only
//!                            production probe with --dry-run)
//!   sentinel --land-escalate land_escalate alone, stdin `<why>\n<evidence>` (sp-31hjr;
//!                            the real-sender suites' way in, no whole pass)

mod audit;
mod cfg;
mod check4;
mod check5;
mod dispatch;
mod fresh;
mod host;
mod legacy;
mod lifecycle;
mod model;
mod open_children;
mod pass;
mod render;
mod seams;
mod store;
mod summon;
mod temps;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use cfg::{lifecycle_enforce, Context};
use host::{Clock, Host, Io, RealClock, RealRunner, RealSink, Runner, Spec};
use pass::{Mode, Sentinel};

/// DESIGN.md §2.7: SPIRA_HOME from the environment, else the first directory holding a
/// lib.sh among the release and cargo layouts around this executable.
pub fn locate_home(env_home: Option<&str>, exe: &Path) -> Option<PathBuf> {
    if let Some(h) = env_home.filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    let dir = exe.parent()?;
    [
        dir.join("../spira"),
        dir.join("../../spira"),
        dir.join("../../../spira"),
    ]
    .into_iter()
    .find(|c| c.join("lib.sh").is_file())
    .map(|c| c.canonicalize().unwrap_or(c))
}

/// S0 — the context probe, run with this process's own environment.
pub fn probe(r: &dyn Runner, home: &Path, repos: bool) -> Result<Context, String> {
    let mut s = Spec::args_owned(
        "bash",
        vec!["-c".into(), seams::PROBE.into(), "sentinel-probe".into()],
    )
    .env("SENTINEL_LIB", home.join("lib.sh").to_string_lossy())
    .out(Io::Capture)
    .err(Io::Inherit);
    if repos {
        s = s.env("SENTINEL_PROBE_REPOS", "1");
    }
    let o = r.run(&s);
    if !o.ok() {
        return Err(format!(
            "the context probe failed (rc={}) sourcing {}",
            o.rc,
            home.join("lib.sh").display()
        ));
    }
    let mut ctx = Context::parse(o.stdout.as_bytes())?;
    merge_resolved_config(&mut ctx, home);
    Ok(ctx)
}

/// Wave 4.8 ("retire conf re-import seams in Rust"): `seams::PROBE`'s `@vars` section used
/// to be a `compgen -v SPIRA_` dump of the bash subprocess that had just sourced
/// `lib.sh`/`conf.sh` — a second, bash-shaped derivation of values
/// `spira_config::resolve()` already computes in-process. This merges that in-process
/// answer into `ctx.vars` instead: `entry().or_insert()` so anything the probe script
/// ITSELF still supplies (`SPIRA_HOME_REPO_RESOLVED`, `SPIRA_TOML_FILE` — both lib.sh's own,
/// not conf.sh's) is never overwritten, matching conf.sh's own `${VAR:=default}` rule.
/// Best-effort, same as every other config read in this binary: a containment refusal or
/// an unreadable registry leaves `ctx.vars` exactly as the probe script alone produced it,
/// rather than failing the whole pass over a secondary derivation.
fn merge_resolved_config(ctx: &mut Context, home: &Path) {
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let repo = spira_config::resolve::derive_home_repo(home, &env_map);
    // SPIRA_HOME/SPIRA_REPO are deliberately NEVER in resolve()'s own output (per-copy
    // facts — see spira_config::resolve::ResolveInput's doc), but the OLD compgen dump did
    // carry them (conf.sh sets both, unexported, in that bash process) and `Cfg::raw`
    // (cfg.rs) reads both back out of `ctx.vars` to pass on, verbatim, to every aeon
    // sentinel summons (dispatch.rs's own `setenv`) — an empty `--setenv=SPIRA_HOME=` would
    // leave a summoned aeon unable to resolve its own harness. Insert them explicitly
    // before anything resolve() itself produces.
    ctx.vars.entry("SPIRA_HOME".into()).or_insert_with(|| home.to_string_lossy().into_owned());
    ctx.vars.entry("SPIRA_REPO".into()).or_insert_with(|| repo.to_string_lossy().into_owned());
    if let Ok(resolved) = spira_config::resolve::resolve_for_process(home, &repo, &env_map) {
        for (k, v) in resolved.values {
            ctx.vars.entry(k).or_insert(v);
        }
    }
}

fn hostname() -> String {
    let mut b = [0u8; 256];
    let rc = unsafe { libc::gethostname(b.as_mut_ptr() as *mut libc::c_char, b.len()) };
    if rc != 0 {
        return "unknown".into();
    }
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

fn fatal(msg: &str) -> i32 {
    eprintln!(
        "{} spira: FATAL sentinel: {msg}",
        host::utc(RealClock.now())
    );
    1
}

fn main() {
    temps::install_handlers();
    let first = std::env::args().nth(1);
    let second = std::env::args().nth(2);
    let mode = Mode::from_args(first.as_deref(), second.as_deref());
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sentinel"));
    let Some(home) = locate_home(std::env::var("SPIRA_HOME").ok().as_deref(), &exe) else {
        std::process::exit(fatal("cannot find lib.sh (set SPIRA_HOME)"));
    };
    let ctx = match probe(&RealRunner { base_env: None }, &home, mode == Mode::Audit) {
        Ok(c) => c,
        Err(e) => std::process::exit(fatal(&e)),
    };
    // The unit's own environment, not conf.sh's (which defaults the key to 0).
    let toml = ctx
        .get("SPIRA_TOML_FILE")
        .filter(|t| !t.is_empty())
        .map(PathBuf::from);
    let lc = lifecycle_enforce(
        std::env::var("SPIRA_LIFECYCLE_ENFORCE").ok().as_deref(),
        toml.as_deref(),
    );
    let runner = RealRunner {
        base_env: Some(ctx.env.clone()),
    };
    let (clock, sink) = (RealClock, RealSink);
    let h = Host::new(&runner, &clock, &sink);
    let pass_id = format!("{}-{}-{}", hostname(), std::process::id(), RealClock.now());
    let s = Sentinel::new(
        &h,
        ctx,
        &home,
        mode,
        exe.to_string_lossy().into_owned(),
        pass_id,
        lc,
    );
    let code = s.run();
    temps::cleanup();
    std::process::exit(code);
}
