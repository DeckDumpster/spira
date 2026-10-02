//! Configuration (DESIGN.md §3.1). Read fresh on every attempt (G7): nothing here is
//! cached across calls.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use spira_config::SpiraSection;

/// Where a key's value comes from: the environment first, then `spira.toml`'s `[spira]`
/// table. A test passes a map instead.
pub trait Source {
    fn get(&self, key: &str) -> Option<String>;
}

/// The real source: process environment, then the `spira.toml` `spira_config::discover`
/// finds (loaded through spira-config, never parsed here).
pub struct EnvThenToml {
    toml: Option<SpiraSection>,
}

impl EnvThenToml {
    pub fn load() -> EnvThenToml {
        let toml = spira_config::discover(None)
            .and_then(|p| spira_config::load(&p).ok())
            .and_then(|doc| doc.spira);
        EnvThenToml { toml }
    }
}

fn toml_value(s: &SpiraSection, key: &str) -> Option<String> {
    let v = match key {
        "SPIRA_RUN" => &s.run,
        "SPIRA_PVE_ENV" => &s.pve_env,
        "SPIRA_MAIL_SESSION_MAILBOX" => &s.mail_session_mailbox,
        "SPIRA_ROUND_VM_STATE_DIR" => &s.round_vm_state_dir,
        "SPIRA_ROUND_VM_SSH_USER" => &s.round_vm_ssh_user,
        "SPIRA_ROUND_VM_SSH_PORT" => &s.round_vm_ssh_port,
        "SPIRA_ROUND_VM_HOST_KEY" => &s.round_vm_host_key,
        "SPIRA_ROUND_VM_HOST_PUBKEY" => &s.round_vm_host_pubkey,
        "SPIRA_ROUND_VM_HOST_ADDR" => &s.round_vm_host_addr,
        "SPIRA_ROUND_VM_VCPUS" => &s.round_vm_vcpus,
        "SPIRA_ROUND_VM_MAXPAR" => &s.round_vm_maxpar,
        "SPIRA_ROUND_VM_MAX_RETRIES" => &s.round_vm_max_retries,
        "SPIRA_ROUND_VM_RETRY_INTERVAL" => &s.round_vm_retry_interval,
        "SPIRA_ROUND_VM_MIRROR_PORT" => &s.round_vm_mirror_port,
        "SPIRA_ROUND_VM_CACHE_HOME" => &s.round_vm_cache_home,
        "SPIRA_TESTENV_REGISTRY" => &s.testenv_registry,
        _ => return None,
    };
    v.clone()
}

impl Source for EnvThenToml {
    fn get(&self, key: &str) -> Option<String> {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
        self.toml.as_ref().and_then(|s| toml_value(s, key)).filter(|v| !v.trim().is_empty())
    }
}

impl Source for BTreeMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        BTreeMap::get(self, key).cloned()
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub state_dir: PathBuf,
    pub run_dir: PathBuf,
    pub spira_home: Option<PathBuf>,
    pub pve_env_path: PathBuf,
    pub ssh_user: String,
    pub ssh_port: u16,
    pub host_key: PathBuf,
    pub host_pubkey: PathBuf,
    pub host_addr: Option<String>,
    /// sp-xjnzl-2: the VM-side `CARGO_HOME` every round and every template build points
    /// `sccache` at — this binary's OWN config key (`SPIRA_ROUND_VM_CACHE_HOME` / spira.toml's
    /// `round_vm_cache_home`), never the CALLER's ambient `CARGO_HOME`
    /// (`law-a-binary-resolves-the-config-it-reads`). A systemd unit never sets `CARGO_HOME`
    /// the way an interactive shell happens to, so reading it ambiently is not "usually
    /// right, occasionally unset" — it is wrong the moment anything but a hand-run shell
    /// calls this binary, and the production sweep hit exactly that: the VM's own ambient
    /// default (`/root/.cargo`) does not match where the template actually put `sccache`.
    /// `None` here is refused by every caller that needs it (run's and template's own
    /// preflight), never silently defaulted.
    pub cache_home: Option<String>,
    /// SPIRA_TESTENV_REGISTRY, forwarded to the VM so a changed image tag costs one pull
    /// there instead of a full rebuild.
    pub testenv_registry: Option<String>,
    pub vcpus: u32,
    pub maxpar: u32,
    pub retry_interval: Duration,
    pub max_retries: u32,
    pub mirror_port: u16,
    pub mailbox: String,
    pub net_iface: String,
    pub boot_tries: u32,
    pub boot_poll: Duration,
    pub ssh_tries: u32,
    pub wait_poll: Duration,
    /// `run --attr-spool`: how often results stream back while the corpus runs.
    pub stream_every: Duration,
    /// `run --attr-spool`: how long the VM waits for the spool's `close` after the corpus.
    pub attr_linger: Duration,
}

