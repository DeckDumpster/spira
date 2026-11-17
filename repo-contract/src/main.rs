//! `repo-contract check <repo>|--all [--tree <dir>] [--clauses <csv>]`: read-only; one line per
//! clause. Exit 0 all met, 1 any not met, 2 cannot tell.

use repo_contract::clause::{self, Clause};
use repo_contract::ports::Ports;
use repo_contract::real::Real;
use std::path::PathBuf;

const USAGE: &str = "usage: repo-contract check <repo>|--all [--tree <dir>] [--clauses <csv>]";

struct Args {
    repo: Option<String>,
    tree: Option<PathBuf>,
    clauses: Vec<Clause>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { repo: None, tree: None, clauses: clause::ALL.to_vec() };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--all" => a.repo = Some("--all".into()),
            "--tree" => a.tree = Some(it.next().ok_or(USAGE)?.into()),
            "--clauses" => {
                a.clauses = it
                    .next()
                    .ok_or(USAGE)?
                    .split(',')
                    .map(|c| Clause::parse(c).ok_or_else(|| format!("no clause {c:?}; the clauses are {}", clause::ALL.map(Clause::name).join(", "))))
                    .collect::<Result<_, _>>()?;
            }
            r if !r.starts_with("--") && a.repo.is_none() => a.repo = Some(r.to_string()),
            _ => return Err(USAGE.into()),
        }
    }
    if a.repo.is_none() || (a.repo.as_deref() == Some("--all") && a.tree.is_some()) {
        return Err(USAGE.into());
    }
    Ok(a)
}

fn main() {
    spira_otel::run::begin("repo-contract", spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref());
    let code = run();
    spira_otel::run::finish(code);
    std::process::exit(code);
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("check") {
        eprintln!("{USAGE}");
        return 2;
    }
    let a = match parse(&args[1..]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let ports = match Real::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("repo-contract: {e}");
            return 2;
        }
    };
    let all = a.repo.as_deref() == Some("--all");
    let repos = if all { ports.product_repos() } else { vec![a.repo.clone().unwrap_or_default()] };
    let mut code = 0;
    for repo in &repos {
        if !ports.registered(repo) {
            eprintln!("repo-contract: {repo} is not in the repo registry");
            code = 2;
            continue;
        }
        if all {
            println!("== {repo}");
        }
        let c = clause::run(&ports, repo, a.tree.as_deref(), &a.clauses, &mut |l| println!("{l}"));
        code = match (code, c) {
            (1, _) | (_, 1) => 1,
            (2, _) | (_, 2) => 2,
            _ => 0,
        };
    }
    code
}
