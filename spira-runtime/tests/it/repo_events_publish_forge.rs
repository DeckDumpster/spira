//! The repo-events observe asks GitHub only about repositories that publish to GitHub.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use spira_config::repos::Registry;
use spira_config::{PublishMode, PublishSection};
use spira_runtime::forge::Forge;
use spira_runtime::world::World;

fn checkout(dir: &Path, origin: &str) {
    for args in [vec!["init", "-q", "-b", "main"], vec!["remote", "add", "origin", origin], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]] {
        assert!(Command::new("git").args(&args).current_dir(dir).status().unwrap().success());
    }
}

#[test]
fn only_a_repository_publishing_to_github_is_asked_for_its_runs() {
    let tmp = testkit::TempDir::new("repo-events-publish-forge");
    let (gh, gitea) = (tmp.path().join("gh"), tmp.path().join("gitea"));
    std::fs::create_dir_all(&gh).unwrap();
    std::fs::create_dir_all(&gitea).unwrap();
    checkout(&gh, "git@github.com:acme/widgets.git");
    checkout(&gitea, "git@gitea.example:acme/gadgets.git");
    let map = format!("widgets|{}|push|main|cargo|true\ngadgets|{}|push|main|cargo|true\n", gh.display(), gitea.display());
    let publish = BTreeMap::from([
        ("widgets".to_string(), PublishSection::default()),
        ("gadgets".to_string(), PublishSection { mode: Some(PublishMode::Push), ..PublishSection::default() }),
    ]);
    let reg = Registry::new(Some(&map), &BTreeMap::new(), tmp.path()).with_publish(publish);
    let map_path = tmp.path().join("registry-rows");
    std::fs::write(&map_path, &map).unwrap();

    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", l.local_addr().unwrap());
    let paths = Arc::new(Mutex::new(vec![]));
    let seen = paths.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let mut c = c.unwrap();
            let mut buf = [0u8; 4096];
            let n = c.read(&mut buf).unwrap();
            let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
            seen.lock().unwrap().push(line);
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"workflow_runs\":[]}\n");
        }
    });

    let world = World { repo_map: map_path, run_dir: tmp.path().join("run"), ..World::default() };
    let forge = Forge { api, token: "t".into(), ..Forge::default() };
    let out = world.repo_events_snapshot_for(&reg, &forge, "{}").unwrap();

    let lines = paths.lock().unwrap().clone();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("/repos/acme/widgets/actions/runs"), "{lines:?}");
    assert!(!out.contains("gadgets"), "{out}");
}
