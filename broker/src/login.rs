use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::policy;

pub const MAX_TTL_SECS: u64 = 3600;

pub struct Request {
    pub fayth: String,
    pub bead: String,
    pub account: String,
    pub ttl_secs: u64,
    pub host: String,
    pub command: String,
}

pub fn run() -> Result<String, String> {
    let fayth = std::env::var("SPIRA_FAYTH").unwrap_or_default();
    let bead = std::env::var("SPIRA_WORK_BEAD_ID").unwrap_or_default();
    let log = log_path()?;
    let req = match request(&fayth, &bead) {
        Ok(r) => r,
        Err(e) => {
            append(&log, &line(&fayth, &bead, "", 0, "refused"));
            return Err(e);
        }
    };
    let out = mint(&req, ssh);
    append(&log, &line(&req.fayth, &req.bead, &req.account, req.ttl_secs, if out.is_ok() { "ok" } else { "failed" }));
    out
}

fn request(fayth: &str, bead: &str) -> Result<Request, String> {
    if !policy::allows_login_code(fayth) {
        return Err(format!("broker identity-login-code: fayth '{fayth}' is not allowed this verb"));
    }
    if bead.is_empty() {
        return Err("broker identity-login-code: SPIRA_WORK_BEAD_ID is unset; every use is logged by bead id".to_string());
    }
    let cfg = |k: &str| -> Result<String, String> {
        let v = spira_config::process::cfg(k).map_err(|e| format!("broker identity-login-code: {e}"))?;
        if v.trim().is_empty() {
            return Err(format!("broker identity-login-code: {k} is empty in the config file — refusing"));
        }
        Ok(v.trim().to_string())
    };
    let account = cfg("SPIRA_PILGRIM_ACCOUNT")?;
    if !account.chars().all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c)) {
        return Err("broker identity-login-code: SPIRA_PILGRIM_ACCOUNT holds characters outside [A-Za-z0-9@._+-]".to_string());
    }
    let ttl_raw = cfg("SPIRA_IDENTITY_LOGIN_TTL_SECS")?;
    let ttl_secs: u64 = ttl_raw
        .parse()
        .map_err(|_| format!("broker identity-login-code: SPIRA_IDENTITY_LOGIN_TTL_SECS = {ttl_raw:?} is not a number"))?;
    if ttl_secs == 0 || ttl_secs > MAX_TTL_SECS {
        return Err(format!("broker identity-login-code: SPIRA_IDENTITY_LOGIN_TTL_SECS must be 1..={MAX_TTL_SECS}, got {ttl_secs}"));
    }
    Ok(Request {
        fayth: fayth.to_string(),
        bead: bead.to_string(),
        account,
        ttl_secs,
        host: cfg("SPIRA_IDENTITY_LOGIN_HOST")?,
        command: cfg("SPIRA_IDENTITY_LOGIN_CMD")?,
    })
}

fn mint(req: &Request, exec: impl FnOnce(&Request) -> Result<String, String>) -> Result<String, String> {
    let raw = exec(req)?;
    let code = raw.trim();
    if code.is_empty() || code.contains(char::is_whitespace) || code.len() > 256 {
        return Err("broker identity-login-code: the product host returned no single-token code".to_string());
    }
    Ok(code.to_string())
}

fn ssh(req: &Request) -> Result<String, String> {
    let remote = format!("{} {} {}", req.command, req.account, req.ttl_secs);
    let out = Command::new("timeout")
        .args(["5", "ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=3", "--", &req.host, &remote])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("broker identity-login-code: cannot run ssh: {e}"))?;
    if !out.status.success() {
        return Err(format!("broker identity-login-code: the product host refused ({})", out.status));
    }
    String::from_utf8(out.stdout).map_err(|_| "broker identity-login-code: the product host's reply is not text".to_string())
}

fn line(fayth: &str, bead: &str, account: &str, ttl_secs: u64, outcome: &str) -> String {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{at} identity-login-code fayth={fayth} bead={bead} account={account} ttl={ttl_secs} outcome={outcome}")
}

fn log_path() -> Result<std::path::PathBuf, String> {
    let run = spira_config::process::cfg("SPIRA_RUN").map_err(|e| format!("broker identity-login-code: {e}"))?;
    if run.is_empty() {
        return Err("broker identity-login-code: SPIRA_RUN is empty — nowhere to log the use".to_string());
    }
    Ok(std::path::PathBuf::from(run).join("broker").join("identity-login-code.log"))
}

fn append(path: &std::path::Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{text}");
    }
    eprintln!("{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "Zq7-secret-code-9f";

    fn req() -> Request {
        Request {
            fayth: "pilgrim".into(),
            bead: "sp-abc".into(),
            account: "acct-x".into(),
            ttl_secs: 600,
            host: "prod.example.test".into(),
            command: "mint-code".into(),
        }
    }

    #[test]
    fn the_code_is_returned_and_the_log_line_carries_the_bead_but_never_the_code() {
        let r = req();
        let code = mint(&r, |_| Ok(format!("{CODE}\n"))).unwrap();
        assert_eq!(code, CODE);
        let l = line(&r.fayth, &r.bead, &r.account, r.ttl_secs, "ok");
        assert!(l.contains("bead=sp-abc") && l.contains("account=acct-x"));
        assert!(!l.contains(CODE));
    }

    #[test]
    fn the_remote_is_asked_for_the_pinned_account_and_ttl_only() {
        let r = req();
        let mut seen = String::new();
        mint(&r, |q| { seen = format!("{} {} {}", q.command, q.account, q.ttl_secs); Ok(CODE.into()) }).unwrap();
        assert_eq!(seen, "mint-code acct-x 600");
    }

    #[test]
    fn a_reply_that_is_not_one_token_is_refused_without_echoing_it() {
        for bad in ["", "two words", "a\nb"] {
            let e = mint(&req(), |_| Ok(bad.into())).unwrap_err();
            assert!(!e.contains("words"), "{e}");
        }
    }

    #[test]
    fn a_failed_host_surfaces_as_an_error() {
        assert!(mint(&req(), |_| Err("down".into())).is_err());
    }

    #[test]
    fn every_fayth_but_pilgrim_is_refused_before_config_is_read() {
        for f in ["builder", "groomer", "concierge", "czar", "ops", "mender", "batcher", "warden", "spike", "maechen", "archivist", "", "Pilgrim", "pilgrim "] {
            let e = request(f, "sp-abc").err().expect(f);
            assert!(e.contains("not allowed"), "{f}: {e}");
        }
    }

    #[test]
    fn pilgrim_without_a_bead_id_is_refused() {
        let e = request("pilgrim", "").err().unwrap();
        assert!(e.contains("SPIRA_WORK_BEAD_ID"), "{e}");
    }

    #[test]
    fn the_log_file_is_appended_per_use() {
        let dir = testkit::TempDir::new("broker-login-log");
        let p = dir.join("broker").join("identity-login-code.log");
        append(&p, &line("pilgrim", "sp-1", "a@b", 60, "ok"));
        append(&p, &line("builder", "sp-2", "", 0, "refused"));
        let got = std::fs::read_to_string(&p).unwrap();
        assert_eq!(got.lines().count(), 2);
        assert!(got.contains("bead=sp-1") && got.contains("bead=sp-2"));
    }
}
