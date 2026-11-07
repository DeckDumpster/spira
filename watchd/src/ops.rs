//! Everything `watchd` asks systemd, `mail` or `/proc` to do, behind one trait so the
//! command logic (`cmd_status`, `cmd_notify`, `cmd_restart`, …) is unit tested with a Fake
//! rather than a real systemd user manager or mailbox. `Real` is the production
//! implementation; DESIGN.md "Test strategy".

use std::collections::HashMap;
use std::process::{Command, Stdio};

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SopMatch {
    pub slug: String,
    pub runs: bool,
}

/// Column 1 is the slug, column 4 `run` when the SOP declares `RUN:`. A line of four columns
/// (symptom in the fourth) comes from an older `sop`, and counts as runnable so `sop show` decides.
pub fn parse_match_output(out: &str) -> Vec<SopMatch> {
    out.lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.splitn(5, '\t').collect();
            let slug = cols[0];
            if slug.is_empty() {
                return None;
            }
            let runs = cols.len() < 5 || cols[3] == "run";
            Some(SopMatch { slug: slug.to_string(), runs })
        })
        .collect()
}

pub trait Ops {
    /// `systemctl --user is-active <units…>` — ONE exec for every unit (the cost of
    /// `status` must not grow with the manifest). Returns one state string per unit, in
    /// the same order; `"?"` when the answer could not be had at all (no systemd, box has
    /// no user manager).
    fn is_active(&self, units: &[String]) -> Vec<String>;

    /// `systemctl --user show <units…> -p Id -p ActiveState -p NRestarts` — keyed by unit
    /// Id so a unit that has gone cannot shift the answers onto its neighbour.
    fn show(&self, units: &[String]) -> HashMap<String, (String, String)>;

    fn restart(&self, units: &[String]) -> Result<(), String>;

    /// `spira_unit` (conf.sh): the instance-qualified unit name if systemd knows it, else
    /// the plain form if THAT is known, else `"?"`.
    fn spira_unit(&self, base: &str, kind: &str, instance: &str) -> String;

    /// A process whose first three cmdline args include `target` (its first word) and
    /// which holds an exclusive flock on one of its own open files — `"pid <pid> holds
    /// <path>"`, or `None`.
    fn orphan_lock(&self, target: &str) -> Option<String>;

    /// Send an escalation through `mail send operator …`.
    fn mail_ask(&self, subject: &str, default: &str, why: &str, evidence: &str) -> Result<(), String>;

    /// `mail-health.sh`'s own exit code (0 nothing to say, 1 escalated, 3 could not check).
    fn mail_health(&self) -> i32;

    /// Every `spira-watch-*-<instance>.service` unit systemd currently knows about — from
    /// `list-unit-files` (installed) and `list-units --all` (loaded) — so `prune` can find a
    /// retired daemon's unit even when the two disagree about which set holds it.
    fn list_watch_units(&self, instance: &str) -> Vec<String>;

    /// `systemctl --user disable --now <unit>` — never touched directly by command logic,
    /// so a test's Fake can never reach a real systemd user manager
    /// (scar 2026-09-30: a `cmd_prune` test that called `Real`'s systemctl and `$HOME`
    /// directly, instead of going through this trait, disabled and deleted ten live
    /// production watcher units).
    fn disable_now(&self, unit: &str);

    /// `sop match`: every SOP whose `MATCH:` fires on the payload, best first.
    fn sop_match(&self, payload: &str) -> Result<Vec<SopMatch>, String>;

    /// `sop show <slug>`: the SOP's full text.
    fn sop_show(&self, slug: &str) -> Result<String, String>;

    /// Run `program` (a bare name, resolved only in the active release's `bin/`) in its own
    /// systemd unit, killed at `deadline_secs`.
    fn run_tool(&self, program: &str, args: &[String], deadline_secs: u64) -> ToolRun;

    /// `sop applied <slug> --bead|--pass <target> --check <check> --held unknown`.
    fn sop_applied(&self, slug: &str, target: &Target, check: &str, why: &str) -> Result<(), String>;

    /// `mail send concierge --kind <kind>`.
    fn mail_concierge(&self, kind: &str, subject: &str, body: &str) -> Result<(), String>;

    /// `inbox-append.sh <text>`: one event line in the Concierge inbox, with its urgent push.
    fn inbox_append(&self, text: &str) -> Result<(), String>;
}

