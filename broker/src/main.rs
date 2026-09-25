mod execute;
mod policy;
mod read;
mod repo_map;
mod submit;
mod token;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str).unwrap_or("");

    match sub {
        "submit"  => run_submit(&args[2..]),
        "execute" => run_execute(),
        "read"    => run_read(&args[2..]),
        "token"   => run_token(),
        _ => {
            eprintln!("broker: usage: broker submit <verb> <target> --reason <text> --bead <id> [--class <class>]");
            eprintln!("broker:        broker execute");
            eprintln!("broker:        broker read <verb> <repo>/<id> [--artifact <name>] [--output-dir <path>]");
            eprintln!("broker:        broker token");
            ExitCode::from(2)
        }
    }
}

fn run_submit(args: &[String]) -> ExitCode {
    // submit <verb> <target> --reason <text> --bead <id> [--class <class>]
    // target format: <repo>/<number>
    if args.len() < 2 {
        eprintln!("broker submit: usage: broker submit <verb> <target> --reason <text> --bead <id>");
        return ExitCode::from(2);
    }

    let verb_str = &args[0];
    let target   = &args[1];

    let verb = match policy::Verb::parse(verb_str) {
        Some(v) => v,
        None => {
            eprintln!("broker submit: unknown verb '{}'; allowed: run-rerun run-cancel pr-close pr-comment issue-comment issue-close", verb_str);
            return ExitCode::from(2);
        }
    };

    let (repo, number) = match target.rfind('/') {
        Some(i) => (target[..i].to_string(), target[i+1..].to_string()),
        None => {
            eprintln!("broker submit: target must be <repo>/<number>, got '{}'", target);
            return ExitCode::from(2);
        }
    };
    if number.is_empty() || repo.is_empty() {
        eprintln!("broker submit: target must be <repo>/<number>, got '{}'", target);
        return ExitCode::from(2);
    }

    let mut reason = String::new();
    let mut bead   = String::new();
    let mut class: Option<String> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--reason" => { i += 1; if i < args.len() { reason = args[i].clone(); } }
            "--bead"   => { i += 1; if i < args.len() { bead   = args[i].clone(); } }
            "--class"  => { i += 1; if i < args.len() { class  = Some(args[i].clone()); } }
            f => { eprintln!("broker submit: unknown flag '{}'", f); return ExitCode::from(2); }
        }
        i += 1;
    }

    if reason.is_empty() {
        eprintln!("broker submit: --reason is required");
        return ExitCode::from(2);
    }
    if bead.is_empty() {
        eprintln!("broker submit: --bead is required");
        return ExitCode::from(2);
    }

    match submit::run(submit::Args { verb, repo, number, reason, bead, class }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("{}", e); ExitCode::FAILURE }
    }
}

fn run_execute() -> ExitCode {
    match execute::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("{}", e); ExitCode::FAILURE }
    }
}

fn run_read(args: &[String]) -> ExitCode {
    match read::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("{}", e); ExitCode::FAILURE }
    }
}

fn run_token() -> ExitCode {
    match token::mint() {
        Ok(t)  => { println!("{}", t); ExitCode::SUCCESS }
        Err(e) => { eprintln!("broker token: {}", e); ExitCode::FAILURE }
    }
}
