//! tmux — the safe primitives `layout` and `rebuild` share: running the `tmux` binary,
//! pane-tag read/write, and the process-identity checks that keep a repair from ever acting
//! on the wrong server or the wrong process.
//!
//! **Never `pgrep -f` / `pkill -f`.** Every process this module finds is found either by a
//! pane's own `pane_pid` (from tmux itself) or, for the tmux *server* process, by walking
//! `/proc/net/unix` for the inode of the listening socket at a known path — never by matching
//! a command-line pattern, which can match the caller's own argv. See `server_pid`.
//!
//! Every `tmux` invocation goes through `Tmux::bin` (default `tmux`, overridable via
//! `TMUX_BIN` the same seam `layout.sh`/`rebuild.sh` used), and every invocation strips
//! `TMUX`/`TMUX_PANE` from the child's environment so this process's own attachment (if any)
//! never steers which server a bare `tmux` call reaches — the same reason both scripts
//! `unset TMUX` before their first call.

use std::io;
use std::process::{Command, Output, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Health,
    Mail,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Health => "health",
            Role::Mail => "mail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    Up,
    Absent,
    Unusable,
    Wedged,
}

/// Classify `tmux list-sessions`'s stderr text when it exits nonzero. Exact substrings
/// ported from `rebuild.sh`'s `server_state()`.
pub fn classify_server_stderr(stderr: &str) -> ServerState {
    if stderr.contains("no server running")
        || stderr.contains("No such file or directory")
        || stderr.contains("Connection refused")
    {
        ServerState::Absent
    } else if stderr.contains("File name too long") {
        ServerState::Unusable
    } else {
        ServerState::Wedged
    }
}

/// `classify_argv` — identity from the executable and (if the executable is a shell) the
/// script it runs, by argv **position**, never by substring of the whole command line (a
/// named scar: a system prompt merely *mentioning* `cockpit/health.sh` once mis-tagged an
/// unrelated agent session).
pub fn classify_argv(exe: &str, script: &str, mail_exe_basename: Option<&str>) -> Option<Role> {
    if exe.ends_with("/cockpit/health.sh") || script.ends_with("/cockpit/health.sh") {
        return Some(Role::Health);
    }
    if let Some(mail) = mail_exe_basename {
        let exe_base = exe.rsplit('/').next().unwrap_or(exe);
        if exe_base == mail {
            return Some(Role::Mail);
        }
    }
    None
}

/// `pane_role` over a set of argv vectors (the pane's own process plus its direct children,
/// in the order they should be checked) — the process-tree walk is the caller's job (it
/// needs `/proc`); this is the pure classification a test can drive with synthetic argv.
pub fn pane_role<'a>(
    argvs: impl IntoIterator<Item = &'a [String]>,
    mail_exe_basename: Option<&str>,
) -> Option<Role> {
    for argv in argvs {
        let Some(exe) = argv.first() else { continue };
        let exe_base = exe.rsplit('/').next().unwrap_or(exe);
        let mut script = "";
        if matches!(exe_base, "bash" | "sh" | "dash") {
            if let Some(a1) = argv.get(1) {
                if !a1.starts_with('-') {
                    script = a1;
                }
            }
        }
        if let Some(role) = classify_argv(exe, script, mail_exe_basename) {
            return Some(role);
        }
    }
    None
}

/// The listening socket's inode for `sock_path`, from `/proc/net/unix`'s text (flag
/// `00010000` is the listening bit; matching it keeps this from also matching connected
/// clients). Pure parse — the file read is the caller's job.
pub fn find_listening_inode(proc_net_unix: &str, sock_path: &str) -> Option<String> {
    for line in proc_net_unix.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Fields: num refcount protocol flags type st inode [path]
        if fields.len() < 8 {
            continue;
        }
        if fields[3] == "00010000" && fields[7] == sock_path {
            return Some(fields[6].to_string());
        }
    }
    None
}

/// Whether `descendants` (pid -> ppid pairs) contains any process descended from `root`,
/// counted — pure walk, capped at depth 64 like the bash `awk` original, to survive a cyclic
/// or corrupt table rather than looping forever.
pub fn descendants_of(root: u32, pairs: &[(u32, u32)]) -> usize {
    let mut count = 0;
    for &(pid, _) in pairs {
        // root's own entry must never walk (and so never count as its own descendant): the
        // bash `awk` this replaces starts its walk AT the candidate pid and checks it against
        // root before ever stepping to a parent, so a live root that appears in its own
        // process table (every live root does) always matched on the first check. Skipping
        // root's own entry outright is the fix, not a faithful port of that quirk.
        if pid == root {
            continue;
        }
        let mut q = pid;
        let mut depth = 0;
        while q != 1 && q != 0 && depth < 64 {
            if q == root {
                count += 1;
                break;
            }
            match pairs.iter().find(|&&(p, _)| p == q) {
                Some(&(_, ppid)) => q = ppid,
                None => break,
            }
            depth += 1;
        }
    }
    count
}

