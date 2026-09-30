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
///
/// Its harness is the SLOT's checkout, never ours (sp-t26yx): a gate's testenv lives in a
/// transient `.gate.harness.*` worktree that the gate removes as soon as we exit, and a
/// refill that booted through that worktree's harness found it gone every time
/// ("no image tag — no spare"), so no gate trial ever claimed a spare and every one paid
/// a cold `up`. The slot holds the tree the trial just tested and outlives it.
fn spawn_refill(i: usize, run: &std::path::Path) {
    let slot = testenv::warm::paths(run, i).0;
    let harness = slot
        .join(testenv::container::HARNESS_MARKER)
        .is_file()
        .then_some(slot);
    spawn_warm(&["refill".to_string(), i.to_string()], run, harness);
}

/// Spawn `testenv warm sweep` detached (DESIGN.md D12), exactly as the refill.
fn spawn_sweep(run: &std::path::Path) {
    spawn_warm(&["sweep".to_string()], run, None);
}

fn spawn_warm(sub: &[String], run: &std::path::Path, harness: Option<std::path::PathBuf>) {
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
    let mut cmd = Command::new(exe);
    if let Some(h) = harness {
        cmd.env("SPIRA_TESTENV_HARNESS", h);
    }
    // Never the caller's cwd: a gate's is the worktree it is about to remove.
    let _ = cmd
        .arg("warm")
        .args(sub)
        .current_dir(run)
        .env("SPIRA_RUN", run)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn();
}

fn load_config(who: &str) -> Option<spira_config::SpiraToml> {
    spira_config::discover(None).and_then(|p| match spira_config::load(&p) {
        Ok(doc) => Some(doc),
        Err(e) => {
            eprintln!("{who}: ignoring unreadable config: {e}");
            None
        }
    })
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
    // `testenv container …` — the fixture container's driver (DESIGN.md §12, sp-s0e1k). A
    // branch literally named `container` is `testenv -- container`.
    if args.first().map(String::as_str) == Some("container") {
        let env = |k: &str| std::env::var(k).ok();
        let harness = Harness::locate(&env).map(|h| h.root);
        let config = load_config("testenv");
        let rc = testenv::container::main(&args[1..], harness, config.as_ref());
        return ExitCode::from(rc.clamp(0, 255) as u8);
    }
    // `testenv plan <json>` — the container setup in one exec, run inside the container
    // (DESIGN.md §11.4, sp-t26yx).
    if args.first().map(String::as_str) == Some(testenv::plan::PLAN_ARG) {
        let rc = testenv::plan::main(&args[1..]);
        return ExitCode::from(rc.clamp(0, 255) as u8);
    }
    // `testenv warm refill <slot>` — boot a warm slot's spare (DESIGN.md §11.2); spawned
    // detached by the trial that used the slot.
    let warm_refill = args.first().map(String::as_str) == Some("warm");
    let warm_sweep = warm_refill && args.get(1).map(String::as_str) == Some("sweep");
    if warm_refill
        && !warm_sweep
        && (args.get(1).map(String::as_str) != Some("refill")
            || args.get(2).and_then(|v| v.parse::<usize>().ok()).is_none())
    {
        eprintln!("usage: testenv warm refill <slot> | testenv warm sweep");
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
        eprintln!("batch: cannot find the harness (spira/testenv/Containerfile) above the testenv binary; set SPIRA_TESTENV_HARNESS");
        println!("VERDICT FAULT rc=2 ran=0 reason=harness-missing");
        return ExitCode::from(2);
    };
    let config = load_config("batch");
    // The container driver is this executable (DESIGN.md §12, D17) against this binary's own
    // harness copy (sp-isom7): in a release, the release; when a gate builds testenv from the
    // tree under test (gate.steps `bin SPIRA_TESTENV_BIN testenv`), that tree — so the runner
    // and the image it boots always come from the same tree.
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("batch: cannot resolve this executable's own path (the container driver)");
        println!("VERDICT FAULT rc=2 ran=0 reason=harness-missing");
        return ExitCode::from(2);
    };
    let rt = Podman {
        exe,
        harness: harness.root.clone(),
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
        warm_refill: &spawn_refill,
        spawn_sweep: &spawn_sweep,
        runner_exe: std::env::current_exe().unwrap_or_default(),
    };
    let rc = match inv {
        Some(inv) => run::execute(inv, &deps),
        None if warm_sweep => run::warm_sweep(&deps),
        None => run::warm_refill(args[2].parse().unwrap_or(0), &deps),
    };
    ExitCode::from(rc.clamp(0, 255) as u8)
}
