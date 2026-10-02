//! suite-select — the one suite selector's binary (DESIGN.md "The binary").
//!
//!   suite-select select (--all | --base <ref> --head <ref> | --files <path>) [options]
//!   suite-select gate <base> <head>
//!   suite-select budget --budget-secs <n> [--parallel-width <n>] [--runs <n>] [--suite-dir <dir>]
//!   suite-select header (covers|tier|uc|requires|exclusive|selects-on|testenv-unmet) <file>
//!
//! Exit: 0 a selection; 1 an unclaimed source file; 2 usage; 75 refused. `header
//! testenv-unmet` is a predicate: exit 0 means unmet (the bash callers' `if ... ; then`).

use std::io::Read;
use std::path::{Path, PathBuf};
use suite_select::budget::{self, TierCaps};
use suite_select::corpus::Corpus;
use suite_select::header;
use suite_select::io::{self, RealGit};
use suite_select::select::{self, Buckets, Fail, Options, Selection};
use suite_select::{gate, timing, Refusal, EXIT_REFUSED, EXIT_UNCLAIMED, EXIT_USAGE};

const USAGE: &str = "usage: suite-select select (--all | --base <ref> --head <ref> | --files <path>) [--repo <path>] [--suite-dir <dir>] [--mode-file <path>] [--report-file <path>] [--no-all-fallback] [--no-nocov] [--tiers <csv>]
       suite-select gate <base> <head>
       suite-select budget --budget-secs <n> [--parallel-width <n>] [--runs <n>] [--suite-dir <dir>]
       suite-select header (covers|tier|uc|requires|exclusive|selects-on|testenv-unmet) <file>";

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn usage(msg: &str) -> i32 {
    eprintln!("suite-select: {msg}");
    eprintln!("{USAGE}");
    EXIT_USAGE
}

fn refused(what: &str, r: &Refusal) -> i32 {
    eprintln!("{what}: REFUSED — {r}");
    EXIT_REFUSED
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("select") => cmd_select(&args[1..]),
        Some("gate") => cmd_gate(&args[1..]),
        Some("budget") => cmd_budget(&args[1..]),
        Some("header") => cmd_header(&args[1..]),
        Some("-h") | Some("--help") => {
            println!("{USAGE}");
            0
        }
        Some(o) => usage(&format!("unknown command: {o}")),
        None => usage("a command is required"),
    };
    std::process::exit(code);
}

/// `--flag value` or `--flag=value`.
fn take(args: &[String], i: &mut usize, flag: &str) -> Result<Option<String>, String> {
    let a = &args[*i];
    if a == flag {
        let v = args.get(*i + 1).ok_or_else(|| format!("{flag} requires an argument"))?;
        *i += 2;
        return Ok(Some(v.clone()));
    }
    if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
        *i += 1;
        return Ok(Some(v.to_string()));
    }
    Ok(None)
}

#[derive(Default)]
struct SelArgs {
    all: bool,
    base: Option<String>,
    head: Option<String>,
    files: Option<String>,
    repo: Option<String>,
    suite_dir: Option<String>,
    mode_file: Option<String>,
    report_file: Option<String>,
    opts: Options,
}

fn parse_select(args: &[String]) -> Result<SelArgs, String> {
    let mut a = SelArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => {
                a.all = true;
                i += 1;
                continue;
            }
            "--no-all-fallback" => {
                a.opts.no_all_fallback = true;
                i += 1;
                continue;
            }
            "--no-nocov" => {
                a.opts.no_nocov = true;
                i += 1;
                continue;
            }
            _ => {}
        }
        let mut hit = false;
        for (flag, slot) in [
            ("--base", &mut a.base),
            ("--head", &mut a.head),
            ("--files", &mut a.files),
            ("--repo", &mut a.repo),
            ("--suite-dir", &mut a.suite_dir),
            ("--mode-file", &mut a.mode_file),
            ("--report-file", &mut a.report_file),
        ] {
            if let Some(v) = take(args, &mut i, flag)? {
                *slot = Some(v);
                hit = true;
                break;
            }
        }
        if hit {
            continue;
        }
        if let Some(v) = take(args, &mut i, "--tiers")? {
            a.opts.tiers = select::parse_tiers(&v);
            continue;
        }
        return Err(format!("unknown option: {}", args[i]));
    }
    let diff = a.base.is_some() || a.head.is_some();
    if a.all && (diff || a.files.is_some()) {
        return Err("--all is mutually exclusive with --base/--head and --files".into());
    }
    if a.files.is_some() && diff {
        return Err("--files is mutually exclusive with --base/--head".into());
    }
    if diff && (a.base.is_none() || a.head.is_none()) {
        return Err("--base and --head must be given together".into());
    }
    if !a.all && !diff && a.files.is_none() {
        return Err("one of --all, --base/--head, --files is required".into());
    }
    Ok(a)
}

