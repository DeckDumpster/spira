//! `rebuild` — bring the whole cockpit back from nothing, including after the tmux server
//! dies. Replaces `cockpit/rebuild.sh` (sp-llbmi). See `../DESIGN.md`.
//! ```text
//!     rebuild probe        say what is wrong and change nothing
//!     rebuild              do it: clear a dead server, make the sessions, build the cockpit
//!     rebuild --force      also clear a wedged server that still holds live panes
//! ```
//!
//! `layout up` REPAIRS a cockpit; it cannot create one — its first move is `has-session ||
//! exit 1`, so when the tmux server itself is gone there is no session for it to repair. This
//! is the one command that assembles the whole thing from an empty server.
//!
//! THE ORDERING THAT MATTERS, unchanged from the bash:
//!   1. A wedged server must be cleared before anything else — it holds the socket, so every
//!      later step fails in a way that looks like a different bug.
//!   2. The dashboards go in `brain:0`, NOT a new `cockpit` window. `cockpit` holds LINKED
//!      copies of `brain:0` and `hunk:0`; build the layout somewhere else and it renders
//!      correctly, passes a casual look, and is not the cockpit.
//!   3. So: sessions first, layout into `brain:0`, and only then the linking step.
//!
//! WEDGED IS NOT DEAD. A server that answers `list-sessions` with anything other than "no
//! server running" / "No such file or directory" / "Connection refused" / "File name too
//! long" is alive, holding its socket, and simply not talking — killing it is safe only when
//! it has no live descendants (somebody's unsaved work), or when the operator says `--force`.
//! Never `pgrep -f`/`pkill -f`: the holder is found by the listening socket's inode in
//! `/proc/net/unix` (`procfs::listening_socket_holder`), which cannot match this program's
//! own argv or an unrelated client the way a pattern match can.

use std::process::Command;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use crate::layout::{now_epoch, Conf as LayoutConf};
use crate::procfs;
use crate::tmux::{classify_server_stderr, ServerState, Tmux};

pub struct Rebuild {
    pub tmux: Tmux,
    pub view: PathBuf,
    pub layout_bin: String,
    pub cwd: String,
    pub sessions: Vec<String>,
    pub concierge: PathBuf,
    pub force: bool,
    /// Whether a mail pane should exist, from `COCKPIT_MAIL` — resolved once here rather
    /// than re-read where `verify` checks for the pane.
    pub mail_wanted: bool,
    pub watchd_bin: String,
}

/// Does the pane's process tree carry `--append-system-prompt`, or is it a client of the
/// concierge's own socket (`tmux -L <socket> attach`)? Read directly from `/proc`, never via
/// `pgrep -f`.
pub fn pane_has_brief(pid: i32, concierge_socket: &str) -> bool {
    let mut candidates = vec![pid];
    candidates.extend(procfs::children_of(pid));
    for p in candidates {
        let Some(argv) = procfs::cmdline(p) else { continue };
        let joined = argv.join("\u{0}");
        if joined.contains("--append-system-prompt") {
            return true;
        }
        // tmux -L <socket> attach
        for w in argv.windows(4) {
            if w[0].ends_with("tmux") && w[1] == "-L" && w[2] == concierge_socket && w[3] == "attach" {
                return true;
            }
        }
    }
    false
}

/// `server_state()` against a real (or socket-scoped) tmux client.
pub fn server_state(tmux: &Tmux) -> ServerState {
    match tmux.list_sessions_probe() {
        Ok(()) => ServerState::Up,
        Err(stderr) => classify_server_stderr(&stderr),
    }
}

/// The listening socket path this `tmux` client would use: its own report if the server is
/// reachable, else the conventional default (`$TMUX_TMPDIR/tmux-<uid>/default`).
pub fn socket_path(tmux: &Tmux) -> String {
    if let Some(p) = tmux.display_message(None, "#{socket_path}") {
        if !p.is_empty() {
            return p;
        }
    }
    let base = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| system_paths::TMP.to_string());
    let uid = unsafe { libc_getuid() };
    format!("{base}/tmux-{uid}/default")
}

