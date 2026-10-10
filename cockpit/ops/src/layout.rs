//! `layout` — build the cockpit: session pane top-left, mail bottom-left, health down the
//! right. Replaces `cockpit/layout.sh` (sp-llbmi). See `../DESIGN.md`.
//! ```text
//!
//!     layout up       create/repair the dashboard pane (idempotent)
//!     layout down     remove it, leaving the session pane full-height
//!     layout status   what is running
//!     layout ensure   heal a cockpit that is already up; SILENT when nothing is wrong
//!
//!     layout up --window <target>   apply to a different tmux window
//! ```
//!
//! ```text
//!              +------------------------------------------+---------------+
//!              |   claude session                         |               |
//!              |   (flips to hunk for review)             |  ops health   |
//!              |                                          |               |
//!              +------------------------------------------+  full height  |
//!              |   mail (COCKPIT_MAIL), COCKPIT_BOTTOM_PCT |  ~33% wide    |
//!              +------------------------------------------+---------------+
//!               <-------- 100% - COCKPIT_RIGHT_PCT -------> <-- RIGHT_PCT ->
//! ```
//!
//! ORDER IS THE GEOMETRY: the health pane is split off the WHOLE WINDOW, which gives it the
//! window's full height. A dashboard with a column's worth of rows is not a cosmetic
//! change — `health` sizes each of its sections to the rows it is given.
//!
//! PANES ARE ADDRESSED BY TAG, NEVER BY INDEX. tmux renumbers pane indices the moment a pane
//! dies, and the session pane is the one most likely to die (it holds a shell the operator
//! can exit). The dashboard pane carries a pane-scoped option `@cockpit` (`health`/`mail`)
//! and is resolved by pane id through it. Identity survives renumbering; an index does not.
//!
//! `up` is a REPAIR, not just a create: it normalises whatever it finds. The window must
//! never reach zero panes, or it dies and takes the session with it.
//!
//! THE ABSENCE OF EVERY `@cockpit` PANE IS AMBIGUOUS ON ITS OWN — it is what `down` leaves
//! behind, and it is also what a crash leaves behind. `down` records the deliberate state in
//! `DOWN_MARKER`; `ensure` rebuilds from nothing only when that marker is ABSENT.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::procfs;
use crate::tmux::Tmux;

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
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "health" => Some(Role::Health),
            "mail" => Some(Role::Mail),
            _ => None,
        }
    }
}

/// Configuration, resolved once at startup from the process environment — the same
/// variables `conf.sh` exports, read directly rather than re-sourcing 2,700 lines of bash
/// per invocation. See `../DESIGN.md` "Configuration".
pub struct Conf {
    pub spira_release: String,
    pub cock: PathBuf,
    pub run: PathBuf,
    pub cwd: String,
    pub right_pct: u32,
    pub bottom_pct: u32,
    pub mail_cmd: Option<String>,
    pub heal_cooldown_secs: u64,
    pub idle_secs: i64,
    pub mouse_on: bool,
    pub clipboard_on: bool,
    pub spira_repo: String,
    pub spira_conf: Option<String>,
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Is `exe`'s basename (after stripping any leading path) found on `PATH`?
fn on_path(exe: &str) -> bool {
    if exe.is_empty() {
        return false;
    }
    if exe.contains('/') {
        return std::path::Path::new(exe).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(exe).is_file())
        })
        .unwrap_or(false)
}

impl Conf {
    /// `Err` matches `layout.sh`'s own refusal: `SPIRA_RELEASE` unset is a launcher defect,
    /// not a fallback case — every pane's PATH is built from it.
    pub fn from_env() -> Result<Conf, String> {
        use spira_config::process::{cfg, cfg_parse};
        let spira_release = env_nonempty("SPIRA_RELEASE").ok_or_else(|| {
            "cockpit: SPIRA_RELEASE is not set — the launcher sets it to the release the \
             running system executes, and every pane's PATH is built from it"
                .to_string()
        })?;

        let mut cock = cfg("SPIRA_COCKPIT")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/nonexistent-cockpit"));
        // Split-checkout mode: the renderer must come from the same release the collector
        // reads, unless SPIRA_DEV_RENDERER=1 opts back in to this checkout's health binary.
        if env_nonempty("SPIRA_DEV_RENDERER").is_none() {
            if let Some(prod) = cfg("SPIRA_PROD").ok().filter(|s| !s.is_empty()) {
                let prod_cock = PathBuf::from(&prod).join("cockpit");
                if prod_cock.is_dir() {
                    cock = prod_cock;
                }
            }
        }

        let run = cfg("SPIRA_RUN")?;
        let run = Some(run)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "cockpit: SPIRA_RUN is unset or empty — refusing to guess a runtime directory".to_string())?;
        fs::create_dir_all(&run)
            .map_err(|_| format!("cockpit: runtime directory {} is not writable", run.display()))?;

        let cwd = cfg("COCKPIT_CWD").unwrap_or_default();
        // COCKPIT_RIGHT_PCT/COCKPIT_BOTTOM_PCT each carry a fixed toml-level default (33/28);
        // a value that fails to parse is a bad config document, not a case to paper over.
        let right_pct: u32 = cfg_parse("COCKPIT_RIGHT_PCT")?;
        let bottom_pct: u32 = cfg_parse("COCKPIT_BOTTOM_PCT")?;

