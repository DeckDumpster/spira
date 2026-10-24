//! acceptance-daily pass --state FILE
//!
//! Exit 0 a verdict was reached and every red it carried is filed; 1 no verdict, or a red could
//! not be filed (named on stderr as ALARM); 2 the configuration is unusable.

use acceptance_daily::{pass, real::Real, State};
use spira_otel::metrics::MetricsSink;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: acceptance-daily pass --state FILE";
const MAX_WAITS: u32 = 240;

fn main() -> ExitCode {
    spira_otel::run::begin("acceptance-daily", spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref());
    let code = run();
    spira_otel::run::finish(i32::from(code));
    ExitCode::from(code)
}

fn land_ref(repo: &str) -> String {
    let has_local = std::process::Command::new("git").args(["-C", repo, "rev-parse", "--verify", "-q", "local/main^{commit}"]).output().map(|o| o.status.success()).unwrap_or(false);
    if has_local {
        return "local/main".into();
    }
    let home = std::env::var("SPIRA_HOME").unwrap_or_default();
    let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&home));
    spira_config::repos::landref(&reg, repo).unwrap_or_else(|| "local/main".into())
}

fn run() -> u8 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let state_path = match argv.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["pass", "--state", f] => PathBuf::from(f),
        _ => {
            eprintln!("acceptance-daily: {USAGE}");
            return 2;
        }
    };
    let need = |k: &str| spira_config::process::cfg(k).ok().filter(|s| !s.is_empty());
    let (Some(repo), Some(run_dir)) = (need("SPIRA_REPO"), need("SPIRA_RUN")) else {
        eprintln!("acceptance-daily: SPIRA_REPO and SPIRA_RUN must both be configured");
        return 2;
    };
    let run_dir = PathBuf::from(run_dir);
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    let real = Real {
        land_ref: land_ref(&repo),
        tree: run_dir.join("acceptance-daily").join("tree"),
        run: run_dir,
        repo,
        accept: "acceptance-local.sh".into(),
        bead: "bead.sh".into(),
        lc: spira_config::lifecycle_row::lc_bin(),
        sink: MetricsSink::for_component(endpoint.as_deref(), "acceptance-daily"),
    };
    let _ = std::fs::create_dir_all(real.run.join("acceptance-daily"));
    let mut state = State::parse(&std::fs::read_to_string(&state_path).unwrap_or_default());
    let mut save = |s: &State| std::fs::write(&state_path, s.render()).map_err(|e| format!("cannot record state in {}: {e}", state_path.display()));
    let o = pass(&real, &mut state, &mut save, MAX_WAITS);
    println!("acceptance-daily: {} {:?}: {} filed, {} amended, {} failed", o.commit, o.verdict, o.filed.len(), o.amended.len(), o.failed.len());
    for f in &o.failed {
        eprintln!("acceptance-daily: ALARM {f}");
    }
    u8::from(!o.failed.is_empty())
}
