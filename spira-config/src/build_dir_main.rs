//! spira-build-dir — lease an owned build directory for a hand build, from the pool dwarves use.
//!
//!   spira-build-dir lease --who <label> --pid <pid> [--root <tree>]
//!
//! Prints the directory for `CARGO_TARGET_DIR`; progress goes to stderr. The lease is held by a
//! detached holder until `--pid` exits (pass the builder shell's `$$`), then the tree at `--root`
//! (default: the current directory) is recorded as the one the directory holds and the directory
//! goes idle. The selective clean runs before the path is printed.

use spira_config::build_dir as bd;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(500);
const USAGE: &str = "usage: spira-build-dir lease --who <label> --pid <pid> [--root <tree>]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("lease") => lease(&args[1..]),
        Some("__hold") => hold(&args[1..]),
        _ => Err(USAGE.to_string()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spira-build-dir: {e}");
            ExitCode::from(2)
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn lease(args: &[String]) -> Result<(), String> {
    let who = flag(args, "--who").filter(|w| !w.is_empty()).ok_or(USAGE)?;
    let pid = flag(args, "--pid").ok_or(USAGE)?;
    pid.parse::<i32>()
        .map_err(|_| format!("--pid {pid:?} is not a process id"))?;
    let root = match flag(args, "--root") {
        Some(r) => PathBuf::from(r),
        None => std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?,
    };
    let exe = std::env::current_exe().map_err(|e| format!("cannot find myself: {e}"))?;
    let mut child = Command::new(exe) // batch-job: the holder outlives this call by design, until the build ends
        .args(["__hold", &who, &pid])
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot start the holder: {e}"))?;
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    while let Some(Ok(l)) = lines.next() {
        if let Some(note) = l.strip_prefix("# ") {
            eprintln!("spira-build-dir: {note}");
        } else if let Some(e) = l.strip_prefix("error\t") {
            return Err(e.to_string());
        } else if let Some((state, dir)) = l.split_once('\t') {
            eprintln!("spira-build-dir: build directory {dir} — {state}");
            println!("{dir}");
            return Ok(());
        }
    }
    Err("the holder exited before leasing a directory".into())
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for the process.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn hold(args: &[String]) -> Result<(), String> {
    let (who, pid, root) = (
        &args[0],
        args[1].parse::<i32>().map_err(|e| e.to_string())?,
        Path::new(&args[2]),
    );
    let mut out = std::io::stdout();
    let held = take(who, root, &mut out);
    let lease = match held {
        Ok(l) => l,
        Err(e) => {
            let _ = writeln!(out, "error\t{e}");
            return Ok(());
        }
    };
    while alive(pid) {
        std::thread::sleep(POLL);
    }
    if root.is_dir() {
        let _ = bd::clean(
            &script()?,
            root,
            lease.dir(),
            &std::env::var("PATH").unwrap_or_default(),
            true,
        );
    }
    let dir = lease.dir().to_path_buf();
    let trimmed = lease.release();
    eprintln!("released build directory {}: {trimmed}", dir.display());
    Ok(())
}

fn script() -> Result<PathBuf, String> {
    Ok(spira_config::resolve::locate_home_for_process()?.join(bd::CLEAN_SCRIPT))
}

fn take(who: &str, root: &Path, out: &mut impl Write) -> Result<bd::Lease, String> {
    let bases = bd::parse_bases(&spira_config::process::cfg(bd::BASE_ENV)?);
    if bases.is_empty() {
        return Err(format!(
            "{} is not set — set dwarf_build_base in the config document",
            bd::BASE_ENV
        ));
    }
    let lease = bd::lease(&bases, who).map_err(|e| format!("cannot lease a build directory under {bases:?}: {e}"))?;
    let path = std::env::var("PATH").unwrap_or_default();
    match bd::clean(&script()?, root, lease.dir(), &path, false) {
        Ok(text) => {
            for l in text.lines().filter(|l| !l.trim().is_empty()) {
                let _ = writeln!(out, "# {l}");
            }
        }
        Err(e) => {
            let _ = writeln!(
                out,
                "# selective clean failed ({e}) — emptied, this build is cold"
            );
            bd::empty(lease.dir());
        }
    }
    let budget = spira_config::process::cfg_parse::<i64>(bd::BUDGET_ENV)?.max(0) as u64;
    for gone in bd::evict_idle(&bases, budget.saturating_mul(1 << 30)) {
        let _ = writeln!(out, "# evicted idle build directory {}", gone.display());
    }
    let state = if lease.reused { "warm" } else { "new" };
    let _ = writeln!(out, "{state}\t{}", lease.dir().display());
    let _ = out.flush();
    Ok(lease)
}