        // The mail pane exists only when its client does: a pane whose program is missing
        // dies at once, and `ensure` would respawn it every minute.
        // `${COCKPIT_MAIL:-}` in the original: unset/empty means no mail pane, full stop.
        // conf.sh (sourced at process start, see ../conf.rs) is the only place a default for
        // this key belongs; a second, hardcoded default here would silently override an
        // operator's deliberate "no mail pane" (COCKPIT_MAIL set empty) the moment conf.sh
        // agreed with bash and left it that way.
        let mail_cmd = cfg("COCKPIT_MAIL").ok().filter(|s| !s.is_empty());
        let mail_cmd = mail_cmd.filter(|cmd| {
            let exe = cmd.split_whitespace().next().unwrap_or("");
            on_path(exe)
        });

        let heal_cooldown_secs = env_nonempty("COCKPIT_HEAL_COOLDOWN")
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
        let idle_secs = cfg("COCKPIT_CLIENT_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(21600);
        // Both carry a fixed toml-level default ("on"); any other value, resolved or not, is
        // read literally rather than coerced to a Rust-side on/off default.
        let mouse_on = !matches!(cfg("COCKPIT_MOUSE")?.as_str(), "off" | "no" | "0");
        let clipboard_on = !matches!(cfg("COCKPIT_CLIPBOARD")?.as_str(), "off" | "no" | "0");
        let spira_repo = env_nonempty("SPIRA_REPO").unwrap_or_default();
        let spira_conf = env_nonempty("SPIRA_CONF");

        Ok(Conf {
            spira_release,
            cock,
            run,
            cwd,
            right_pct,
            bottom_pct,
            mail_cmd,
            heal_cooldown_secs,
            idle_secs,
            mouse_on,
            clipboard_on,
            spira_repo,
            spira_conf,
        })
    }

    /// The prefix every pane command carries, so a respawn (from tmux, or from `ensure`)
    /// uses the same release and config the original `up` did — the tmux server's own
    /// environment is fixed at server start and never updated per-pane.
    pub fn rel_prefix(&self) -> String {
        let mut p = String::new();
        if let Some(c) = &self.spira_conf {
            p.push_str(&format!("SPIRA_CONF='{c}' "));
        }
        p.push_str(&format!(
            "SPIRA_RELEASE='{r}' PATH='{r}/bin:{r}/spira:/usr/local/bin:/usr/bin:/bin' ",
            r = self.spira_release
        ));
        p
    }

    /// The ops pane is the lifecycle lens (`lc-view`, sp-lpw5ol), which replaced `health`:
    /// it reads state only from the lifecycle machine and publishes the phone page's snapshot.
    /// Interactive since sp-5j35g5: it reads keys and clicks, and sizes itself to the pane.
    pub fn health_cmd(&self) -> String {
        format!("{}lc-view tui 15", self.rel_prefix())
    }

    pub fn down_marker(&self) -> PathBuf {
        self.run.join("cockpit.down")
    }

    pub fn heal_log_path(&self) -> PathBuf {
        self.run.join("cockpit-heal.log")
    }

    pub fn heal_stamp_path(&self) -> PathBuf {
        self.run.join(".cockpit-heal-stamp")
    }
}

/// Does `exe`'s (optionally `script`'s, when `exe` is a shell) argv identify this pane's
/// role? Read from what the pane is RUNNING, never `pane_start_command` (that survives
/// `respawn-pane`, so a swapped pane keeps the tag it was first created with) and never as
/// a substring of the whole command line (a session launched with a system prompt that
/// merely MENTIONS `cockpit/health.sh` must not be classified `health`). `exe` is argv[0];
/// `script` is argv[1] when `exe` is a shell running a script.
pub fn classify_argv(exe: &str, script: &str, mail_exe: Option<&str>) -> Option<Role> {
    if exe.ends_with("/health") || exe == "health" || script.ends_with("/health.sh") || script.ends_with("/health")
        || exe.ends_with("/lc-view") || exe == "lc-view"
    {
        return Some(Role::Health);
    }
    if let Some(mail_exe) = mail_exe {
        fn base(s: &str) -> &str {
            s.rsplit('/').next().unwrap_or(s)
        }
        if base(exe) == base(mail_exe) {
            return Some(Role::Mail);
        }
    }
    None
}

/// The role a pane at `pid` is running, read from its own process and its immediate
/// children (matching `pane_role`'s `pgrep -P`, via `procfs::children_of`).
pub fn pane_role(pid: i32, mail_exe: Option<&str>) -> Option<Role> {
    let mut candidates = vec![pid];
    candidates.extend(procfs::children_of(pid));
    for p in candidates {
        let Some(argv) = procfs::cmdline(p) else { continue };
        if argv.is_empty() {
            continue;
        }
        let exe = &argv[0];
        let mut script = "";
        let base = exe.rsplit('/').next().unwrap_or(exe);
        if matches!(base, "bash" | "sh" | "dash") && argv.len() > 1 && !argv[1].starts_with('-') {
            script = &argv[1];
        }
        if let Some(role) = classify_argv(exe, script, mail_exe) {
            return Some(role);
        }
    }
    None
}

