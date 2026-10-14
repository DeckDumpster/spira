//! Provisioning for spira-submission: the tailnet address, a renewed tailscale certificate, the
//! password file derived from the IMAP users file, and the firewall rule. Every effect on the
//! host goes through [`Host`], so the decisions here are testable without tailscale or root.

use crate::clock;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const RENEW_DAYS: i64 = 30;
pub const WARN_DAYS: i64 = 21;
pub const UNIT: &str = "spira-submission.service";

pub trait Host {
    fn run(&self, argv: &[&str]) -> Result<String, String>;
    fn now(&self) -> i64;
}

pub struct RealHost;

impl Host for RealHost {
    fn run(&self, argv: &[&str]) -> Result<String, String> {
        let (prog, args) = argv.split_first().ok_or("empty command")?;
        let out = Command::new(prog).args(args).stdin(Stdio::null()).output().map_err(|e| format!("{prog}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("{} exited {}: {}", argv.join(" "), out.status.code().map_or("signal".into(), |c| c.to_string()), err.trim()))
        }
    }

    fn now(&self) -> i64 {
        clock::now()
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub toml_spec: String,
    pub addr: String,
    pub cert: String,
    pub key: String,
    pub password_file: String,
    pub from: String,
    pub user: String,
    pub port: u16,
    pub rcpt_domain: String,
    pub users_file: String,
    pub dir: PathBuf,
    pub ufw_conf: PathBuf,
}

impl Plan {
    pub fn from_spira(dir: PathBuf, ufw_conf: PathBuf) -> Result<Plan, String> {
        let get = spira_config::process::cfg;
        let port: u16 = get("SPIRA_SUBMISSION_PORT")?.trim().parse().map_err(|e| format!("SPIRA_SUBMISSION_PORT does not parse: {e}"))?;
        let s = |k: &str| get(k).map(|v| v.trim().to_string());
        Ok(Plan {
            toml_spec: spira_config::process::spec()?,
            addr: s("SPIRA_SUBMISSION_ADDR")?,
            cert: s("SPIRA_SUBMISSION_CERT")?,
            key: s("SPIRA_SUBMISSION_KEY")?,
            password_file: s("SPIRA_SUBMISSION_PASSWORD_FILE")?,
            from: s("SPIRA_SUBMISSION_FROM")?,
            user: s("SPIRA_SUBMISSION_USER")?,
            port,
            rcpt_domain: s("SPIRA_SUBMISSION_RCPT_DOMAIN")?,
            users_file: s("SPIRA_SUBMISSION_USERS_FILE")?,
            dir,
            ufw_conf,
        })
    }
}

pub fn tailnet_ip(out: &str) -> Option<String> {
    let l = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    l.parse::<std::net::Ipv4Addr>().ok().map(|_| l.to_string())
}

pub fn dns_name(status_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(status_json).ok()?;
    let n = v.get("Self")?.get("DNSName")?.as_str()?.trim_end_matches('.');
    (!n.is_empty()).then(|| n.to_string())
}

/// The interface owning `ip` in `ip -o -4 addr show` output.
pub fn iface_for(ip: &str, ip_output: &str) -> Option<String> {
    ip_output.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        let _index = w.next()?;
        let iface = w.next()?;
        let mut rest = w;
        while let Some(t) = rest.next() {
            if t == "inet" {
                let cidr = rest.next()?;
                return (cidr.split('/').next() == Some(ip)).then(|| iface.trim_end_matches(':').to_string());
            }
        }
        None
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Firewall {
    /// No host firewall is active, so nothing can be closed.
    Absent,
    Open,
    Closed,
    Unknown(String),
}

impl Firewall {
    pub fn word(&self) -> &'static str {
        match self {
            Firewall::Absent => "none",
            Firewall::Open => "open",
            Firewall::Closed => "closed",
            Firewall::Unknown(_) => "unknown",
        }
    }
}

pub fn ufw_allows(status: &str, port: u16, iface: &str) -> bool {
    status.lines().any(|l| {
        let t: Vec<&str> = l.split_whitespace().collect();
        let Some(first) = t.first() else { return false };
        let on_port = *first == format!("{port}/tcp") || *first == port.to_string();
        let on_iface = match t.iter().position(|w| *w == "on") {
            Some(i) => t.get(i + 1) == Some(&iface),
            None => true,
        };
        on_port && on_iface && t.contains(&"ALLOW")
    })
}

pub fn allow_command(iface: &str, port: u16) -> Vec<String> {
    ["sudo", "-n", "ufw", "allow", "in", "on", iface, "to", "any", "port", &port.to_string(), "proto", "tcp"].iter().map(|s| s.to_string()).collect()
}

pub fn firewall_state(host: &dyn Host, plan: &Plan, iface: &str) -> Firewall {
    let enabled = std::fs::read_to_string(&plan.ufw_conf).is_ok_and(|t| t.lines().any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes")));
    if !enabled {
        return Firewall::Absent;
    }
    match host.run(&["sudo", "-n", "ufw", "status"]) {
        Ok(s) if ufw_allows(&s, plan.port, iface) => Firewall::Open,
        Ok(_) => Firewall::Closed,
        Err(e) => Firewall::Unknown(e),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Password {
    Ok,
    Missing,
    /// Group or other bits set: the server refuses the file.
    Mode,
    Drifted,
    NoSource(String),
}

impl Password {
    pub fn word(&self) -> &'static str {
        match self {
            Password::Ok => "ok",
            Password::Missing => "missing",
            Password::Mode => "mode",
            Password::Drifted => "drifted",
            Password::NoSource(_) => "nosource",
        }
    }
}

/// `user:<hash>` from a dovecot-style users file: the same credential, minus the fields the
/// submission server has no use for.
pub fn derive_line(users: &str, user: &str) -> Option<String> {
    let prefix = format!("{user}:");
    users.lines().map(str::trim).find_map(|l| {
        let rest = l.strip_prefix(&prefix)?;
        let hash = rest.split(':').next().unwrap_or("");
        (!hash.is_empty()).then(|| format!("{user}:{hash}"))
    })
}

fn derived(plan: &Plan) -> Result<String, Password> {
    let text = std::fs::read_to_string(&plan.users_file).map_err(|e| Password::NoSource(format!("{}: {e}", plan.users_file)))?;
    derive_line(&text, &plan.user)
        .ok_or_else(|| Password::NoSource(format!("{} has no entry for {}", plan.users_file, plan.user)))
}

pub fn password_state(plan: &Plan, password_file: &str) -> Password {
    let want = match derived(plan) {
        Ok(l) => l,
        Err(p) => return p,
    };
    let path = Path::new(password_file);
    let Ok(meta) = std::fs::metadata(path) else { return Password::Missing };
    if meta.permissions().mode() & 0o077 != 0 {
        return Password::Mode;
    }
    match std::fs::read_to_string(path) {
        Ok(have) if have.trim() == want => Password::Ok,
        _ => Password::Drifted,
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let w = || -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    w().map_err(|e| format!("{}: {e}", path.display()))
}

pub fn cert_days(cert: &Path, now: i64) -> Option<i64> {
    let f = std::fs::File::open(cert).ok()?;
    let der = rustls_pemfile::certs(&mut std::io::BufReader::new(f)).next()?.ok()?;
    Some((clock::not_after(&der)? - now).div_euclid(86400))
}

#[derive(Debug, Default)]
pub struct Report {
    pub notes: Vec<String>,
    pub errors: Vec<String>,
}

pub fn provision(host: &dyn Host, plan: &Plan) -> Report {
    let mut r = Report::default();
    let mut restart = false;
    let layer = plan.toml_spec.rsplit(':').next().unwrap_or("").to_string();
    let mut eff = plan.clone();

    let ip = host.run(&["tailscale", "ip", "-4"]).ok().and_then(|o| tailnet_ip(&o));
    let name = host.run(&["tailscale", "status", "--json"]).ok().and_then(|o| dns_name(&o));
    if ip.is_none() {
        r.notes.push("tailscale is not up: no tailnet address, certificate or firewall rule to provision".into());
    }

    let mut set = |r: &mut Report, key: &str, value: &str, restart: &mut bool| {
        match host.run(&["spira-config", "set", &format!("spira.{key}"), value, &layer]) {
            Ok(_) => {
                r.notes.push(format!("set spira.{key} = {value}"));
                *restart = true;
            }
            Err(e) => r.errors.push(format!("cannot set spira.{key}: {e}")),
        }
    };
    if let Some(ip) = &ip {
        if eff.addr.is_empty() {
            set(&mut r, "submission_addr", ip, &mut restart);
            eff.addr = ip.clone();
        }
    }
    if !eff.addr.is_empty() {
        let d = plan.dir.display();
        for (key, cur, default) in [
            ("submission_cert", &mut eff.cert, format!("{d}/cert.pem")),
            ("submission_key", &mut eff.key, format!("{d}/key.pem")),
            ("submission_password_file", &mut eff.password_file, format!("{d}/password")),
            ("submission_from", &mut eff.from, format!("{}@{}", plan.user, plan.rcpt_domain)),
        ] {
            if cur.is_empty() {
                set(&mut r, key, &default, &mut restart);
                *cur = default;
            }
        }
        if let Err(e) = std::fs::create_dir_all(&plan.dir).and_then(|_| std::fs::set_permissions(&plan.dir, std::fs::Permissions::from_mode(0o700))) {
            r.errors.push(format!("{}: {e}", plan.dir.display()));
        }
    }

    if let (Some(name), false) = (&name, eff.cert.is_empty() || eff.key.is_empty()) {
        let now = host.now();
        let days = cert_days(Path::new(&eff.cert), now);
        if days.is_none_or(|d| d < RENEW_DAYS) {
            let before = (std::fs::read(&eff.cert).ok(), std::fs::read(&eff.key).ok());
            match host.run(&["tailscale", "cert", "--cert-file", &eff.cert, "--key-file", &eff.key, name]) {
                Err(e) => r.errors.push(format!("tailscale cert {name}: {e}")),
                Ok(_) => {
                    let _ = std::fs::set_permissions(&eff.key, std::fs::Permissions::from_mode(0o600));
                    let after = (std::fs::read(&eff.cert).ok(), std::fs::read(&eff.key).ok());
                    if after != before {
                        r.notes.push(format!("renewed the certificate for {name}"));
                        restart = true;
                    } else {
                        r.notes.push(format!("tailscale cert left the certificate unchanged ({} days left)", cert_days(Path::new(&eff.cert), now).map_or("?".into(), |d| d.to_string())));
                    }
                }
            }
        } else {
            r.notes.push(format!("certificate for {name} has {} days left", days.unwrap_or(0)));
        }
    }

    if !plan.users_file.is_empty() && !eff.password_file.is_empty() {
        match password_state(&eff, &eff.password_file) {
            Password::Ok => {}
            Password::NoSource(e) => r.errors.push(format!("password source unusable: {e}")),
            _ => match derived(&eff) {
                Ok(line) => match write_private(Path::new(&eff.password_file), format!("{line}\n").as_bytes()) {
                    Ok(()) => r.notes.push(format!("wrote {} from {}", eff.password_file, plan.users_file)),
                    Err(e) => r.errors.push(e),
                },
                Err(Password::NoSource(e)) => r.errors.push(format!("password source unusable: {e}")),
                Err(_) => {}
            },
        }
    }

    if let (Some(ip), false) = (&ip, eff.addr.is_empty()) {
        match host.run(&["ip", "-o", "-4", "addr", "show"]).ok().and_then(|o| iface_for(ip, &o)) {
            None => r.errors.push(format!("no interface owns {ip}: cannot place the firewall rule")),
            Some(iface) => match firewall_state(host, &eff, &iface) {
                Firewall::Closed => {
                    let cmd = allow_command(&iface, eff.port);
                    let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
                    match host.run(&argv) {
                        Ok(_) => r.notes.push(format!("opened {}/tcp on {iface}", eff.port)),
                        Err(e) => r.errors.push(format!("cannot open {}/tcp on {iface} ({e}); as root: {}", eff.port, cmd[2..].join(" "))),
                    }
                }
                Firewall::Unknown(e) => r.errors.push(format!("cannot read the firewall ({e}); as root: {}", allow_command(&iface, eff.port)[2..].join(" "))),
                Firewall::Open | Firewall::Absent => {}
            },
        }
    }

    if restart {
        match host.run(&["systemctl", "--user", "restart", UNIT]) {
            Ok(_) => r.notes.push(format!("restarted {UNIT}")),
            Err(e) => r.errors.push(format!("cannot restart {UNIT}: {e}")),
        }
    }
    r
}

/// What `doctor` reads: one `key=value` per line.
pub fn status(host: &dyn Host, plan: &Plan) -> String {
    let mut out = Vec::new();
    let configured = !plan.addr.is_empty();
    let ip = host.run(&["tailscale", "ip", "-4"]).ok().and_then(|o| tailnet_ip(&o));
    out.push(format!("configured={}", u8::from(configured)));
    out.push(format!("tailnet={}", u8::from(ip.is_some())));
    if !configured {
        return out.join("\n") + "\n";
    }
    let listening = plan.addr.parse().map(|a| crate::health(std::net::SocketAddr::new(a, plan.port)).is_ok()).unwrap_or(false);
    out.push(format!("listening={}", u8::from(listening)));
    out.push(format!("addr={}:{}", plan.addr, plan.port));
    let iface = host.run(&["ip", "-o", "-4", "addr", "show"]).ok().and_then(|o| iface_for(&plan.addr, &o));
    out.push(format!("iface={}", iface.clone().unwrap_or_default()));
    let fw = match &iface {
        Some(i) => firewall_state(host, plan, i),
        None => Firewall::Unknown("no interface owns the address".into()),
    };
    out.push(format!("firewall={}", fw.word()));
    out.push(format!("cert_days={}", cert_days(Path::new(&plan.cert), host.now()).map_or("none".into(), |d| d.to_string())));
    let pw = if plan.users_file.is_empty() { None } else { Some(password_state(plan, &plan.password_file)) };
    out.push(format!("password={}", pw.as_ref().map_or("n/a", Password::word)));
    out.join("\n") + "\n"
}