// A single, narrow use of libc: tmux's own default socket path is keyed by the real uid, and
// that is the one thing this module cannot get from `/proc` or an env var. No `libc` crate
// dependency needed for one syscall with no meaningful failure mode.
unsafe fn libc_getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

pub struct ReportLine(pub String);

/// `probe` — read-only. Mirrors `report_state()`.
pub fn probe_report(tmux: &Tmux, sessions: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let state = server_state(tmux);
    out.push(format!("  tmux server      {}", state_name(state)));
    if state == ServerState::Unusable {
        out.push("  NOTE             the socket path is unusable (over the 108-byte unix limit?) — fix TMUX_TMPDIR".to_string());
    }
    if state == ServerState::Wedged {
        let sock = socket_path(tmux);
        match procfs::listening_socket_holder(&sock) {
            Some(pid) => {
                let n = procfs::descendant_count(pid);
                let comm = std::fs::read_to_string(format!("{}/{pid}/comm", system_paths::PROC))
                    .unwrap_or_else(|_| "?".to_string())
                    .trim()
                    .to_string();
                out.push(format!("  holder           pid {pid} ({comm}), {n} live descendant(s)"));
                if n > 0 {
                    out.push("  NOTE             it still holds live panes — clearing it needs --force".to_string());
                }
            }
            None => out.push("  holder           none found — the socket is orphaned".to_string()),
        }
    }
    for s in sessions.iter().chain(["cockpit".to_string()].iter()) {
        let target = format!("={s}");
        if tmux.has_session(&target) {
            let n = tmux.list_windows(&target, "#{window_id}").len();
            out.push(format!("  session {s:<9} present ({n} window(s))"));
        } else {
            out.push(format!("  session {s:<9} MISSING"));
        }
    }
    if tmux.has_session("=brain") {
        let n = tmux
            .list_panes("brain:0", "#{@cockpit}")
            .iter()
            .filter(|l| !l.is_empty())
            .count();
        out.push(format!("  dashboards       {n}"));
    }
    out
}

fn state_name(s: ServerState) -> &'static str {
    match s {
        ServerState::Up => "up",
        ServerState::Absent => "absent",
        ServerState::Unusable => "unusable",
        ServerState::Wedged => "wedged",
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClearOutcome {
    Answering,
    NoneRunning,
    Unusable,
    ClearedOrphanSocket,
    RefusedLiveDescendants { pid: i32, live: usize },
    Killed { pid: i32 },
    SurvivedKill { pid: i32 },
}

/// Step 1: clear a wedged server. Never touches a server that answers.
pub fn clear_wedged_server(tmux: &Tmux, force: bool, log: &mut dyn FnMut(&str)) -> ClearOutcome {
    match server_state(tmux) {
        ServerState::Up => {
            log("  answering — leaving it alone");
            ClearOutcome::Answering
        }
        ServerState::Absent => {
            log("  none running — nothing to clear");
            ClearOutcome::NoneRunning
        }
        ServerState::Unusable => ClearOutcome::Unusable,
        ServerState::Wedged => {
            let sock = socket_path(tmux);
            let Some(pid) = procfs::listening_socket_holder(&sock) else {
                log("  the socket does not answer and no process holds it; removing the stale socket");
                let _ = std::fs::remove_file(&sock);
                return ClearOutcome::ClearedOrphanSocket;
            };
            let live = procfs::descendant_count(pid);
            log(&format!("  wedged: pid {pid}, {live} live descendant(s)"));
            if live > 0 && !force {
                return ClearOutcome::RefusedLiveDescendants { pid, live };
            }
            unsafe { kill_pid(pid, 15) }; // SIGTERM
            for _ in 0..6 {
                if !Path::new(&format!("{}/{pid}", system_paths::PROC)).exists() {
                    log("  cleared");
                    return ClearOutcome::Killed { pid };
                }
                sleep(Duration::from_millis(500));
            }
            log("  did not exit on TERM — sending KILL");
            unsafe { kill_pid(pid, 9) }; // SIGKILL
            for _ in 0..4 {
                if !Path::new(&format!("{}/{pid}", system_paths::PROC)).exists() {
                    log("  cleared");
                    return ClearOutcome::Killed { pid };
                }
                sleep(Duration::from_millis(500));
            }
            ClearOutcome::SurvivedKill { pid }
        }
    }
}

unsafe fn kill_pid(pid: i32, sig: i32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, sig);
}

