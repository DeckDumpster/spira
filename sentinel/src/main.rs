//! sentinel — the loop's heartbeat (DESIGN.md). Replaces spira/sentinel.sh.
//!
//!   sentinel                 one full pass                     (spira-sentinel.service)
//!   sentinel --report        print the open beads under SPIRA_GOAL, change nothing
//!   sentinel --summon-only   CHECK 7 alone                     (spira-summon.service, and
//!                            every aeon unit's ExecStopPost)
//!   sentinel --audit         the decoupled audit worker        (the spira-audit unit)

mod audit;
mod cfg;
mod check4;
mod check5;
mod dispatch;
mod host;
mod lifecycle;
mod model;
mod pass;
mod render;
mod seams;
mod store;
mod summon;
mod temps;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use cfg::Context;
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
    Context::parse(o.stdout.as_bytes())
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
    let mode = Mode::from_first(first.as_deref());
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sentinel"));
    let Some(home) = locate_home(std::env::var("SPIRA_HOME").ok().as_deref(), &exe) else {
        std::process::exit(fatal("cannot find lib.sh (set SPIRA_HOME)"));
    };
    let ctx = match probe(&RealRunner { base_env: None }, &home, mode == Mode::Audit) {
        Ok(c) => c,
        Err(e) => std::process::exit(fatal(&e)),
    };
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
    );
    let code = s.run();
    temps::cleanup();
    std::process::exit(code);
}
