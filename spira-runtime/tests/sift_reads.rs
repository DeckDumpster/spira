//! sift's `observe_repo` reads every candidate's stack from the pool rows and every prerequisite
//! in one `list --ids`, never a `show` per bead. No projection is installed in this process, so
//! every read goes over the client.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use spira_store::lc_call::{LcClient, Reply, Transport};

struct Counting {
    rows: Vec<Value>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Transport for Counting {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
        let ok = |out: String| Ok(Reply { code: 0, out, err: String::new() });
        let named = |flag: &str| argv.iter().position(|a| a == flag).map(|i| argv[i + 1].clone());
        let by_id = |id: &str| self.rows.iter().find(|r| r["bead_id"] == id).cloned();
        self.asked.lock().unwrap().push(argv.join(" "));
        match argv[0].as_str() {
            "rc" => ok(json!({ "ok": { "items": [], "list_rev": 5 } }).to_string()),
            "list" if argv.iter().any(|a| a == "--batches") => ok("[]".into()),
            "list" => {
                let rows: Vec<&Value> = match (named("--ids"), named("--state")) {
                    (Some(ids), _) => self.rows.iter().filter(|r| ids.split(',').any(|i| Some(i) == r["bead_id"].as_str())).collect(),
                    (None, Some(states)) => self.rows.iter().filter(|r| states.split(',').any(|s| Some(s) == r["state"].as_str())).collect(),
                    _ => self.rows.iter().collect(),
                };
                ok(serde_json::to_string(&rows).unwrap())
            }
            "show" => ok(json!({ "bead": by_id(&argv[1]) }).to_string()),
            "plan" => {
                let items: Vec<Value> = named("--ids").iter().flat_map(|ids| ids.split(',')).map(|id| json!({ "id": id, "title": "t", "priority": 2, "description": "d", "notes": "", "dependencies": [] })).collect();
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

#[test]
fn observe_repo_reads_stacks_from_the_pool_rows_and_prerequisites_in_one_list_ids() {
    let dir = testkit::TempDir::new("sift-reads");
    let repo_dir = dir.path().join("alpha");
    std::fs::create_dir(&repo_dir).unwrap();
    git(&repo_dir, &["init", "-q", "-b", "main"]);
    git(&repo_dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]);
    let tip = git(&repo_dir, &["rev-parse", "HEAD"]);

    let mut rows = Vec::new();
    for i in 0..20 {
        git(&repo_dir, &["branch", &format!("spira/sp-c{i:02}")]);
        rows.push(row(&format!("sp-c{i:02}"), "CERTIFIED", &tip, &format!("{{\"sp-p{i:02}\":\"t0\"}}")));
        rows.push(row(&format!("sp-p{i:02}"), if i < 10 { "LANDED" } else { "WORKING" }, "t0", "{}"));
    }
    let asked = Arc::new(Mutex::new(Vec::new()));
    let lc = LcClient::over(Arc::new(Counting { rows, asked: asked.clone() }), true);

    let map = format!("alpha|{}|queue.local|main\n", repo_dir.display());
    let reg = spira_config::repos::Registry::new(Some(&map), &BTreeMap::new(), dir.path());
    let views = store_view::Projection::new(lc.clone(), Duration::from_secs(30), Arc::new(std::time::Instant::now));
    let reads = spira_runtime::tasks::a::sift::Reads { lc: &lc, views: &views, reg: &reg, wiki: "", identity_projects: "", work: dir.path().join("sift") };

    let f = spira_runtime::tasks::a::sift::observe_repo(&reads, "alpha", true).expect("sift reads its repo");

    let asked = asked.lock().unwrap();
    let shows = asked.iter().filter(|a| a.starts_with("show")).count();
    let list_ids = asked.iter().filter(|a| a.starts_with("list --ids")).count();
    assert_eq!((shows, list_ids), (0, 1), "asked: {asked:?}");

    let stack: BTreeMap<String, Vec<String>> = (0..20).map(|i| (format!("sp-c{i:02}"), vec![format!("sp-p{i:02}")])).collect();
    assert_eq!(f.stack, stack);
    let prereq_state: BTreeMap<String, String> = (0..20).map(|i| (format!("sp-p{i:02}"), (if i < 10 { "LANDED" } else { "WORKING" }).to_string())).collect();
    assert_eq!(f.prereq_state, prereq_state);
    let outside: Vec<(String, String)> = f.outside.iter().map(|p| (p.id.clone(), p.state.clone())).collect();
    let want: Vec<(String, String)> = (0..20).map(|i| (format!("sp-p{i:02}"), (if i < 10 { "LANDED" } else { "WORKING" }).to_string())).collect();
    assert_eq!(outside, want);
}
