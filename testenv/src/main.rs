//! testenv — select suites for a tree, build that tree in place, run the suites in the
//! fixture container, report the verdict. Contract: DESIGN.md.

use std::io::Read;
use std::process::ExitCode;
use testenv::build::Cargo;
use testenv::cli;
use testenv::run::{self, Deps, Harness};
use testenv::runtime::{self, Podman};

/// Spawn `testenv warm refill <i>` detached: its own process group, no stdio of ours (a
/// gate reading our stdout must not wait on it), output appended to
/// `$SPIRA_RUN/testenv-warm.log`.
fn spawn_refill(i: usize, run: &std::path::Path) {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run.join("testenv-warm.log"));
    let (out, err) = match log.and_then(|f| f.try_clone().map(|g| (f, g))) {
        Ok((f, g)) => (Stdio::from(f), Stdio::from(g)),
        Err(_) => (Stdio::null(), Stdio::null()),
    };
    let _ = Command::new(exe)
        .args(["warm", "refill", &i.to_string()])
        .env("SPIRA_RUN", run)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn();
}

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
    // `testenv warm refill <slot>` — boot a warm slot's spare (DESIGN.md §11.2); spawned
    // detached by the trial that used the slot.
    let warm_refill = args.first().map(String::as_str) == Some("warm");
    if warm_refill
        && (args.get(1).map(String::as_str) != Some("refill")
            || args.get(2).and_then(|v| v.parse::<usize>().ok()).is_none())
    {
        eprintln!("usage: testenv warm refill <slot>");
        return ExitCode::from(2);
    }
    let inv = if warm_refill {
        None
    } else {
        Some(match cli::parse(&args) {
            Ok(i) => i,
            Err(cli::UsageError(lines)) => {
                for l in lines {
                    eprintln!("{l}");
                }
                println!("VERDICT FAULT rc=2 ran=0 reason=usage");
                return ExitCode::from(2);
            }
        })
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
    // The container helper is the one in this binary's own harness copy (sp-isom7): the
    // directory Harness::locate found by that very file. In a release that is the release's
    // spira/testenv.sh — what a PATH lookup would find — and when a gate builds testenv from
    // the tree under test (gate.steps `bin SPIRA_TESTENV_BIN testenv`) it is that tree's, so
    // the runner and the container it drives always come from the same tree. Missing is a
    // harness fault naming it.
    let testenv_sh = harness.script("testenv.sh");
    if !testenv_sh.is_file() {
        eprintln!("batch: {} is missing (the harness copy this testenv belongs to)", testenv_sh.display());
        println!("VERDICT FAULT rc=2 ran=0 reason=harness-missing");
        return ExitCode::from(2);
    }
    let rt = Podman { testenv_sh };
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
        warm_refill: &spawn_refill,
    };
    let rc = match inv {
        Some(inv) => run::execute(inv, &deps),
        None => run::warm_refill(args[2].parse().unwrap_or(0), &deps),
    };
    ExitCode::from(rc.clamp(0, 255) as u8)
}