/// What an application is recorded against: the incident's bead if it names one, else a
/// pass id built from the watcher row.
pub enum Target {
    Bead(String),
    Pass(String),
}

pub enum ToolRun {
    Exited { code: i32, output: String },
    TimedOut { output: String },
    /// Never started: not in the release's `bin/`, or the unit could not be launched.
    Unavailable(String),
}

pub struct Real {
    pub systemctl: String,
    pub systemd_run: String,
}

impl Real {
    pub fn new(systemctl: String) -> Real {
        Real { systemctl, systemd_run: std::env::var("SPIRA_SYSTEMD_RUN").unwrap_or_else(|_| "systemd-run".to_string()) }
    }
}

/// `<release>/bin/<program>` for the release `$SPIRA_RELEASE` names — never `PATH`, so a SOP
/// can only name what the release ships.
fn release_bin_tool(program: &str) -> Result<std::path::PathBuf, String> {
    let p = spira_config::release_env::release_root_for_process()?.join("bin").join(program);
    if p.is_file() {
        Ok(p)
    } else {
        Err(format!("{program} is not in the active release's bin/ ({})", p.display()))
    }
}

// FORK-EDGE: needs Units list-by-pattern, is-enabled, disable and NRestarts (only facts/start/stop/restart exist), and a config-selected test double: SPIRA_SYSTEMCTL stubs in test-launchers-as-launched.sh would be bypassed onto the live user manager.
fn systemctl_cmd(systemctl: &str) -> Command {
    let mut c = Command::new(systemctl);
    c.arg("--user");
    c
}

