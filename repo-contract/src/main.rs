//! `repo-contract check <repo>|--all [--tree <dir>] [--clauses <csv>] [--cached-shell]`: read-only; one line per
//! clause. Exit 0 all met, 1 any not met, 2 cannot tell.
//! `repo-contract scaffold <repo> [--into <dir>]`: writes the contract files the tree lacks.

use repo_contract::clause::{self, Clause};
use repo_contract::ports::Ports;
use repo_contract::real::Real;
use std::path::PathBuf;

const SCAFFOLD_USAGE: &str = "usage: repo-contract scaffold <repo> [--into <dir>]";
const USAGE: &str = "usage: repo-contract check <repo>|--all [--tree <dir>] [--clauses <csv>] [--cached-shell]";

struct Args {
    repo: Option<String>,
    tree: Option<PathBuf>,
    clauses: Vec<Clause>,
    cached_shell: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { repo: None, tree: None, clauses: clause::ALL.to_vec(), cached_shell: false };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--cached-shell" => a.cached_shell = true,
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
    if args.first().map(String::as_str) == Some("scaffold") {
        return scaffold(&args[1..]);
    }
    if args.first().map(String::as_str) != Some("check") {
        eprintln!("{USAGE}\n{SCAFFOLD_USAGE}");
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
        Ok(p) => p.cached_shell(a.cached_shell),
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

fn git_out(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = spira_config::bounded::bounded("git").args(args).current_dir(dir).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn scaffold(args: &[String]) -> i32 {
    match scaffold_in(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("repo-contract: {e}");
            2
        }
    }
}

fn scaffold_in(args: &[String]) -> Result<(), String> {
    use repo_contract::scaffold as sc;
    let (mut repo, mut into) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--into" => into = Some(PathBuf::from(it.next().ok_or(SCAFFOLD_USAGE)?)),
            r if !r.starts_with("--") && repo.is_none() => repo = Some(r.to_string()),
            _ => return Err(SCAFFOLD_USAGE.into()),
        }
    }
    let repo = repo.ok_or(SCAFFOLD_USAGE)?;
    let ports = Real::from_env()?;
    if !ports.registered(&repo) {
        return Err(format!("{repo} is not in the repo registry"));
    }
    let into = match into {
        Some(d) => d,
        None => {
            let cwd = std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
            git_out(&cwd, &["rev-parse", "--show-toplevel"]).map(PathBuf::from).ok_or("the current directory is not in a git worktree; pass --into <a worktree>")?
        }
    };
    let land = ports.land(&repo)?;
    let head = git_out(&into, &["symbolic-ref", "-q", "--short", "HEAD"]);
    sc::guard_target(&into, std::path::Path::new(&ports.root(&repo)?), head.as_deref(), &spira_config::repos::ref_branch(&land))?;
    let release = spira_config::release_env::release_root_for_process()?;
    let read = |rel: &str| std::fs::read_to_string(release.join(rel)).map_err(|e| format!("{}: {e}", release.join(rel).display()));
    let table = sc::ToolTable::parse(&read(sc::TOOLS)?)?;
    let plan = sc::plan(&into, &table, &sc::nixpkgs_rev(&read("flake.lock")?)?, &land)?;
    let mut written = sc::write(&into, &plan)?;
    for n in &plan.notes {
        println!("{n}");
    }
    if !into.join("flake.lock").exists() && into.join("flake.nix").exists() {
        let nix = repo_contract::shell::nix_bin()?;
        let out = spira_config::bounded::bounded_for(&nix, "600").args(["flake", "lock"]).arg(format!("path:{}", into.display())).output().map_err(|e| format!("cannot run {}: {e}", nix.display()))?;
        if !out.status.success() {
            return Err(format!("nix flake lock: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        written.push(into.join("flake.lock"));
    }
    let rels: Vec<String> = written.iter().filter_map(|w| w.strip_prefix(&into).ok().map(|r| r.display().to_string())).collect();
    if !rels.is_empty() && git_out(&into, &["add", "-N", "--"].iter().copied().chain(rels.iter().map(String::as_str)).collect::<Vec<_>>()).is_none() {
        println!("git add -N failed; nix reads only tracked files, so run `git add -N {}` before `check`", rels.join(" "));
    }
    for w in &written {
        println!("wrote {}", w.display());
    }
    Ok(())
}
