//! Configuration (DESIGN.md §3.1). Every `spira/conf.d`-registered key (`SPIRA_RUN`,
//! `SPIRA_ROUND_VM_*`, `SPIRA_TESTENV_REGISTRY`) comes from `spira_config::process::cfg` /
//! `cfg_parse` — resolved once per process and cached there, so `Config::load` no longer
//! re-reads those fresh on every attempt the way this module used to (G7); see its own doc.
//! A handful of round-vm's own knobs are not declared in `spira/conf.d` at all — those are
//! still read straight from the environment, with their existing defaults, on every call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use spira_config::process::{cfg, cfg_parse};

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
    /// preflight), never silently defaulted. Declared TOML-only (`spira/conf.toml.d`, not
    /// `spira/conf.d`), so it does not go through `cfg`/`cfg_parse`; see
    /// `cache_home_from_env_or_toml`.
    pub cache_home: Option<String>,
    /// SPIRA_TESTENV_REGISTRY, forwarded to the VM so a changed image tag costs one pull
    /// there instead of a full rebuild.
    pub testenv_registry: Option<String>,
    /// A round whose setup (before its suites start) exceeds this many seconds reports SETUP-SLOW.
    pub setup_alarm_secs: u64,
    pub vcpus: u32,
    pub maxpar: u32,
    pub retry_interval: Duration,
    pub max_retries: u32,
    pub acquire_deadline: Duration,
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
    /// SPIRA_ROUND_CERTIFY_WALL_SECS: the cap the live progress file measures the suites' clock against.
    pub cap_secs: u64,
    /// A progress phase with no change for this long reports itself stalled.
    pub vm_budget_secs: u64,
    pub build_budget_secs: u64,
}

/// Every value `Config::build` needs, already resolved by the caller — `cfg`/`cfg_parse` for
/// a key `spira/conf.d` declares, the environment (with its existing default) for one that
/// isn't. Pure: `build` only converts types, so tests construct this directly instead of
/// driving it through the environment or a process-global config cache.
pub(crate) struct Fields {
    pub run: String,
    pub spira_home: Option<String>,
    pub pve_env: String,
    pub ssh_user: String,
    pub ssh_port: u16,
    pub state_dir: String,
    pub host_key: String,
    pub host_pubkey: String,
    pub host_addr: Option<String>,
    pub cache_home: Option<String>,
    pub testenv_registry: Option<String>,
    pub vcpus: u32,
    pub maxpar: u32,
    pub max_retries: u32,
    pub retry_interval_secs: u64,
    pub mirror_port: u16,
    pub setup_alarm_secs: u64,
    pub acquire_deadline_secs: u64,
    pub mailbox: String,
    pub net_iface: String,
    pub boot_tries: u32,
    pub boot_poll_secs: u64,
    pub ssh_tries: u32,
    pub stream_every_secs: u64,
    pub attr_linger_secs: u64,
    pub cap_secs: u64,
    pub vm_budget_secs: u64,
    pub build_budget_secs: u64,
}

/// [`cfg`], refused with this binary's own "round-vm: " prefix (its existing error idiom —
/// every caller just `eprintln!`s the `Result`'s `Err` with no prefix of its own). Never a
/// default: a key `cfg` cannot resolve names itself in the refusal.
fn must_cfg(key: &str) -> Result<String, String> {
    cfg(key).map_err(|e| format!("round-vm: {e}"))
}

/// [`must_cfg`], parsed.
fn must_cfg_parse<T: std::str::FromStr>(key: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    cfg_parse::<T>(key).map_err(|e| format!("round-vm: {e}"))
}

/// One suite slot per vCPU unless the config names a number.
pub(crate) fn maxpar_or_vcpus(raw: &str, vcpus: u32) -> Result<u32, String> {
    match raw.trim() {
        "" => Ok(vcpus),
        v => v.parse().map_err(|_| format!("round-vm: SPIRA_ROUND_VM_MAXPAR: not a number: {v:?}")),
    }
}

