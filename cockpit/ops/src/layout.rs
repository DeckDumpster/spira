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
    /// The user runtime directory the ops pane reads the scheduler's document under.
    pub runtime_dir: Option<String>,
}

impl Conf {
    /// `concierge.sh` lives in the release; `SPIRA_REPO` is only a fallback. An unresolvable
    /// path is an error, never a bare shell.
    pub fn concierge_script(&self) -> Result<String, String> {
        let mut tried = Vec::new();
        for root in [&self.spira_release, &self.spira_repo] {
            if root.is_empty() {
                continue;
            }
            let p = format!("{root}/concierge.sh");
            if std::path::Path::new(&p).is_file() {
                return Ok(p);
            }
            tried.push(p);
        }
        Err(format!(
            "cockpit: concierge.sh not found (SPIRA_RELEASE='{}', SPIRA_REPO='{}'; tried: {})",
            self.spira_release,
            self.spira_repo,
            if tried.is_empty() { "nothing — both unset".to_string() } else { tried.join(", ") }
        ))
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// The mail client's command if its program is installed: a pane whose program is missing
/// dies at once, and `ensure` would respawn it every minute.
fn installed_mail_cmd(cmd: Option<String>) -> Option<String> {
    cmd.filter(|c| on_path(c.split_whitespace().next().unwrap_or("")))
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
            .unwrap_or_else(|| PathBuf::from(format!("{}-cockpit", system_paths::NONEXISTENT)));
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
        let mail_cmd = installed_mail_cmd(mail_cmd);

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
            runtime_dir: env_nonempty("XDG_RUNTIME_DIR"),
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
        if let Some(d) = &self.runtime_dir {
            p.push_str(&format!("XDG_RUNTIME_DIR='{d}' "));
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

/// The one read `ensure` makes: every pane of every window, with the options the per-window
/// steps would otherwise each fetch with their own tmux process.
const SNAPSHOT_FORMAT: &str = "#{window_id}|#{session_name}:#{window_index}|#{@cockpit}|#{@cockpit_up}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_active}|#{pane_height}|#{window_height}|#{mouse}|#{set-clipboard}|#{window-size}|#{window-resized}|#{client-resized}|#{client-attached}";
const SNAPSHOT_FIELDS: usize = 16;
const RESIZE_HOOKS: [&str; 3] = ["window-resized", "client-resized", "client-attached"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    pub window_id: String,
    pub target: String,
    pub tag: String,
    pub cockpit_up: String,
    pub pane_id: String,
    pub pid: Option<i32>,
    pub dead: bool,
    pub active: bool,
    pub pane_height: Option<i64>,
    pub window_height: Option<i64>,
    pub mouse: String,
    pub clipboard: String,
    pub window_size: String,
    pub hooks: [String; 3],
}

pub fn parse_snapshot(text: &str) -> Vec<PaneRow> {
    text.lines()
        .filter_map(|line| {
            let c: Vec<&str> = line.splitn(SNAPSHOT_FIELDS, '|').collect();
            if c.len() != SNAPSHOT_FIELDS {
                return None;
            }
            Some(PaneRow {
                window_id: c[0].into(),
                target: c[1].into(),
                tag: c[2].into(),
                cockpit_up: c[3].into(),
                pane_id: c[4].into(),
                pid: c[5].parse().ok(),
                dead: c[6] == "1",
                active: c[7] == "1",
                pane_height: c[8].parse().ok(),
                window_height: c[9].parse().ok(),
                mouse: c[10].into(),
                clipboard: c[11].into(),
                window_size: c[12].into(),
                hooks: [c[13].into(), c[14].into(), c[15].into()],
            })
        })
        .collect()
}

const CLIENTS_MARK: &str = "spira-section:clients";
const ENV_MARK: &str = "spira-section:environment";

/// What one `ensure` read of the server answered, cut at the section marks.
#[derive(Default)]
struct ServerRead {
    panes: String,
    clients: String,
    /// The global environment; `None` when the read did not reach it.
    env: Option<String>,
}

fn split_server_read(text: &str) -> ServerRead {
    let (before_env, env) = match text.split_once(&format!("{ENV_MARK}\n")) {
        Some((b, e)) => (b, Some(e.to_string())),
        None => (text, None),
    };
    let (panes, clients) = before_env.split_once(&format!("{CLIENTS_MARK}\n")).unwrap_or((before_env, ""));
    ServerRead { panes: panes.to_string(), clients: clients.to_string(), env }
}

/// `cockpit_window_targets` over a snapshot.
pub fn snapshot_window_targets(rows: &[PaneRow]) -> Vec<String> {
    let panes: String = rows.iter().map(|r| format!("{}|{}|{}\n", r.window_id, r.target, r.tag)).collect();
    let wins: String = rows.iter().map(|r| format!("{}|{}|{}\n", r.window_id, r.target, r.cockpit_up)).collect();
    cockpit_window_targets(&panes, &wins)
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// The tmux writes that bring the server-wide options to what the config wants. The mouse
/// binding is written with the mouse option: a server that lost one lost both.
pub fn option_writes(rows: &[PaneRow], mouse_on: bool, clipboard_on: bool) -> Vec<Vec<String>> {
    let Some(first) = rows.first() else { return Vec::new() };
    let mut out = Vec::new();
    if mouse_on && !matches!(first.mouse.as_str(), "1" | "on") {
        out.push(args(&["set-option", "-g", "mouse", "on"]));
        out.push(args(&[
            "bind-key",
            "-T",
            "root",
            "MouseDrag1Pane",
            "if-shell",
            "-F",
            "#{||:#{pane_in_mode},#{mouse_any_flag}}",
            "send-keys -M",
            "copy-mode -M",
        ]));
    }
    if clipboard_on && first.clipboard != "on" {
        out.push(args(&["set-option", "-g", "set-clipboard", "on"]));
    }
    out
}

/// One `window-size latest` write per session holding a cockpit window that lacks it.
pub fn window_size_writes(rows: &[PaneRow], windows: &[String]) -> Vec<Vec<String>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for w in windows {
        let session = session_of(w);
        let lacks = rows.iter().any(|r| r.target == *w && r.window_size != "latest");
        if lacks && seen.insert(session.to_string()) {
            out.push(args(&["set-option", "-t", session, "window-size", "latest"]));
        }
    }
    out
}

fn without_quotes(s: &str) -> String {
    s.chars().filter(|c| *c != '"').collect()
}

/// The `set-hook` writes for one window whose hooks differ from the resize command for its
/// health pane. tmux echoes a hook back with its arguments quoted, so the compare ignores quotes.
pub fn hook_writes(window_rows: &[&PaneRow], right_pct: u32) -> Vec<Vec<String>> {
    let Some(health) = window_rows.iter().find(|r| r.tag == "health") else { return Vec::new() };
    let cmd = format!("resize-pane -t {} -x {right_pct}%", health.pane_id);
    let session = session_of(&health.target);
    RESIZE_HOOKS
        .iter()
        .enumerate()
        .filter(|(i, _)| without_quotes(&health.hooks[*i]) != cmd)
        .map(|(_, hook)| args(&["set-hook", "-t", session, hook, &cmd]))
        .collect()
}

/// What `window_is_healthy` needs to know that the snapshot cannot say.
pub struct HealthFacts<'a> {
    pub mail_wanted: bool,
    pub role_of: &'a dyn Fn(i32) -> Option<Role>,
    pub health_stale: &'a dyn Fn(i32) -> bool,
}

/// True when every per-window repair step would find nothing to do: panes tagged as what they
/// run, a session pane, exactly one health pane at full height, mail when wanted, an active
/// pane, and a health pane running current code. False sends the window down the repairing
/// path, which reads tmux for itself.
pub fn window_is_healthy(rows: &[&PaneRow], facts: &HealthFacts) -> bool {
    for r in rows {
        let Some(pid) = r.pid else { continue };
        let tag_ok = match (facts.role_of)(pid) {
            Some(role) => r.tag == role.as_str(),
            None => r.tag.is_empty() || r.dead,
        };
        if !tag_ok {
            return false;
        }
    }
    let health: Vec<&&PaneRow> = rows.iter().filter(|r| r.tag == "health").collect();
    let [health] = health.as_slice() else { return false };
    if !rows.iter().any(|r| r.tag != "health" && r.tag != "mail") {
        return false;
    }
    if facts.mail_wanted && !rows.iter().any(|r| r.tag == "mail") {
        return false;
    }
    if let (Some(ph), Some(wh)) = (health.pane_height, health.window_height) {
        if ph < wh {
            return false;
        }
    }
    if !rows.iter().any(|r| r.active) {
        return false;
    }
    !health.pid.is_some_and(|pid| (facts.health_stale)(pid))
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

    fn release_tool(&self, name: &str) -> PathBuf {
        let in_release = self.conf.spira_release.as_str();
        if !in_release.is_empty() {
            let p = std::path::Path::new(in_release).join("bin").join(name);
            if p.is_file() {
                return p;
            }
        }
        PathBuf::from(name)
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
        let out = self
            .tmux
            .run(&["list-panes", "-t", window, "-F", "#{@cockpit} #{pane_id}"])?;
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
            let conc = self.conf.concierge_script()?;
            let cmd = format!("{}{} here", self.conf.rel_prefix(), conc);
            let args = [
                "split-window", "-P", "-F", "#{pane_id}", "-b", "-v", "-l", "60%", "-t", &anchor,
                "-c", &self.conf.cwd, &cmd,
            ];
            let new_sess = self
                .tmux
                .run(&args)
                .ok_or_else(|| "cockpit: could not restore the session pane".to_string())?;
            println!("cockpit: session pane was gone — opened {conc} here at {new_sess}");
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

    fn detach_idle_clients(&self, clients: &str) {
        for (tty, age) in stale_clients(clients, now_epoch(), self.conf.idle_secs) {
            self.heal_log(&format!("detach: client {tty} idle {age}s — detaching"));
            self.tmux.run_ok(&["detach-client", "-t", &tty]);
        }
    }

    /// The panes, the clients (only while an idle limit is set) and the global environment, in
    /// the one tmux invocation `ensure` makes; `None` when it failed, as with no server.
    fn read_server(&self) -> Option<ServerRead> {
        let mut args = vec!["list-panes", "-a", "-F", SNAPSHOT_FORMAT];
        if self.conf.idle_secs > 0 {
            args.extend([";", "display-message", "-p", CLIENTS_MARK, ";", "list-clients", "-F", "#{client_tty} #{client_activity}"]);
        }
        args.extend([";", "display-message", "-p", ENV_MARK, ";", "show-environment", "-g"]);
        self.tmux.run(&args).map(|text| split_server_read(&text))
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

        let read = self.read_server();
        if read.is_none() && !self.tmux.installed() {
            self.heal_log("headless install: no tmux, nothing to heal");
            return Ok(String::new());
        }
        let read = read.unwrap_or_default();
        let rows = parse_snapshot(&read.panes);

        if let Some(env) = &read.env {
            crate::tmux_env::scrub_from(&self.tmux, env, std::env::var(spira_config::process::TOML_ENV).ok().as_deref());
        }
        self.detach_idle_clients(&read.clients);
        let windows = snapshot_window_targets(&rows);
        let writes = option_writes(&rows, self.conf.mouse_on, self.conf.clipboard_on)
            .into_iter()
            .chain(window_size_writes(&rows, &windows));
        for w in writes {
            let a: Vec<&str> = w.iter().map(String::as_str).collect();
            self.tmux.run_ok(&a);
        }

        if windows.is_empty() {
            if self.conf.down_marker().is_file() {
                // deliberate — `down` was run; rebuilding it is a fight, not a repair.
            } else if self.heal_ready() {
                self.touch_heal_stamp();
                self.heal_log("no cockpit window found anywhere, and no down marker — rebuilding from scratch");
                // The heal unit's PATH need not include the release, so `rebuild` is resolved
                // under the active release's `bin/`, falling back to PATH.
                // batch-job: this runs whatever its caller names, as long as that takes
                // FORK-EDGE: sibling binary `rebuild` of this crate; in process its from_env exits
                // on a config refusal and would take the heal down — needs a fallible from_env.
                match std::process::Command::new(self.release_tool("rebuild")).output() {
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

        let mail_exe = self.mail_exe();
        let role_of = |pid: i32| pane_role(pid, mail_exe.as_deref());
        let health_stale = |pid: i32| self.health_needs_restart(pid);
        let facts = HealthFacts {
            mail_wanted: self.conf.mail_cmd.is_some(),
            role_of: &role_of,
            health_stale: &health_stale,
        };
        for w in &windows {
            let window_rows: Vec<&PaneRow> = rows.iter().filter(|r| r.target == *w).collect();
            if window_is_healthy(&window_rows, &facts) {
                for write in hook_writes(&window_rows, self.conf.right_pct) {
                    let a: Vec<&str> = write.iter().map(String::as_str).collect();
                    self.tmux.run_ok(&a);
                }
                continue;
            }
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

    /// Would `restart_if_stale` respawn the health pane whose process is `pid`? An unreadable
    /// start time counts: that path logs, and logging belongs to the repairing path.
    fn health_needs_restart(&self, pid: i32) -> bool {
        let src = std::path::Path::new(&self.conf.spira_release).join("bin").join(Role::Health.as_str());
        let Ok(src_meta) = fs::metadata(&src) else { return false };
        let Some(started) = proc_start(pid) else { return true };
        let mtime = src_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        mtime > started
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
}

fn session_of(window: &str) -> &str {
    window.split(':').next().unwrap_or(window)
}

/// `proc_start(pid)` — epoch seconds at which `pid` started, or `None` on any failure.
/// MUST NEVER answer 0 for "could not tell": every caller compares this against a file
/// mtime, and 0 reads as "running code from before the file existed", forcing a respawn.
fn proc_start(pid: i32) -> Option<i64> {
    let stat = fs::read_to_string(format!("{}/{pid}/stat", system_paths::PROC)).ok()?;
    let close = stat.rfind(')')?;
    let rest = stat.get(close + 1..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5) flags(6) minflt(7) cminflt(8)
    // majflt(9) cmajflt(10) utime(11) stime(12) cutime(13) cstime(14) priority(15) nice(16)
    // num_threads(17) itrealvalue(18) starttime(19) — field 21 overall, index 19 after the
    // `)`. Converted to wall-clock via the kernel boot time and clock ticks/sec.
    let starttime_ticks: i64 = fields.get(19)?.parse().ok()?;
    let hz = 100i64; // sysconf(_SC_CLK_TCK); 100 on every Linux this harness targets.
    let uptime = fs::read_to_string(format!("{}/uptime", system_paths::PROC)).ok()?;
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
    // `date '+%Y-%m-%dT%H:%M:%S'` in the box's configured TZ (SPIRA_TZ, else TZ). SPIRA_TZ's
    // own toml default is empty ("Empty means the host's own") — a sentinel, not an absent
    // value — and then this process's own zone answers, read in process by localtime_r.
    let tz = spira_config::process::cfg("SPIRA_TZ").ok().filter(|s| !s.is_empty());
    let Some(tz) = tz else {
        return local_time(now_epoch()).map(|t| t.iso()).unwrap_or_default();
    };
    // FORK-EDGE: needs a zone-aware clock (a named zone other than this process's own); date
    // is not a Spira binary.
    let mut cmd = spira_config::bounded::bounded("date");
    cmd.arg("+%Y-%m-%dT%H:%M:%S").env("TZ", tz);
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A wall-clock instant in this process's own zone (`TZ`, else the host's).
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl LocalTime {
    /// `date +%H:%M`.
    pub fn hhmm(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// `date +%Y-%m-%dT%H:%M:%S`.
    pub fn iso(&self) -> String {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second)
    }
}

/// `t` (epoch seconds) in this process's own zone, by `localtime_r`: no `date` child.
pub fn local_time(t: i64) -> Option<LocalTime> {
    let tt = t as libc::time_t;
    // SAFETY: localtime_r writes only into the tm we own; a zeroed tm is a valid initial value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
        return None;
    }
    Some(LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::fake::FakeTmux;

    #[test_id(taxmfqr)]
    #[test]
    fn local_time_formats_as_date_did() {
        let t = local_time(1_762_171_200).expect("localtime_r answers");
        let (hhmm, iso) = (t.hhmm(), t.iso());
        assert_eq!(hhmm.len(), 5, "{hhmm}");
        assert_eq!(iso.len(), 19, "{iso}");
        assert_eq!(&iso[11..16], hhmm);
        assert!(iso.starts_with("2025-11-0"), "{iso}");
    }

    #[test_id(t3lb0yt)]
    #[test]
    fn classify_argv_matches_health_by_script_suffix_never_substring() {
        assert_eq!(classify_argv("bash", "opt/spira/cockpit/health.sh", None), Some(Role::Health));
        // The lifecycle lens is the ops pane now (sp-lpw5ol).
        assert_eq!(classify_argv("opt/bin/lc-view", "loop", None), Some(Role::Health));
        assert_eq!(classify_argv("lc-view", "", None), Some(Role::Health));
        // A system prompt that merely mentions the path as argv text (not argv[0]/[1] of a
        // shell) must not classify — callers only ever pass exe/script, not the whole line,
        // which is what makes this safe: "argv position, never substring".
        assert_eq!(classify_argv("claude", "", None), None);
    }

    #[test_id(tnzowy2)]
    #[test]
    fn classify_argv_matches_mail_by_basename_only() {
        assert_eq!(classify_argv("usr/bin/aerc", "", Some("aerc")), Some(Role::Mail));
        assert_eq!(classify_argv("usr/bin/aerclike", "", Some("aerc")), None);
    }

    #[test_id(tyeyn1s)]
    #[test]
    fn classify_argv_neither() {
        assert_eq!(classify_argv("zsh", "", Some("aerc")), None);
    }

    #[test_id(tvc3lrs)]
    #[test]
    fn stale_clients_filters_by_idle_secs() {
        let lines = "dev/pts/0 1000\ndev/pts/1 500\n";
        let now = 1000 + 21600 + 1;
        let out = stale_clients(lines, now, 21600);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "dev/pts/0");
    }

    #[test_id(toi8exw)]
    #[test]
    fn stale_clients_disabled_at_zero() {
        assert!(stale_clients("dev/pts/0 0\n", 100_000, 0).is_empty());
    }

    #[test_id(t4ozn73)]
    #[test]
    fn stale_clients_ignores_malformed_lines() {
        assert!(stale_clients("garbage\n\n", 100_000, 10).is_empty());
    }

    #[test_id(trvyd23)]
    #[test]
    fn cockpit_window_targets_dedups_by_window_id() {
        let panes = "@1|cockpit:2|health\n@1|cockpit:2|mail\n";
        let wins = "@1|brain:0|1\n@2|hunk:0|1\n";
        let out = cockpit_window_targets(panes, wins);
        assert_eq!(out, vec!["cockpit:2".to_string(), "hunk:0".to_string()]);
    }

    fn conf_with(release: &str, repo: &str) -> Conf {
        Conf {
            spira_release: release.into(),
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
            spira_repo: repo.into(),
            spira_conf: None,
            runtime_dir: None,
        }
    }

    #[test_id(twjzh8l)]
    #[test]
    fn concierge_script_with_neither_set_names_both() {
        let e = conf_with("", "").concierge_script().unwrap_err();
        assert!(e.contains("SPIRA_RELEASE") && e.contains("SPIRA_REPO"), "{e}");
    }

    #[test_id(tayks1e)]
    #[test]
    fn concierge_script_missing_file_names_the_path_tried() {
        let root = testkit::TempDir::new("layout-concierge-missing");
        let rel = root.join("nonexistent-rel");
        let e = conf_with(rel.to_str().unwrap(), "").concierge_script().unwrap_err();
        assert!(e.contains(&format!("{}/concierge.sh", rel.display())), "{e}");
    }

    #[test_id(tishc0f)]
    #[test]
    fn rel_prefix_carries_release_and_conf() {
        let c = Conf {
            spira_release: "rel".into(),
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
            spira_conf: Some("etc/spira-custom.conf".into()),
            runtime_dir: Some("run/u7".into()),
        };
        let p = c.rel_prefix();
        assert!(p.contains("XDG_RUNTIME_DIR='run/u7'"), "the pane reads the document under the runtime directory it is launched with: {p}");
        assert!(p.contains("SPIRA_CONF='etc/spira-custom.conf'"));
        assert!(p.contains("SPIRA_RELEASE='rel'"));
        assert!(p.contains("PATH='rel/bin:rel/spira:/usr/local/bin:/usr/bin:/bin'"));
    }

    fn snapshot_row(window: &str, target: &str, tag: &str, pane: &str, pid: u32, active: &str, height: u32) -> String {
        let hook = "resize-pane -t \"%2\" -x \"33%\"";
        format!("{window}|{target}|{tag}||{pane}|{pid}|0|{active}|{height}|24|1|on|latest|{hook}|{hook}|{hook}")
    }

    /// Runs `ensure` once over `rows` with a fake tmux, a live health pane and a live
    /// session pane, and returns what tmux was asked.
    fn ensure_calls(name: &str, rows: impl Fn(u32, u32) -> Vec<String>, idle_secs: i64) -> Vec<String> {
        ensure_over(name, rows, idle_secs, None, |f| f)
    }

    /// `ensure_calls` with the mail client set to `mail`, and `script` adding what the fake
    /// tmux answers beyond the snapshot.
    fn ensure_over(
        name: &str,
        rows: impl Fn(u32, u32) -> Vec<String>,
        idle_secs: i64,
        mail: Option<&str>,
        script: impl FnOnce(FakeTmux) -> FakeTmux,
    ) -> Vec<String> {
        let dir = testkit::TempDir::new(name);
        let script_path = dir.join("health.sh");
        fs::write(&script_path, "sleep 30\n").unwrap();
        let mut health = std::process::Command::new("sh").arg(&script_path).spawn().unwrap();
        let mut sess = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let fixture = rows(sess.id(), health.id()).join("\n") + "\n";
        let fake = script(FakeTmux::new(&dir).answer("list-panes -a", &fixture));
        let mut conf = conf_with("", "");
        conf.run = dir.path().to_path_buf();
        conf.clipboard_on = true;
        conf.idle_secs = idle_secs;
        conf.mail_cmd = mail.map(str::to_string);
        let layout = Layout { tmux: fake.tmux(), conf };
        let me = std::env::current_exe().unwrap();
        let out = layout.ensure(&me, &me);
        for c in [&mut health, &mut sess] {
            let _ = c.kill();
            let _ = c.wait();
        }
        assert_eq!(out, Ok(String::new()));
        fake.calls()
    }

    fn healthy_fixture(sess: u32, health: u32) -> Vec<String> {
        vec![
            snapshot_row("@1", "cockpit:0", "", "%1", sess, "1", 24),
            snapshot_row("@1", "cockpit:0", "health", "%2", health, "0", 24),
        ]
    }

    #[test_id(tpt7ba1)]
    #[test]
    fn ensure_over_a_healthy_layout_makes_one_tmux_read_and_no_writes() {
        let calls = ensure_calls("layout-ensure-healthy", healthy_fixture, 0);
        assert_eq!(calls.len(), 1, "one snapshot read and nothing else: {calls:?}");
        assert!(calls[0].starts_with("list-panes -a"), "{calls:?}");
    }

    #[test_id(tf2k9xc)]
    #[test]
    fn ensure_with_an_idle_limit_reads_the_clients_in_the_same_tmux_call() {
        let calls = ensure_calls("layout-ensure-idle", healthy_fixture, 600);
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].starts_with("list-panes -a") && calls[0].contains("list-clients"), "{calls:?}");
    }

    #[test_id(t2bgej5)]
    #[test]
    fn the_one_read_is_cut_at_its_marks_into_panes_clients_and_environment() {
        let text = format!("row-a\nrow-b\n{CLIENTS_MARK}\npts0 100\n{ENV_MARK}\nCLAUDECODE=1\n-X\n");
        let r = split_server_read(&text);
        assert_eq!((r.panes.as_str(), r.clients.as_str(), r.env.as_deref()), ("row-a\nrow-b\n", "pts0 100\n", Some("CLAUDECODE=1\n-X\n")));
        let r = split_server_read(&format!("row-a\n{ENV_MARK}\nA=b\n"));
        assert_eq!((r.panes.as_str(), r.clients.as_str(), r.env.as_deref()), ("row-a\n", "", Some("A=b\n")), "no idle limit, no client section");
        assert_eq!(split_server_read("row-a\n").env, None);
    }

    #[test_id(txrbz49)]
    #[test]
    fn ensure_scrubs_a_session_identity_the_one_read_showed_and_writes_nothing_else() {
        let calls = ensure_over(
            "layout-ensure-scrub",
            |s, h| healthy_fixture(s, h).into_iter().chain([ENV_MARK.to_string(), "CLAUDECODE=1".into(), "PATH=/bin".into()]).collect(),
            0,
            None,
            |f| f,
        );
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[1], "set-environment -gu CLAUDECODE");
    }

    #[test_id(tg8m1wz)]
    #[test]
    fn ensure_over_a_window_whose_session_pane_is_gone_takes_the_repair_path() {
        let calls = ensure_calls(
            "layout-ensure-repair",
            |_, health| vec![snapshot_row("@1", "cockpit:0", "health", "%2", health, "1", 24)],
            0,
        );
        assert!(calls.iter().any(|c| c.starts_with("list-panes -t cockpit:0")), "{calls:?}");
    }

    #[test_id(tb5r7nd)]
    #[test]
    fn ensure_over_a_server_with_mouse_off_writes_exactly_the_mouse_option_and_binding() {
        let calls = ensure_calls(
            "layout-ensure-mouse",
            |s, h| healthy_fixture(s, h).into_iter().map(|r| r.replace("|1|on|latest|", "|0|on|latest|")).collect(),
            0,
        );
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(calls[1], "set-option -g mouse on");
        assert!(calls[2].starts_with("bind-key -T root MouseDrag1Pane"), "{calls:?}");
    }

    /// The tmux calls that shape panes, each cut before its `-c <cwd>` command tail.
    fn pane_writes(calls: &[String]) -> Vec<String> {
        calls
            .iter()
            .filter(|c| c.starts_with("split-window") || c.starts_with("set-option -p -t"))
            .map(|c| c.split(" -c ").next().unwrap().to_string())
            .collect()
    }

    const SESSION_AND_HEALTH: &str = "%1\nhealth %2\n";

    // covers: UC-cockpit-observability-42
    #[test_id(toyprx8)]
    #[test]
    fn mail_sits_below_the_session_pane_and_health_keeps_the_full_height() {
        let dir = testkit::TempDir::new("layout-mail-up");
        let fake = FakeTmux::new(&dir)
            .answer("#{@cockpit} #{pane_id}", "%1\n")
            .answer("-d -h -f", "%2\n")
            .answer("-d -v -l", "%3\n");
        let mut conf = conf_with("", "");
        conf.run = dir.path().to_path_buf();
        conf.cwd = "work".into();
        conf.mail_cmd = Some("fakemail".into());
        Layout { tmux: fake.tmux(), conf }.up("cockpit:0").unwrap();
        assert_eq!(
            pane_writes(&fake.calls()),
            [
                "split-window -P -F #{pane_id} -d -h -f -l 33% -t %1",
                "set-option -p -t %2 @cockpit health",
                "split-window -P -F #{pane_id} -d -v -l 28% -t %1",
                "set-option -p -t %3 @cockpit mail",
            ],
            "health is cut full-height first, then mail under the session pane alone"
        );

        let bare = testkit::TempDir::new("layout-no-mail-up");
        let fake = FakeTmux::new(&bare).answer("#{@cockpit} #{pane_id}", "%1\n").answer("-d -h -f", "%2\n");
        let mut conf = conf_with("", "");
        conf.run = bare.path().to_path_buf();
        Layout { tmux: fake.tmux(), conf }.up("cockpit:0").unwrap();
        let writes = pane_writes(&fake.calls());
        assert_eq!(writes.len(), 2, "no mail client, no mail pane: {writes:?}");
        assert!(writes.iter().all(|w| !w.contains(" -v ") && !w.contains("mail")), "{writes:?}");

        let installed = dir.join("fakemail");
        fs::write(&installed, "").unwrap();
        let installed = installed.to_str().unwrap().to_string();
        assert_eq!(installed_mail_cmd(Some(installed.clone())), Some(installed));
        assert_eq!(installed_mail_cmd(Some(dir.join("absent").to_str().unwrap().to_string())), None);
        assert_eq!(installed_mail_cmd(Some(String::new())), None);
        assert_eq!(installed_mail_cmd(None), None);

        let resolved = |mail: &str| {
            let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_RUN", &dir.join("run").display().to_string()), ("COCKPIT_MAIL", mail)]);
            let _env = testkit::env(&[
                ("SPIRA_TOML", Some(toml.to_str().unwrap())),
                ("SPIRA_RELEASE", Some("rel")),
                ("COCKPIT_MAIL", None),
                ("SPIRA_RUN", None),
            ]);
            spira_config::process::with_fresh(|| Conf::from_env().unwrap().mail_cmd).unwrap()
        };
        let present = dir.join("present-mail");
        fs::write(&present, "").unwrap();
        assert_eq!(resolved(present.to_str().unwrap()), Some(present.display().to_string()));
        assert_eq!(resolved(dir.join("absent-mail").to_str().unwrap()), None, "an uninstalled client gets no pane");
        assert_eq!(resolved(""), None);
    }

    // covers: UC-cockpit-observability-42
    #[test_id(t2o6m6t)]
    #[test]
    fn ensure_cuts_a_closed_mail_pane_back_in_under_the_session_pane() {
        let rows = |s, h| healthy_fixture(s, h);
        let script = |f: FakeTmux| f.answer("#{@cockpit} #{pane_id}", SESSION_AND_HEALTH).answer("-d -v -l", "%3\n");
        let calls = ensure_over("layout-ensure-mail", rows, 0, Some("fakemail"), script);
        assert_eq!(
            pane_writes(&calls),
            ["split-window -P -F #{pane_id} -d -v -l 28% -t %1", "set-option -p -t %3 @cockpit mail"],
            "{calls:?}"
        );

        let calls = ensure_over("layout-ensure-no-mail", rows, 0, None, script);
        assert!(pane_writes(&calls).is_empty(), "{calls:?}");
    }

    fn healthy_rows() -> Vec<PaneRow> {
        let text = format!(
            "{}\n{}\n{}\n",
            snapshot_row("@1", "cockpit:0", "", "%1", 10, "1", 24),
            snapshot_row("@1", "cockpit:0", "health", "%2", 11, "0", 24),
            snapshot_row("@1", "cockpit:0", "mail", "%3", 12, "0", 7),
        );
        parse_snapshot(&text)
    }

    fn facts<'a>(
        role_of: &'a dyn Fn(i32) -> Option<Role>,
        stale: &'a dyn Fn(i32) -> bool,
        mail_wanted: bool,
    ) -> HealthFacts<'a> {
        HealthFacts { mail_wanted, role_of, health_stale: stale }
    }

    fn role_by_pid(pid: i32) -> Option<Role> {
        match pid {
            11 => Some(Role::Health),
            12 => Some(Role::Mail),
            _ => None,
        }
    }

    #[test_id(tkw1h9d)]
    #[test]
    fn a_healthy_snapshot_decides_zero_writes() {
        let rows = healthy_rows();
        assert!(option_writes(&rows, true, true).is_empty());
        assert!(window_size_writes(&rows, &["cockpit:0".to_string()]).is_empty());
        let refs: Vec<&PaneRow> = rows.iter().collect();
        assert!(hook_writes(&refs, 33).is_empty());
        assert!(window_is_healthy(&refs, &facts(&role_by_pid, &|_| false, true)));
    }

    #[test_id(tm5q2zr)]
    #[test]
    fn mouse_off_decides_exactly_the_mouse_option_and_its_binding() {
        let mut rows = healthy_rows();
        for r in &mut rows {
            r.mouse = "0".into();
        }
        let w = option_writes(&rows, true, true);
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!(w[0], vec!["set-option", "-g", "mouse", "on"]);
        assert_eq!(&w[1][..4], ["bind-key", "-T", "root", "MouseDrag1Pane"]);
    }

    #[test_id(tc8n4vx)]
    #[test]
    fn a_config_that_wants_no_mouse_or_clipboard_decides_no_option_writes() {
        let mut rows = healthy_rows();
        for r in &mut rows {
            r.mouse = "0".into();
            r.clipboard = "external".into();
        }
        assert!(option_writes(&rows, false, false).is_empty());
    }

    #[test_id(tj3x6ue)]
    #[test]
    fn a_window_not_on_latest_size_gets_one_session_write() {
        let mut rows = healthy_rows();
        for r in &mut rows {
            r.window_size = "smallest".into();
        }
        let w = window_size_writes(&rows, &["cockpit:0".to_string()]);
        assert_eq!(w, vec![vec!["set-option", "-t", "cockpit", "window-size", "latest"]]);
    }

    #[test_id(tr0f7sa)]
    #[test]
    fn a_missing_resize_hook_decides_only_that_hook() {
        let mut rows = healthy_rows();
        rows[1].hooks[1] = String::new();
        let refs: Vec<&PaneRow> = rows.iter().collect();
        let w = hook_writes(&refs, 33);
        assert_eq!(w, vec![vec!["set-hook", "-t", "cockpit", "client-resized", "resize-pane -t %2 -x 33%"]]);
    }

    #[test_id(tz9b1ye)]
    #[test]
    fn a_window_whose_session_pane_is_gone_takes_the_repair_path() {
        let rows: Vec<PaneRow> = healthy_rows().into_iter().filter(|r| !r.tag.is_empty()).collect();
        let refs: Vec<&PaneRow> = rows.iter().collect();
        assert!(!window_is_healthy(&refs, &facts(&role_by_pid, &|_| false, true)));
    }

    #[test_id(tq4d8wm)]
    #[test]
    fn an_untagged_dashboard_a_stale_health_or_a_short_health_takes_the_repair_path() {
        let rows = healthy_rows();
        let refs: Vec<&PaneRow> = rows.iter().collect();
        let untagged = |pid: i32| if pid == 10 { Some(Role::Mail) } else { role_by_pid(pid) };
        assert!(!window_is_healthy(&refs, &facts(&untagged, &|_| false, true)));
        assert!(!window_is_healthy(&refs, &facts(&role_by_pid, &|_| true, true)));
        let mut short = healthy_rows();
        short[1].pane_height = Some(12);
        let refs: Vec<&PaneRow> = short.iter().collect();
        assert!(!window_is_healthy(&refs, &facts(&role_by_pid, &|_| false, true)));
    }

    #[test_id(tn6v3ho)]
    #[test]
    fn a_missing_mail_pane_is_only_a_fault_when_mail_is_wanted() {
        let rows: Vec<PaneRow> = healthy_rows().into_iter().filter(|r| r.tag != "mail").collect();
        let refs: Vec<&PaneRow> = rows.iter().collect();
        assert!(window_is_healthy(&refs, &facts(&role_by_pid, &|_| false, false)));
        assert!(!window_is_healthy(&refs, &facts(&role_by_pid, &|_| false, true)));
    }
}
