//! A CERTIFIED item whose blocker is finished by the claim rule is cut into a round: a DONE item
//! and a closed rowless decision release it, a CERTIFIED item outside the pool still holds it.
//! A member blocked by a change to the gate's own files waits until the running release carries it.
//! One test: the projection is installed once per process.

#[macro_use]
extern crate spira_test_id;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use batcher_cut::io::{self, Env, Hypervisor, Land, Repo};
use serde_json::{json, Value};
use spira_store::lc_call::{LcClient, Reply, Transport};
use store_view::Projection;

struct Store {
    rows: Vec<Value>,
    items: Vec<Value>,
}

impl Transport for Store {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
        let ok = |out: String| Ok(Reply { code: 0, out, err: String::new(), read_ended: false });
        let named = |flag: &str| argv.iter().position(|a| a == flag).map(|i| argv[i + 1].clone());
        match argv[0].as_str() {
            "rc" => ok(json!({ "ok": { "items": [], "list_rev": 5 } }).to_string()),
            "list" if argv.iter().any(|a| a == "--batches") => ok("[]".into()),
            "list" => {
                let ids = named("--ids");
                let states = named("--state");
                let rows: Vec<&Value> = self
                    .rows
                    .iter()
                    .filter(|r| ids.as_ref().is_none_or(|ids| ids.split(',').any(|i| Some(i) == r["bead_id"].as_str())))
                    .filter(|r| states.as_ref().is_none_or(|s| s.split(',').any(|s| Some(s) == r["state"].as_str())))
                    .collect();
                ok(serde_json::to_string(&rows).unwrap())
            }
            "watch" => {
                // clock-safe: the double's long-poll, ending after a pause
                std::thread::sleep(Duration::from_millis(5));
                ok(json!({ "progress_rev": 5 }).to_string())
            }
            "content" => ok("[]".into()),
            "plan" if argv.get(1).map(String::as_str) == Some("edges") => ok(json!({ "edges": [] }).to_string()),
            "plan" => {
                let ids = named("--ids").unwrap_or_default();
                let items: Vec<&Value> = self.items.iter().filter(|i| ids.split(',').any(|id| Some(id) == i["id"].as_str())).collect();
                ok(json!({ "watermark": 1, "items": items }).to_string())
            }
            other => Err(format!("unscripted {other}")),
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn row(id: &str, state: &str, tip: &str, stack: &str) -> Value {
    json!({ "bead_id": id, "state": state, "tip": tip, "holds": "[]", "express": "0", "repo": "alpha", "stack": stack, "updated_at": 4_000_000_000_i64 })
}

fn env(dir: &Path) -> Env {
    Env {
        lc_rows: Default::default(),
        home: dir.to_path_buf(),
        run: dir.join("run"),
        queue_dir: dir.join("queue"),
        forge: "forge".into(),
        tsd_bin: None,
        round_vm: "round-vm".into(),
        queue_bin: "queue".into(),
        rebase_stale_bin: "rebase-stale".into(),
        testenv_bin: "testenv".into(),
        gate_bin: "gate".into(),
        test_registry_bin: None,
        round_slots: None,
        hypervisor: Hypervisor { host_mib: 0, round_vm_mib: 0, runner_mib: 0 },
        poll: Duration::from_secs(1),
        maxpar: 1,
        wall_secs: 1,
        stall_secs: 1,
        git_name: "t".into(),
        git_email: "t@t".into(),
        lc_bin: None,
        lc_timeout: 1,
        verdicts: dir.join("verdicts"),
        land_lock_attempts: 1,
        land_lock_wait: Duration::from_secs(1),
        orphan_redrive_after: 1,
        pause_lift_after: 1,
        land_unpause: None,
        results_retention: spira_config::results_retention::Retention { keep_days: 7, max: 50 },
        commit: Err("no release".into()),
        repo: Err("no release".into()),
    }
}

fn with_repo(mut r: Value, repo: &str) -> Value {
    r["repo"] = json!(repo);
    r
}

/// A repo whose `gate.steps` owns `owned/`: landed changes to it (one inside the release at
/// `commit`, one after it) and pool branches that do and do not touch it.
fn gamma_repo(dir: &Path) -> (String, std::collections::BTreeMap<String, String>, (String, String)) {
    std::fs::create_dir(dir).unwrap();
    let commit = |msg: &str| git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", msg]);
    let touch = |path: &str| {
        std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
        std::fs::write(dir.join(path), path).unwrap();
        git(dir, &["add", path]);
    };
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("gate.steps"), "step true\nown owned/\n").unwrap();
    git(dir, &["add", "gate.steps"]);
    commit("base");
    let land = |name: &str, path: &str| {
        git(dir, &["checkout", "-q", "-b", name, "main"]);
        touch(path);
        commit(name);
        let tip = git(dir, &["rev-parse", "HEAD"]);
        git(dir, &["checkout", "-q", "main"]);
        git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "-q", "--no-ff", "-m", name, name]);
        tip
    };
    let inside = land("l-in", "owned/in");
    let release = git(dir, &["rev-parse", "HEAD"]);
    let outside = land("l-out", "owned/out");
    let mut tips = std::collections::BTreeMap::new();
    for (id, path) in [("b-own", Some("owned/b")), ("b-free", Some("free/b")), ("a-own", None), ("a-free", None), ("a-lo", None), ("a-li", None)] {
        git(dir, &["checkout", "-q", "-b", &format!("spira/{id}"), "main"]);
        if let Some(path) = path {
            touch(path);
            commit(id);
        }
        tips.insert(id.to_string(), git(dir, &["rev-parse", "HEAD"]));
    }
    git(dir, &["checkout", "-q", "main"]);
    (release, tips, (outside, inside))
}

