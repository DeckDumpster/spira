//! `rebase-stale <bead-id> [repo]` — exit 0 ok/nothing to do, 1 real conflict, 2 red gate,
//! 3 not attempted. See DESIGN.md.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

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

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(id) = args.next().filter(|s| !s.is_empty()) else {
        eprintln!("rebase-stale: bead id required\nusage: rebase-stale <bead-id> [repo]");
        return ExitCode::from(3);
    };
    let repo_arg = args.next();

    let keys = Keys::load();
    let Some(home) = env::var_os("SPIRA_HOME").map(PathBuf::from) else {
        eprintln!("rebase-stale: SPIRA_HOME is not set (lib.sh and queue.sh live there)");
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
        keys.get("SPIRA_GOAL", |s| s.goal.clone())
            .unwrap_or_else(|| "sp-spira".into()),
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
