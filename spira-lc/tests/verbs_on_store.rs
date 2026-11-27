//! Every verb that once read or wrote the dolt lifecycle database, run as the binary against a
//! real control store file: what a caller gets.

#[macro_use]
extern crate spira_test_id;
use std::process::Command;

use serde_json::Value;

struct World {
    _dir: testkit::TempDir,
    toml: std::path::PathBuf,
    home: std::path::PathBuf,
}

impl World {
    fn new() -> World {
        let dir = testkit::TempDir::new("spira-lc-verbs-on-store");
        let run_dir = dir.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (d, r, s) = (dir.display().to_string(), run_dir.display().to_string(), dir.join("control.db").display().to_string());
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_WORKSPACES", &d), ("SPIRA_RUN", &r), ("SPIRA_LC_STORE_PATH", &s), ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9")]);
        let home = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
        World { _dir: dir, toml, home }
    }

    fn release(&self) -> std::path::PathBuf {
        let dir = self._dir.join("release");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        dir
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let mut answer = self.run_once(args);
        for _ in 0..8 {
            if !answer.1.contains("read ended") {
                break;
            }
            answer = self.run_once(args);
        }
        answer
    }

    // A starved process can be ended at the 100 ms limit whatever it reads; what a verb answers is
    // under test here, not the host's scheduling.
    fn run_once(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(testkit::bin_exe("spira-lc"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("SPIRA_TOML", &self.toml)
            .env("SPIRA_HOME", &self.home)
            .env("SPIRA_RELEASE", self.release())
            .env("HOME", self._dir.join("home"))
            .output()
            .expect("spira-lc runs");
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout).trim_end().to_string(), String::from_utf8_lossy(&out.stderr).to_string());
        let code = out.status.code().unwrap_or(-1);
        (code, if code == 0 { stdout } else { format!("{stdout}{stderr}") })
    }

    fn ok(&self, args: &[&str]) -> String {
        let (rc, out) = self.run(args);
        assert_eq!(rc, 0, "{args:?}: {out}");
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}"))
    }
}

#[test_id(tpr6dzm)]
#[test]
fn an_edge_verb_typed_at_the_command_line_exits_refused_and_writes_nothing() {
    let w = World::new();
    w.ok(&["admin-create-store"]);
    w.ok(&["create-bead", "sp-a", "--ready", "--repo", "harness"]);
    let before = w.ok(&["show", "sp-a"]);
    for verb in ["dep-add", "dep-remove"] {
        let (rc, out) = w.run(&[verb, "sp-a", "sp-b"]);
        assert!(rc == 3 && out.contains("bead.sh dep add") && out.contains("bead.sh dep remove"), "{verb}: {rc} {out}");
        assert_eq!(w.ok(&["show", "sp-a"]), before, "{verb}");
    }
}

#[test_id(tplmupn)]
#[test]
fn the_ported_verbs_answer_from_the_control_store() {
    let w = World::new();
    let (ok, json) = (|a: &[&str]| w.ok(a), |a: &[&str]| w.json(a));
    ok(&["admin-create-store"]);

    ok(&["create-bead", "sp-a", "--ready", "--repo", "harness"]);
    ok(&["fact", "add", "sp-a", "--kind", "claimed", "--actor", "dwarf-1", "--cause", "first"]);
    ok(&["fact", "add", "sp-a", "--kind", "requeued", "--actor", "harness", "--cause", "gate-red"]);
    let facts = json(&["fact", "list", "--ids", "sp-a"]);
    assert_eq!(facts.as_array().unwrap().iter().map(|f| f["event_type"].as_str().unwrap()).collect::<Vec<_>>(), ["claimed", "requeued"]);
    assert_eq!(facts[1]["new_value"], "gate-red");
    assert_eq!(json(&["fact", "list", "--kinds", "claimed"]).as_array().unwrap().len(), 1);

    let turn = r#"{"state":[{"task":"t","skey":"k","value":"v","at":1}],"schedules":[{"id":"r1","at":5,"stream":"s","kind":{"tag":"tick","body":""},"cause":null,"correlation":"c"}]}"#;
    ok(&["journal", "turn", turn]);
    assert_eq!(json(&["journal", "state"])["rows"][0]["value"], "v");
    assert_eq!(json(&["journal", "due", "10"])[0]["id"], "r1");

    assert_eq!(json(&["ops-view", "ops_live"])[0]["bead_id"], "sp-a");
    assert_eq!(json(&["ops-view", "ops_live"])[0]["claimable"], "1");
    assert_eq!(json(&["ops-view", "ops_live"])[0]["repo"], "harness");
    assert!(json(&["ops-transitions"])["events"].is_array());
    let (rc, out) = w.run(&["ops-refusals", "3600"]);
    assert_eq!(rc, 2, "{out}");
    assert!(out.contains("cannot tell: the refusal tally is filling"), "{out}");
    assert_eq!(json(&["ops-bead", "sp-a"])["bead"]["state"], "READY");
    assert_eq!(w.run(&["ops-bead", "sp-none"]).0, 1);
    assert!(!json(&["ops-graph"]).as_array().unwrap().is_empty());

    ok(&["mend-watch"]);
}

#[test_id(ttepkjc)]
#[test]
fn ship_status_answers_the_stores_head_after_a_commit_with_no_shipper_running() {
    let w = World::new();
    let (rc, out) = w.run(&["ship-status"]);
    assert_ne!(rc, 0, "no control store: {out}");
    w.ok(&["admin-create-store"]);
    let head = |w: &World| w.json(&["ship-status"])["head"].as_u64().expect("head is a number");
    let before = head(&w);
    w.ok(&["create-bead", "sp-a", "--ready", "--repo", "harness"]);
    assert!(head(&w) > before, "a commit moves the head the verb answers");
}
