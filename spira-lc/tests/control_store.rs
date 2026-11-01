//! The control store's own door, through the binary: `admin-create-store` makes the store, `rc`
//! reads and writes it in direct mode, a lock `rc` takes is the one `lock` sees, and the bead-state
//! verbs stay on dolt until SPIRA_LC_LIFECYCLE_ON_STORE moves them.

use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

struct World {
    _t: testkit::TempDir,
    toml: PathBuf,
    store: PathBuf,
    db: Db,
}

struct Db {
    port: u16,
    accepted: Arc<AtomicUsize>,
}

/// A dolt stand-in that accepts and hangs up, so "reached dolt" is observed.
fn hanging_up_db() -> Db {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let seen = accepted.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            seen.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    Db { port, accepted }
}

fn world(declare: &[(&str, &str)]) -> World {
    let t = testkit::TempDir::new("spira-lc-control-store");
    let store = t.path().join("control.db");
    let run = t.path().display().to_string();
    let path = store.display().to_string();
    let mut keys = vec![
        ("SPIRA_RUN", run.as_str()),
        ("SPIRA_LC_SOCKET", ""),
        ("SPIRA_LC_PASSWORD_FILE", ""),
        ("SPIRA_LC_STORE_PATH", path.as_str()),
        // Not the shipped default, so the floor is shown to be read from the config.
        ("SPIRA_LC_LOCK_TTL_MIN_MS", "2000"),
    ];
    keys.extend_from_slice(declare);
    let toml = spira_config::process::fixture_toml(t.path(), &keys);
    World {
        _t: t,
        toml,
        store,
        db: hanging_up_db(),
    }
}

impl World {
    fn run(&self, args: &[&str], stdin: &str) -> (i32, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
            .args(args)
            .env_remove("SPIRA_LIFECYCLE_ENFORCE")
            .env("SPIRA_LC_HOST", "127.0.0.1")
            .env("SPIRA_LC_PORT", self.db.port.to_string())
            .env("SPIRA_LC_PASSWORD", "")
            .env("SPIRA_HOME", concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"))
            .env("SPIRA_TOML", &self.toml)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout).into_owned()
            + &String::from_utf8_lossy(&o.stderr);
        (o.status.code().unwrap_or(-1), out)
    }

    fn rc(&self, verb: &str, body: Value) -> (i32, Value) {
        let (code, out) = self.run(&["rc", verb, "--json", "-"], &body.to_string());
        let v = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
        (code, v)
    }

    fn created(&self) -> &Self {
        let (code, out) = self.run(&["admin-create-store"], "");
        assert_eq!(code, 0, "{out}");
        self
    }

    fn reached_dolt(&self) -> usize {
        self.db.accepted.load(Ordering::SeqCst)
    }
}

fn request(id: &str) -> Value {
    json!({ "id": id, "actor": "round-controller", "kind": "test" })
}

#[test]
fn admin_create_store_makes_the_file_once_and_refuses_a_second_time() {
    let w = world(&[]);
    assert!(!w.store.exists());
    let (code, out) = w.run(&["admin-create-store"], "");
    assert_eq!(code, 0, "{out}");
    assert!(w.store.is_file(), "{out}");
    assert!(out.contains("2000 ms"), "the configured floor: {out}");
    let (code, out) = w.run(&["admin-create-store"], "");
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("already exists"), "{out}");
    assert_eq!(w.reached_dolt(), 0);
}

#[test]
fn admin_create_store_refuses_an_empty_path_and_one_outside_the_workspaces() {
    let w = world(&[("SPIRA_LC_STORE_PATH", "")]);
    let (code, out) = w.run(&["admin-create-store"], "");
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("SPIRA_LC_STORE_PATH is empty"), "{out}");
    let elsewhere = testkit::TempDir::new("spira-lc-workspaces");
    let ws = elsewhere.path().display().to_string();
    let w = world(&[("SPIRA_INSTANCE", "probe"), ("SPIRA_WORKSPACES", ws.as_str())]);
    let (code, out) = w.run(&["admin-create-store"], "");
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("containment"), "{out}");
    assert!(!w.store.exists());
}