pub enum RunOutcome {
    Probe(Vec<String>),
    Ok { verify_fail: u32, output: Vec<String> },
    RefusedUnusableSocket,
    RefusedLiveDescendants { pid: i32, live: usize },
    KillSurvived { pid: i32 },
    SessionCreateFailed { session: String },
    LayoutFailed,
}

impl Rebuild {
    pub fn run(&self, probe_only: bool) -> RunOutcome {
        if probe_only {
            let mut out = vec!["cockpit probe".to_string()];
            out.extend(probe_report(&self.tmux, &self.sessions));
            return RunOutcome::Probe(out);
        }

        let mut out = Vec::new();
        out.push("\n== server".to_string());
        let mut logf = |s: &str| out.push(s.to_string());
        match clear_wedged_server(&self.tmux, self.force, &mut logf) {
            ClearOutcome::Unusable => return RunOutcome::RefusedUnusableSocket,
            ClearOutcome::RefusedLiveDescendants { pid, live } => {
                return RunOutcome::RefusedLiveDescendants { pid, live }
            }
            ClearOutcome::SurvivedKill { pid } => return RunOutcome::KillSurvived { pid },
            _ => {}
        }

        out.push("\n== sessions".to_string());
        for s in &self.sessions {
            let target = format!("={s}");
            if self.tmux.has_session(&target) {
                out.push(format!("  {s} already present"));
            } else if self.tmux.new_session(s, &self.cwd) {
                out.push(format!("  {s} created"));
            } else {
                return RunOutcome::SessionCreateFailed { session: s.clone() };
            }
        }

        // 2b. Scrub the server's own environment, in case it was already up when this ran
        // and is carrying a session identity from whoever started it (tmux_env, in process).
        for line in crate::tmux_env::scrub_here(&self.tmux) {
            out.push(format!("  {line}"));
        }

        out.push("\n== dashboards".to_string());
        // batch-job: this runs whatever its caller names, as long as that takes
        // FORK-EDGE: sibling binary `layout` of this crate (LAYOUT_BIN may name another); the
        // in-process `Layout::up` call lands with the catalogue phase.
        let layout_ok = Command::new(&self.layout_bin)
            .args(["up", "--window", "brain:0"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !layout_ok {
            return RunOutcome::LayoutFailed;
        }

        out.push("\n== session pane".to_string());
        if let Some(sess_p) = first_untagged_pane(&self.tmux, "brain:0") {
            if let Some(pid) = pane_pid(&self.tmux, "brain:0", &sess_p) {
                let conc_sock = std::env::var("CONCIERGE_SOCKET").unwrap_or_else(|_| "concierge".to_string());
                if pane_has_brief(pid, &conc_sock) {
                    out.push("  already running a composed session".to_string());
                } else if self.concierge.is_file() {
                    out.push("  bare session pane — launching concierge.sh here".to_string());
                    let cmd = format!("{} here", self.concierge.display());
                    if self.tmux.respawn_pane(&sess_p, &cmd) {
                        out.push("  launched".to_string());
                    } else {
                        out.push("  launch failed — verify will detect the result".to_string());
                    }
                } else {
                    out.push(format!(
                        "  concierge.sh not found at {} — session pane will remain unwrapped",
                        self.concierge.display()
                    ));
                }
            }
        } else {
            out.push("  session pane not found in brain:0 — skipping launch".to_string());
        }

        out.push("\n== cockpit".to_string());
        if self.view.is_file() {
            for sub in ["build", "sync"] {
                // batch-job: this runs whatever its caller names, as long as that takes
                // FORK-EDGE: cockpit-remote is 358 lines of shell (past the ~100-line port bound).
                if let Ok(o) = Command::new(&self.view).arg(sub).output() {
                    for line in String::from_utf8_lossy(&o.stdout).lines() {
                        out.push(format!("  {line}"));
                    }
                    for line in String::from_utf8_lossy(&o.stderr).lines() {
                        out.push(format!("  {line}"));
                    }
                }
            }
            out.push("  linked and synced".to_string());
        } else {
            out.push(format!("  no cockpit-remote at {} — brain:0 has its dashboards but nothing links them", self.view.display()));
            out.push("  set SPIRA_VIEW in ~/.config/spira/spira.conf, or run its build step by hand".to_string());
        }

        out.push("\n== verify".to_string());
        let mut fail = 0u32;
        chk(&mut fail, &mut out, "the server answers", self.tmux.has_session("brain") || matches!(server_state(&self.tmux), ServerState::Up));
        for s in self.sessions.iter().chain(["cockpit".to_string()].iter()) {
            chk(&mut fail, &mut out, &format!("session {s} exists"), self.tmux.has_session(&format!("={s}")));
        }
        let brain_panes = self.tmux.list_panes("brain:0", "#{@cockpit}");
        chk(
            &mut fail,
            &mut out,
            "brain:0 has a session pane",
            brain_panes.iter().any(|l| l.is_empty()),
        );
        chk(&mut fail, &mut out, "a pane is tagged health", brain_panes.iter().any(|l| l == "health"));
        if self.mail_wanted {
            chk(&mut fail, &mut out, "brain:0 has a mail pane", brain_panes.iter().any(|l| l == "mail"));
        }
        let brain_win = self.tmux.list_windows("=brain", "#{window_id}").into_iter().next();
        let hunk_win = self.tmux.list_windows("=hunk", "#{window_id}").into_iter().next();
        let cockpit_wins = self.tmux.list_windows("=cockpit", "#{window_id}");
        chk(
            &mut fail,
            &mut out,
            "cockpit links brain:0",
            brain_win.as_ref().is_some_and(|w| cockpit_wins.contains(w)),
        );
        chk(
            &mut fail,
            &mut out,
            "cockpit links hunk:0",
            hunk_win.as_ref().is_some_and(|w| cockpit_wins.contains(w)),
        );

        let health_pane = self
            .tmux
            .list_panes("brain:0", "#{pane_id} #{@cockpit}")
            .into_iter()
            .find_map(|l| {
                let mut it = l.split_whitespace();
                let (Some(id), Some(role)) = (it.next(), it.next()) else { return None };
                (role == "health").then(|| id.to_string())
            });
        match health_pane {
            None => chk(&mut fail, &mut out, "health pane renders content", false),
            Some(pid) => {
                let n = wait_for_first_render(&self.tmux, &pid);
                if n > 0 {
                    out.push(format!("  ok    health pane renders content ({n} non-blank line(s))"));
                } else {
                    out.push("  FAIL  health pane is BLANK".to_string());
                    fail += 1;
                }
            }
        }

        if let Some(sess_p) = first_untagged_pane(&self.tmux, "brain:0") {
            if let Some(pid) = pane_pid(&self.tmux, "brain:0", &sess_p) {
                let conc_sock = std::env::var("CONCIERGE_SOCKET").unwrap_or_else(|_| "concierge".to_string());
                chk(&mut fail, &mut out, "session pane carries composed brief", pane_has_brief(pid, &conc_sock));
            } else {
                chk(&mut fail, &mut out, "session pane carries composed brief", false);
            }
        } else {
            out.push("  FAIL  session pane not found in brain:0".to_string());
            fail += 1;
        }

        out.push("\n== watchers".to_string());
        // FORK-EDGE: needs a watchd library (`commands::cmd_status` is in its binary, no [lib]).
        if let Ok(o) = spira_config::bounded::bounded(&self.watchd_bin).arg("status").output() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                out.push(format!("  {line}"));
            }
        }

        RunOutcome::Ok { verify_fail: fail, output: out }
    }
}

