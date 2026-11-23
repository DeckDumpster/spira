//! The backlog snapshot measures a fixture repository (a landing ref two commits ahead of a bare
//! forge remote) in the scheduler's own process and records what it read for lc-serve; the body it
//! sends is the one `git-fact put unpublished` accepts (spira-lc's `unpublished_record_*` tests).

#[macro_use]
extern crate spira_test_id;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::world::World;

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test_id(tpr5txo)]
#[test]
fn the_snapshot_records_the_unpublished_range_it_measured_and_runs_no_verb_that_measures() {
    let dir = testkit::TempDir::new("world-publish-backlog");
    let (forge, repo) = (dir.path().join("forge.git"), dir.path().join("alpha"));
    std::fs::create_dir_all(&forge).unwrap();
    git(&forge, &["init", "-q", "--bare", "-b", "main"]);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "published"]);
    git(&repo, &["remote", "add", "origin", forge.to_str().unwrap()]);
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin", "main"]);
    git(&repo, &["branch", "local/main"]);
    git(&repo, &["checkout", "-q", "local/main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
    let (forge_tip, landing_tip) = (
        git(&repo, &["rev-parse", "origin/main"]),
        git(&repo, &["rev-parse", "local/main"]),
    );

    let map = dir.path().join("repos");
    std::fs::write(
        &map,
        format!("alpha|{}|queue.local|local/main||\n", repo.display()),
    )
    .unwrap();
    let run = dir.path().join("run");
    let toml = spira_config::process::fixture_toml(
        &dir,
        &[
            ("SPIRA_RUN", &run.display().to_string()),
            ("SPIRA_REPO_MAP", &map.display().to_string()),
            ("SPIRA_HOME_REPO", "alpha"),
            ("SPIRA_WORKSPACES", &dir.path().display().to_string()),
        ],
    );
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let _env = testkit::env(&[
        ("SPIRA_TOML", Some(toml.to_str().unwrap())),
        ("SPIRA_HOME", Some(home.to_str().unwrap())),
        ("SPIRA_RELEASE", None),
        ("SPIRA_REPO_MAP", None),
        ("SPIRA_HOME_REPO", None),
    ]);

    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let asked: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let seen = asked.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            seen.lock().unwrap().push(argv.to_vec());
            match argv.first().map(String::as_str) {
                Some("git-fact") => Ok("{}".into()),
                _ => Err(RuntimeError::new("the double has no such verb")),
            }
        })
    });

    let world = World {
        lc_socket: sock,
        repo_map: map,
        run_dir: run,
        home_repo: "alpha".into(),
        ..World::default()
    };
    let snapshot = world.publish_backlog_snapshot().unwrap();
    assert!(snapshot.contains("\"count\":2"), "{snapshot}");

    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1, "one call, the record: {asked:?}");
    assert_eq!(asked[0][..4], ["git-fact", "put", "unpublished", "--json"]);
    let body: serde_json::Value = serde_json::from_str(&asked[0][4]).unwrap();
    assert_eq!(
        (body["repo"].as_str(), body["count"].as_u64()),
        (Some("alpha"), Some(2))
    );
    assert_eq!(
        (body["forge_tip"].as_str(), body["landing_tip"].as_str()),
        (Some(forge_tip.as_str()), Some(landing_tip.as_str()))
    );
    assert!(
        body["oldest_arrival"].is_u64() && body["read_at"].is_u64(),
        "{body}"
    );
}
