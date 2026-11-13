//! The Host section's ports on the real box: `spira-config` for `certify_par`, `target-reap`
//! for the reaps, the store's socket for the section, and the statvfs and `/proc/pressure`
//! reads the sample is made of.

use std::path::{Path, PathBuf};
use std::process::Command;

use reconciler_engine::goodput_pass::Socket;

use crate::host_section::{self as hs, Cfg, Config, Ports, Reaper, Stale};

/// `spira-config get|set` against the one config file `$SPIRA_TOML` names.
pub struct SpiraConfig {
    pub bin: String,
    pub file: String,
    pub env: Vec<(String, String)>,
}

impl SpiraConfig {
    fn call(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args).envs(self.env.clone());
        let out = crate::deadline::output("spira-config (host section)", &mut cmd).map_err(|e| format!("`spira-config {}`: {e}", args.join(" ")))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(format!("`spira-config {}` exited {:?}", args.join(" "), out.status.code()))
        }
    }
}

impl Config for SpiraConfig {
    fn get(&self, key: &str) -> Result<u64, String> {
        let v = self.call(&["get", key, &self.file])?;
        v.parse().map_err(|_| format!("{key} is {v:?}, not a whole number"))
    }

    fn set(&self, key: &str, value: u64) -> Result<(), String> {
        self.call(&["set", key, &value.to_string(), &self.file]).map(|_| ())
    }
}

/// `target-reap` over the worktrees directory: a dry run counts what is stale, a bounded run
/// takes the oldest share of it.
pub struct TargetReap {
    pub bin: String,
    pub worktrees: PathBuf,
    pub env: Vec<(String, String)>,
}

/// The count after `verb` (`would remove` / `removed`) on the first line that carries one.
pub fn count_after(out: &str, verb: &str) -> Option<u64> {
    out.lines().find_map(|l| {
        let rest = l.split_once(&format!("{verb} "))?.1;
        rest.split_whitespace().next()?.parse().ok()
    })
}

impl TargetReap {
    fn call(&self, what: Stale, dry: bool, fraction: f64) -> Result<String, String> {
        let mut cmd = Command::new(&self.bin);
        if dry {
            cmd.arg("--dry-run");
        }
        cmd.args(["--fraction", &fraction.to_string()]);
        if what == Stale::Scratch {
            cmd.arg("--scratch");
        }
        cmd.arg("--worktrees").arg(&self.worktrees).envs(self.env.clone());
        let out = crate::deadline::output("target-reap (host section)", &mut cmd).map_err(|e| format!("target-reap: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!("target-reap exited {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }
}

impl Reaper for TargetReap {
    fn stale(&self, what: Stale) -> Result<u64, String> {
        let out = self.call(what, true, 1.0)?;
        count_after(&out, "would remove").ok_or_else(|| format!("target-reap said no count: {out:?}"))
    }

    fn reap(&self, what: Stale, fraction: f64) -> Result<u64, String> {
        let out = self.call(what, false, fraction)?;
        count_after(&out, "removed").ok_or_else(|| format!("target-reap said no count: {out:?}"))
    }
}

/// The paths the roles are measured at, from the declared config and the probes' own overrides.
pub fn cfg_of(run: &Path) -> Result<Cfg, String> {
    let get = crate::process::getenv;
    Ok(Cfg {
        scratch: get("SPIRA_TMP_PROBE_PATH").unwrap_or_else(|| crate::probes::DEFAULT_TMP_PATH.to_string()),
        worktrees: run.join(spira_config::process::cfg("SPIRA_HOST_WORKTREE_SUBDIR")?).to_string_lossy().into_owned(),
        root: crate::probes::ROOT_PATH.to_string(),
        psi_dir: get("SPIRA_PSI_DIR").unwrap_or_else(|| crate::probes::DEFAULT_PSI_DIR.to_string()),
    })
}

/// One watchtower pass over the section, at the real box.
pub fn pass(now: u64, cfg: &Cfg, run: &Path) -> Result<String, String> {
    let file = spira_config::process::spec()?.rsplit(':').next().map(str::to_string).ok_or("SPIRA_TOML names no config file")?;
    let env: Vec<(String, String)> = spira_config::release_env::child_path_env_for_process()?.into_iter().collect();
    let config = SpiraConfig { bin: spira_config::release_env::tool("spira-config")?, file, env: env.clone() };
    let reaper = TargetReap { bin: spira_config::release_env::tool("target-reap")?, worktrees: PathBuf::from(&cfg.worktrees), env };
    let lc = Socket(spira_store::lc_call::LcClient::configured()?);
    let step_log = run.join("host-steps.log");
    let record = |line: &str| {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&step_log) {
            let _ = writeln!(f, "{now} {line}");
        }
    };
    let sample = hs::sample(cfg, &|p| crate::fsstat::usage(p), &|p| std::fs::read_to_string(p).ok());
    let r = hs::run(&Ports { lc: &lc, config: &config, reaper: &reaper, record: &record }, now, &sample)?;
    Ok(format!("watchtower: host section — acked {} step(s){}", r.acked.len(), if r.published { ", published" } else { "" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_is_read_from_the_line_that_carries_the_verb() {
        let out = "target-reap: removed 3 stale foreign-cache run dir(s)\nidle: target-reap: would remove 20 target dir(s), 4 MiB (sp-a)\n";
        assert_eq!(count_after(out, "would remove"), Some(20));
        assert_eq!(count_after(out, "removed"), Some(3));
        assert_eq!(count_after("nothing", "removed"), None);
    }
}