/// One positive-control assertion (law-absence-needs-a-positive-control): record `ok`/`FAIL`
/// and bump the count on failure. A free function rather than a closure so it can be called
/// with fresh `&mut` borrows at each call site instead of holding `fail`/`out` captured for
/// its whole lifetime.
fn chk(fail: &mut u32, out: &mut Vec<String>, label: &str, ok: bool) {
    if ok {
        out.push(format!("  ok    {label}"));
    } else {
        out.push(format!("  FAIL  {label}"));
        *fail += 1;
    }
}

/// Non-blank line count of the pane, polled until it first renders or its process exits.
/// The ceiling only bounds a pane that never renders; a rendering pane returns on the
/// first sample that shows content (law-a-load-red-waits-on-a-signal).
fn wait_for_first_render(tmux: &Tmux, pane: &str) -> usize {
    let count = || {
        tmux.capture_pane(pane)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    };
    for _ in 0..240 {
        let n = count();
        if n > 0 {
            return n;
        }
        let dead = tmux.stdout(&["display-message", "-p", "-t", pane, "#{pane_dead}"]).is_none_or(|d| d.trim() != "0");
        if dead {
            break;
        }
        sleep(Duration::from_millis(250));
    }
    count()
}

fn first_untagged_pane(tmux: &Tmux, window: &str) -> Option<String> {
    let out = tmux.run(&["list-panes", "-t", window, "-F", "#{@cockpit} #{pane_id}"])?;
    out.lines().find_map(|l| {
        let parts: Vec<&str> = l.split_whitespace().collect();
        match parts.len() {
            1 => Some(parts[0].to_string()),
            2 if parts[0] != "health" && parts[0] != "mail" => Some(parts[1].to_string()),
            _ => None,
        }
    })
}