impl Ops for Real {
    fn is_active(&self, units: &[String]) -> Vec<String> {
        if units.is_empty() {
            return Vec::new();
        }
        let out = systemctl_cmd(&self.systemctl)
            .arg("is-active")
            .args(units)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let text = match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            Err(_) => String::new(),
        };
        let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        while lines.len() < units.len() {
            lines.push("?".to_string());
        }
        lines.truncate(units.len());
        lines.iter().map(|l| if l.is_empty() { "?".to_string() } else { l.clone() }).collect()
    }

    fn show(&self, units: &[String]) -> HashMap<String, (String, String)> {
        let mut out = HashMap::new();
        if units.is_empty() {
            return out;
        }
        let mut cmd = systemctl_cmd(&self.systemctl);
        cmd.arg("show").args(units).args(["-p", "Id", "-p", "ActiveState", "-p", "NRestarts", "--no-pager"]);
        let text = match cmd.stdin(Stdio::null()).stderr(Stdio::null()).output() {
            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            Err(_) => return out,
        };
        let mut id = String::new();
        let mut state = String::new();
        let mut restarts = String::new();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "Id" => {
                    id = v.to_string();
                    state.clear();
                    restarts.clear();
                }
                "ActiveState" if !id.is_empty() => state = v.to_string(),
                "NRestarts" if !id.is_empty() => {
                    restarts = v.to_string();
                    out.insert(id.clone(), (state.clone(), restarts.clone()));
                }
                _ => {}
            }
        }
        out
    }

    fn restart(&self, units: &[String]) -> Result<(), String> {
        let st = systemctl_cmd(&self.systemctl)
            .arg("restart")
            .args(units)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("systemd refused the restart — ask it why with 'systemctl --user status {}'", units.first().cloned().unwrap_or_default()))
        }
    }

    fn spira_unit(&self, base: &str, kind: &str, instance: &str) -> String {
        let inst = if instance.is_empty() { format!("spira-{base}.{kind}") } else { format!("spira-{base}-{instance}.{kind}") };
        let plain = format!("spira-{base}.{kind}");
        let known = |unit: &str| -> bool {
            let ok = |sub: &str| systemctl_cmd(&self.systemctl).arg(sub).arg(unit).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
            ok("is-enabled") || ok("is-active")
        };
        if known(&inst) {
            inst
        } else if known(&plain) {
            plain
        } else {
            "?".to_string()
        }
    }

    fn orphan_lock(&self, target: &str) -> Option<String> {
        // `target` is the manifest row's whole target — "<program> <args...>" — but the
        // cmdline match below (like the /proc scan) is against the PROGRAM alone, never
        // the row's full string with its arguments still attached. Scar: passing `target`
        // whole here meant no live orphan ever matched, because /proc/<pid>/cmdline's argv
        // is NUL-separated (argv[0] is the program, argv[1] a separate element) and never
        // reassembles into "program args" as one space-joined string.
        let prog = target.split(' ').next().unwrap_or("");
        if prog.is_empty() {
            return None;
        }
        std::path::Path::new(prog).file_name()?.to_str()?;
        for pid in pids_with_arg(std::path::Path::new("/proc"), prog, std::process::id() as i32) {
            let fd_dir = format!("/proc/{pid}/fd");
            let Ok(entries) = std::fs::read_dir(&fd_dir) else { continue };
            for entry in entries.flatten() {
                let Ok(link) = std::fs::read_link(entry.path()) else { continue };
                if !link.is_absolute() || !link.is_file() {
                    continue;
                }
                if is_locked_by_someone(&link) {
                    return Some(format!("pid {pid} holds {}", link.display()));
                }
            }
        }
        None
    }

    fn mail_ask(&self, subject: &str, default: &str, why: &str, evidence: &str) -> Result<(), String> {
        let body = format!("## Question\n{subject}\n\n## Default\n{default}\n\n{why}\n\n{evidence}\n");
        crate::send::send_configured(&crate::send::Letter { mailbox: "operator", subject, kind: "question", default, body: &body })
    }

    // FORK-EDGE: mail-health.sh is 122 lines of shell (over the ~100-line port bound).
    fn mail_health(&self) -> i32 {
        // A spawn failure was a silent 3 ("could not check") — name it (sp-xp0u2).
        let (mail_health, envs) = match spira_config::release_env::tool("mail-health.sh").and_then(|t| Ok((t, spira_config::release_env::child_path_env_for_process()?))) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("watchd: cannot run mail-health.sh: {e}");
                return 3;
            }
        };
        match spira_config::bounded::bounded(mail_health).envs(envs).stdin(Stdio::null()).status() {
            Ok(s) => s.code().unwrap_or(3),
            Err(e) => {
                eprintln!("watchd: cannot run mail-health.sh: {e}");
                3
            }
        }
    }

    fn list_watch_units(&self, instance: &str) -> Vec<String> {
        let pattern = format!("spira-watch-*-{instance}.service");
        let mut names = std::collections::BTreeSet::new();
        for sub in [&["list-unit-files", "--no-legend"][..], &["list-units", "--all", "--no-legend"][..]] {
            let mut args: Vec<&str> = sub.to_vec();
            args.push(&pattern);
            if let Ok(out) = systemctl_cmd(&self.systemctl).args(&args).stdin(Stdio::null()).stderr(Stdio::null()).output() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some(unit) = line.trim_start_matches(['●', ' ']).split_whitespace().next() {
                        names.insert(unit.to_string());
                    }
                }
            }
        }
        names.into_iter().collect()
    }

    fn disable_now(&self, unit: &str) {
        let _ = systemctl_cmd(&self.systemctl).args(["disable", "--now", unit]).stdin(Stdio::null()).status();
    }

    fn sop_match(&self, payload: &str) -> Result<Vec<SopMatch>, String> {
        // FORK-EDGE: sop match reads the shelf through bash lib.sh bdjson (needs a served content memories verb).
        let out = run_capture(spira_config::bounded::bounded_for("sop", "30").arg("match").arg("-"), payload)?;
        if out.0 != 0 {
            return Err(format!("sop match exited {}: {}", out.0, out.1.trim()));
        }
        Ok(parse_match_output(&out.1))
    }

    fn sop_show(&self, slug: &str) -> Result<String, String> {
        // FORK-EDGE: sop show reads the shelf through bash lib.sh bdjson (needs a served content memories verb).
        let out = run_capture(spira_config::bounded::bounded_for("sop", "30").arg("show").arg(slug), "")?;
        if out.0 != 0 {
            return Err(format!("sop show {slug} exited {}: {}", out.0, out.1.trim()));
        }
        Ok(out.1)
    }

    fn run_tool(&self, program: &str, args: &[String], deadline_secs: u64) -> ToolRun {
        let path = match release_bin_tool(program) {
            Ok(p) => p,
            Err(e) => return ToolRun::Unavailable(e),
        };
        let child_env = match spira_config::release_env::child_path_env_for_process() {
            Ok(e) => e,
            Err(e) => return ToolRun::Unavailable(e),
        };
        let spec = match spira_config::process::spec() {
            Ok(spec) => spec,
            Err(e) => return ToolRun::Unavailable(format!("{e}; {program} would run without config")),
        };
        // FORK-EDGE: a release tool run in its own transient unit with --wait --pipe: needs a Units start that pipes output and waits (Units::start returns at once).
        let mut cmd = Command::new(&self.systemd_run);
        cmd.args(["--user", "--collect", "--wait", "--pipe", "--quiet"]).arg(format!("--setenv=SPIRA_TOML={spec}"));
        for (k, v) in child_env {
            cmd.arg(format!("--setenv={k}={v}"));
        }
        cmd.arg("--")
            .args(["timeout", "--kill-after=5", &deadline_secs.to_string()])
            .arg(&path)
            .args(args);
        match run_capture(&mut cmd, "") {
            Err(e) => ToolRun::Unavailable(e),
            Ok((124, output)) => ToolRun::TimedOut { output },
            Ok((code, output)) => ToolRun::Exited { code, output },
        }
    }

    fn sop_applied(&self, slug: &str, target: &Target, check: &str, why: &str) -> Result<(), String> {
        let (flag, id) = match target {
            Target::Bead(id) => ("--bead", id),
            Target::Pass(id) => ("--pass", id),
        };
        // FORK-EDGE: sop applied writes its ledger and metric in the sop binary; its shelf reads fork bash lib.sh bdjson, so a library call would carry the same fork (needs a served content memories verb).
        let mut cmd = spira_config::bounded::bounded_for("sop", "60");
        cmd.args(["applied", slug, flag, id, "--check", check, "--held", "unknown", "--why", "-"]);
        match run_capture(&mut cmd, why)? {
            (0, _) => Ok(()),
            (code, out) => Err(format!("sop applied exited {code}: {}", out.trim())),
        }
    }

    fn mail_concierge(&self, kind: &str, subject: &str, body: &str) -> Result<(), String> {
        crate::send::send_configured(&crate::send::Letter { mailbox: "concierge", subject, kind, default: "", body })
    }

    /// `inbox-append.sh`, in this process for every line it would not push. A line it pushes
    /// still goes through the script, whose detached flusher must outlive this process.
    fn inbox_append(&self, text: &str) -> Result<(), String> {
        let cfg = spira_config::process::cfg;
        if !inbox_pushes(text, &cfg("SPIRA_CONCIERGE_PUSH_WAKE")?, &cfg("SPIRA_CONCIERGE_PUSH_PATTERN")?) {
            let inbox = std::path::PathBuf::from(cfg("SPIRA_CONCIERGE_INBOX")?);
            if let Some(dir) = inbox.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            use std::io::Write;
            return std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&inbox)
                .and_then(|mut f| writeln!(f, "{}", inbox_line(now, text)))
                .map_err(|e| format!("cannot append to {}: {e}", inbox.display()));
        }
        // FORK-EDGE: the push leg's detached flusher (flock + quiet-window + concierge.sh wake) outlives this process.
        let out = spira_config::bounded::bounded("inbox-append.sh")
            .arg(text)
            .envs(spira_config::release_env::child_path_env_for_process()?)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run inbox-append.sh: {e}"))?;
        if out.status.success() { Ok(()) } else { Err(format!("inbox-append.sh exited {}", out.status.code().unwrap_or(-1))) }
    }
}

