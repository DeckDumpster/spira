//! `--sccache-wedge-check` — a wedged sccache server leaves every `RUSTC_WRAPPER` client
//! alive at 0% CPU while no `rustc` runs anywhere, and nothing times out. Detection is the
//! pair (clients older than the bound, zero rustc); the remedy is `sccache --stop-server`,
//! after which the next client starts a fresh server.

use crate::incident::{self, Finding};
use crate::log::log;
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct Cfg {
    pub sccache: String,
    pub stale_secs: i64,
    pub cmd_timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub pid: u32,
    pub age_s: i64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scan {
    pub clients: Vec<Client>,
    pub rustc: usize,
}

fn argv(pid_dir: &Path) -> Vec<String> {
    let raw = fs::read(pid_dir.join("cmdline")).unwrap_or_default();
    raw.split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

fn base(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// A client is `sccache <compiler> …`; the server and `sccache --flag` calls are not.
pub fn is_client(argv: &[String]) -> bool {
    argv.len() > 1 && base(&argv[0]) == "sccache" && !argv[1].starts_with('-')
}

pub fn scan(proc_root: &Path, now: i64) -> Scan {
    let mut out = Scan::default();
    let Ok(rd) = fs::read_dir(proc_root) else { return out };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<u32>().ok() else { continue };
        let a = argv(&e.path());
        let Some(first) = a.first() else { continue };
        if base(first) == "rustc" {
            out.rustc += 1;
        } else if is_client(&a) {
            let age_s = fs::metadata(e.path())
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| (now - d.as_secs() as i64).max(0))
                .unwrap_or(0);
            out.clients.push(Client { pid, age_s });
        }
    }
    out.clients.sort_by_key(|c| c.pid);
    out
}

/// Wedged: at least one client past the bound and no rustc doing the work they wait on.
pub fn wedged(s: &Scan, stale_secs: i64) -> bool {
    s.rustc == 0 && s.clients.iter().any(|c| c.age_s >= stale_secs)
}

fn sccache(cfg: &Cfg, arg: &str) -> String {
    let out = Command::new("timeout").arg(cfg.cmd_timeout_secs.to_string()).arg(&cfg.sccache).arg(arg).output();
    match out {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            format!("exit {}\n{}", o.status.code().map_or("signal".to_string(), |c| c.to_string()), s.trim_end())
        }
        Err(e) => format!("could not run {}: {e}", cfg.sccache),
    }
}

pub fn run(now: i64, proc_root: &Path, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    let s = scan(proc_root, now);
    if !wedged(&s, cfg.stale_secs) {
        log(&format!("watchtower: sccache-wedge-check — ok ({} client(s), {} rustc)", s.clients.len(), s.rustc));
        return;
    }
    let oldest = s.clients.iter().map(|c| c.age_s).max().unwrap_or(0);
    log(&format!("watchtower: sccache-wedge-check — WEDGED ({} client(s), oldest {oldest}s, 0 rustc); stopping server", s.clients.len()));
    let stats = sccache(cfg, "--show-stats");
    let stop = sccache(cfg, "--stop-server");
    if !incident::is_usable(incident_sh) {
        return;
    }
    let body = format!(
        "{} sccache client(s) (oldest {oldest}s) are alive with no rustc running: the cache server is wedged and every Rust build stalls behind it. The server was restarted with `sccache --stop-server`; the next client starts a fresh one.\n\n--show-stats before restart:\n{stats}\n\n--stop-server:\n{stop}\n",
        s.clients.len()
    );
    let f = Finding::new(db, home_repo, "SCCACHE WEDGED: server restarted", &body)
        .priority(2)
        .reference("incident:sccache-wedge")
        .cause("sccache-wedge");
    incident::file(incident_sh, &f);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake(root: &Path, pid: u32, argv: &[&str]) {
        let d = root.join(pid.to_string());
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("cmdline"), argv.iter().map(|a| format!("{a}\0")).collect::<String>()).unwrap();
    }

    #[test]
    fn clients_are_told_from_the_server_and_flag_calls() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_client(&v(&["/usr/bin/sccache", "rustc", "--crate-name", "x"])));
        assert!(!is_client(&v(&["sccache"])));
        assert!(!is_client(&v(&["sccache", "--show-stats"])));
        assert!(!is_client(&v(&["rustc", "x"])));
    }

    #[test]
    fn stuck_clients_with_no_rustc_are_wedged_and_a_running_rustc_clears_it() {
        let dir = testkit::TempDir::new("wt-sccache-scan");
        let procr = dir.join("proc");
        fake(&procr, 10, &["sccache", "rustc", "--crate-name", "a"]);
        fake(&procr, 11, &["sccache"]);
        let s = scan(&procr, i64::MAX / 4);
        assert_eq!(s.clients.len(), 1);
        assert_eq!(s.rustc, 0);
        assert!(wedged(&s, 600));
        assert!(!wedged(&scan(&procr, 0), 600), "a fresh client is not stuck");
        fake(&procr, 12, &["/x/rustc", "--crate-name", "a"]);
        assert!(!wedged(&scan(&procr, i64::MAX / 4), 600), "a running rustc means work is progressing");
    }

    #[test]
    fn a_wedge_stops_the_server_and_a_healthy_box_does_not() {
        let dir = testkit::TempDir::new("wt-sccache-run");
        let procr = dir.join("proc");
        fake(&procr, 10, &["sccache", "rustc", "--crate-name", "a"]);
        let stub = dir.join("sccache");
        let log = dir.join("calls");
        fs::write(&stub, format!("#!/bin/sh\necho \"$1\" >> {}\nif [ \"$1\" = --show-stats ]; then sleep 30; fi\n", log.display())).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = Cfg { sccache: stub.display().to_string(), stale_secs: 0, cmd_timeout_secs: 1 };

        run(i64::MAX / 4, &procr, "", "spira", "/nonexistent", &Cfg { stale_secs: i64::MAX, ..cfg_clone(&cfg) });
        assert!(!log.exists(), "a client under the bound must not touch the server");

        run(i64::MAX / 4, &procr, "", "spira", "/nonexistent", &cfg);
        let calls = fs::read_to_string(&log).unwrap();
        assert!(calls.contains("--stop-server"), "a stub that never answers --show-stats must not block the restart: {calls}");
    }

    fn cfg_clone(c: &Cfg) -> Cfg {
        Cfg { sccache: c.sccache.clone(), stale_secs: c.stale_secs, cmd_timeout_secs: c.cmd_timeout_secs }
    }
}