/// `stale_clients` — clients idle past `idle_secs`, given `"<tty> <activity-epoch>"` lines
/// (as `tmux list-clients -F '#{client_tty} #{client_activity}'` prints) and the current
/// epoch. Pure, so a test drives it with synthetic lines and a fixed `now` instead of a real
/// attached client.
pub fn stale_clients(now: i64, lines: &str, idle_secs: i64) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for line in lines.lines() {
        let mut parts = line.split_whitespace();
        let (Some(tty), Some(act)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(act) = act.parse::<i64>() else { continue };
        let age = now - act;
        if age > idle_secs {
            out.push((tty.to_string(), age));
        }
    }
    out
}

/// A repair that fails repeatedly must not be retried on every poll.
pub fn heal_ready(stamp_epoch: Option<i64>, now: i64, cooldown: i64) -> bool {
    match stamp_epoch {
        None => true,
        Some(t) => now - t >= cooldown,
    }
}

/// `proc_start` — a process's start epoch from `ps -o etimes=`'s output (elapsed seconds),
/// computed against `now`. Must never be called with an empty/unreadable `etimes`; the
/// caller (which does the `ps` IO) treats that as "unreadable" and leaves the subject alone
/// rather than reading a failed probe as epoch 0 — "infinitely old", which is what respawned
/// a stale-detector's subject 2212 times in 70 minutes under load (the scar `layout.sh`
/// itself cites). This function only does the arithmetic once `etimes` is known good.
pub fn proc_start_from_etimes(now: i64, etimes: i64) -> i64 {
    now - etimes
}

/// Should a pane (or process) running code since `started` be restarted, given the source
/// file's mtime? Strictly newer, matching `[ "$mtime" -gt "$started" ]`.
pub fn is_stale(mtime: i64, started: i64) -> bool {
    mtime > started
}

/// Does the health pane need to be re-cut as a full-height column? Strictly shorter than
/// the window, matching `[ "$hh" -lt "$wh" ]`.
pub fn needs_full_height_rebuild(pane_height: i64, window_height: i64) -> bool {
    pane_height < window_height
}

/// Thin wrapper over the `tmux` binary. Every call strips `TMUX`/`TMUX_PANE` from the
/// child's environment (see module docs).
pub struct Tmux {
    pub bin: String,
}

impl Default for Tmux {
    fn default() -> Self {
        Self::new()
    }
}