/// A round-vm knob `spira/conf.d` does not declare: read straight from the environment, with
/// its own default — unaffected by the `cfg`/`cfg_parse` migration above.
fn num_env<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String> {
    match std::env::var(key) {
        Err(_) => Ok(default),
        Ok(v) if v.trim().is_empty() => Ok(default),
        Ok(v) => v.trim().parse().map_err(|_| format!("{key}: not a number: {v:?}")),
    }
}

/// A string-valued knob with no default: None when unset or blank.
pub fn str_env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// [`num_env`] for a string-valued knob.
fn str_env(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

/// `SPIRA_ROUND_VM_CACHE_HOME` is declared TOML-only (`spira/conf.toml.d`), not in
/// `spira/conf.d` — `cfg`/`cfg_parse` never resolve it (their `values` map comes from
/// `spira/conf.d` alone), so it is read here exactly as before this migration: the
/// environment first, then `spira.toml`'s `[spira]` table, never a bare ambient `CARGO_HOME`.
fn cache_home_from_env_or_toml() -> Option<String> {
    if let Ok(v) = std::env::var("SPIRA_ROUND_VM_CACHE_HOME") {
        if !v.trim().is_empty() {
            return Some(v);
        }
    }
    spira_config::discover(None)
        .and_then(|p| spira_config::load(&p).ok())
        .and_then(|doc| doc.spira)
        .and_then(|s| s.round_vm_cache_home)
        .filter(|v| !v.trim().is_empty())
}

impl Config {
    /// The process's top-level config read (DESIGN.md §3.1). Every `spira/conf.d`-registered
    /// key goes through `cfg`/`cfg_parse` (`must_cfg`/`must_cfg_parse`): no Rust-side default,
    /// so a key spira.toml does not declare resolves to whatever `spira/conf.d` says (often
    /// empty — see this crate's migration notes) rather than one of round-vm's old literals.
    /// Everything else here is one of round-vm's own knobs, not declared in `spira/conf.d`;
    /// those keep reading the environment with their existing defaults.
    pub fn load() -> Result<Config, String> {
        let host_addr = match spira_config::hostaddr::resolve(&must_cfg("SPIRA_ROUND_VM_HOST_ADDR")?, spira_config::hostaddr::PROBE) {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("round-vm: this host's address does not resolve: {e}");
                None
            }
        };
        let testenv_registry = Some(must_cfg("SPIRA_TESTENV_REGISTRY")?).filter(|v| !v.is_empty());
        let vcpus: u32 = must_cfg_parse("SPIRA_ROUND_VM_VCPUS")?;
        let maxpar = maxpar_or_vcpus(&must_cfg("SPIRA_ROUND_VM_MAXPAR")?, vcpus)?;
        Ok(Config::build(Fields {
            run: must_cfg("SPIRA_RUN")?,
            state_dir: must_cfg("SPIRA_ROUND_VM_STATE_DIR")?,
            pve_env: must_cfg("SPIRA_PVE_ENV")?,
            ssh_user: must_cfg("SPIRA_ROUND_VM_SSH_USER")?,
            ssh_port: must_cfg_parse("SPIRA_ROUND_VM_SSH_PORT")?,
            host_key: must_cfg("SPIRA_ROUND_VM_HOST_KEY")?,
            host_pubkey: must_cfg("SPIRA_ROUND_VM_HOST_PUBKEY")?,
            host_addr,
            testenv_registry,
            vcpus,
            maxpar,
            max_retries: must_cfg_parse("SPIRA_ROUND_VM_MAX_RETRIES")?,
            retry_interval_secs: must_cfg_parse("SPIRA_ROUND_VM_RETRY_INTERVAL")?,
            mirror_port: must_cfg_parse("SPIRA_ROUND_VM_MIRROR_PORT")?,

            // Not declared in spira/conf.d — round-vm's own per-invocation knobs, read
            // straight from the environment; defaults unchanged by this migration.
            spira_home: std::env::var("SPIRA_HOME").ok().filter(|v| !v.trim().is_empty()),
            cache_home: cache_home_from_env_or_toml(),
            setup_alarm_secs: num_env("SPIRA_ROUND_VM_SETUP_ALARM_SECS", 180)?,
            acquire_deadline_secs: num_env("SPIRA_ROUND_VM_ACQUIRE_DEADLINE", 3600)?,
            mailbox: str_env("SPIRA_ROUND_VM_MAIL_MAILBOX", "operator"),
            net_iface: str_env("SPIRA_ROUND_VM_NET_IFACE", "ens18"),
            boot_tries: num_env("SPIRA_ROUND_VM_BOOT_TRIES", 60)?,
            boot_poll_secs: num_env("SPIRA_ROUND_VM_BOOT_POLL", 2)?,
            ssh_tries: num_env("SPIRA_ROUND_VM_SSH_TRIES", 30)?,
            stream_every_secs: num_env("SPIRA_ROUND_VM_STREAM_SECS", 10)?,
            attr_linger_secs: num_env("SPIRA_ROUND_VM_ATTR_LINGER", 3600)?,
            cap_secs: must_cfg_parse("SPIRA_ROUND_CERTIFY_WALL_SECS")?,
            vm_budget_secs: num_env("SPIRA_ROUND_VM_PHASE_VM_SECS", 300)?,
            build_budget_secs: num_env("SPIRA_ROUND_VM_PHASE_BUILD_SECS", 600)?,
        }))
    }

    /// Pure: every parse that can fail already happened in [`Config::load`] (or in a test's
    /// literal `Fields`), so this only converts types — no I/O, no defaults to invent.
    pub(crate) fn build(f: Fields) -> Config {
        Config {
            run_dir: PathBuf::from(f.run),
            state_dir: PathBuf::from(f.state_dir),
            spira_home: f.spira_home.map(PathBuf::from),
            pve_env_path: PathBuf::from(f.pve_env),
            ssh_user: f.ssh_user,
            ssh_port: f.ssh_port,
            host_key: PathBuf::from(f.host_key),
            host_pubkey: PathBuf::from(f.host_pubkey),
            host_addr: f.host_addr,
            cache_home: f.cache_home,
            testenv_registry: f.testenv_registry,
            setup_alarm_secs: f.setup_alarm_secs,
            vcpus: f.vcpus,
            maxpar: f.maxpar,
            retry_interval: Duration::from_secs(f.retry_interval_secs),
            max_retries: f.max_retries,
            acquire_deadline: Duration::from_secs(f.acquire_deadline_secs),
            mirror_port: f.mirror_port,
            mailbox: f.mailbox,
            net_iface: f.net_iface,
            boot_tries: f.boot_tries,
            boot_poll: Duration::from_secs(f.boot_poll_secs),
            ssh_tries: f.ssh_tries,
            wait_poll: Duration::from_secs(1),
            stream_every: Duration::from_secs(f.stream_every_secs),
            attr_linger: Duration::from_secs(f.attr_linger_secs),
            cap_secs: f.cap_secs,
            vm_budget_secs: f.vm_budget_secs,
            build_budget_secs: f.build_budget_secs,
        }
    }
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
    use std::sync::Mutex;

    /// Serializes tests below that mutate the real process environment (there is no
    /// crate-wide env-mutation lock elsewhere in round-vm to reuse — none of its other tests
    /// touch real env vars, since they already passed values straight to pure logic).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn base_fields() -> Fields {
        Fields {
            run: "/r".into(),
            spira_home: None,
            pve_env: "/r/pve.env".into(),
            ssh_user: "root".into(),
            ssh_port: 22,
            state_dir: "/r/round-vm".into(),
            host_key: "/r/round-vm/host_key".into(),
            host_pubkey: "/r/round-vm/host_key.pub".into(),
            host_addr: None,
            cache_home: None,
            testenv_registry: None,
            vcpus: 16,
            maxpar: 16,
            max_retries: 0,
            retry_interval_secs: 60,
            mirror_port: 9430,
            setup_alarm_secs: 180,
            acquire_deadline_secs: 3600,
            mailbox: "operator".into(),
            net_iface: "ens18".into(),
            boot_tries: 60,
            boot_poll_secs: 2,
            ssh_tries: 30,
            stream_every_secs: 10,
            attr_linger_secs: 3600,
            cap_secs: 900,
            vm_budget_secs: 300,
            build_budget_secs: 600,
        }
    }

    #[test]
    fn maxpar_defaults_to_the_vcpu_count() {
        assert_eq!(maxpar_or_vcpus("", 32).unwrap(), 32);
        assert_eq!(maxpar_or_vcpus(" ", 32).unwrap(), 32);
        assert_eq!(maxpar_or_vcpus("8", 32).unwrap(), 8);
        assert!(maxpar_or_vcpus("x", 32).is_err());
    }

    #[test]
    fn build_converts_every_field_without_inventing_a_value() {
        let c = Config::build(base_fields());
        assert_eq!(c.run_dir, PathBuf::from("/r"));
        assert_eq!(c.state_dir, PathBuf::from("/r/round-vm"));
        assert_eq!(c.host_key, PathBuf::from("/r/round-vm/host_key"));
        assert_eq!(c.host_pubkey, PathBuf::from("/r/round-vm/host_key.pub"));
        assert_eq!(c.ssh_user, "root");
        assert_eq!((c.vcpus, c.maxpar, c.mirror_port, c.max_retries), (16, 16, 9430, 0));
        assert_eq!(c.retry_interval, Duration::from_secs(60));
        assert_eq!(c.mailbox, "operator");
        assert!(c.host_addr.is_none());
        assert!(c.testenv_registry.is_none());
        assert!(c.cache_home.is_none());
    }

    #[test]
    fn testenv_registry_passes_through_unchanged() {
        let mut f = base_fields();
        f.testenv_registry = Some("registry.example/spira".into());
        let c = Config::build(f);
        assert_eq!(c.testenv_registry.as_deref(), Some("registry.example/spira"));
    }

    #[test]
    fn cache_home_reads_its_own_dedicated_key_never_the_ambient_cargo_home() {
        // sp-xjnzl-2: a bare CARGO_HOME in the caller's environment must NOT leak in — the
        // production fault this bead fixes was exactly that leak (a systemd unit's own
        // ambient default, /root/.cargo, silently used in place of the template's actual
        // one). Only this binary's own key, env or spira.toml, ever sets cache_home.
        let got = {
            let _env = testkit::env(&[("SPIRA_TOML", None), ("SPIRA_ROUND_VM_CACHE_HOME", None), ("CARGO_HOME", Some("/opt/spira/cargo"))]);
            cache_home_from_env_or_toml()
        };
        assert!(got.is_none(), "a bare CARGO_HOME must never be read");

        let got = {
            let _env = testkit::env(&[("SPIRA_ROUND_VM_CACHE_HOME", Some("/opt/spira/cargo"))]);
            cache_home_from_env_or_toml()
        };
        assert_eq!(got.as_deref(), Some("/opt/spira/cargo"));
    }

    #[test]
    fn a_bad_number_on_an_unregistered_knob_names_the_key() {
        // SPIRA_ROUND_VM_BOOT_TRIES is not declared in spira/conf.d — round-vm still reads
        // it, and still refuses by name, straight from the environment (see this module's
        // migration notes); unlike a spira/conf.d key, there is no spira.toml fallback.
        let e = {
            let _env = testkit::env(&[("SPIRA_ROUND_VM_BOOT_TRIES", Some("lots"))]);
            num_env::<u32>("SPIRA_ROUND_VM_BOOT_TRIES", 60).unwrap_err()
        };
        assert!(e.contains("SPIRA_ROUND_VM_BOOT_TRIES"), "{e}");
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
