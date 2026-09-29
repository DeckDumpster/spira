//! testenv — select suites for a tree, build that tree in place, run the suites in the
//! fixture container, report the verdict. Contract: DESIGN.md.

use std::io::Read;
use std::process::ExitCode;
use testenv::build::Cargo;
use testenv::cli;
use testenv::run::{self, Deps, Harness};
use testenv::runtime::{self, Podman};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `testenv suites …` — the suite-state tooling (DESIGN-suites.md). A branch literally
    // named `suites` is `testenv -- suites`.
    if args.first().map(String::as_str) == Some("suites") {
        let rc = testenv::suites::main(&args[1..]);
        return ExitCode::from(rc.clamp(0, 255) as u8);
    }
    // `testenv testdb …` — server-mode test databases (DESIGN-testdb.md).
    if args.first().map(String::as_str) == Some("testdb") {
        let rc = testenv::testdb::main(&args[1..]);
        return ExitCode::from(rc.clamp(0, 255) as u8);
    }
    let inv = match cli::parse(&args) {
        Ok(i) => i,
        Err(cli::UsageError(lines)) => {
            for l in lines {
                eprintln!("{l}");
            }
            println!("VERDICT FAULT rc=2 ran=0 reason=usage");
            return ExitCode::from(2);
        }
    };
    runtime::install_signal_handlers();
    let env = |k: &str| std::env::var(k).ok();
    let Some(harness) = Harness::locate(&env) else {
        eprintln!("batch: cannot find the harness (spira/testenv.sh) above the testenv binary; set SPIRA_TESTENV_HARNESS");
        println!("VERDICT FAULT rc=2 ran=0 reason=harness-missing");
        return ExitCode::from(2);
    };
    let config = spira_config::discover(None).and_then(|p| match spira_config::load(&p) {
        Ok(doc) => Some(doc),
        Err(e) => {
            eprintln!("batch: ignoring unreadable config: {e}");
            None
        }
    });
    let rt = Podman {
        testenv_sh: harness.script("testenv.sh"),
    };
    let stdin = || {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        s
    };
    let out = |l: &str| {
        use std::io::Write;
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{l}");
        let _ = o.flush();
    };
    let identity = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .unwrap_or_default();
    let deps = Deps {
        rt: &rt,
        builder: &Cargo,
        harness,
        env: &env,
        config: config.as_ref(),
        stdin: &stdin,
        out: &out,
        owner_dir: std::path::PathBuf::from("/tmp"),
        cwd: std::env::current_dir().unwrap_or_default(),
        runner_identity: identity,
    };
    let rc = run::execute(inv, &deps);
    ExitCode::from(rc.clamp(0, 255) as u8)
}
