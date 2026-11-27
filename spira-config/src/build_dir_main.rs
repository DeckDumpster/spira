//! spira-build-dir — lease an owned build directory for a hand build, from the pool dwarves use.
//!
//!   spira-build-dir lease --who <label> --pid <pid> [--root <tree>] --repo <name>
//!
//! Prints the directory for `CARGO_TARGET_DIR`; progress goes to stderr. The lease is held by a
//! detached holder until `--pid` exits (pass the builder shell's `$$`), then the tree at `--root`
//! (default: the current directory) is recorded as the one the directory holds and the directory
//! goes idle. The selective clean runs before the path is printed. `--repo` names the repository
//! the tree belongs to: the directory is one of that repository's own. At release the lease's
//! report is appended to `$SPIRA_RUN/build-dir.log`.

use spira_config::build_dir as bd;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

/// The file under `SPIRA_RUN` each hand lease's report is appended to.
const LOG: &str = "build-dir.log";
/// At this size the log becomes `<LOG>.1`, replacing the one before.
const LOG_CAP_BYTES: u64 = 1 << 20;
const POLL: Duration = Duration::from_millis(500);
const USAGE: &str = "usage: spira-build-dir lease --who <label> --pid <pid> [--root <tree>] --repo <name>";

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
    let repo = flag(args, "--repo").filter(|r| !r.is_empty()).ok_or(USAGE)?;
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
        .arg(repo)
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
    let repo = args.get(3).map(String::as_str).ok_or(USAGE)?;
    let mut out = std::io::stdout();
    let held = take(who, root, repo, &mut out);
    let mut lease = match held {
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
            true, // release
            who,
            Some(repo),
        );
        lease.note_root(root);
    }
    let dir = lease.dir().to_path_buf();
    let report = lease.report(who);
    let released = lease.release();
    eprintln!("released build directory {}: {}", dir.display(), released.trimmed);
    log_report(&report);
    bd::meter_report(&report);
    released.deleted.iter().for_each(|d| eprintln!("{d}"));
    bd::meter_evictions(&released.deleted);
    if let Some(bases) = spira_config::process::cfg(bd::BASE_ENV).ok().map(|v| bd::parse_bases(&v)) {
        bd::reap_in_background(&bases);
        let budget = spira_config::process::cfg_parse::<i64>(bd::BUDGET_ENV).unwrap_or(0).max(0) as u64;
        bd::meter(&bases, &dir, budget.saturating_mul(1 << 30));
    }
    Ok(())
}

/// The holder outlives its caller's stderr, so its report goes to a log under `SPIRA_RUN`.
fn log_report(report: &bd::LeaseReport) {
    let at = tsd::iso_utc(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs());
    let written = spira_config::process::cfg("SPIRA_RUN").and_then(|run| append(Path::new(&run), &format!("{at} {report}\n")).map_err(|e| format!("{run}/{LOG}: {e}")));
    if let Err(e) = written {
        eprintln!("cannot log the lease report ({e}): {report}");
    }
}

fn append(run: &Path, line: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(run)?;
    if std::fs::metadata(run.join(LOG)).is_ok_and(|m| m.len() >= LOG_CAP_BYTES) {
        std::fs::rename(run.join(LOG), run.join(format!("{LOG}.1")))?;
    }
    std::fs::OpenOptions::new().append(true).create(true).open(run.join(LOG))?.write_all(line.as_bytes())
}

fn script() -> Result<PathBuf, String> {
    Ok(spira_config::resolve::locate_home_for_process()?.join(bd::CLEAN_SCRIPT))
}

fn take(who: &str, root: &Path, repo: &str, out: &mut impl Write) -> Result<bd::Lease, String> {
    let bases = bd::parse_bases(&spira_config::process::cfg(bd::BASE_ENV)?);
    if bases.is_empty() {
        return Err(format!(
            "{} is not set — set dwarf_build_base in the config document",
            bd::BASE_ENV
        ));
    }
    let ceiling = spira_config::process::cfg_parse(bd::CEILING_ENV)?;
    let mut lease = bd::lease_for(&bases, who, repo, ceiling).map_err(|e| format!("cannot lease a build directory under {bases:?}: {e}"))?;
    for note in &lease.notes {
        let _ = writeln!(out, "# {note}");
    }
    let path = std::env::var("PATH").unwrap_or_default();
    match bd::clean(&script()?, root, lease.dir(), &path, false, who, Some(repo)) {
        Ok(text) => {
            if let Some(report) = bd::clean_report(lease.dir()) {
                lease.note_clean(&report);
            }
            for l in text.lines().filter(|l| !l.trim().is_empty()) {
                let _ = writeln!(out, "# {l}");
            }
        }
        Err(e) => {
            let _ = writeln!(
                out,
                "# selective clean failed ({e}) — emptied, this build is cold"
            );
            lease.empty_after_failed_clean();
        }
    }
    let budget = spira_config::process::cfg_parse::<i64>(bd::BUDGET_ENV)?.max(0) as u64;
    let budget = budget.saturating_mul(1 << 30);
    let registry = spira_config::repos::Registry::from_env(std::env::vars().collect(), &spira_config::resolve::locate_home_for_process()?);
    let (deleted, refused) = bd::evict_at_lease(&bases, budget, &registry);
    for line in refused.into_iter().chain(deleted.iter().map(ToString::to_string)) {
        let _ = writeln!(out, "# {line}");
    }
    bd::meter_evictions(&deleted);
    bd::reap_in_background(&bases);
    bd::meter(&bases, lease.dir(), budget);
    let _ = writeln!(out, "{}\t{}", lease.start(), lease.dir().display());
    let _ = out.flush();
    Ok(lease)
}
