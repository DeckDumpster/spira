//! Command dispatch and the real wiring: config from the environment and spira.toml, the
//! Proxmox provider, mail.sh alarms, ssh/rsync, git.

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use crate::alarm::MailAlarm;
use crate::config::{Config, EnvThenToml, PveEnv};
use crate::pool::{Attempt, Deps, Pool, Spawner};
use crate::procs::{block_termination_signals, command};
use crate::provider::Timing;
use crate::pve::{HttpTransport, Pve};
use crate::run::{parse_run_args, run, GitHost, RunEnv, SshRemote};
use crate::schema::ProcId;

pub const USAGE: &str = "usage: round-vm acquire|release <handle>|run <tree-dir> [--suites <csv>] [--maxpar <n>] [--toolchain <ver>] [--results-dir <dir>] [--attr-spool <dir>]|status";

fn secs_env(key: &str, default: u64) -> Duration {
    Duration::from_secs(std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default))
}

/// One attempt's worth of provider, built from config and pve.env read NOW (G7).
pub fn real_attempt() -> Result<Attempt, String> {
    let cfg = Config::load(&EnvThenToml::load())?;
    let pubkey = std::fs::read_to_string(&cfg.host_pubkey)
        .map_err(|_| format!("round-vm: SPIRA_ROUND_VM_HOST_PUBKEY not readable: {}", cfg.host_pubkey.display()))?;
    let env = PveEnv::load(&cfg.pve_env_path, &|k| std::env::var(k).ok())?;
    let transport = HttpTransport::new(&env)?;
    Ok(Attempt {
        provider: Box::new(Pve {
            t: transport,
            env,
            task_timeout: secs_env("PVE_TASK_TIMEOUT", 300),
            exec_timeout: secs_env("PVE_EXEC_TIMEOUT", 30),
            poll: Duration::from_secs(1),
        }),
        iface: cfg.net_iface.clone(),
        ssh_user: cfg.ssh_user.clone(),
        pubkey,
        timing: Timing { boot_tries: cfg.boot_tries, poll: cfg.boot_poll, gone_tries: 30 },
    })
}

/// Starts `round-vm _provision-bg` in its own process group (so a `timeout` killing the
/// caller's group does not kill it), logging to `<state>/provisioning.out`.
struct SelfSpawner {
    state_dir: PathBuf,
}

impl Spawner for SelfSpawner {
    fn spawn_next(&self) -> Result<ProcId, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state_dir.join("provisioning.out"))
            .map_err(|e| e.to_string())?;
        let mut child = command(exe)
            .arg("_provision-bg")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| e.to_string())?;
        let id = ProcId::of(child.id()).unwrap_or(ProcId { pid: child.id(), start: 0 });
        // Reap it whenever it finishes, so a long-lived caller holds no zombie.
        std::thread::spawn(move || child.wait());
        Ok(id)
    }
}

fn pool_for(cfg: &Config) -> Pool {
    Pool {
        state_dir: cfg.state_dir.clone(),
        retry_interval: cfg.retry_interval,
        max_retries: cfg.max_retries,
        wait_poll: cfg.wait_poll,
    }
}

pub fn main_with(args: Vec<String>) -> i32 {
    let Some((verb, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return 1;
    };
    let rest = rest.to_vec();
    match verb.as_str() {
        "acquire" | "release" | "run" | "status" | "_provision-bg" => {}
        other => {
            eprintln!("round-vm: unknown verb: {other}");
            eprintln!("{USAGE}");
            return 1;
        }
    }
    if verb == "release" && rest.is_empty() {
        eprintln!("round-vm release: usage: round-vm release <handle>");
        return 2;
    }
    let run_args = if verb == "run" {
        match parse_run_args(&rest) {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        }
    } else {
        None
    };
    let cfg = match Config::load(&EnvThenToml::load()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return if verb == "run" { 2 } else { 1 };
        }
    };
    if let Err(e) = std::fs::create_dir_all(&cfg.state_dir) {
        eprintln!("round-vm: {}: {e}", cfg.state_dir.display());
        return if verb == "run" { 2 } else { 1 };
    }
    let pool = pool_for(&cfg);
    let alarm = MailAlarm { spira_home: cfg.spira_home.clone(), mailbox: cfg.mailbox.clone(), retry_secs: cfg.retry_interval.as_secs() };
    let spawner = SelfSpawner { state_dir: cfg.state_dir.clone() };
    let factory = real_attempt;
    let deps = Deps { factory: &factory, alarm: &alarm, spawner: &spawner };

    match verb.as_str() {
        "acquire" => match pool.acquire(&deps, None) {
            Ok((vm, mode)) => {
                println!("{} {} {}", vm.handle, vm.addr, mode.as_str());
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        "release" => match pool.release(&rest[0], &factory) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        "status" => match pool.status() {
            Ok(s) => {
                print!("{s}");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        "_provision-bg" => match pool.provision_background(&factory) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        "run" => {
            let me = ProcId::current();
            let sig_pool = pool_for(&cfg);
            block_termination_signals(move |sig| {
                eprintln!("round-vm run: signal {sig}: releasing this run's VM");
                sig_pool.release_owned_by(me, &real_attempt);
                std::process::exit(128 + sig);
            });
            let host = GitHost { state_dir: cfg.state_dir.clone(), mirror_port: cfg.mirror_port };
            let remote = SshRemote { user: cfg.ssh_user.clone(), port: cfg.ssh_port, key: cfg.host_key.clone() };
            run(&RunEnv { cfg: &cfg, pool: &pool, deps: &deps, host: &host, remote: &remote }, run_args.as_ref().unwrap())
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn usage_errors_exit_before_touching_config() {
        assert_eq!(main_with(vec![]), 1);
        assert_eq!(main_with(s(&["frobnicate"])), 1);
        assert_eq!(main_with(s(&["release"])), 2);
        assert_eq!(main_with(s(&["run"])), 2);
        assert_eq!(main_with(s(&["run", "/t", "--bogus"])), 2);
    }
}