/// Lines idle past `idle_secs` as of `now`, from `tty act` pairs (`#{client_tty}
/// #{client_activity}`) — split out so a test can feed synthetic clients and a fixed clock
/// instead of a real attached terminal.
pub fn stale_clients(lines: &str, now: i64, idle_secs: i64) -> Vec<(String, i64)> {
    if idle_secs <= 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lines.lines() {
        let mut it = line.split_whitespace();
        let (Some(tty), Some(act)) = (it.next(), it.next()) else { continue };
        let Ok(act) = act.parse::<i64>() else { continue };
        let age = now - act;
        if age > idle_secs {
            out.push((tty.to_string(), age));
        }
    }
    out
}

/// One target window id per distinct window holding cockpit panes OR marked `@cockpit_up`.
/// Dedup is by window_id, not by name: `cockpit:2` and `brain:0` can be the same window
/// object (linked windows), and a name-keyed scan would find — and repair — the break twice.
pub fn cockpit_window_targets(
    pane_lines: &str, // "#{window_id}|#{session_name}:#{window_index}|#{@cockpit}"
    win_lines: &str,  // "#{window_id}|#{session_name}:#{window_index}|#{@cockpit_up}"
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in pane_lines.lines().chain(win_lines.lines()) {
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() != 3 {
            continue;
        }
        let matches = cols[2] == "health" || cols[2] == "mail" || cols[2] == "1";
        if matches && seen.insert(cols[0].to_string()) {
            out.push(cols[1].to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_argv_matches_health_by_script_suffix_never_substring() {
        assert_eq!(classify_argv("bash", "/opt/spira/cockpit/health.sh", None), Some(Role::Health));
        // The lifecycle lens is the ops pane now (sp-lpw5ol).
        assert_eq!(classify_argv("/opt/bin/lc-view", "loop", None), Some(Role::Health));
        assert_eq!(classify_argv("lc-view", "", None), Some(Role::Health));
        // A system prompt that merely mentions the path as argv text (not argv[0]/[1] of a
        // shell) must not classify — callers only ever pass exe/script, not the whole line,
        // which is what makes this safe: "argv position, never substring".
        assert_eq!(classify_argv("claude", "", None), None);
    }

    #[test]
    fn classify_argv_matches_mail_by_basename_only() {
        assert_eq!(classify_argv("/usr/bin/aerc", "", Some("aerc")), Some(Role::Mail));
        assert_eq!(classify_argv("/usr/bin/aerclike", "", Some("aerc")), None);
    }

    #[test]
    fn classify_argv_neither() {
        assert_eq!(classify_argv("zsh", "", Some("aerc")), None);
    }

    #[test]
    fn stale_clients_filters_by_idle_secs() {
        let lines = "/dev/pts/0 1000\n/dev/pts/1 500\n";
        let now = 1000 + 21600 + 1;
        let out = stale_clients(lines, now, 21600);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "/dev/pts/0");
    }

    #[test]
    fn stale_clients_disabled_at_zero() {
        assert!(stale_clients("/dev/pts/0 0\n", 100_000, 0).is_empty());
    }

    #[test]
    fn stale_clients_ignores_malformed_lines() {
        assert!(stale_clients("garbage\n\n", 100_000, 10).is_empty());
    }

    #[test]
    fn cockpit_window_targets_dedups_by_window_id() {
        let panes = "@1|cockpit:2|health\n@1|cockpit:2|mail\n";
        let wins = "@1|brain:0|1\n@2|hunk:0|1\n";
        let out = cockpit_window_targets(panes, wins);
        assert_eq!(out, vec!["cockpit:2".to_string(), "hunk:0".to_string()]);
    }

    #[test]
    fn rel_prefix_carries_release_and_conf() {
        let c = Conf {
            spira_release: "/rel".into(),
            cock: PathBuf::new(),
            run: PathBuf::new(),
            cwd: String::new(),
            right_pct: 33,
            bottom_pct: 28,
            mail_cmd: None,
            heal_cooldown_secs: 60,
            idle_secs: 21600,
            mouse_on: true,
            clipboard_on: true,
            spira_repo: String::new(),
            spira_conf: Some("/etc/spira-custom.conf".into()),
        };
        let p = c.rel_prefix();
        assert!(p.contains("SPIRA_CONF='/etc/spira-custom.conf'"));
        assert!(p.contains("SPIRA_RELEASE='/rel'"));
        assert!(p.contains("PATH='/rel/bin:/rel/spira:/usr/local/bin:/usr/bin:/bin'"));
    }

    #[test]
    fn apply_mouse_mode_binds_drag_without_alternate_on() {
        use std::process::Command;
        if Command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        let dir = testkit::TempDir::new("sp-ihxxy-drag");
        let sock = format!("sp-ihxxy-{}", std::process::id());
        let wrap = dir.join("tmux-scratch");
testkit::write_exe(
            &wrap,
            &format!("#!/bin/sh\nTMUX_TMPDIR='{}' exec tmux -L {sock} \"$@\"\n", dir.display()),
        );
        let tmux = Tmux::with_bin(wrap.to_string_lossy().to_string());
        assert!(tmux.run_ok(&["new-session", "-d", "-s", "x"]));
        let conf = Conf {
            spira_release: String::new(),
            cock: PathBuf::new(),
            run: PathBuf::new(),
            cwd: String::new(),
            right_pct: 33,
            bottom_pct: 28,
            mail_cmd: None,
            heal_cooldown_secs: 60,
            idle_secs: 0,
            mouse_on: true,
            clipboard_on: false,
            spira_repo: String::new(),
            spira_conf: None,
        };
        let before = tmux.stdout(&["list-keys", "-T", "root", "MouseDrag1Pane"]).unwrap_or_default();
        assert!(before.contains("alternate_on"), "control: default binding must test alternate_on: {before}");
        Layout { tmux, conf }.apply_mouse_mode();
        let t = Tmux::with_bin(wrap.to_string_lossy().to_string());
        let after = t.stdout(&["list-keys", "-T", "root", "MouseDrag1Pane"]).unwrap_or_default();
        t.run_ok(&["kill-server"]);
        assert!(after.contains("mouse_any_flag"), "{after}");
        assert!(!after.contains("alternate_on"), "{after}");
    }
}

/// Wall-clock seconds since the Unix epoch, saturating to 0 on a clock error — used only
/// for idle-client detection and heal cooldown timing, neither of which is safety-critical
/// enough to justify a panic on an unreadable clock.
pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Runtime orchestration: everything that talks to a real (or faked-via-socket) tmux
/// server. Kept separate from the pure functions above so the logic that decides WHAT to
/// do stays unit-testable without a server, while this stays a thin, auditable sequence of
/// tmux calls matching `layout.sh`'s `up`/`down`/`ensure`/`status`.
pub struct Layout {
    pub tmux: Tmux,
    pub conf: Conf,
}

impl Layout {
    fn heal_log(&self, msg: &str) {
        let now = chrono_like_timestamp();
        let line = format!("{now} {msg}\n");
        print!("{line}");
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.conf.heal_log_path())
        {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// `#{@cockpit} #{pane_id}` lines -> the pane id tagged `role`, if any.
    fn tagged(&self, window: &str, role: Role) -> Option<String> {
        let out = self
            .tmux
            .run(&["list-panes", "-t", window, "-F", "#{@cockpit} #{pane_id}"])?;
        out.lines().find_map(|l| {
            let mut it = l.split_whitespace();
            let (Some(t), Some(id)) = (it.next(), it.next()) else { return None };
            (t == role.as_str()).then(|| id.to_string())
        })
    }

    fn all_tagged(&self, window: &str) -> Vec<String> {
        let Some(out) = self
            .tmux
            .run(&["list-panes", "-t", window, "-F", "#{@cockpit} #{pane_id}"])
        else {
            return Vec::new();
        };
        out.lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let (Some(t), Some(id)) = (it.next(), it.next()) else { return None };
                (t == "health" || t == "mail").then(|| id.to_string())
            })
            .collect()
    }

    /// The session pane: the first untagged pane, preferring this process's own pane if it
    /// is one of them (an agent running `up` from inside the cockpit is the normal case).
    fn session_pane(&self, window: &str) -> Option<String> {
        let Some(out) = self
            .tmux
            .run(&["list-panes", "-t", window, "-F", "#{@cockpit} #{pane_id}"])
        else {
            return None;
        };
        let untagged: Vec<String> = out
            .lines()
            .filter_map(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                match parts.len() {
                    1 => Some(parts[0].to_string()),
                    2 if parts[0] != "health" && parts[0] != "mail" => Some(parts[1].to_string()),
                    _ => None,
                }
            })
            .collect();
        if let Ok(me) = std::env::var("TMUX_PANE") {
            if untagged.iter().any(|p| p == &me) {
                return Some(me);
            }
        }
        untagged.into_iter().next()
    }

    fn tag_pane(&self, pane: &str, role: Role) {
        self.tmux
            .run_ok(&["set-option", "-p", "-t", pane, "@cockpit", role.as_str()]);
    }

    /// Re-derive every pane's tag from what it is actually running, tagging anything
    /// untagged that turns out to be a dashboard.
    fn adopt_untagged(&self, window: &str) {
        let Some(out) = self.tmux.run(&[
            "list-panes",
            "-t",
            window,
            "-F",
            "#{@cockpit}|#{pane_id}|#{pane_pid}",
        ]) else {
            return;
        };
        let mail_exe = self.mail_exe();
        for line in out.lines() {
            let cols: Vec<&str> = line.split('|').collect();
            if cols.len() != 3 || !cols[0].is_empty() {
                continue;
            }
            let Ok(pid) = cols[2].parse::<i32>() else { continue };
            if let Some(role) = pane_role(pid, mail_exe.as_deref()) {
                self.tag_pane(cols[1], role);
            }
        }
    }

    /// Correct every tag that no longer matches what the pane runs — including clearing a
    /// tag on a live pane that runs no dashboard, which is otherwise permanent (`up` kills
    /// every tagged pane, so a wrongly-tagged session pane would be killed on next repair).
    fn retag_dashboards(&self, window: &str) {
        let Some(out) = self.tmux.run(&[
            "list-panes",
            "-t",
            window,
            "-F",
            "#{pane_id}|#{pane_pid}|#{pane_dead}|#{@cockpit}",
        ]) else {
            return;
        };
        let mail_exe = self.mail_exe();
        for line in out.lines() {
            let cols: Vec<&str> = line.split('|').collect();
            if cols.len() != 4 {
                continue;
            }
            let (id, pid_s, dead, tag) = (cols[0], cols[1], cols[2], cols[3]);
            let Ok(pid) = pid_s.parse::<i32>() else { continue };
            match pane_role(pid, mail_exe.as_deref()) {
                Some(role) => {
                    if tag != role.as_str() {
                        self.tag_pane(id, role);
                    }
                }
                None => {
                    if !tag.is_empty() && dead != "1" {
                        self.heal_log(&format!(
                            "{window}: {id} is tagged {tag} but runs no dashboard — clearing the tag"
                        ));
                        self.tmux.run_ok(&["set-option", "-p", "-u", "-t", id, "@cockpit"]);
                    }
                }
            }
        }
    }

    fn mail_exe(&self) -> Option<String> {
        self.conf
            .mail_cmd
            .as_deref()
            .and_then(|c| c.split_whitespace().next())
            .map(str::to_string)
    }

    fn health_width(&self, target: &str) -> String {
        const MIN_COLS: u32 = 45;
        let width = self
            .tmux
            .run(&["display-message", "-p", "-t", target, "#{window_width}"])
            .and_then(|w| w.trim().parse::<u32>().ok());
        match width {
            Some(w) if w * self.conf.right_pct / 100 < MIN_COLS => MIN_COLS.min(w / 2).to_string(),
            _ => format!("{}%", self.conf.right_pct),
        }
    }

    fn install_resize_hooks(&self, window: &str) {
        let Some(h) = self.tagged(window, Role::Health) else { return };
        let cmd = format!("resize-pane -t {h} -x {}%", self.conf.right_pct);
        for hook in ["window-resized", "client-resized", "client-attached"] {
            self.tmux.run_ok(&["set-hook", "-t", session_of(window), hook, &cmd]);
        }
    }

    fn split_health(&self, target: &str) -> Option<String> {
        self.tmux.run(&[
            "split-window",
            "-P",
            "-F",
            "#{pane_id}",
            "-d",
            "-h",
            "-f",
            "-l",
            &self.health_width(target),
            "-t",
            target,
            "-c",
            &self.conf.cwd,
            &self.conf.health_cmd(),
        ])
    }

    fn split_mail(&self, target: &str) -> Option<String> {
        let mail_cmd = self.conf.mail_cmd.as_ref()?;
        let cmd = format!("{}{}", self.conf.rel_prefix_conf_only(), mail_cmd);
        self.tmux.run(&[
            "split-window",
            "-P",
            "-F",
            "#{pane_id}",
            "-d",
            "-v",
            "-l",
            &format!("{}%", self.conf.bottom_pct),
            "-t",
            target,
            "-c",
            &self.conf.cwd,
            &cmd,
        ])
    }

    /// `up` — create or repair the dashboard in `window`.
    pub fn up(&self, window: &str) -> Result<String, String> {
        if self.tmux.run(&["has-session", "-t", session_of(window)]).is_none() {
            return Err(format!("no tmux session for '{window}'"));
        }
        let _ = fs::remove_file(self.conf.down_marker());
        self.adopt_untagged(window);
        self.retag_dashboards(window);

        let mut sess = self.session_pane(window);
        if sess.is_none() {
            let anchor = self
                .all_tagged(window)
                .into_iter()
                .next()
                .ok_or_else(|| "cockpit: could not restore the session pane".to_string())?;
            let conc = format!("{}/concierge.sh", self.conf.spira_repo);
            let is_exec = std::path::Path::new(&conc).is_file();
            let cmd = if is_exec {
                format!("{}{} here", self.conf.rel_prefix(), conc)
            } else {
                String::new()
            };
            let mut args = vec![
                "split-window", "-P", "-F", "#{pane_id}", "-b", "-v", "-l", "60%", "-t", &anchor,
                "-c", &self.conf.cwd,
            ];
            if !cmd.is_empty() {
                args.push(&cmd);
            }
            let new_sess = self
                .tmux
                .run(&args)
                .ok_or_else(|| "cockpit: could not restore the session pane".to_string())?;
            if is_exec {
                println!("cockpit: session pane was gone — opened concierge.sh here at {new_sess}");
            } else {
                println!("cockpit: session pane was gone — opened a shell at {new_sess} (concierge.sh not found)");
            }
            sess = Some(new_sess);
        }
        let sess = sess.unwrap();

        for p in self.all_tagged(window) {
            self.tmux.run_ok(&["kill-pane", "-t", &p]);
        }

        let hea = self
            .split_health(&sess)
            .ok_or_else(|| "cockpit: health split failed".to_string())?;
        self.tag_pane(&hea, Role::Health);
        let mut mai = None;
        if self.conf.mail_cmd.is_some() {
            let m = self.split_mail(&sess).ok_or_else(|| "cockpit: mail split failed".to_string())?;
            self.tag_pane(&m, Role::Mail);
            mai = Some(m);
        }

        let mut msg = format!("cockpit: up in {window} (session {sess} · health {hea}");
        if let Some(m) = &mai {
            msg.push_str(&format!(" · mail {m}"));
        }
        msg.push(')');

        self.tmux
            .run_ok(&["set-option", "-w", "-t", window, "@cockpit_up", "1"]);
        self.tmux
            .run_ok(&["set-option", "-t", session_of(window), "window-size", "latest"]);
        self.install_resize_hooks(window);
        self.apply_mouse_mode();
        self.apply_clipboard_mode();
        self.tmux.run_ok(&["select-pane", "-t", &sess]);
        Ok(msg)
    }

    pub fn down(&self, window: &str) -> Result<String, String> {
        self.adopt_untagged(window);
        self.retag_dashboards(window);
        let sess = self.session_pane(window).ok_or_else(|| {
            format!("cockpit: no session pane in {window} — run 'layout up' to restore it")
        })?;
        for p in self.all_tagged(window) {
            self.tmux.run_ok(&["kill-pane", "-t", &p]);
        }
        self.tmux.run_ok(&["set-option", "-w", "-u", "-t", window, "@cockpit_up"]);
        self.tmux.run_ok(&["select-pane", "-t", &sess]);
        let _ = fs::create_dir_all(&self.conf.run);
        let _ = fs::write(self.conf.down_marker(), format!("{}\n", chrono_like_timestamp()));
        Ok(format!("cockpit: down in {window}"))
    }

    pub fn status(&self, window: &str) -> String {
        self.adopt_untagged(window);
        let panes = self
            .tmux
            .run(&["list-panes", "-t", window])
            .map(|s| s.lines().count())
            .unwrap_or(0);
        let mut out = format!("window:    {window} ({panes} panes)\n");
        match self.session_pane(window) {
            Some(s) => out.push_str(&format!("session:   {s}\n")),
            None => out.push_str("session:   MISSING — run 'layout up' to restore it\n"),
        }
        match self.tagged(window, Role::Health) {
            Some(p) => out.push_str(&format!("health:    {p}\n")),
            None => out.push_str("health:    absent\n"),
        }
        if self.conf.mail_cmd.is_none() {
            out.push_str("mail:      off (COCKPIT_MAIL is empty or not on PATH)\n");
        } else {
            match self.tagged(window, Role::Mail) {
                Some(p) => out.push_str(&format!("mail:      {p}\n")),
                None => out.push_str("mail:      absent\n"),
            }
        }
        out
    }

    fn apply_mouse_mode(&self) {
        if self.conf.mouse_on {
            self.tmux.run_ok(&["set-option", "-g", "mouse", "on"]);
            // tmux's default also tests alternate_on, which forwards a drag to any
            // full-screen app that never asked for the mouse; only mouse_any_flag may.
            self.tmux.run_ok(&[
                "bind-key",
                "-T",
                "root",
                "MouseDrag1Pane",
                "if-shell",
                "-F",
                "#{||:#{pane_in_mode},#{mouse_any_flag}}",
                "send-keys -M",
                "copy-mode -M",
            ]);
        }
    }

    fn apply_clipboard_mode(&self) {
        if self.conf.clipboard_on {
            self.tmux.run_ok(&["set-option", "-g", "set-clipboard", "on"]);
        }
    }

    fn detach_idle_clients(&self) {
        if self.conf.idle_secs <= 0 {
            return;
        }
        let Some(out) = self
            .tmux
            .run(&["list-clients", "-F", "#{client_tty} #{client_activity}"])
        else {
            return;
        };
        for (tty, age) in stale_clients(&out, now_epoch(), self.conf.idle_secs) {
            self.heal_log(&format!("detach: client {tty} idle {age}s — detaching"));
            self.tmux.run_ok(&["detach-client", "-t", &tty]);
        }
    }

    fn heal_ready(&self) -> bool {
        let Ok(meta) = fs::metadata(self.conf.heal_stamp_path()) else { return true };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        now_epoch() - mtime >= self.conf.heal_cooldown_secs as i64
    }

    fn touch_heal_stamp(&self) {
        let _ = fs::write(self.conf.heal_stamp_path(), "");
    }

    fn cockpit_windows(&self) -> Vec<String> {
        let panes = self
            .tmux
            .run(&["list-panes", "-a", "-F", "#{window_id}|#{session_name}:#{window_index}|#{@cockpit}"])
            .unwrap_or_default();
        let wins = self
            .tmux
            .run(&["list-windows", "-a", "-F", "#{window_id}|#{session_name}:#{window_index}|#{@cockpit_up}"])
            .unwrap_or_default();
        cockpit_window_targets(&panes, &wins)
    }

    /// `ensure` — heal every window already carrying a cockpit; silent when nothing needs
    /// it. Refuses (exit 0) when this binary is not the installed one, so a worktree copy
    /// never adopts and respawns the operator's live panes with its own paths.
    pub fn ensure(&self, installed_bin: &std::path::Path, self_bin: &std::path::Path) -> Result<String, String> {
        let installed = fs::canonicalize(installed_bin).unwrap_or_else(|_| installed_bin.to_path_buf());
        let this = fs::canonicalize(self_bin).unwrap_or_else(|_| self_bin.to_path_buf());
        if installed != this {
            return Err(format!(
                "cockpit: ensure refused — this is a copy, not the installed layout binary; run {} ensure",
                installed_bin.display()
            ));
        }

        self.detach_idle_clients();
        self.apply_mouse_mode();
        self.apply_clipboard_mode();

        let windows = self.cockpit_windows();
        for w in &windows {
            self.tmux.run_ok(&["set-option", "-t", session_of(w), "window-size", "latest"]);
        }

        if windows.is_empty() {
            if self.conf.down_marker().is_file() {
                // deliberate — `down` was run; rebuilding it is a fight, not a repair.
            } else if self.heal_ready() {
                self.touch_heal_stamp();
                self.heal_log("no cockpit window found anywhere, and no down marker — rebuilding from scratch");
                // Bare name on the release PATH (sp-llbmi convention: every binary here is
                // invoked that way, never by a path constructed by hand) — `rebuild` lives
                // in `$SPIRA_RELEASE/bin`, not under `$SPIRA_COCKPIT`, which is where the
                // bash scripts it replaces used to live.
                // batch-job: this runs whatever its caller names, as long as that takes
                match std::process::Command::new("rebuild").output() {
                    Ok(out) if out.status.success() => {
                        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
                        s.push_str(&String::from_utf8_lossy(&out.stderr));
                        self.heal_log(&format!("rebuild: {}", s.replace('\n', " ")));
                    }
                    Ok(out) => {
                        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
                        s.push_str(&String::from_utf8_lossy(&out.stderr));
                        self.heal_log(&format!("rebuild FAILED — {}", s.replace('\n', " ")));
                    }
                    Err(e) => self.heal_log(&format!("rebuild FAILED — {e}")),
                }
            }
        }

        for w in &windows {
            self.adopt_untagged(w);
            self.retag_dashboards(w);
            if self.session_pane(w).is_some() {
                self.repair_dashboards(w);
                self.restart_if_stale(w, Role::Health);
                self.install_resize_hooks(w);
                continue;
            }
            if !self.heal_ready() {
                continue;
            }
            self.touch_heal_stamp();
            self.heal_log(&format!("{w}: session pane gone — repairing"));
            match self.up(w) {
                Ok(s) => self.heal_log(&format!("{w}: {}", s.replace('\n', " "))),
                Err(e) => self.heal_log(&format!("{w}: REPAIR FAILED — {}", e.replace('\n', " "))),
            }
        }

        // No watchdog over the collector's liveness — only over the code it is running,
        // and only when at least one cockpit window exists. Process supervision belongs to
        // systemd (`spira-cockpit.service`, Restart=always); this only checks whether a
        // running collector predates its own source.
        if !windows.is_empty() {
            self.restart_spira_collector_if_stale();
        }
        Ok(String::new())
    }

    fn repair_dashboards(&self, window: &str) {
        let Some(sess) = self.session_pane(window) else { return };
        let active = self
            .tmux
            .run(&["display-message", "-t", window, "-p", "#{pane_id}"]);

        let Some(list) = self
            .tmux
            .run(&["list-panes", "-t", window, "-F", "#{@cockpit} #{pane_id}"])
        else {
            return;
        };
        let health_panes: Vec<&str> = list
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let (Some(t), Some(id)) = (it.next(), it.next()) else { return None };
                (t == "health").then_some(id)
            })
            .collect();
        for &dup in health_panes.iter().skip(1) {
            self.heal_log(&format!("{window}: duplicate health pane {dup} — closing"));
            self.tmux.run_ok(&["kill-pane", "-t", dup]);
        }

        if self.tagged(window, Role::Health).is_none() {
            self.heal_log(&format!("{window}: health pane gone — respawning as the full-height right column"));
            if let Some(h) = self.split_health(&sess) {
                self.tag_pane(&h, Role::Health);
            }
        }
        self.normalize_geometry(window);
        if self.conf.mail_cmd.is_some() && self.tagged(window, Role::Mail).is_none() {
            self.heal_log(&format!("{window}: mail pane gone — respawning under the session"));
            if let Some(m) = self.split_mail(&sess) {
                self.tag_pane(&m, Role::Mail);
            }
        }
        if let Some(active) = active {
            let still_there = self
                .tmux
                .run(&["list-panes", "-t", window, "-F", "#{pane_id}"])
                .map(|s| s.lines().any(|l| l == active))
                .unwrap_or(false);
            if still_there {
                self.tmux.run_ok(&["select-pane", "-t", &active]);
                return;
            }
        }
        self.tmux.run_ok(&["select-pane", "-t", &sess]);
    }

    /// HEALTH MUST BE FULL WINDOW HEIGHT. A half-height health pane is on the right side,
    /// the right width, running the right program — only its height is wrong, and the eye
    /// reads it as the layout working. So the height is measured against the window.
    fn normalize_geometry(&self, window: &str) {
        let Some(h) = self.tagged(window, Role::Health) else { return };
        let hh = self.tmux.run(&["display-message", "-p", "-t", &h, "#{pane_height}"]);
        let wh = self.tmux.run(&["display-message", "-p", "-t", &h, "#{window_height}"]);
        let (Some(hh), Some(wh)) = (hh.and_then(|s| s.parse::<i64>().ok()), wh.and_then(|s| s.parse::<i64>().ok())) else {
            return;
        };
        if hh >= wh {
            return;
        }
        self.heal_log(&format!("{window}: health is {hh} of {wh} rows — rebuilding it as a full-height column"));
        if let Some(anchor) = self.session_pane(window) {
            if anchor != h {
                self.tmux.run_ok(&["kill-pane", "-t", &h]);
                if let Some(nh) = self.split_health(&anchor) {
                    self.tag_pane(&nh, Role::Health);
                }
            }
        }
    }

    fn restart_if_stale(&self, window: &str, role: Role) {
        let Some(pane) = self.tagged(window, role) else { return };
        // The BINARY's own path, not a script under `$SPIRA_COCKPIT` (there is no
        // `cockpit/health` file any more — the bash original compared against its own
        // script's mtime; the equivalent fact for a compiled binary is the installed
        // binary's mtime on `$SPIRA_RELEASE/bin`).
        let src = std::path::Path::new(&self.conf.spira_release).join("bin").join(role.as_str());
        let Ok(src_meta) = fs::metadata(&src) else { return };
        let Some(list) = self
            .tmux
            .run(&["list-panes", "-t", window, "-F", "#{@cockpit} #{pane_pid}"])
        else {
            return;
        };
        let Some(pid) = list.lines().find_map(|l| {
            let mut it = l.split_whitespace();
            let (Some(t), Some(p)) = (it.next(), it.next()) else { return None };
            (t == role.as_str()).then(|| p.to_string())
        }) else {
            return;
        };
        let Ok(pid_n) = pid.parse::<i32>() else { return };
        let Some(started) = proc_start(pid_n) else {
            self.heal_log(&format!("{window}: {} start time unreadable — leaving it alone", role.as_str()));
            return;
        };
        let mtime = src_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if mtime <= started {
            return;
        }
        self.heal_log(&format!(
            "{window}: {} is running code older than {} — respawning",
            role.as_str(),
            src.display()
        ));
        self.tmux.run_ok(&["respawn-pane", "-k", "-t", &pane, &self.conf.health_cmd()]);
        self.tag_pane(&pane, role);
    }

    fn restart_spira_collector_if_stale(&self) {
        // Left to the collector's own crate (sp-kt4l3, concurrent — spira/cockpit.sh,
        // collect.sh). `layout` only checked this because `health.sh` happened to source
        // the same conf.sh; it belongs to the collector's supervision story, not the
        // tmux layout's, and is intentionally not reimplemented here. See ../DESIGN.md
        // Decisions.
    }
}

