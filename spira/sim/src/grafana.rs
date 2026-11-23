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

fn refusal(r: &Reply, what: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(&r.body).ok().and_then(|v| v.get("message")?.as_str().map(str::to_string)).unwrap_or_else(|| r.body.trim().to_string());
    format!("grafana {what}: HTTP {}: {message}", r.status)
}

fn json(r: &Reply, what: &str, field: &str) -> Result<serde_json::Value, String> {
    if !(200..300).contains(&r.status) {
        return Err(refusal(r, what));
    }
    serde_json::from_str::<serde_json::Value>(&r.body)
        .ok()
        .and_then(|v| v.get(field).cloned())
        .ok_or_else(|| format!("grafana {what}: no {field} in the answer"))
}

fn encode(text: &str) -> String {
    text.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn already_exists(r: &Reply) -> bool {
    r.status == 400 && serde_json::from_str::<serde_json::Value>(&r.body).ok().is_some_and(|v| v.get("messageId").and_then(|m| m.as_str()) == Some("serviceaccounts.ErrAlreadyExists"))
}

/// Deletes the account a dead world of this name left, found by exact name among the search hits.
fn delete_leftover(http: &dyn Http, base: &str, admin: &str, name: &str) -> Result<(), String> {
    let found = http.call("GET", &format!("{base}/api/serviceaccounts/search?query={}", encode(name)), Some(admin), None, None)?;
    let list = json(&found, "search accounts", "serviceAccounts")?;
    let id = list
        .as_array()
        .and_then(|l| l.iter().find(|a| a["name"].as_str() == Some(name)))
        .and_then(|a| a["id"].as_u64())
        .ok_or_else(|| format!("grafana create account: service account {name} already exists and was not found to replace"))?;
    let gone = http.call("DELETE", &format!("{base}/api/serviceaccounts/{id}"), Some(admin), None, None)?;
    if (200..300).contains(&gone.status) || gone.status == 404 {
        return Ok(());
    }
    Err(refusal(&gone, &format!("delete account {id}")))
}

/// Creates the world's Viewer account and token. The token is written readable by the owner
/// only, and the account's id is kept for [`revoke`]; the world never holds the admin's sign-in.
/// An account of the same name left by an earlier claim is replaced.
pub fn provision(http: &dyn Http, world: &Path, base: &str, admin: &str) -> Result<(), String> {
    let name = format!("pilgrim-{}", world.file_name().and_then(|n| n.to_str()).unwrap_or("world"));
    let create = || http.call("POST", &format!("{base}/api/serviceaccounts"), Some(admin), None, Some(&serde_json::json!({"name": name, "role": "Viewer"}).to_string()));
    let mut made = create()?;
    if already_exists(&made) {
        delete_leftover(http, base, admin, &name)?;
        made = create()?;
    }
    let id = json(&made, "create account", "id")?.as_u64().ok_or("grafana create account: id is not a number")?;
    std::fs::write(world.join(ACCOUNT_FILE), id.to_string()).map_err(|e| e.to_string())?;
    let minted = http.call("POST", &format!("{base}/api/serviceaccounts/{id}/tokens"), Some(admin), None, Some(&serde_json::json!({"name": format!("{name}-{id}")}).to_string()))?;
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
    Err(refusal(&r, &format!("delete account {}", id.trim())))
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

    /// Grafana's rules that matter here: a token answers only while its account exists, account
    /// names are unique, and token names are unique across the whole organisation.
    #[derive(Default)]
    struct FakeGrafana {
        accounts: RefCell<BTreeMap<u64, String>>,
        tokens: RefCell<Vec<(u64, String, String)>>,
        next: RefCell<u64>,
    }
    impl Http for FakeGrafana {
        fn call(&self, method: &str, url: &str, admin: Option<&str>, bearer: Option<&str>, body: Option<&str>) -> Result<Reply, String> {
            let path = url.strip_prefix("http://g").unwrap();
            let reply = |status, body: &str| Ok(Reply { status, body: body.to_string() });
            let named = || serde_json::from_str::<serde_json::Value>(body.unwrap()).unwrap()["name"].as_str().unwrap().to_string();
            match (method, path) {
                ("POST", "/api/serviceaccounts") if admin == Some("admin:pw") => { // not a path: an API route
                    let name = named();
                    if self.accounts.borrow().values().any(|n| *n == name) {
                        return reply(400, r#"{"messageId":"serviceaccounts.ErrAlreadyExists","message":"service account already exists"}"#);
                    }
                    *self.next.borrow_mut() += 1;
                    let id = *self.next.borrow();
                    self.accounts.borrow_mut().insert(id, name);
                    reply(201, &format!("{{\"id\":{id}}}"))
                }
                ("GET", p) if p.starts_with("/api/serviceaccounts/search?query=") && admin == Some("admin:pw") => { // not a path: an API route
                    let q = &p["/api/serviceaccounts/search?query=".len()..]; // not a path: an API route
                    let hits: Vec<_> = self.accounts.borrow().iter().filter(|(_, n)| n.contains(q)).map(|(id, n)| serde_json::json!({"id": id, "name": n})).collect();
                    reply(200, &serde_json::json!({"serviceAccounts": hits}).to_string())
                }
                ("POST", p) if p.ends_with("/tokens") && admin == Some("admin:pw") => {
                    let id: u64 = p.split('/').nth(3).unwrap().parse().unwrap();
                    let name = named();
                    if self.tokens.borrow().iter().any(|(_, n, _)| *n == name) {
                        return reply(400, r#"{"messageId":"serviceaccounts.ErrTokenAlreadyExists","message":"service account token with given name already exists in the organization"}"#);
                    }
                    *self.next.borrow_mut() += 1;
                    let key = format!("key{}", self.next.borrow());
                    self.tokens.borrow_mut().push((id, name, key.clone()));
                    reply(200, &format!("{{\"key\":\"{key}\"}}"))
                }
                ("DELETE", p) if admin == Some("admin:pw") => {
                    let id: u64 = p.rsplit('/').next().unwrap().parse().unwrap();
                    self.tokens.borrow_mut().retain(|(a, _, _)| *a != id);
                    reply(if self.accounts.borrow_mut().remove(&id).is_some() { 200 } else { 404 }, "{}")
                }
                ("GET", "/api/dashboards/uid/spira-bead-store") => { // not a path: an API route
                    let ok = bearer.is_some_and(|t| self.tokens.borrow().iter().any(|(a, _, k)| k == t && self.accounts.borrow().contains_key(a)));
                    reply(if ok { 200 } else { 401 }, "{}")
                }
                _ => reply(403, "{}"),
            }
        }
    }

    /// The fake, except that one call is answered with a fixed refusal.
    struct Refusing<'a> {
        inner: &'a FakeGrafana,
        method: &'static str,
        suffix: &'static str,
        answer: (u16, &'static str),
    }
    impl Http for Refusing<'_> {
        fn call(&self, method: &str, url: &str, admin: Option<&str>, bearer: Option<&str>, body: Option<&str>) -> Result<Reply, String> {
            if method == self.method && url.ends_with(self.suffix) {
                return Ok(Reply { status: self.answer.0, body: self.answer.1.to_string() });
            }
            self.inner.call(method, url, admin, bearer, body)
        }
    }

    fn world(t: &testkit::TempDir, name: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(t.join(name).join("config")).unwrap();
        std::fs::write(t.join(name).join("config/sim.env"), "A=1\n").unwrap();
        t.join(name)
    }

    fn dashboard(g: &FakeGrafana, token: Option<&str>) -> u16 {
        g.call("GET", "http://g/api/dashboards/uid/spira-bead-store", None, token, None).unwrap().status
    }

    #[test_id(tu1nmax)]
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

    #[test_id(t5pz5xb)]
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

    #[test_id(tw2wrms)]
    #[test]
    fn a_revoke_that_fails_keeps_the_account_id_for_a_retry() {
        let t = testkit::TempDir::new("pilgrim-grafana-retry");
        std::fs::create_dir_all(t.join("w/config")).unwrap();
        let g = FakeGrafana::default();
        provision(&g, &t.join("w"), "http://g", "admin:pw").unwrap();
        assert!(revoke(&g, &t.join("w"), "http://g", "admin:wrong").is_err());
        assert!(t.join("w").join(ACCOUNT_FILE).exists());
    }

    #[test_id(txsg5ks)]
    #[test]
    fn the_token_file_is_readable_by_the_session_alone() {
        use std::os::unix::fs::PermissionsExt;
        let t = testkit::TempDir::new("pilgrim-grafana-mode");
        std::fs::create_dir_all(t.join("w/config")).unwrap();
        provision(&FakeGrafana::default(), &t.join("w"), "http://g", "admin:pw").unwrap();
        let mode = std::fs::metadata(t.join("w").join(TOKEN_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test_id(tnwnqtx)]
    #[test]
    fn a_wrong_admin_sign_in_provisions_nothing() {
        let t = testkit::TempDir::new("pilgrim-grafana-bad");
        std::fs::create_dir_all(t.join("w/config")).unwrap();
        assert!(provision(&FakeGrafana::default(), &t.join("w"), "http://g", "admin:wrong").is_err());
        assert!(!t.join("w").join(TOKEN_FILE).exists());
    }

    #[test_id(tzxh6iv)]
    #[test]
    fn an_install_without_grafana_is_none_and_one_with_it_names_the_admin() {
        assert_eq!(install("", "data").unwrap(), None);
        let t = testkit::TempDir::new("pilgrim-grafana-install");
        std::fs::write(t.join("grafana.credential"), "s3\n").unwrap();
        let got = install("1.2.3.4:3000", &t.path().display().to_string()).unwrap().unwrap();
        assert_eq!(got, ("http://1.2.3.4:3000".to_string(), "admin:s3".to_string()));
    }

    #[test_id(twbt0ct)]
    #[test]
    fn two_worlds_come_up_on_one_grafana_without_a_revoke_between() {
        let t = testkit::TempDir::new("pilgrim-grafana-two");
        let (a, b) = (world(&t, "wa"), world(&t, "wb"));
        let g = FakeGrafana::default();
        provision(&g, &a, "http://g", "admin:pw").unwrap();
        provision(&g, &b, "http://g", "admin:pw").unwrap();
        for w in [&a, &b] {
            assert_eq!(dashboard(&g, Some(&std::fs::read_to_string(w.join(TOKEN_FILE)).unwrap())), 200);
        }
    }

    #[test_id(tzc9ht1)]
    #[test]
    fn a_world_claimed_again_replaces_its_leftover_account_and_only_that_one() {
        let t = testkit::TempDir::new("pilgrim-grafana-again");
        let w = world(&t, "w");
        let g = FakeGrafana::default();
        g.call("POST", "http://g/api/serviceaccounts", Some("admin:pw"), None, Some(r#"{"name":"pilgrim-w-extra"}"#)).unwrap();
        up_world(&g, &w, "http://g", "admin:pw").unwrap();
        let old = std::fs::read_to_string(w.join(TOKEN_FILE)).unwrap();
        std::fs::remove_dir_all(w.join("config")).unwrap();
        std::fs::create_dir_all(w.join("config")).unwrap();
        std::fs::write(w.join("config/sim.env"), "A=1\n").unwrap();
        up_world(&g, &w, "http://g", "admin:pw").unwrap();
        assert_eq!(dashboard(&g, Some(&old)), 401);
        assert_eq!(dashboard(&g, Some(&std::fs::read_to_string(w.join(TOKEN_FILE)).unwrap())), 200);
        let names: Vec<String> = g.accounts.borrow().values().cloned().collect();
        assert_eq!(names.iter().filter(|n| *n == "pilgrim-w").count(), 1, "{names:?}");
        assert!(names.contains(&"pilgrim-w-extra".to_string()), "{names:?}");
    }

    #[test_id(tkx7ytp)]
    #[test]
    fn a_refused_token_create_carries_grafanas_own_message() {
        let t = testkit::TempDir::new("pilgrim-grafana-msg");
        let g = FakeGrafana::default();
        let json_no = Refusing { inner: &g, method: "POST", suffix: "/tokens", answer: (400, r#"{"messageId":"x","message":"no tokens today"}"#) }; // not a path: an API route
        let err = provision(&json_no, &world(&t, "a"), "http://g", "admin:pw").unwrap_err();
        assert!(err.contains("HTTP 400") && err.contains("no tokens today"), "{err}");
        let plain = Refusing { inner: &g, method: "POST", suffix: "/tokens", answer: (502, "bad gateway\n") }; // not a path: an API route
        let err = provision(&plain, &world(&t, "b"), "http://g", "admin:pw").unwrap_err();
        assert!(err.ends_with("HTTP 502: bad gateway"), "{err}");
    }

    #[test_id(tdywp8j)]
    #[test]
    fn a_refused_delete_carries_grafanas_own_message() {
        let t = testkit::TempDir::new("pilgrim-grafana-delmsg");
        let w = world(&t, "w");
        let g = FakeGrafana::default();
        provision(&g, &w, "http://g", "admin:pw").unwrap();
        let refusing = Refusing { inner: &g, method: "DELETE", suffix: "/1", answer: (403, r#"{"message":"not allowed"}"#) }; // not a path: an API route
        let err = revoke(&refusing, &w, "http://g", "admin:pw").unwrap_err();
        assert!(err.contains("HTTP 403") && err.contains("not allowed"), "{err}");
    }

    #[test_id(tu9o4p5)]
    #[test]
    fn a_create_refusal_with_another_message_id_deletes_nothing() {
        let t = testkit::TempDir::new("pilgrim-grafana-other");
        let g = FakeGrafana::default();
        g.call("POST", "http://g/api/serviceaccounts", Some("admin:pw"), None, Some(r#"{"name":"pilgrim-w"}"#)).unwrap();
        let refusing = Refusing { inner: &g, method: "POST", suffix: "/api/serviceaccounts", answer: (400, r#"{"messageId":"serviceaccounts.ErrOther","message":"nope"}"#) }; // not a path: an API route
        let err = provision(&refusing, &world(&t, "w"), "http://g", "admin:pw").unwrap_err();
        assert!(err.contains("nope"), "{err}");
        assert_eq!(g.accounts.borrow().len(), 1);
    }

    #[test_id(tp7wgh3)]
    #[test]
    fn a_leftover_that_cannot_be_replaced_is_the_error() {
        const EXISTS: (u16, &str) = (400, r#"{"messageId":"serviceaccounts.ErrAlreadyExists","message":"service account already exists"}"#);
        let t = testkit::TempDir::new("pilgrim-grafana-stuck");
        let g = FakeGrafana::default();
        g.call("POST", "http://g/api/serviceaccounts", Some("admin:pw"), None, Some(r#"{"name":"pilgrim-w"}"#)).unwrap();
        let unfound = Refusing { inner: &g, method: "GET", suffix: "query=pilgrim-w", answer: (200, r#"{"serviceAccounts":[{"id":1,"name":"pilgrim-w-extra"}]}"#) };
        assert!(provision(&unfound, &world(&t, "w"), "http://g", "admin:pw").is_err());
        let search = Refusing { inner: &g, method: "GET", suffix: "query=pilgrim-w", answer: (500, "search down") };
        assert!(provision(&search, &world(&t, "w"), "http://g", "admin:pw").unwrap_err().contains("search down"));
        let delete = Refusing { inner: &g, method: "DELETE", suffix: "/1", answer: (403, r#"{"message":"no delete"}"#) }; // not a path: an API route
        assert!(provision(&delete, &world(&t, "w"), "http://g", "admin:pw").unwrap_err().contains("no delete"));
        assert_eq!(g.accounts.borrow().len(), 1, "nothing was deleted");
        let again = Refusing { inner: &g, method: "POST", suffix: "/api/serviceaccounts", answer: EXISTS }; // not a path: an API route
        assert!(provision(&again, &world(&t, "w"), "http://g", "admin:pw").unwrap_err().contains("already exists"));
    }

    #[test_id(ta8fjj0)]
    #[test]
    fn an_account_name_is_percent_encoded_into_the_search_query() {
        assert_eq!(encode("pilgrim-a b&c#d+e.f_g~h"), "pilgrim-a%20b%26c%23d%2Be.f_g~h");
    }
}
