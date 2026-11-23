//! `cr pick | count | open | seal | unseal | close | thread | reply | status` — see `cr -h`.

use cr::cli::{self, Outcome};
use cr::ports::{Clock, Config, GitTree, Ports, VirtualClock, WallClock};
use spira_config::process::cfg_parse;
use spira_config::repos::{Column, Registry};
use std::collections::BTreeMap;

fn source_url(root: &str) -> Result<String, String> {
    let remote = spira_config::process::cfg("SPIRA_PUBLISH_REMOTE").map_err(|e| format!("SPIRA_PUBLISH_REMOTE: {e}"))?;
    let out = spira_config::bounded::bounded("git").args(["-C", root, "remote", "get-url", &remote]).output().map_err(|e| format!("git remote get-url {remote}: {e}"))?;
    if !out.status.success() {
        return Err(format!("remote {remote:?} (SPIRA_PUBLISH_REMOTE) has no URL in {root}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let url = String::from_utf8_lossy(&out.stdout);
    let slug = gh_intake::model::github_slug(&url).ok_or_else(|| format!("remote {remote:?} (SPIRA_PUBLISH_REMOTE) is not a github.com URL: {}", url.trim()))?;
    Ok(format!("https://github.com/{slug}"))
}

fn build_ports() -> Result<Ports, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = Registry::from_env(env, &home);
    let repo = reg.home_repo().to_string();
    let root = reg.root(&repo).ok_or_else(|| format!("the repo map has no path for the home repository {repo:?}"))?;
    let base = reg.field(&repo, Column::Base).filter(|b| !b.is_empty()).ok_or_else(|| format!("the repo map names no base for the home repository {repo:?}"))?;
    let tree = GitTree::new(&root, &base)?;
    let exclude: String = cfg_parse("SPIRA_CR_EXCLUDE")?;
    let config = Config {
        size_unit_lines: cfg_parse("SPIRA_CR_SIZE_UNIT_LINES")?,
        churn_days: cfg_parse("SPIRA_CR_CHURN_DAYS")?,
        rewrite_days: cfg_parse("SPIRA_CR_REWRITE_DAYS")?,
        count_limit: cfg_parse::<u32>("SPIRA_CR_COUNT_LIMIT")? as usize,
        status_limit: cfg_parse::<u32>("SPIRA_CR_STATUS_LIMIT")? as usize,
        exclude: exclude.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
    };
    let clock: Box<dyn Clock> = match spira_config::vtime::override_epoch() {
        Some(secs) => Box::new(VirtualClock::new(secs.saturating_mul(1000))),
        None => Box::new(WallClock),
    };
    let run_dir = spira_config::resolve::run_dir_for_process()?;
    let registry_path = test_registry::cli::with_run_default(spira_config::process::cfg, || Ok(run_dir.to_string_lossy().into_owned()))(test_registry::cli::PATH_KEY)?;
    let key = |k: &str| spira_config::process::cfg(k).unwrap_or_default();
    let stdin = if matches!(std::env::args().nth(1).as_deref(), Some("seal" | "finding" | "reply")) {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(|e| format!("reading stdin: {e}"))?;
        s
    } else {
        String::new()
    };
    Ok(Ports {
        run_dir,
        margin: match margin_client::HttpMargin::configured(spira_config::process::cfg) {
            Ok(m) => Box::new(m),
            Err(e) => Box::new(margin_client::Unconfigured(e)),
        },
        findings_dir: key(cr::findings::DIR_KEY),
        committer: Box::new(cr::ports::GitCommitter),
        source_url: source_url(&root),
        source_repo: repo.clone(),
        stdin,
        clock, tree: Box::new(tree), beads: Box::new(cr::ports::LcBeads { incident_label: key("SPIRA_INCIDENT_LABEL") }), registry: Box::new(cr::ports::SqliteRegistry { path: registry_path }), config,
        env_seed: spira_config::process::cfg("SPIRA_SEED").ok().filter(|s| !s.is_empty()),
    })
}

fn emit(o: &Outcome) -> std::process::ExitCode {
    print!("{}", o.stdout);
    eprint!("{}", o.stderr);
    std::process::ExitCode::from(o.code.clamp(0, 255) as u8)
}

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if cli::wants_help(&argv) {
        return emit(&Outcome { code: 0, stdout: cli::HELP.to_string(), stderr: String::new() });
    }
    let metrics = spira_otel::metrics::Run::begin(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "cr", argv.first().map(String::as_str).filter(|v| matches!(*v, "pick" | "count" | "open" | "seal" | "unseal" | "close" | "thread" | "reply" | "status" | "finding" | "findings")).unwrap_or("none"));
    let outcome = match build_ports() {
        Ok(ports) => cli::run(&argv, &ports),
        Err(e) => cli::error_outcome(1, cli::wants_json(&argv), &e),
    };
    metrics.end(spira_otel::metrics::outcome_of_exit(std::process::ExitCode::from(outcome.code.clamp(0, 255) as u8)));
    emit(&outcome)
}