fn num<T: std::str::FromStr>(src: &dyn Source, key: &str, default: T) -> Result<T, String> {
    match src.get(key) {
        None => Ok(default),
        Some(v) => v.trim().parse().map_err(|_| format!("{key}: not a number: {v:?}")),
    }
}

impl Config {
    pub fn load(src: &dyn Source) -> Result<Config, String> {
        let run_dir = PathBuf::from(
            src.get("SPIRA_RUN").ok_or("round-vm: SPIRA_RUN not set (and no `run` in spira.toml)")?,
        );
        let state_dir = src
            .get("SPIRA_ROUND_VM_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| run_dir.join("round-vm"));
        let host_key = src
            .get("SPIRA_ROUND_VM_HOST_KEY")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_dir.join("host_key"));
        let host_pubkey = src.get("SPIRA_ROUND_VM_HOST_PUBKEY").map(PathBuf::from).unwrap_or_else(|| {
            let mut s = host_key.clone().into_os_string();
            s.push(".pub");
            PathBuf::from(s)
        });
        let pve_env_path = src.get("SPIRA_PVE_ENV").map(PathBuf::from).unwrap_or_else(default_pve_env);
        Ok(Config {
            spira_home: src.get("SPIRA_HOME").map(PathBuf::from),
            pve_env_path,
            ssh_user: src.get("SPIRA_ROUND_VM_SSH_USER").unwrap_or_else(|| "root".into()),
            ssh_port: num(src, "SPIRA_ROUND_VM_SSH_PORT", 22)?,
            host_addr: src.get("SPIRA_ROUND_VM_HOST_ADDR"),
            cache_home: src.get("SPIRA_ROUND_VM_CACHE_HOME"),
            testenv_registry: src.get("SPIRA_TESTENV_REGISTRY"),
            vcpus: num(src, "SPIRA_ROUND_VM_VCPUS", 16)?,
            maxpar: num(src, "SPIRA_ROUND_VM_MAXPAR", 16)?,
            retry_interval: Duration::from_secs(num(src, "SPIRA_ROUND_VM_RETRY_INTERVAL", 60)?),
            max_retries: num(src, "SPIRA_ROUND_VM_MAX_RETRIES", 0)?,
            mirror_port: num(src, "SPIRA_ROUND_VM_MIRROR_PORT", 9430)?,
            mailbox: src.get("SPIRA_ROUND_VM_MAIL_MAILBOX").unwrap_or_else(|| "operator".into()),
            net_iface: src.get("SPIRA_ROUND_VM_NET_IFACE").unwrap_or_else(|| "ens18".into()),
            boot_tries: num(src, "SPIRA_ROUND_VM_BOOT_TRIES", 60)?,
            boot_poll: Duration::from_secs(num(src, "SPIRA_ROUND_VM_BOOT_POLL", 2)?),
            ssh_tries: num(src, "SPIRA_ROUND_VM_SSH_TRIES", 30)?,
            wait_poll: Duration::from_secs(1),
            stream_every: Duration::from_secs(num(src, "SPIRA_ROUND_VM_STREAM_SECS", 10)?),
            attr_linger: Duration::from_secs(num(src, "SPIRA_ROUND_VM_ATTR_LINGER", 3600)?),
            host_key,
            host_pubkey,
            state_dir,
            run_dir,
        })
    }
}

fn default_pve_env() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    base.join("spira").join("pve.env")
}