impl Tmux {
    pub fn new() -> Self {
        Tmux {
            bin: std::env::var("TMUX_BIN")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "tmux".to_string()),
        }
    }

    pub fn with_bin(bin: impl Into<String>) -> Self {
        Tmux { bin: bin.into() }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.env_remove("TMUX");
        c.env_remove("TMUX_PANE");
        c
    }

    fn output(&self, args: &[&str]) -> io::Result<Output> {
        self.cmd().args(args).stdin(Stdio::null()).output()
    }

    /// Runs `tmux <args>`, true iff it exited 0.
    pub fn ok(&self, args: &[&str]) -> bool {
        self.output(args).map(|o| o.status.success()).unwrap_or(false)
    }

    /// `ok`, under the name `layout.rs` calls it by (mutation calls: kill-pane, select-pane,
    /// set-option, ...).
    pub fn run_ok(&self, args: &[&str]) -> bool {
        self.ok(args)
    }

    /// Runs `tmux <args>`, returning trimmed stdout iff it exited 0.
    pub fn stdout(&self, args: &[&str]) -> Option<String> {
        match self.output(args) {
            Ok(o) if o.status.success() => {
                Some(String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
            }
            _ => None,
        }
    }

    /// `stdout`, under the name `layout.rs` calls it by (read calls: list-panes,
    /// display-message, has-session as a probe, ...).
    pub fn run(&self, args: &[&str]) -> Option<String> {
        self.stdout(args)
    }

    /// Runs `tmux <args>`, returning raw stdout (not trimmed — callers that split on lines
    /// want every line, including one that might matter as empty) regardless of exit code.
    pub fn stdout_lines(&self, args: &[&str]) -> Vec<String> {
        match self.output(args) {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|s| s.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// `tmux list-sessions` for `server_state()`: `Ok(())` when it answers, `Err(stderr)`
    /// otherwise (empty string if the process could not even be spawned).
    pub fn list_sessions_probe(&self) -> Result<(), String> {
        match self.output(&["list-sessions"]) {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).into_owned()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn has_session(&self, target: &str) -> bool {
        self.ok(&["has-session", "-t", target])
    }

    pub fn new_session(&self, name: &str, cwd: &str) -> bool {
        self.ok(&["new-session", "-d", "-s", name, "-c", cwd])
    }

    pub fn list_panes(&self, target: &str, fmt: &str) -> Vec<String> {
        self.stdout_lines(&["list-panes", "-t", target, "-F", fmt])
    }

    pub fn list_panes_all(&self, fmt: &str) -> Vec<String> {
        self.stdout_lines(&["list-panes", "-a", "-F", fmt])
    }

    pub fn list_windows(&self, target: &str, fmt: &str) -> Vec<String> {
        self.stdout_lines(&["list-windows", "-t", target, "-F", fmt])
    }

    pub fn list_windows_all(&self, fmt: &str) -> Vec<String> {
        self.stdout_lines(&["list-windows", "-a", "-F", fmt])
    }

    pub fn list_clients(&self, fmt: &str) -> Vec<String> {
        self.stdout_lines(&["list-clients", "-F", fmt])
    }

    pub fn detach_client(&self, tty: &str) {
        self.ok(&["detach-client", "-t", tty]);
    }

    pub fn set_pane_option(&self, pane: &str, key: &str, value: &str) {
        self.ok(&["set-option", "-p", "-t", pane, key, value]);
    }

    pub fn unset_pane_option(&self, pane: &str, key: &str) {
        self.ok(&["set-option", "-p", "-u", "-t", pane, key]);
    }

    pub fn set_window_option(&self, window: &str, key: &str, value: &str) {
        self.ok(&["set-option", "-w", "-t", window, key, value]);
    }

    pub fn unset_window_option(&self, window: &str, key: &str) {
        self.ok(&["set-option", "-w", "-u", "-t", window, key]);
    }

    pub fn set_global_option(&self, key: &str, value: &str) {
        self.ok(&["set-option", "-g", key, value]);
    }

    pub fn set_session_option(&self, session: &str, key: &str, value: &str) {
        self.ok(&["set-option", "-t", session, key, value]);
    }

    /// Full-window-height split (`-f`), detached (`-d`), right of `target` (`-h`), sized to
    /// `pct`% of the window. Returns the new pane id.
    pub fn split_full_height(&self, target: &str, pct: &str, cwd: &str, cmd: &str) -> Option<String> {
        self.stdout(&[
            "split-window", "-P", "-F", "#{pane_id}", "-d", "-h", "-f", "-l",
            &format!("{pct}%"), "-t", target, "-c", cwd, cmd,
        ])
    }

    /// Vertical split (`-v`) under `target`, sized to `pct`% of the window. Returns the new
    /// pane id.
    pub fn split_below(&self, target: &str, pct: &str, cwd: &str, cmd: &str) -> Option<String> {
        self.stdout(&[
            "split-window", "-P", "-F", "#{pane_id}", "-d", "-v", "-l",
            &format!("{pct}%"), "-t", target, "-c", cwd, cmd,
        ])
    }

    /// Split before (`-b`) `target`, vertical (`-v`), sized to `pct`% — used to restore a
    /// gone session pane above the dashboards.
    pub fn split_before(&self, target: &str, pct: &str, cwd: &str, cmd: &str) -> Option<String> {
        self.stdout(&[
            "split-window", "-P", "-F", "#{pane_id}", "-b", "-v", "-l",
            &format!("{pct}%"), "-t", target, "-c", cwd, cmd,
        ])
    }

    pub fn kill_pane(&self, pane: &str) {
        self.ok(&["kill-pane", "-t", pane]);
    }

    pub fn respawn_pane(&self, pane: &str, cmd: &str) -> bool {
        self.ok(&["respawn-pane", "-k", "-t", pane, cmd])
    }

    pub fn select_pane(&self, pane: &str) {
        self.ok(&["select-pane", "-t", pane]);
    }

    pub fn display_message(&self, target: Option<&str>, fmt: &str) -> Option<String> {
        match target {
            Some(t) => self.stdout(&["display-message", "-p", "-t", t, fmt]),
            None => self.stdout(&["display-message", "-p", fmt]),
        }
    }

    pub fn capture_pane(&self, pane: &str) -> Option<String> {
        self.stdout(&["capture-pane", "-p", "-t", pane])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_server_stderr_absent_variants() {
        assert_eq!(classify_server_stderr("no server running on ..."), ServerState::Absent);
        assert_eq!(
            classify_server_stderr("error connecting: No such file or directory"),
            ServerState::Absent
        );
        assert_eq!(classify_server_stderr("Connection refused"), ServerState::Absent);
    }

    #[test]
    fn classify_server_stderr_unusable_is_named_not_wedged() {
        assert_eq!(
            classify_server_stderr("open terminal failed: File name too long"),
            ServerState::Unusable
        );
    }

    #[test]
    fn classify_server_stderr_wedged_is_the_catchall() {
        assert_eq!(classify_server_stderr("server exited unexpectedly"), ServerState::Wedged);
        assert_eq!(classify_server_stderr(""), ServerState::Wedged);
    }

    #[test]
    fn classify_argv_health_by_exe_or_script() {
        assert_eq!(
            classify_argv("/opt/spira/cockpit/health.sh", "", None),
            Some(Role::Health)
        );
        assert_eq!(
            classify_argv("/usr/bin/bash", "/opt/spira/cockpit/health.sh", None),
            Some(Role::Health)
        );
    }

    #[test]
    fn classify_argv_mail_by_basename_only() {
        assert_eq!(
            classify_argv("/usr/local/bin/mailclient", "", Some("mailclient")),
            Some(Role::Mail)
        );
        assert_eq!(classify_argv("/usr/local/bin/other", "", Some("mailclient")), None);
    }

    #[test]
    fn classify_argv_never_matches_substring_of_whole_command_line() {
        // The named scar: a system prompt merely mentioning "cockpit/health.sh" must not
        // classify as health when it is not argv[0] or the shell's script argument.
        assert_eq!(
            classify_argv(
                "/usr/bin/claude",
                "",
                None
            ),
            None
        );
    }

    #[test]
    fn pane_role_checks_pid_then_children_in_order() {
        let child_argv = vec!["/usr/bin/bash".to_string(), "/opt/spira/cockpit/health.sh".to_string()];
        let parent_argv = vec!["/usr/bin/tmux".to_string()];
        let argvs: Vec<&[String]> = vec![&parent_argv, &child_argv];
        assert_eq!(pane_role(argvs, None), Some(Role::Health));
    }

    #[test]
    fn pane_role_shell_dash_arg_is_not_treated_as_script() {
        let argv = vec!["/usr/bin/bash".to_string(), "-c".to_string()];
        let argvs: Vec<&[String]> = vec![&argv];
        assert_eq!(pane_role(argvs, None), None);
    }

    #[test]
    fn find_listening_inode_matches_flag_and_path() {
        let table = "\
Num       RefCount Protocol Flags    Type St Inode Path
0000000000000000: 00000002 00000000 00010000 0001 01 12345 /tmp/tmux-1000/default
0000000000000000: 00000003 00000000 00000000 0001 03 99999 /tmp/tmux-1000/default";
        assert_eq!(
            find_listening_inode(table, "/tmp/tmux-1000/default"),
            Some("12345".to_string())
        );
    }

    #[test]
    fn find_listening_inode_ignores_non_listening_flag() {
        let table = "\
0000000000000000: 00000003 00000000 00000000 0001 03 99999 /tmp/tmux-1000/default";
        assert_eq!(find_listening_inode(table, "/tmp/tmux-1000/default"), None);
    }

    #[test]
    fn descendants_of_counts_transitive_children() {
        // root=100; 200 is a direct child; 300 is a grandchild; 400 is unrelated.
        let pairs = [(200, 100), (300, 200), (400, 1)];
        assert_eq!(descendants_of(100, &pairs), 2);
        assert_eq!(descendants_of(400, &pairs), 0);
    }

    #[test]
    fn descendants_of_survives_a_cycle() {
        let pairs = [(10, 20), (20, 10)];
        // Neither 10 nor 20 is "root", and the cycle must not hang the walk.
        assert_eq!(descendants_of(999, &pairs), 0);
    }

    #[test]
    fn stale_clients_only_past_threshold() {
        let lines = "/dev/pts/0 1000\n/dev/pts/1 1900\n";
        let out = stale_clients(2000, lines, 500);
        assert_eq!(out, vec![("/dev/pts/0".to_string(), 1000)]);
    }

    #[test]
    fn heal_ready_true_when_no_stamp() {
        assert!(heal_ready(None, 1000, 60));
    }

    #[test]
    fn heal_ready_false_within_cooldown() {
        assert!(!heal_ready(Some(950), 1000, 60));
    }

    #[test]
    fn heal_ready_true_after_cooldown() {
        assert!(heal_ready(Some(900), 1000, 60));
    }

    #[test]
    fn is_stale_strictly_greater() {
        assert!(is_stale(100, 50));
        assert!(!is_stale(50, 50));
        assert!(!is_stale(50, 100));
    }

    #[test]
    fn needs_full_height_rebuild_strictly_less() {
        assert!(needs_full_height_rebuild(10, 40));
        assert!(!needs_full_height_rebuild(40, 40));
    }
}
