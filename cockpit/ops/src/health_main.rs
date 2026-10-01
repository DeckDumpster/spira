//! `health [once [rows [cols]]|render-many <dir> [rows [cols]]|loop]` — see `src/health.rs`.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use cockpit_ops::health::frame::{render, FrameInputs};
use cockpit_ops::health::model::Snapshot;
use cockpit_ops::health::sections::{DrainState, HaltState};

const HELP: &str = "usage: health [once [rows [cols]]|render-many <dir> [rows [cols]]|loop]";

fn now_epoch() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn env_nonempty(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// `TIOCGWINSZ` on whichever of stdout/stdin/stderr is a tty, then `$LINES`/`$COLUMNS`.
///
/// THE BUG THIS REPLACES (found on the operator's own live pane within a day of landing):
/// the original shelled out to `stty size`, and `std::process::Command::output()` gives the
/// child a *null* stdin, never the parent's own tty — so `stty size` failed with "standard
/// input: Inappropriate ioctl for device" on literally every call, in every pane, always,
/// and `term_size` fell to its hardcoded small default every single tick. A direct ioctl on
/// this PROCESS's own fds has no such gap: there is no child to lose the tty across.
///
/// NEVER A SMALL DEFAULT (the operator's own instruction after this incident): the final
/// fallback, reached only when no fd is a tty and neither env var is set, is 24×80 — the
/// oldest conventional terminal size there is — never the 5-row "pessimistic" floor the
/// original chose. A pane that is actually 5 rows tall still gets the same real ioctl
/// reading it always got; this fallback is for the no-tty case only (piped output, a
/// fixture with neither $LINES nor $COLUMNS set), where there is no "real" size to under- or
/// over-guess from.
fn term_rows() -> i64 {
    term_size().0
}
fn term_cols() -> i64 {
    term_size().1
}
fn term_size() -> (i64, i64) {
    for fd in [libc::STDOUT_FILENO, libc::STDIN_FILENO, libc::STDERR_FILENO] {
        if let Some(rc) = cockpit_ops::health::term::winsize_of_fd(fd) {
            return rc;
        }
    }
    let r = std::env::var("LINES").ok().and_then(|s| s.parse().ok()).filter(|&v: &i64| v > 0);
    let c = std::env::var("COLUMNS").ok().and_then(|s| s.parse().ok()).filter(|&v: &i64| v > 0);
    (r.unwrap_or(24), c.unwrap_or(80))
}

fn read_snapshot(path: &Path) -> (String, bool) {
    match std::fs::read_to_string(path) {
        Ok(s) => (s, true),
        Err(_) => (String::new(), false),
    }
}

/// `spira_unit sentinel timer` then `systemctl --user is-active <unit>`, best-effort: tries
/// the per-instance unit name first (`sentinel-<instance>.timer`), falling back to the plain
/// name — `conf.sh`'s own `spira_unit` helper is out of this bead's scope (group 4, last),
/// so this is a deliberately narrower reimplementation of just the one call site needs. See
/// `../DESIGN.md` Decisions.
fn sentinel_active() -> Option<bool> {
    let instance = env_nonempty("SPIRA_INSTANCE");
    let mut candidates = Vec::new();
    if let Some(i) = &instance {
        if i != "prod" {
            candidates.push(format!("spira-sentinel-{i}.timer"));
        }
    }
    candidates.push("spira-sentinel.timer".to_string());
    let systemctl = env_nonempty("SPIRA_SYSTEMCTL").unwrap_or_else(|| "systemctl".to_string());
    for unit in candidates {
        if let Ok(out) = Command::new(&systemctl).args(["--user", "is-active", &unit]).output() {
            let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !state.is_empty() {
                return Some(state == "active");
            }
        }
    }
    None
}

fn gather_halt(run: &str) -> HaltState {
    let stamp = Path::new(run).join("world.halted");
    match std::fs::read_to_string(&stamp) {
        Ok(content) => {
            let mut lines = content.lines();
            let since = lines.next().unwrap_or("").to_string();
            let why = content
                .lines()
                .nth(1)
                .and_then(|l| l.strip_prefix("why: "))
                .unwrap_or("")
                .to_string();
            HaltState { stamp_exists: true, since, why, sentinel_active: sentinel_active() }
        }
        Err(_) => HaltState { stamp_exists: false, since: String::new(), why: String::new(), sentinel_active: sentinel_active() },
    }
}

fn gather_drain(run: &str) -> DrainState {
    let stamp = Path::new(run).join("world.draining");
    let mtime = std::fs::metadata(&stamp).ok().and_then(|m| m.modified().ok()).and_then(|t| {
        t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
    });
    DrainState { stamp_mtime: mtime }
}

/// Aeons genuinely live right now, from `/proc` (cheap, exact — the snapshot can be up to
/// 120s stale, so a short-lived aeon can start and finish inside one collector pass and read
/// as absent for its whole life without this).
fn live_aeon_n(run: &str) -> i64 {
    let mut n = 0i64;
    let Ok(rd) = std::fs::read_dir(run) else { return 0 };
    for ent in rd.flatten() {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("aeon-") && name.ends_with(".pid")) {
            continue;
        }
        let Ok(pid_s) = std::fs::read_to_string(ent.path()) else { continue };
        let Ok(pid) = pid_s.trim().parse::<i32>() else { continue };
        let Some(argv) = cockpit_ops::procfs::cmdline(pid) else { continue };
        let joined = argv.join(" ");
        if joined.contains("aeon.sh") || joined.split('/').next_back().map(|b| b == "aeon" || b.starts_with("aeon ")).unwrap_or(false) || joined.contains("/aeon ") || joined.ends_with("/aeon") {
            n += 1;
        }
    }
    n
}

