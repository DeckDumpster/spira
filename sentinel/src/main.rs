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

use cfg::{lifecycle_enforce, Context, Repo};
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

/// S0 — the context probe, run with this process's own environment. `repos` (audit's own
/// need, CHECK5) no longer widens the bash script (sp-k6lku, "wave 4.13") — it now gates
/// [`resolve_repos`], which reads the probe's own `@vars` section (already every `SPIRA_*`
/// key, exported or not) to build a `spira_config::repos::Registry` in-process.
pub fn probe(r: &dyn Runner, home: &Path, repos: bool) -> Result<Context, String> {
    let s = Spec::args_owned(
        "bash",
        vec!["-c".into(), seams::PROBE.into(), "sentinel-probe".into()],
    )
    .env("SENTINEL_LIB", home.join("lib.sh").to_string_lossy())
    .out(Io::Capture)
    .err(Io::Inherit);
    let o = r.run(&s);
    if !o.ok() {
        return Err(format!(
            "the context probe failed (rc={}) sourcing {}",
            o.rc,
            home.join("lib.sh").display()
        ));
    }
    let mut ctx = Context::parse(o.stdout.as_bytes())?;
    if repos {
        ctx.repos = resolve_repos(&ctx.vars, home);
    }
    Ok(ctx)
}

/// `spira_repos` (every mapped repository, home first), then per repository `repo_root`/
/// `spira_landrefs`/`repo_land_queued` — in-process via `spira_config::repos` instead of
/// the bash `@repos` block this replaced (sp-k6lku, "wave 4.13"). `vars` is the probe's own
/// `@vars` section: `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`
/// travel through it already, so this needs no seam call of its own.
fn resolve_repos(vars: &std::collections::BTreeMap<String, String>, home: &Path) -> Vec<Repo> {
    let map_text = vars.get("SPIRA_REPO_MAP").filter(|p| !p.is_empty()).and_then(|p| std::fs::read_to_string(p).ok());
    let reg = spira_config::repos::Registry::new(map_text.as_deref(), vars, home);
    reg.all()
        .into_iter()
        .map(|name| {
            let root = reg.root(&name);
            let landrefs = root
                .as_deref()
                .and_then(|r| spira_config::repos::landrefs(&reg, r))
                .map(|(base, local)| std::iter::once(base).chain(local).collect())
                .unwrap_or_default();
            Repo { name: name.clone(), root, landrefs, queued: reg.land_queued(&name) }
        })
        .collect()
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