/// Runs `cmd` with `stdin_body` on stdin; returns its exit code and stdout+stderr together.
/// `Err` only when the process could not be started or waited on.
fn run_capture(cmd: &mut Command, stdin_body: &str) -> Result<(i32, String), String> {
    use std::io::Write;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut s) = child.stdin.take() {
        let _ = s.write_all(stdin_body.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.code().unwrap_or(-1), text))
}

/// Every pid under `proc_root` (but `except`) whose first three argv elements include `prog`,
/// ascending — `/proc` read directly, never a `pgrep` child.
pub fn pids_with_arg(proc_root: &std::path::Path, prog: &str, except: i32) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir(proc_root) else { return Vec::new() };
    let mut pids: Vec<i32> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| pid != except && cmdline_has_arg(proc_root, pid, prog))
        .collect();
    pids.sort_unstable();
    pids
}

/// Whether `inbox-append.sh` would push `text` (its `[ -n WAKE ] && grep -qE PATTERN` gate):
/// only then does the detached flusher have work, and only then is the script run.
pub fn inbox_pushes(text: &str, wake: &str, pattern: &str) -> bool {
    !wake.is_empty() && regex::Regex::new(pattern).is_ok_and(|re| re.is_match(text))
}

/// `inbox-append.sh`'s own line: `<UTC %FT%TZ> <text>`.
pub fn inbox_line(epoch: i64, text: &str) -> String {
    format!("{} {text}", crate::iso8601::format_utc(epoch))
}