fn renderer_rev() -> String {
    let Some(release) = env_nonempty("SPIRA_RELEASE") else { return String::new() };
    let dir = Path::new(&release).join("cockpit");
    Command::new("git")
        .args(["-C"])
        .arg(&dir)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The alternate `$SPIRA_RUN` derivation that actually holds a `cockpit.env`, when the
/// snapshot is absent at `run` but present at the repo-relative or XDG alternative —
/// `conf.sh` derives `$SPIRA_RUN` from whether `$SPIRA_REPO` is writable, so a writer and
/// a reader can legitimately disagree. Only meaningful (and only called) when the
/// snapshot is absent at `run`; mirrors `snap_absent_banner`'s own probe exactly.
fn mismatch_alt(run: &str) -> Option<String> {
    let instance = env_nonempty("SPIRA_INSTANCE").unwrap_or_else(|| "prod".to_string());
    let inst_sfx = if instance == "prod" { String::new() } else { format!("-{instance}") };
    let mut alts = Vec::new();
    if let Some(repo) = env_nonempty("SPIRA_REPO") {
        alts.push(format!("{repo}/.runtime/spira{inst_sfx}"));
    }
    let xdg_data = env_nonempty("XDG_DATA_HOME")
        .unwrap_or_else(|| format!("{}/.local/share", env_nonempty("HOME").unwrap_or_default()));
    alts.push(format!("{xdg_data}/spira{inst_sfx}/run"));
    for alt in alts {
        if alt == run {
            continue;
        }
        if Path::new(&alt).join("cockpit.env").is_file() {
            return Some(alt);
        }
    }
    None
}

fn build_inputs(run: &str) -> (FrameInputs<'static>, String) {
    let snap_path = Path::new(run).join("cockpit.env");
    let (content, exists) = read_snapshot(&snap_path);
    let snap = Snapshot::parse(&content);
    let now = now_epoch();
    let age_secs = snap.get("SP_AT").and_then(|v| v.parse::<i64>().ok()).map(|at| now - at);
    let hhmm = {
        let out = Command::new("date").arg("+%H:%M").output().ok();
        out.filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let snap_stale_s = env_nonempty("SPIRA_SNAP_STALE_S").and_then(|s| s.parse().ok()).unwrap_or(60);
    let trace_lines = env_nonempty("SPIRA_COCKPIT_TRACE_LINES").and_then(|s| s.parse().ok()).unwrap_or(2);

    let inputs = FrameInputs {
        // 'static is a lie we immediately own up to: content is leaked intentionally for the
        // life of one frame build in `loop` mode's tight repaint cycle to keep this function's
        // signature simple; `once`/`render-many` call it once and exit.
        snapshot_content: Box::leak(content.clone().into_boxed_str()),
        snapshot_exists: exists,
        cols: 0,
        hhmm,
        age_secs,
        snap_stale_s,
        live_aeon_n: live_aeon_n(run),
        trace_lines,
        halt: gather_halt(run),
        drain: gather_drain(run),
        run_dir: run.to_string(),
        mismatch_alt: if exists { None } else { mismatch_alt(run) },
        renderer_rev: renderer_rev(),
        collector_rev: snap.get("SP_COLLECTOR_REV").unwrap_or("").to_string(),
        tok_win_spark: String::new(),
        now,
    };
    (inputs, content)
}

fn run_dir() -> String {
    env_nonempty("SPIRA_RUN").unwrap_or_else(|| "/tmp/spira-run".to_string())
}

fn do_render(rows: i64, cols: i64) -> Vec<String> {
    let run = run_dir();
    let (mut inputs, _content) = build_inputs(&run);
    inputs.cols = cols;
    render(rows, cols, &inputs)
}

fn paint(last_frame: &mut String) {
    let rows = term_rows();
    let cols = term_cols();
    let lines = do_render(rows, cols);
    let buf = lines.join("\n");
    if buf == *last_frame {
        return;
    }
    *last_frame = buf.clone();
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[?2026h\x1b[H");
    let n = lines.len();
    for (i, l) in lines.iter().enumerate() {
        let _ = write!(out, "{l}\x1b[K");
        if i + 1 < n {
            let _ = write!(out, "\n");
        }
    }
    let _ = write!(out, "\x1b[J\x1b[?2026l");
    let _ = out.flush();
}

fn conf_mtime(path: &str) -> i64 {
    if path.is_empty() {
        return 0;
    }
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn main() {
    cockpit_ops::conf::self_source();
    std::env::set_var("LC_ALL", env_nonempty("LC_ALL").unwrap_or_else(|| "C.UTF-8".to_string()));

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(|s| s.as_str()).unwrap_or("loop");
    match mode {
        "once" => {
            let rows: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let cols: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            for line in do_render(rows, cols) {
                println!("{line}");
            }
        }
        "render-many" => {
            let Some(dir) = args.get(1) else {
                eprintln!("health: render-many needs a directory of *.env fragments");
                std::process::exit(2);
            };
            let rows: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let cols: i64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .map(|rd| rd.flatten().collect())
                .unwrap_or_else(|_| Vec::new());
            entries.sort_by_key(|e| e.file_name());
            for ent in entries {
                let path = ent.path();
                if path.extension().and_then(|e| e.to_str()) != Some("env") {
                    continue;
                }
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                println!("=== {stem} ===");
                let run = run_dir();
                let (mut inputs, _c) = build_inputs(&run);
                let (content, exists) = read_snapshot(&path);
                let frag_snap = Snapshot::parse(&content);
                inputs.collector_rev = frag_snap.get("SP_COLLECTOR_REV").unwrap_or("").to_string();
                inputs.age_secs = frag_snap.get("SP_AT").and_then(|v| v.parse::<i64>().ok()).map(|at| inputs.now - at);
                inputs.snapshot_content = Box::leak(content.into_boxed_str());
                inputs.snapshot_exists = exists;
                inputs.mismatch_alt = if exists { None } else { mismatch_alt(&run) };
                inputs.cols = cols;
                for line in render(rows, cols, &inputs) {
                    println!("{line}");
                }
            }
        }
        "loop" if args.len() <= 1 => {
            print!("\x1b[?25l\x1b[?7l");
            let _ = std::io::stdout().flush();
            let conf_file = env_nonempty("SPIRA_CONF_FILE").unwrap_or_default();
            let conf_mtime_0 = conf_mtime(&conf_file);
            let tick: u64 = env_nonempty("SPIRA_HEALTH_TICK").and_then(|s| s.parse().ok()).unwrap_or(2);
            let mut last_frame = String::new();
            let cleanup = || {
                print!("\x1b[?25h\x1b[?7h\x1b[?2026l\n");
                let _ = std::io::stdout().flush();
            };
            ctrlc_like_setup(cleanup);
            install_sigwinch_handler();
            loop {
                paint(&mut last_frame);
                // Sleep in short slices rather than one flat `sleep(tick)`, so a resize
                // (SIGWINCH) repaints within a fraction of a second instead of waiting out
                // whatever is left of the current tick — belt-and-suspenders on top of the
                // plain re-read every tick, which already picks up a resize on its own.
                let slice = std::time::Duration::from_millis(200);
                let mut waited = std::time::Duration::ZERO;
                let target = std::time::Duration::from_secs(tick);
                while waited < target {
                    if take_sigwinch() {
                        break;
                    }
                    std::thread::sleep(slice.min(target - waited));
                    waited += slice;
                }
                if !conf_file.is_empty() && conf_mtime(&conf_file) != conf_mtime_0 {
                    eprintln!("health: config changed — restarting");
                    let exe = std::env::current_exe().unwrap_or_else(|_| "health".into());
                    let _ = Command::new(exe).arg("loop").exec_replace();
                }
            }
        }
        _ => {
            eprintln!("{HELP}");
            std::process::exit(1);
        }
    }
}

/// Best-effort signal handling so the cursor and autowrap are restored on Ctrl-C/TERM/HUP —
/// std has no portable signal API without a crate, so this covers the common interactive
/// case (Ctrl-C) and leaves systemd's own termination path (which kills the pane, not this
/// process cleanly) as the one case that skips the restore, same risk profile as a bash trap
/// racing a SIGKILL.
fn ctrlc_like_setup(cleanup: impl Fn() + Send + 'static) {
    unsafe {
        static mut CLEANUP: Option<Box<dyn Fn() + Send>> = None;
        CLEANUP = Some(Box::new(cleanup));
        extern "C" fn handler(_sig: i32) {
            unsafe {
                if let Some(f) = CLEANUP.as_ref() {
                    f();
                }
            }
            std::process::exit(0);
        }
        extern "C" {
            fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
        }
        for sig in [2 /* SIGINT */, 15 /* SIGTERM */, 1 /* SIGHUP */] {
            signal(sig, handler);
        }
    }
}

static SIGWINCH_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// SIGWINCH's default disposition is already "ignore", so installing a handler changes no
/// behaviour on its own — this exists so the loop can notice a resize and repaint sooner
/// than the next full tick (see the `loop` arm's sleep-in-slices). The handler itself does
/// only the one thing a signal handler may safely do here: flip an atomic flag.
fn install_sigwinch_handler() {
    extern "C" fn handler(_sig: i32) {
        SIGWINCH_SEEN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    extern "C" {
        fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    }
    unsafe {
        signal(libc::SIGWINCH, handler);
    }
}

/// Has a SIGWINCH landed since the last call? Consumes the flag either way.
fn take_sigwinch() -> bool {
    SIGWINCH_SEEN.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// `exec()` in place (matching `exec bash "$0" loop`): the pane's process must not exit and
/// restart, since exiting closes the tmux pane and takes its `@cockpit` tag with it.
trait ExecReplace {
    fn exec_replace(&mut self) -> std::io::Error;
}
impl ExecReplace for Command {
    #[cfg(unix)]
    fn exec_replace(&mut self) -> std::io::Error {
        use std::os::unix::process::CommandExt;
        self.exec()
    }
    #[cfg(not(unix))]
    fn exec_replace(&mut self) -> std::io::Error {
        match self.status() {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(e) => e,
        }
    }
}