fn write(path: &Option<String>, text: &str) -> Result<(), Refusal> {
    if let Some(p) = path {
        std::fs::write(p, text).map_err(|e| Refusal(format!("cannot write {p}: {e}")))?;
    }
    Ok(())
}

fn cmd_select(args: &[String]) -> i32 {
    let a = match parse_select(args) {
        Ok(a) => a,
        Err(e) => return usage(&e),
    };
    let suite_dir = PathBuf::from(
        a.suite_dir
            .clone()
            .or_else(|| env("SPIRA_BATCH_SUITE_DIR").filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "spira".into()),
    );
    let repo = a
        .repo
        .clone()
        .or_else(|| env("SPIRA_REPO").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(|| io::parent_of(&suite_dir));
    let buckets = Buckets::from_env(&env);
    let mut a = a;
    match suite_select::reach::Reach::load(&repo) {
        Ok(r) => a.opts.reach = r,
        Err(r) => return refused("select", &r),
    }
    let corpus = match Corpus::load(&suite_dir) {
        Ok(c) => c,
        Err(r) => return refused("select", &r),
    };
    let g = RealGit;
    let result: Result<Selection, Fail> = if a.all {
        Ok(select::all(&corpus, &a.opts))
    } else if let Some(f) = &a.files {
        io::file_changes(Path::new(f))
            .map_err(Fail::from)
            .and_then(|ch| select::select(&corpus, &ch, &mut |_| vec![], &buckets, &a.opts))
    } else {
        io::select_diff(
            &g,
            &repo,
            &corpus,
            a.base.as_deref().unwrap_or(""),
            a.head.as_deref().unwrap_or(""),
            &buckets,
            &a.opts,
        )
    };
    let unplaced = match &result {
        Ok(s) => s.unplaced.clone(),
        Err(Fail::Unclaimed { unplaced, .. }) => unplaced.clone(),
        Err(Fail::Refused(r)) => return refused("select", r),
    };
    if a.report_file.is_some() {
        let rep = match io::report(&g, &repo, &corpus, &unplaced) {
            Ok(t) => t,
            Err(r) => return refused("select", &r),
        };
        if let Err(r) = write(&a.report_file, &rep) {
            return refused("select", &r);
        }
    }
    match result {
        Err(Fail::Unclaimed { files, .. }) => {
            for f in files {
                eprintln!("select: unclaimed source file: {f}");
            }
            EXIT_UNCLAIMED
        }
        Err(Fail::Refused(r)) => refused("select", &r),
        Ok(s) => {
            if let Err(r) = write(&a.mode_file, &format!("{}\n", s.mode.word())) {
                return refused("select", &r);
            }
            for l in &s.log {
                eprintln!("{l}");
            }
            let mut out = String::new();
            for n in &s.suites {
                out.push_str(n);
                out.push('\n');
            }
            print!("{out}");
            0
        }
    }
}

fn cmd_gate(args: &[String]) -> i32 {
    let [base, head] = args else {
        return usage("gate takes <base> <head>");
    };
    let genv = match gate::GateEnv::from_env(&env) {
        Ok(e) => e,
        Err(r) => return refused("suite-select gate", &r),
    };
    match gate::run(&genv, &RealGit, base, head) {
        Ok(o) => {
            for l in &o.log {
                eprintln!("{l}");
            }
            for n in &o.suites {
                println!("{n}");
            }
            0
        }
        Err(Fail::Unclaimed { files, .. }) => {
            for f in files {
                eprintln!("select: unclaimed source file: {f}");
            }
            eprintln!("suite-select gate: the branch changes source files no suite claims — add the file to a suite's # covers:");
            EXIT_UNCLAIMED
        }
        Err(Fail::Refused(r)) => refused("suite-select gate", &r),
    }
}

fn cmd_budget(args: &[String]) -> i32 {
    let mut budget: Option<String> = None;
    let mut width = "1".to_string();
    let mut runs = "20".to_string();
    let mut dir = env("SPIRA_BATCH_SUITE_DIR").filter(|v| !v.is_empty()).unwrap_or_else(|| "spira".into());
    let mut i = 0;
    while i < args.len() {
        let r = (|| -> Result<bool, String> {
            if let Some(v) = take(args, &mut i, "--budget-secs")? {
                budget = Some(v);
            } else if let Some(v) = take(args, &mut i, "--parallel-width")? {
                width = v;
            } else if let Some(v) = take(args, &mut i, "--runs")? {
                runs = v;
            } else if let Some(v) = take(args, &mut i, "--suite-dir")? {
                dir = v;
            } else {
                return Ok(false);
            }
            Ok(true)
        })();
        match r {
            Ok(true) => {}
            Ok(false) => return usage(&format!("unknown option: {}", args[i])),
            Err(e) => return usage(&e),
        }
    }
    let Some(b) = budget.as_deref().and_then(budget::number) else {
        return usage("--budget-secs must be a number");
    };
    let width: u64 = width.parse().ok().filter(|w| *w > 0).unwrap_or(1);
    let runs: usize = runs.parse().ok().filter(|r| *r > 0).unwrap_or(20);
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        return refused("suite-select budget", &Refusal(format!("cannot read stdin: {e}")));
    }
    let mut names: Vec<String> = input.split_whitespace().map(str::to_string).collect();
    names.sort();
    names.dedup();
    let caps = match TierCaps::from_env(&env) {
        Ok(c) => c,
        Err(r) => return refused("suite-select budget", &r),
    };
    let corpus = if names.is_empty() {
        Corpus::default()
    } else {
        match Corpus::load_named(Path::new(&dir), &names) {
            Ok(c) => c,
            Err(r) => return refused("suite-select budget", &r),
        }
    };
    let p90 = match env("SPIRA_RUN").filter(|v| !v.is_empty()) {
        Some(r) => match timing::load(Path::new(&r), runs) {
            Ok(p) => p.by_suite,
            Err(r) => return refused("suite-select budget", &r),
        },
        None => Default::default(),
    };
    let cands: Vec<&suite_select::corpus::Suite> = corpus.suites.iter().collect();
    let cut = budget::fill(&budget::rank(&cands, &p90, &caps), b, width);
    for l in &cut.log {
        eprintln!("{l}");
    }
    for n in &cut.selected {
        println!("{n}");
    }
    0
}