fn cmdline_has_arg(proc_root: &std::path::Path, pid: i32, target: &str) -> bool {
    let Ok(raw) = std::fs::read(proc_root.join(pid.to_string()).join("cmdline")) else { return false };
    raw.split(|&b| b == 0)
        .take(3)
        .any(|arg| std::str::from_utf8(arg).map(|s| s == target).unwrap_or(false))
}

/// True when opening `path` and attempting a non-blocking exclusive flock FAILS — i.e.
/// somebody else already holds it. The attempt itself takes and immediately releases the
/// lock (dropping the `File` closes it) when it succeeds, so this never disturbs a lock
/// nobody holds.
fn is_locked_by_someone(path: &std::path::Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Ok(f) = std::fs::OpenOptions::new().append(true).open(path) else { return false };
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    rc != 0
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Fake {
        pub active: HashMap<String, String>,
        pub shown: HashMap<String, (String, String)>,
        pub restarted: RefCell<Vec<Vec<String>>>,
        pub restart_fails: bool,
        pub units: HashMap<(String, String), String>,
        pub orphans: HashMap<String, String>,
        pub asks: RefCell<Vec<(String, String, String, String)>>,
        pub refuse_asks: bool,
        pub mail_health_rc: i32,
        pub watch_units: Vec<String>,
        pub disabled: RefCell<Vec<String>>,
        /// (slug, full text): `sop_match` fires on a slug whose text's `MATCH:` regex hits.
        pub sops: Vec<(String, String)>,
        pub tool_result: Option<ToolRunFake>,
        pub tool_calls: RefCell<Vec<(String, Vec<String>, u64)>>,
        pub applied: RefCell<Vec<(String, String, String)>>,
        pub concierge_mail: RefCell<Vec<(String, String, String)>>,
        pub inbox: RefCell<Vec<String>>,
    }

    #[derive(Clone)]
    pub enum ToolRunFake {
        Exit(i32, String),
        Timeout(String),
        Unavailable(String),
    }

    impl Ops for Fake {
        fn is_active(&self, units: &[String]) -> Vec<String> {
            units.iter().map(|u| self.active.get(u).cloned().unwrap_or_else(|| "?".to_string())).collect()
        }
        fn show(&self, units: &[String]) -> HashMap<String, (String, String)> {
            units.iter().filter_map(|u| self.shown.get(u).cloned().map(|v| (u.clone(), v))).collect()
        }
        fn restart(&self, units: &[String]) -> Result<(), String> {
            if self.restart_fails {
                return Err("systemd refused".into());
            }
            self.restarted.borrow_mut().push(units.to_vec());
            Ok(())
        }
        fn spira_unit(&self, base: &str, kind: &str, _instance: &str) -> String {
            self.units.get(&(base.to_string(), kind.to_string())).cloned().unwrap_or_else(|| "?".to_string())
        }
        fn orphan_lock(&self, target: &str) -> Option<String> {
            self.orphans.get(target).cloned()
        }
        fn mail_ask(&self, subject: &str, default: &str, why: &str, evidence: &str) -> Result<(), String> {
            if self.refuse_asks {
                return Err("refused".into());
            }
            self.asks.borrow_mut().push((subject.into(), default.into(), why.into(), evidence.into()));
            Ok(())
        }
        fn mail_health(&self) -> i32 {
            self.mail_health_rc
        }
        fn list_watch_units(&self, _instance: &str) -> Vec<String> {
            self.watch_units.clone()
        }
        fn disable_now(&self, unit: &str) {
            self.disabled.borrow_mut().push(unit.to_string());
        }
        fn sop_match(&self, payload: &str) -> Result<Vec<SopMatch>, String> {
            Ok(self
                .sops
                .iter()
                .filter(|(_, text)| {
                    sop::validate::match_line(text)
                        .and_then(|p| regex::Regex::new(&p).ok())
                        .is_some_and(|re| re.is_match(payload))
                })
                .map(|(k, text)| SopMatch { slug: k.clone(), runs: sop::run_spec::declares_run(text) })
                .collect())
        }
        fn sop_show(&self, slug: &str) -> Result<String, String> {
            self.sops.iter().find(|(k, _)| k == slug).map(|(_, t)| t.clone()).ok_or_else(|| "no such SOP".to_string())
        }
        fn run_tool(&self, program: &str, args: &[String], deadline_secs: u64) -> ToolRun {
            self.tool_calls.borrow_mut().push((program.to_string(), args.to_vec(), deadline_secs));
            match self.tool_result.clone().unwrap_or(ToolRunFake::Exit(0, String::new())) {
                ToolRunFake::Exit(code, output) => ToolRun::Exited { code, output },
                ToolRunFake::Timeout(output) => ToolRun::TimedOut { output },
                ToolRunFake::Unavailable(why) => ToolRun::Unavailable(why),
            }
        }
        fn sop_applied(&self, slug: &str, target: &Target, check: &str, _why: &str) -> Result<(), String> {
            let id = match target {
                Target::Bead(i) | Target::Pass(i) => i.clone(),
            };
            self.applied.borrow_mut().push((slug.to_string(), id, check.to_string()));
            Ok(())
        }
        fn mail_concierge(&self, kind: &str, subject: &str, body: &str) -> Result<(), String> {
            self.concierge_mail.borrow_mut().push((kind.into(), subject.into(), body.into()));
            Ok(())
        }
        fn inbox_append(&self, text: &str) -> Result<(), String> {
            self.inbox.borrow_mut().push(text.into());
            Ok(())
        }
    }

    #[test]
    fn fake_records_restarts() {
        let f = Fake::default();
        f.restart(&["a".into(), "b".into()]).unwrap();
        assert_eq!(f.restarted.borrow().len(), 1);
    }
}