#[test]
fn an_rc_txn_put_is_read_back_by_get_and_list_with_its_revs() {
    let w = world(&[]);
    w.created();
    let (code, put) = w.rc(
        "txn",
        json!({
            "txn": { "compares": [{ "mod_rev": ["batches/sim/b1", null] }], "ops": [{ "put": ["batches/sim/b1", { "data": { "state": "OPEN" } }] }], "else_ops": [] },
            "request": request("rq-put"),
        }),
    );
    assert_eq!(code, 0, "{put}");
    assert_eq!(put["ok"]["succeeded"], true, "{put}");
    let rev = put["ok"]["rev"].as_u64().unwrap();
    let (code, got) = w.rc("get", json!({ "key": "batches/sim/b1" }));
    assert_eq!(code, 0, "{got}");
    assert_eq!(got["ok"]["value"]["data"]["state"], "OPEN", "{got}");
    assert_eq!(got["ok"]["mod_rev"]["main"], rev, "{got}");
    assert_eq!(got["ok"]["create_rev"]["main"], rev, "{got}");
    let (code, listed) = w.rc(
        "list",
        json!({ "prefixes": ["batches/sim/"], "selector": "everything" }),
    );
    assert_eq!(code, 0, "{listed}");
    let items = listed["ok"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{listed}");
    assert_eq!(items[0]["mod_rev"]["main"], rev);
    assert!(listed["ok"]["list_rev"].as_u64().unwrap() >= rev, "{listed}");
}

#[test]
fn a_lock_rc_takes_is_the_lock_spira_lc_lock_sees() {
    let w = world(&[]);
    w.created();
    let (code, fence) = w.rc(
        "lock",
        json!({ "resource": "round/sim/b1", "ttl_ms": 60000, "request": request("rq-lock") }),
    );
    assert_eq!(code, 0, "{fence}");
    assert_eq!(fence["ok"]["resource"], "round/sim/b1", "{fence}");
    let (code, out) = w.run(
        &["lock", "acquire", "round/sim/b1", "--ttl", "60", "--holder", "someone-else"],
        "",
    );
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("\"code\":\"locked\""), "{out}");
}

#[test]
fn an_rc_write_outside_the_round_controllers_prefixes_is_refused_naming_them() {
    let w = world(&[]);
    w.created();
    let (code, out) = w.rc(
        "txn",
        json!({
            "txn": { "compares": [], "ops": [{ "put": ["beads/x", { "data": 1 }] }], "else_ops": [] },
            "request": request("rq-beads"),
        }),
    );
    assert_eq!(code, 3, "{out}");
    assert_eq!(out["error"], json!({ "reserved_key": { "key": "beads/x" } }), "{out}");
    let rule = out["rule"].as_str().unwrap();
    assert!(rule.contains("beads/x") && rule.contains("batches/") && rule.contains("locks/round/"), "{rule}");
    let (_, got) = w.rc("get", json!({ "key": "beads/x" }));
    assert_eq!(got["ok"], Value::Null, "nothing was written: {got}");
}

#[test]
fn rc_with_the_path_unset_cannot_tell_naming_the_key() {
    let w = world(&[("SPIRA_LC_STORE_PATH", "")]);
    let (code, out) = w.rc("get", json!({ "key": "batches/sim/b1" }));
    assert_eq!(code, 2, "{out}");
    assert!(out["cannot_tell"].as_str().unwrap().contains("SPIRA_LC_STORE_PATH"), "{out}");
    assert_eq!(w.reached_dolt(), 0);
}

#[test]
fn with_the_path_set_and_the_lifecycle_key_off_list_reaches_dolt() {
    let w = world(&[("SPIRA_LC_LIFECYCLE_ON_STORE", "false")]);
    w.created();
    let (code, out) = w.run(&["list"], "");
    assert_eq!(code, 2, "{out}");
    assert!(w.reached_dolt() > 0, "list was answered without asking dolt: {out}");
}

#[test]
fn with_the_lifecycle_key_on_list_answers_from_the_control_store() {
    let w = world(&[("SPIRA_LC_LIFECYCLE_ON_STORE", "true")]);
    w.created();
    let (code, out) = w.run(&["list"], "");
    assert_eq!(code, 0, "{out}");
    assert_eq!(w.reached_dolt(), 0, "{out}");
}