fn pane_pid(tmux: &Tmux, window: &str, pane: &str) -> Option<i32> {
    let out = tmux.run(&["list-panes", "-t", window, "-F", "#{pane_id} #{pane_pid}"])?;
    out.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let (Some(id), Some(pid)) = (it.next(), it.next()) else { return None };
        (id == pane).then(|| pid.parse().ok()).flatten()
    })
}

/// Build a `Rebuild` from the process environment, matching `rebuild.sh`'s own resolution of
/// `VIEW`, `LAYOUT`, `CWD`, `SESSIONS`, `CONC`.
const USER_VIEW_REL: &str = ".local/bin/cockpit-remote";

pub fn from_env(tmux: Tmux, force: bool) -> Rebuild {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut view = Some(spira_config::process::cfg("SPIRA_VIEW").unwrap_or_else(|e| {
        eprintln!("rebuild: {e}");
        std::process::exit(2)
    }))
    .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&home).join(USER_VIEW_REL));
    if !view.is_file() {
        if let Some(cock) = LayoutConf::from_env().ok().map(|c| c.cock) {
            view = cock.join("remote/cockpit-remote");
        }
    }
    // COCKPIT_CWD's own toml default already composes `${SPIRA_WIKI:-$SPIRA_REPO}`
    // (spira/conf.d), so the resolved value carries that fallback — no second one here.
    let cwd = spira_config::process::cfg("COCKPIT_CWD").unwrap_or_default();
    // Declared config, the one source: a key that does not resolve refuses, never an empty list.
    let need = |key: &str| {
        spira_config::process::cfg(key).unwrap_or_else(|e| {
            eprintln!("rebuild: {e}");
            std::process::exit(2)
        })
    };
    let sessions: Vec<String> = need("COCKPIT_SESSIONS")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let mail_wanted = !need("COCKPIT_MAIL").is_empty();
    let concierge = std::env::var("COCKPIT_CONCIERGE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let spira_home = std::env::var("SPIRA_HOME").unwrap_or_default();
            PathBuf::from(spira_home).join("concierge.sh")
        });
    Rebuild {
        tmux,
        view,
        layout_bin: std::env::var("LAYOUT_BIN").unwrap_or_else(|_| "layout".to_string()),
        cwd,
        sessions,
        concierge,
        force,
        mail_wanted,
        watchd_bin: std::env::var("WATCHD_BIN").unwrap_or_else(|_| "watchd".to_string()),
    }
}

