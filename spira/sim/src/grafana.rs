//! A pilgrim world's own read-only Grafana sign-in: a Viewer service account and one token,
//! written under the world's config and revoked when the world comes down.

use std::path::Path;
use std::process::{Command, Stdio};

pub const URL_ENV: &str = "PILGRIM_GRAFANA_URL";
pub const TOKEN_FILE: &str = "config/grafana-token";
const ACCOUNT_FILE: &str = "config/grafana-account";

pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// One Grafana API call. `admin` is the `user:password` the call is made as, if any.
pub trait Http {
    fn call(&self, method: &str, url: &str, admin: Option<&str>, bearer: Option<&str>, body: Option<&str>) -> Result<Reply, String>;
}

/// `curl`, with the credential on stdin as a config so it never reaches an argv.
pub struct Curl;
impl Http for Curl {
    fn call(&self, method: &str, url: &str, admin: Option<&str>, bearer: Option<&str>, body: Option<&str>) -> Result<Reply, String> {
        use std::io::Write;
        let mut cfg = String::new();
        if let Some(a) = admin {
            cfg += &format!("user = \"{a}\"\n");
        }
        if let Some(t) = bearer {
            cfg += &format!("header = \"Authorization: Bearer {t}\"\n");
        }
        let mut cmd = Command::new("timeout");
        cmd.args(["5", "curl", "-sS", "--max-time", "4", "-K", "-", "-X", method, "-w", "\n%{http_code}", url]);
        if let Some(b) = body {
            cmd.args(["-H", "Content-Type: application/json", "--data-binary", b]);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("curl: {e}"))?;
        child.stdin.take().ok_or("curl: no stdin")?.write_all(cfg.as_bytes()).map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("curl {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (body, code) = text.rsplit_once('\n').ok_or("curl: no status line")?;
        Ok(Reply { status: code.trim().parse().map_err(|_| format!("curl: bad status {code:?}"))?, body: body.to_string() })
    }
}

fn json(r: &Reply, what: &str, field: &str) -> Result<serde_json::Value, String> {
    if !(200..300).contains(&r.status) {
        return Err(format!("grafana {what}: HTTP {}", r.status));
    }
    serde_json::from_str::<serde_json::Value>(&r.body)
        .ok()
        .and_then(|v| v.get(field).cloned())
        .ok_or_else(|| format!("grafana {what}: no {field} in the answer"))
}

/// Creates the world's Viewer account and token. The token is written readable by the owner
/// only, and the account's id is kept for [`revoke`]; the world never holds the admin's sign-in.
pub fn provision(http: &dyn Http, world: &Path, base: &str, admin: &str) -> Result<(), String> {
    let name = format!("pilgrim-{}", world.file_name().and_then(|n| n.to_str()).unwrap_or("world"));
    let made = http.call("POST", &format!("{base}/api/serviceaccounts"), Some(admin), None, Some(&serde_json::json!({"name": name, "role": "Viewer"}).to_string()))?;
    let id = json(&made, "create account", "id")?.as_u64().ok_or("grafana create account: id is not a number")?;
    std::fs::write(world.join(ACCOUNT_FILE), id.to_string()).map_err(|e| e.to_string())?;
    let minted = http.call("POST", &format!("{base}/api/serviceaccounts/{id}/tokens"), Some(admin), None, Some(&serde_json::json!({"name": "session"}).to_string()))?;
    let key = json(&minted, "create token", "key")?;
    let key = key.as_str().ok_or("grafana create token: key is not a string")?;
    write_private(&world.join(TOKEN_FILE), key)
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// Deletes the account [`provision`] made, which ends its token. A world that never provisioned
/// one has nothing to revoke.
pub fn revoke(http: &dyn Http, world: &Path, base: &str, admin: &str) -> Result<(), String> {
    let Ok(id) = std::fs::read_to_string(world.join(ACCOUNT_FILE)) else { return Ok(()) };
    let r = http.call("DELETE", &format!("{base}/api/serviceaccounts/{}", id.trim()), Some(admin), None, None)?;
    if (200..300).contains(&r.status) || r.status == 404 {
        let _ = std::fs::remove_file(world.join(ACCOUNT_FILE));
        let _ = std::fs::remove_file(world.join(TOKEN_FILE));
        return Ok(());
    }
    Err(format!("grafana delete account {}: HTTP {}", id.trim(), r.status))
}

/// Provisions the world's sign-in and names it to the session in `sim.env`. A failure takes back
/// what was made, so no account outlives a world that never came up.
pub fn up_world(http: &dyn Http, world: &Path, base: &str, admin: &str) -> Result<(), String> {
    let made = provision(http, world, base, admin).and_then(|()| {
        use std::io::Write;
        let mut env = std::fs::OpenOptions::new().append(true).open(world.join("config/sim.env")).map_err(|e| e.to_string())?;
        writeln!(env, "{URL_ENV}={base}").map_err(|e| e.to_string())
    });
    made.inspect_err(|_| {
        let _ = revoke(http, world, base, admin);
    })
}

/// Where Grafana is and how to sign in as its admin, from the install's config; `None` when the
/// install runs no Grafana (an unset address or data directory).
pub fn install(addr: &str, data: &str) -> Result<Option<(String, String)>, String> {
    if addr.is_empty() || data.is_empty() {
        return Ok(None);
    }
    let path = Path::new(data).join("grafana.credential");
    let pw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some((format!("http://{addr}"), format!("admin:{}", pw.trim()))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Grafana's rule that matters here: a token answers only while its account exists.
    #[derive(Default)]
    struct FakeGrafana {
        accounts: RefCell<BTreeMap<u64, String>>,
        next: RefCell<u64>,
    }
    impl Http for FakeGrafana {
        fn call(&self, method: &str, url: &str, admin: Option<&str>, bearer: Option<&str>, _: Option<&str>) -> Result<Reply, String> {
            let path = url.strip_prefix("http://g").unwrap();
            let reply = |status, body: &str| Ok(Reply { status, body: body.to_string() });
            match (method, path) {
                ("POST", "/api/serviceaccounts") if admin == Some("admin:pw") => { // not a path: an API route
                    *self.next.borrow_mut() += 1;
                    let id = *self.next.borrow();
                    self.accounts.borrow_mut().insert(id, format!("tok{id}"));
                    reply(201, &format!("{{\"id\":{id}}}"))
                }
                ("POST", p) if p.ends_with("/tokens") && admin == Some("admin:pw") => {
                    let id: u64 = p.split('/').nth(3).unwrap().parse().unwrap();
                    reply(200, &format!("{{\"key\":\"{}\"}}", self.accounts.borrow()[&id]))
                }
                ("DELETE", p) if admin == Some("admin:pw") => {
                    let id: u64 = p.rsplit('/').next().unwrap().parse().unwrap();
                    reply(if self.accounts.borrow_mut().remove(&id).is_some() { 200 } else { 404 }, "{}")
                }
                ("GET", "/api/dashboards/uid/spira-bead-store") => { // not a path: an API route
                    let ok = bearer.is_some_and(|t| self.accounts.borrow().values().any(|v| v == t));
                    reply(if ok { 200 } else { 401 }, "{}")
                }
                _ => reply(403, "{}"),
            }
        }
    }

    fn dashboard(g: &FakeGrafana, token: Option<&str>) -> u16 {
        g.call("GET", "http://g/api/dashboards/uid/spira-bead-store", None, token, None).unwrap().status
    }

    #[test]
    fn a_pilgrims_own_token_reads_the_dashboard_until_the_world_comes_down() {
        let t = testkit::TempDir::new("pilgrim-grafana");
        std::fs::create_dir_all(t.join("w/config")).unwrap();
        let g = FakeGrafana::default();
        assert_eq!(dashboard(&g, None), 401, "positive control: no sign-in is refused");
        provision(&g, &t.join("w"), "http://g", "admin:pw").unwrap();
        let token = std::fs::read_to_string(t.join("w").join(TOKEN_FILE)).unwrap();
        assert_eq!(dashboard(&g, Some(&token)), 200);
        revoke(&g, &t.join("w"), "http://g", "admin:pw").unwrap();
        assert_eq!(dashboard(&g, Some(&token)), 401);
        assert!(!t.join("w").join(TOKEN_FILE).exists());
    }

    #[test]
    fn a_world_that_comes_up_names_the_grafana_url_and_a_failed_one_leaves_no_account() {
        let t = testkit::TempDir::new("pilgrim-grafana-up");
        std::fs::create_dir_all(t.join("w/config")).unwrap();
        std::fs::write(t.join("w/config/sim.env"), "A=1\n").unwrap();
        let g = FakeGrafana::default();
        up_world(&g, &t.join("w"), "http://g", "admin:pw").unwrap();
        let env = std::fs::read_to_string(t.join("w/config/sim.env")).unwrap();
        assert!(env.contains("PILGRIM_GRAFANA_URL=http://g\n"), "{env}");
        let bad = FakeGrafana::default();
        std::fs::remove_file(t.join("w/config/sim.env")).unwrap();
        assert!(up_world(&bad, &t.join("w"), "http://g", "admin:pw").is_err());
        assert!(bad.accounts.borrow().is_empty(), "the account made before the failure is deleted");
    }

    #[test]
    fn a_revoke_that_fails_keeps_the_account_id_for_a_retry() {
        let t = testkit::TempDir::new("pilgrim-grafana-retry");
        std::fs::create_dir_all(t.join("w/config")).unwrap();
        let g = FakeGrafana::default();
        provision(&g, &t.join("w"), "http://g", "admin:pw").unwrap();
        assert!(revoke(&g, &t.join("w"), "http://g", "admin:wrong").is_err());
        assert!(t.join("w").join(ACCOUNT_FILE).exists());
    }

    #[test]
    fn the_token_file_is_readable_by_the_session_alone() {
        use std::os::unix::fs::PermissionsExt;
        let t = testkit::TempDir::new("pilgrim-grafana-mode");
        std::fs::create_dir_all(t.join("w/config")).unwrap();
        provision(&FakeGrafana::default(), &t.join("w"), "http://g", "admin:pw").unwrap();
        let mode = std::fs::metadata(t.join("w").join(TOKEN_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_wrong_admin_sign_in_provisions_nothing() {
        let t = testkit::TempDir::new("pilgrim-grafana-bad");
        std::fs::create_dir_all(t.join("w/config")).unwrap();
        assert!(provision(&FakeGrafana::default(), &t.join("w"), "http://g", "admin:wrong").is_err());
        assert!(!t.join("w").join(TOKEN_FILE).exists());
    }

    #[test]
    fn an_install_without_grafana_is_none_and_one_with_it_names_the_admin() {
        assert_eq!(install("", "data").unwrap(), None);
        let t = testkit::TempDir::new("pilgrim-grafana-install");
        std::fs::write(t.join("grafana.credential"), "s3\n").unwrap();
        let got = install("1.2.3.4:3000", &t.path().display().to_string()).unwrap().unwrap();
        assert_eq!(got, ("http://1.2.3.4:3000".to_string(), "admin:s3".to_string()));
    }
}