fn session_of(window: &str) -> &str {
    window.split(':').next().unwrap_or(window)
}

/// `proc_start(pid)` — epoch seconds at which `pid` started, or `None` on any failure.
/// MUST NEVER answer 0 for "could not tell": every caller compares this against a file
/// mtime, and 0 reads as "running code from before the file existed", forcing a respawn.
fn proc_start(pid: i32) -> Option<i64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let rest = stat.get(close + 1..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5) flags(6) minflt(7) cminflt(8)
    // majflt(9) cmajflt(10) utime(11) stime(12) cutime(13) cstime(14) priority(15) nice(16)
    // num_threads(17) itrealvalue(18) starttime(19) — field 21 overall, index 19 after the
    // `)`. Converted to wall-clock via the kernel boot time and clock ticks/sec.
    let starttime_ticks: i64 = fields.get(19)?.parse().ok()?;
    let hz = 100i64; // sysconf(_SC_CLK_TCK); 100 on every Linux this harness targets.
    let uptime = fs::read_to_string("/proc/uptime").ok()?;
    let uptime_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    let now = now_epoch();
    let boot_time = now - uptime_secs as i64;
    Some(boot_time + starttime_ticks / hz)
}

/// `Conf::rel_prefix`, minus the `PATH`/`SPIRA_RELEASE` clause — the mail pane runs the
/// operator's own client, not a Spira tool, and keeps the tmux server's inherited `PATH`.
impl Conf {
    fn rel_prefix_conf_only(&self) -> String {
        match &self.spira_conf {
            Some(c) => format!("SPIRA_CONF='{c}' "),
            None => String::new(),
        }
    }
}

fn chrono_like_timestamp() -> String {
    // `date '+%Y-%m-%dT%H:%M:%S'` in the box's configured TZ (SPIRA_TZ, else TZ). Formatted
    // by hand rather than pulling in a datetime crate for one log line; `date` is always on
    // PATH in this harness's environment and the exact format matches the bash original's.
    // SPIRA_TZ's own toml default is empty ("Empty means the host's own") — a sentinel,
    // not an absent value, so this falls through to the OS's own TZ, never a Spira default.
    let tz = spira_config::process::cfg("SPIRA_TZ")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("TZ").ok());
    let mut cmd = spira_config::bounded::bounded("date");
    cmd.arg("+%Y-%m-%dT%H:%M:%S");
    if let Some(tz) = tz {
        cmd.env("TZ", tz);
    }
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}