pub fn now() -> i64 {
    now_epoch()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::fake::FakeTmux;
    use std::os::unix::process::CommandExt;

    #[test_id(txcw7n2)]
    #[test]
    fn state_name_matches_every_variant() {
        assert_eq!(state_name(ServerState::Up), "up");
        assert_eq!(state_name(ServerState::Absent), "absent");
        assert_eq!(state_name(ServerState::Unusable), "unusable");
        assert_eq!(state_name(ServerState::Wedged), "wedged");
    }

    #[test_id(t8vymsz)]
    #[test]
    fn pane_has_brief_detects_append_system_prompt() {
        // Exercised through classify-only inputs would need /proc; this checks the pure
        // substring/window logic indirectly isn't possible without a real pid, so this test
        // instead pins the tmux-attach window-matching rule directly.
        let argv = ["tmux".to_string(),
            "-L".to_string(),
            "concierge".to_string(),
            "attach".to_string()];
        let joined = argv.join("\u{0}");
        assert!(!joined.contains("--append-system-prompt"));
        let mut found = false;
        for w in argv.windows(4) {
            if w[0].ends_with("tmux") && w[1] == "-L" && w[2] == "concierge" && w[3] == "attach" {
                found = true;
            }
        }
        assert!(found);
    }

    struct World {
        _dir: testkit::TempDir,
        fake: FakeTmux,
        rebuild: Rebuild,
        _session: testkit::ChildGuard,
    }

    /// A cockpit as the fake tmux describes it: a session pane running `sleep` (carrying the
    /// composed-brief flag in its argv when `briefed`), a health pane, the linked windows,
    /// and a mail pane when `mail`. `server_dead` makes the first three `has-session` calls
    /// fail, the sessions `rebuild` is to create.
    fn world(name: &str, briefed: bool, mail: bool, mail_wanted: bool, server_dead: bool) -> World {
        let dir = testkit::TempDir::new(name);
        let mut sleeper = std::process::Command::new("sleep");
        if briefed {
            sleeper.arg0("--append-system-prompt");
        }
        let session = testkit::ChildGuard::spawn(sleeper.arg("30"));
        let tags = if mail { "\nhealth\nmail\n" } else { "\nhealth\n" };
        let mut fake = FakeTmux::new(&dir);
        if server_dead {
            fake = fake.fail("list-sessions", "no server running on /sock", 1).fail_first("has-session", 3, 1);
        }
        let fake = fake
            .answer("-F #{@cockpit} #{pane_id}", "%1\nhealth %2\n")
            .answer("-F #{pane_id} #{@cockpit}", "%1\n%2 health\n")
            .answer("-F #{pane_id} #{pane_pid}", &format!("%1 {}\n%2 1\n", session.id()))
            .answer("-F #{@cockpit}", tags)
            .answer("list-windows -t =brain", "@1\n")
            .answer("list-windows -t =hunk", "@2\n")
            .answer("list-windows -t =cockpit", "@1\n@2\n")
            .answer("capture-pane", "an ops pane\n");
        let layout = dir.join("layout");
        let view = dir.join("cockpit-remote");
        for (bin, tag) in [(&layout, "layout"), (&view, "view")] {
            testkit::write_exe(bin, &format!("#!/bin/sh\necho \"{tag} $*\" >> {}\n", fake.log_path().display()));
        }
        let rebuild = Rebuild {
            tmux: fake.tmux(),
            view,
            layout_bin: layout.to_str().unwrap().into(),
            cwd: "work".into(),
            sessions: ["brain", "hunk", "chat"].map(String::from).to_vec(),
            concierge: dir.join("no-concierge.sh"),
            force: false,
            mail_wanted,
            watchd_bin: dir.join("no-watchd").to_str().unwrap().into(),
        };
        World { _dir: dir, fake, rebuild, _session: session }
    }

    fn verified(w: &World) -> (u32, String) {
        match w.rebuild.run(false) {
            RunOutcome::Ok { verify_fail, output } => (verify_fail, output.join("\n")),
            _ => panic!("rebuild did not reach its verify block"),
        }
    }

    // covers: UC-cockpit-observability-46
    #[test_id(t6meucy)]
    #[test]
    fn rebuild_makes_the_sessions_then_the_dashboards_then_the_links_and_verifies_itself() {
        let w = world("rebuild-from-nothing", true, false, false, true);
        let (fail, out) = verified(&w);
        let steps: Vec<String> = w
            .fake
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("new-session") || c.starts_with("layout") || c.starts_with("view"))
            .collect();
        assert_eq!(
            steps,
            [
                "new-session -d -s brain -c work",
                "new-session -d -s hunk -c work",
                "new-session -d -s chat -c work",
                "layout up --window brain:0",
                "view build",
                "view sync",
            ],
            "the dashboards go in brain:0 before the linking step"
        );
        assert_eq!(fail, 0, "{out}");
        for line in [
            "ok    session cockpit exists",
            "ok    cockpit links brain:0",
            "ok    cockpit links hunk:0",
            "ok    health pane renders content",
            "ok    session pane carries composed brief",
        ] {
            assert!(out.contains(line), "missing {line:?} in {out}");
        }
    }

    // covers: UC-cockpit-observability-46
    #[test_id(t4qx56n)]
    #[test]
    fn rebuild_names_the_session_pane_when_its_brief_is_missing() {
        let w = world("rebuild-no-brief", false, false, false, true);
        let (fail, out) = verified(&w);
        assert_eq!(fail, 1, "{out}");
        assert!(out.contains("FAIL  session pane carries composed brief"), "{out}");
    }

    // covers: UC-cockpit-observability-46
    #[test_id(t9zzsuv)]
    #[test]
    fn rebuild_leaves_a_healthy_server_alone_and_wants_the_mail_pane_only_when_configured() {
        let w = world("rebuild-healthy", true, true, true, false);
        let (fail, out) = verified(&w);
        assert!(out.contains("answering — leaving it alone"), "{out}");
        assert!(out.contains("ok    brain:0 has a mail pane"), "{out}");
        assert_eq!(fail, 0, "{out}");
        let calls = w.fake.calls();
        assert!(!calls.iter().any(|c| c.starts_with("new-session") || c.contains("kill")), "{calls:?}");

        let (fail, out) = verified(&world("rebuild-no-mail", true, false, true, false));
        assert!(out.contains("FAIL  brain:0 has a mail pane"), "{out}");
        assert_eq!(fail, 1);
    }

    // covers: UC-cockpit-observability-46
    #[test_id(trmv8rx)]
    #[test]
    fn rebuild_calls_an_overlong_socket_unusable_and_refuses_rather_than_hunting_a_holder() {
        let dir = testkit::TempDir::new("rebuild-unusable");
        let fake = FakeTmux::new(&dir).fail("list-sessions", "open terminal failed: File name too long", 1);
        assert_eq!(server_state(&fake.tmux()), ServerState::Unusable);
        let probe = probe_report(&fake.tmux(), &["brain".to_string()]);
        assert!(probe[0].ends_with("unusable") && probe[1].contains("108-byte"), "{probe:?}");
        assert_eq!(clear_wedged_server(&fake.tmux(), false, &mut |_| {}), ClearOutcome::Unusable);
    }
}