/// `pve.env` (DESIGN.md §3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PveEnv {
    pub api_host: String,
    pub api_port: u16,
    pub node: String,
    pub token_id: String,
    pub token_secret: String,
    pub cacert: PathBuf,
    pub template_vmid: String,
    pub pool: Option<String>,
}

/// Parses shell `KEY=value` assignment lines (optional `export`, optional single or double
/// quotes, `#` comments). Anything else is ignored: this file is data, never executed.
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let v = v.trim();
        let v = if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
        {
            &v[1..v.len() - 1]
        } else {
            v.split(" #").next().unwrap_or(v).trim()
        };
        out.insert(k.to_string(), v.to_string());
    }
    out
}

impl PveEnv {
    /// Reads `path` now (never a cached copy, G7). The file's values win over the
    /// environment's, as sourcing it did; the environment only fills what the file lacks.
    pub fn load(path: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<PveEnv, String> {
        let file = match std::fs::read_to_string(path) {
            Ok(t) => parse_env_file(&t),
            Err(_) => BTreeMap::new(),
        };
        let get = |k: &str| file.get(k).cloned().filter(|v| !v.is_empty()).or_else(|| env(k).filter(|v| !v.is_empty()));
        let need = |k: &str| {
            get(k).ok_or_else(|| format!("round-vm: {k} not set — is {} present and readable?", path.display()))
        };
        let cacert = PathBuf::from(get("PVE_CACERT").unwrap_or_else(|| "/etc/pve/pve-root-ca.pem".into()));
        let token_id = need("PVE_TOKEN_ID")?;
        let token_secret = need("PVE_TOKEN_SECRET")?;
        let node = need("PVE_NODE")?;
        let template_vmid = need("PVE_TEMPLATE_VMID")?;
        if !cacert.is_file() {
            return Err(format!(
                "round-vm: PVE_CACERT not found: {} (set it in {})",
                cacert.display(),
                path.display()
            ));
        }
        let api_port = match get("PVE_API_PORT") {
            None => 8006,
            Some(v) => v.parse().map_err(|_| format!("round-vm: PVE_API_PORT not a port: {v:?}"))?,
        };
        Ok(PveEnv {
            api_host: get("PVE_API_HOST").unwrap_or_else(|| "localhost".into()),
            api_port,
            node,
            token_id,
            token_secret,
            cacert,
            template_vmid,
            pool: get("PVE_RUNNER_POOL"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn defaults_derive_from_spira_run() {
        let c = Config::load(&map(&[("SPIRA_RUN", "/r")])).unwrap();
        assert_eq!(c.state_dir, PathBuf::from("/r/round-vm"));
        assert_eq!(c.host_key, PathBuf::from("/r/round-vm/host_key"));
        assert_eq!(c.host_pubkey, PathBuf::from("/r/round-vm/host_key.pub"));
        assert_eq!(c.ssh_user, "root");
        assert_eq!((c.vcpus, c.maxpar, c.mirror_port, c.max_retries), (16, 16, 9430, 0));
        assert_eq!(c.retry_interval, Duration::from_secs(60));
        assert_eq!(c.mailbox, "operator");
        assert!(c.host_addr.is_none());
        assert!(c.testenv_registry.is_none());
        assert!(c.cache_home.is_none(), "no hardcoded literal and no ambient CARGO_HOME read — unset is unset, refused by run's and template's own preflight");
    }

    #[test]
    fn testenv_registry_is_read_from_the_environment() {
        let c = Config::load(&map(&[("SPIRA_RUN", "/r"), ("SPIRA_TESTENV_REGISTRY", "registry.example/spira")])).unwrap();
        assert_eq!(c.testenv_registry.as_deref(), Some("registry.example/spira"));
    }

    #[test]
    fn cache_home_reads_its_own_dedicated_key_never_the_ambient_cargo_home() {
        // sp-xjnzl-2: a bare CARGO_HOME in the caller's environment must NOT leak in — the
        // production fault this bead fixes was exactly that leak (a systemd unit's own
        // ambient default, /root/.cargo, silently used in place of the template's actual
        // one). Only this binary's own key, env or spira.toml, ever sets cache_home.
        let c = Config::load(&map(&[("SPIRA_RUN", "/r"), ("CARGO_HOME", "/opt/spira/cargo")])).unwrap();
        assert!(c.cache_home.is_none(), "a bare CARGO_HOME must never be read");

        let c = Config::load(&map(&[("SPIRA_RUN", "/r"), ("SPIRA_ROUND_VM_CACHE_HOME", "/opt/spira/cargo")])).unwrap();
        assert_eq!(c.cache_home.as_deref(), Some("/opt/spira/cargo"));
    }

    #[test]
    fn missing_spira_run_names_the_variable() {
        let e = Config::load(&map(&[])).unwrap_err();
        assert!(e.contains("SPIRA_RUN"), "{e}");
    }

    #[test]
    fn a_bad_number_names_the_key() {
        let e = Config::load(&map(&[("SPIRA_RUN", "/r"), ("SPIRA_ROUND_VM_MAXPAR", "lots")])).unwrap_err();
        assert!(e.contains("SPIRA_ROUND_VM_MAXPAR"), "{e}");
    }

    #[test]
    fn env_file_parsing_handles_export_quotes_and_comments() {
        let m = parse_env_file("# c\nexport PVE_NODE=\"pve1\"\nPVE_TOKEN_ID='a@pve!t'\nPVE_API_HOST=10.0.0.1 # lan\nnot a line\n");
        assert_eq!(m.get("PVE_NODE").unwrap(), "pve1");
        assert_eq!(m.get("PVE_TOKEN_ID").unwrap(), "a@pve!t");
        assert_eq!(m.get("PVE_API_HOST").unwrap(), "10.0.0.1");
        assert_eq!(m.len(), 3);
    }

    fn write_env(dir: &TempDir, extra: &str) -> PathBuf {
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "x").unwrap();
        let p = dir.path().join("pve.env");
        std::fs::write(
            &p,
            format!("PVE_NODE=pve\nPVE_TOKEN_ID=id\nPVE_TOKEN_SECRET=s\nPVE_CACERT={}\n{extra}", ca.display()),
        )
        .unwrap();
        p
    }

    #[test]
    fn missing_template_vmid_names_the_variable() {
        let d = TempDir::new();
        let p = write_env(&d, "");
        let e = PveEnv::load(&p, &|_| None).unwrap_err();
        assert!(e.contains("PVE_TEMPLATE_VMID"), "{e}");
    }

    #[test]
    fn missing_cacert_is_refused_not_skipped() {
        let d = TempDir::new();
        let p = d.path().join("pve.env");
        std::fs::write(&p, "PVE_NODE=pve\nPVE_TOKEN_ID=id\nPVE_TOKEN_SECRET=s\nPVE_TEMPLATE_VMID=9000\nPVE_CACERT=/no/such/ca.pem\n").unwrap();
        let e = PveEnv::load(&p, &|_| None).unwrap_err();
        assert!(e.contains("/no/such/ca.pem"), "{e}");
    }

    #[test]
    fn pve_env_is_reread_so_a_fixed_file_is_picked_up() {
        let d = TempDir::new();
        let p = write_env(&d, "");
        assert!(PveEnv::load(&p, &|_| None).is_err());
        let p = write_env(&d, "PVE_TEMPLATE_VMID=9000\nPVE_RUNNER_POOL=ci\n");
        let e = PveEnv::load(&p, &|_| None).unwrap();
        assert_eq!(e.template_vmid, "9000");
        assert_eq!(e.pool.as_deref(), Some("ci"));
        assert_eq!((e.api_host.as_str(), e.api_port), ("localhost", 8006));
    }

    #[test]
    fn file_wins_over_environment() {
        let d = TempDir::new();
        let p = write_env(&d, "PVE_TEMPLATE_VMID=9000\n");
        let e = PveEnv::load(&p, &|k| (k == "PVE_TEMPLATE_VMID" || k == "PVE_API_HOST").then(|| "env".to_string())).unwrap();
        assert_eq!(e.template_vmid, "9000");
        assert_eq!(e.api_host, "env");
    }
}