fn item(id: &str, issue_type: &str, status: &str, blocker: Option<&str>) -> Value {
    let deps: Vec<Value> = blocker.iter().map(|b| json!({ "depends_on_id": b, "type": "blocks" })).collect();
    json!({ "id": id, "title": "t", "priority": 2, "description": "d", "notes": "", "issue_type": issue_type, "status": status, "dependencies": deps })
}

#[test_id(tvhh0mz)]
#[test]
fn a_certified_item_blocked_by_a_finished_item_is_cut_and_one_blocked_by_a_certified_item_outside_the_pool_is_left_out() {
    let dir = testkit::TempDir::new("batcher-cut-blockers");
    let repo_dir = dir.path().join("alpha");
    std::fs::create_dir(&repo_dir).unwrap();
    git(&repo_dir, &["init", "-q", "-b", "main"]);
    git(&repo_dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]);
    let tip = git(&repo_dir, &["rev-parse", "HEAD"]);
    for id in ["sp-done", "sp-dec", "sp-live", "sp-weird"] {
        git(&repo_dir, &["branch", &format!("spira/sp-on-{id}")]);
    }
    let other = row("sp-other", "CERTIFIED", "t9", "{}");
    let other = Value::Object(other.as_object().unwrap().iter().map(|(k, v)| (k.clone(), if k == "repo" { json!("beta") } else { v.clone() })).collect());
    let gamma = dir.path().join("gamma");
    let (gamma_commit, gamma_tips, landed) = gamma_repo(&gamma);
    let mut gamma_rows: Vec<Value> = ["b-own", "a-own", "b-free", "a-free", "a-lo", "a-li"]
        .iter()
        .map(|id| with_repo(row(id, "CERTIFIED", &gamma_tips[*id], "{}"), "gamma"))
        .collect();
    for (id, sha) in [("l-out", &landed.0), ("l-in", &landed.1)] {
        let mut r = with_repo(row(id, "LANDED", "t0", "{}"), "gamma");
        r["reason"] = json!(sha);
        gamma_rows.push(r);
    }
    let gamma_items = vec![
        item("b-own", "task", "open", None),
        item("b-free", "task", "open", None),
        item("a-own", "task", "open", Some("b-own")),
        item("a-free", "task", "open", Some("b-free")),
        item("a-lo", "task", "open", Some("l-out")),
        item("a-li", "task", "open", Some("l-in")),
    ];
    let mut store = Store {
        rows: vec![
            row("sp-on-sp-done", "CERTIFIED", &tip, "{\"sp-done\":\"t0\",\"sp-dec\":\"t0\"}"),
            row("sp-on-sp-weird", "CERTIFIED", &tip, "{}"),
            row("sp-weird", "UNHEARD-OF", "t0", "{}"),
            row("sp-on-sp-dec", "CERTIFIED", &tip, "{}"),
            row("sp-on-sp-live", "CERTIFIED", &tip, "{}"),
            row("sp-done", "DONE", "t0", "{}"),
            other,
        ],
        items: vec![
            item("sp-on-sp-done", "task", "open", Some("sp-done")),
            item("sp-on-sp-dec", "task", "open", Some("sp-dec")),
            item("sp-on-sp-live", "task", "open", Some("sp-other")),
            item("sp-on-sp-weird", "task", "open", Some("sp-weird")),
            item("sp-dec", "decision", "closed", None),
        ],
    };
    store.rows.extend(gamma_rows);
    store.items.extend(gamma_items);
    let frozen = Arc::new(Mutex::new(std::time::Instant::now()));
    let views = Projection::new(LcClient::over(Arc::new(store), true), Duration::from_secs(30), Arc::new(move || *frozen.lock().unwrap()));
    let _watch = spira_runtime::store::run(views).expect("the first install stands");
    let installed = store_view::shared().unwrap();
    // clock-safe: a cap on the wait for the watch thread to start
    for _ in 0..6000 {
        if installed.watching() {
            break;
        }
        // clock-safe: a poll interval under that cap
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(installed.watching());

    let env = env(dir.path());
    let repo = Repo { name: "alpha".into(), path: repo_dir, base: "main".into(), forge: "forge".into(), land: Land::Local };
    let (pool, left) = io::certified_pool_left_out(&env, &repo).expect("the pool reads");
    let (cut, refusals) = batcher::core::refuse_blocked(pool);
    let tips = cut.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let left = io::cut_left_out(left, &tips, &refusals);

    let mut cut_ids: Vec<&str> = cut.iter().map(|m| m.id.as_str()).collect();
    cut_ids.sort();
    assert_eq!(cut_ids, ["sp-on-sp-dec", "sp-on-sp-done"], "a DONE blocker and a closed rowless decision release their dependents");
    assert!(cut.iter().all(|m| m.stack.is_empty()), "finished prerequisites sequence nothing: {cut:?}");
    let mut refused: Vec<(&str, &str)> = refusals.iter().map(|r| (r.id.as_str(), r.blocker.as_str())).collect();
    refused.sort();
    assert_eq!(refused, [("sp-on-sp-live", "sp-other"), ("sp-on-sp-weird", "sp-weird")], "a state the machine does not know stays live");
    assert!(left.iter().any(|l| l.id == "sp-on-sp-live" && l.cause == io::Cause::Blocked { dep: "sp-other".into() }), "{left:?}");

    let mut env = env;
    env.commit = Ok(gamma_commit);
    env.repo = Ok("gamma".into());
    let repo = Repo { name: "gamma".into(), path: gamma, base: "main".into(), forge: "forge".into(), land: Land::Local };
    let (pool, left) = io::certified_pool_left_out(&env, &repo).expect("the pool reads");
    let mut pooled: Vec<&str> = pool.iter().map(|m| m.id.as_str()).collect();
    pooled.sort();
    assert_eq!(
        pooled,
        ["a-free", "a-li", "b-free", "b-own"],
        "a blocker touching no owned path rides with its dependant, a change the release carries is in force; the rest wait: {left:?}"
    );
    for (id, dep) in [("a-own", "b-own"), ("a-lo", "l-out")] {
        assert!(left.iter().any(|l| l.id == id && l.cause == io::Cause::NotInForce { dep: dep.into() }), "{id}: {left:?}");
    }
    assert_eq!(left.iter().find(|l| l.id == "a-own").unwrap().tag(), "blocked-not-in-force");

    env.repo = Ok("elsewhere".into());
    let (pool, _) = io::certified_pool_left_out(&env, &repo).expect("the pool reads");
    assert!(pool.iter().any(|m| m.id == "a-own"), "a release of another repo says nothing about this one's own files");
    env.repo = Err("unreadable".into());
    let (_, left) = io::certified_pool_left_out(&env, &repo).expect("the pool reads");
    assert!(left.iter().any(|l| l.id == "a-own"), "an unreadable release leaves the dependent out");
}
