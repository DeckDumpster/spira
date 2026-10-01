//! Everything `watchd` asks systemd, `mail.sh` or `/proc` to do, behind one trait so the
//! command logic (`cmd_status`, `cmd_notify`, `cmd_restart`, …) is unit tested with a Fake
//! rather than a real systemd user manager or mailbox. `Real` is the production
//! implementation; DESIGN.md "Test strategy".

use std::collections::HashMap;
use std::process::{Command, Stdio};

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

    /// Send an escalation through `mail.sh send operator …`.
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
}

pub struct Real {
    pub systemctl: String,
}

impl Real {
    pub fn new(systemctl: String) -> Real {
        Real { systemctl }
    }
}

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
        // cmdline match below (like the pgrep search) is against the PROGRAM alone, never
        // the row's full string with its arguments still attached. Scar: passing `target`
        // whole here meant no live orphan ever matched, because /proc/<pid>/cmdline's argv
        // is NUL-separated (argv[0] is the program, argv[1] a separate element) and never
        // reassembles into "program args" as one space-joined string.
        let prog = target.split(' ').next().unwrap_or("");
        if prog.is_empty() {
            return None;
        }
        let base = std::path::Path::new(prog).file_name()?.to_str()?.to_string();
        let out = Command::new("pgrep").arg("-f").arg(&base).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Ok(pid) = line.trim().parse::<i32>() else { continue };
            if !cmdline_has_arg(pid, prog) {
                continue;
            }
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
        run_with_stdin(
            "mail.sh",
            &["send", "operator", "--from", "Watchd <watchd@spira>", "--subject", subject, "--kind", "question", "--default", default],
            &body,
        )
    }

    fn mail_health(&self) -> i32 {
        Command::new("mail-health.sh").stdin(Stdio::null()).status().ok().and_then(|s| s.code()).unwrap_or(3)
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
}

fn cmdline_has_arg(pid: i32, target: &str) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return false };
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

fn run_with_stdin(program: &str, args: &[&str], stdin_body: &str) -> Result<(), String> {
    use std::io::Write;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut s) = child.stdin.take() {
        let _ = s.write_all(stdin_body.as_bytes());
    }
    let st = child.wait().map_err(|e| e.to_string())?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("{program} exited {}", st.code().unwrap_or(-1)))
    }
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
        pub mail_health_rc: i32,
        pub watch_units: Vec<String>,
        pub disabled: RefCell<Vec<String>>,
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
    }

    #[test]
    fn fake_records_restarts() {
        let f = Fake::default();
        f.restart(&["a".into(), "b".into()]).unwrap();
        assert_eq!(f.restarted.borrow().len(), 1);
    }
}