#[cfg(test)]
mod real_tests {
    use super::*;

    #[test]
    fn pids_with_arg_reads_proc_argv_exactly_and_skips_the_excluded_pid() {
        let tmp = testkit::TempDir::new("watchd-proc");
        let root = tmp.path().to_path_buf();
        for (pid, argv) in [("10", &b"/usr/bin/foo\0--x\0"[..]), ("11", b"bash\0/usr/bin/foo\0"), ("12", b"/usr/bin/foobar\0"), ("13", b"/usr/bin/foo\0"), ("self", b"/usr/bin/foo\0")] {
            std::fs::create_dir_all(root.join(pid)).unwrap();
            std::fs::write(root.join(pid).join("cmdline"), argv).unwrap();
        }
        assert_eq!(pids_with_arg(&root, "/usr/bin/foo", 13), vec![10, 11]);
    }

    #[test]
    fn inbox_pushes_only_with_a_wake_and_a_matching_pattern() {
        let pat = r"\bP0\b|RED on|PAUSED";
        assert!(inbox_pushes("watchd: P0 down", "a-wake-command", pat));
        assert!(!inbox_pushes("watchd: P01 down", "a-wake-command", pat));
        assert!(!inbox_pushes("watchd: P0 down", "", pat));
        assert!(!inbox_pushes("watchd: P0 down", "a-wake-command", "("));
    }

    #[test]
    fn inbox_line_is_the_scripts_utc_stamp_then_the_text() {
        assert_eq!(inbox_line(0, "hello there"), "1970-01-01T00:00:00Z hello there");
    }

    #[test]
    fn match_output_reads_the_run_column_and_treats_a_legacy_line_as_runnable() {
        let m = |slug: &str, runs| SopMatch { slug: slug.into(), runs };
        assert_eq!(parse_match_output("sop-a\tMATCH\t1\trun\tsym\nsop-b\tMATCH\t1\t-\tsym\n"), vec![m("sop-a", true), m("sop-b", false)]);
        assert_eq!(parse_match_output("sop-c\tMATCH\t1\tsym\n"), vec![m("sop-c", true)]);
    }


}