/// Suite-header text at `path`, or empty when it cannot be read — the same fail-soft the
/// bash accessors carried (`2>/dev/null || true`): a missing or unreadable file is an
/// undeclared header, never a hard error, because every caller already treats "undeclared"
/// as its own valid (usually "run always"/"not required") case.
fn read_header(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// `suite_in_container`: true only with STRUCTURAL evidence of running inside a container
/// (podman writes `/run/.containerenv`, docker `/.dockerenv`) — never from a variable alone,
/// which is forgeable by exactly the actor the guard exists to bind (law-guard-binds-the-caller).
fn in_container() -> bool {
    Path::new("/run/.containerenv").exists() || Path::new("/.dockerenv").exists()
}

fn cmd_header(args: &[String]) -> i32 {
    let [sub, file] = args else {
        return usage("header takes (covers|tier|uc|requires|exclusive|selects-on|testenv-unmet) <file>");
    };
    let text = read_header(file);
    match sub.as_str() {
        "covers" => {
            println!("{}", header::covers_of(&text).unwrap_or_default().join(" "));
            0
        }
        "tier" => {
            println!("{}", header::tier_of(&text).unwrap_or_default());
            0
        }
        "uc" => {
            println!("{}", header::uc_of(&text).join(" "));
            0
        }
        "requires" => {
            println!("{}", header::requires_of(&text).join(" "));
            0
        }
        "exclusive" => {
            println!("{}", header::exclusive_of(&text).unwrap_or_default());
            0
        }
        "selects-on" => {
            println!("{}", header::selects_on_of(&text).join(" "));
            0
        }
        "testenv-unmet" => {
            let in_testenv = env("SPIRA_IN_TESTENV").as_deref() == Some("1");
            let reqs = header::requires_of(&text);
            if header::testenv_unmet(&reqs, in_testenv, in_container()) {
                0
            } else {
                1
            }
        }
        other => usage(&format!("unknown header query: {other}")),
    }
}
