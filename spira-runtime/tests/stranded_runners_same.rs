//! `Forge::stranded_runners` (spira-runtime, over HTTP) and `forge stranded-runners` (the forge
//! crate, over `gh`) are two readings of one question. They answer the same fixture the same way.
// covers: spira-runtime/src/forge.rs forge/src/cmds.rs

use std::io::{Read, Write};
use std::path::Path;

use forge::ports::{Gh, GhOut};
use spira_runtime::forge::Forge;

const NOW: u64 = 1_800_000_000;

fn stamp(age: u64) -> String {
    reconciler_engine::clock::iso_of(NOW - age)
}

/// What the forge answers for the three paths a question touches, by their tail.
struct World {
    runs: String,
    runners: String,
    jobs: Vec<(u64, String)>,
}

impl World {
    fn answer(&self, path: &str) -> String {
        if path.contains("/actions/runs?") {
            self.runs.clone()
        } else if path.contains("/actions/runners") {
            self.runners.clone()
        } else {
            self.jobs.iter().find(|(id, _)| path.contains(&format!("/actions/runs/{id}/jobs"))).map_or_else(|| "{}".into(), |(_, j)| j.clone())
        }
    }
}

struct FakeGh<'a>(&'a World);

impl Gh for FakeGh<'_> {
    fn call(&self, _repo: Option<&Path>, args: &[&str]) -> GhOut {
        GhOut { code: 0, stdout: self.0.answer(args[1]).into_bytes() }
    }
    fn call_merged(&self, repo: Option<&Path>, args: &[&str], _input: Option<&[u8]>) -> GhOut {
        self.call(repo, args)
    }
}

fn serve(world: std::sync::Arc<World>) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let n = c.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = world.answer(req.split_whitespace().nth(1).unwrap_or(""));
            let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
        }
    });
    format!("http://{addr}")
}

fn job(name: &str, age: u64, labels: &[&str]) -> String {
    format!(r#"{{"jobs":[{{"name":"{name}","status":"queued","created_at":"{}","labels":{}}}]}}"#, stamp(age), serde_json::to_string(labels).unwrap())
}

fn both(world: World) -> (Vec<String>, Vec<String>) {
    let by_gh = forge::cmds::stranded_runners(&FakeGh(&world), Path::new("/r"), NOW, 300).lines;
    let world = std::sync::Arc::new(world);
    let api = serve(world);
    (Forge { api, token: String::new() }.stranded_runners("acme/widgets", NOW, 300), by_gh)
}

const RUNS: &str = r#"{"workflow_runs":[{"id":55,"status":"queued"},{"id":56,"status":"completed"}]}"#;

#[test]
fn the_two_readings_agree_on_every_shape_of_a_queued_job() {
    let cases: Vec<(&str, World, usize)> = vec![
        ("no runner carries the label", World { runs: RUNS.into(), runners: r#"{"runners":[]}"#.into(), jobs: vec![(55, job("build", 900, &["self-hosted", "ci-r-55-2"]))] }, 1),
        (
            "an offline runner carries it: named, with its vm id",
            World { runs: RUNS.into(), runners: r#"{"runners":[{"name":"vm-9001-a7","status":"offline","labels":[{"name":"ci-r-55-2"}]}]}"#.into(), jobs: vec![(55, job("build", 900, &["self-hosted", "ci-r-55-2"]))] },
            1,
        ),
        (
            "an online runner carries it",
            World { runs: RUNS.into(), runners: r#"{"runners":[{"name":"vm-9001","status":"online","labels":[{"name":"ci-r-55-2"}]}]}"#.into(), jobs: vec![(55, job("build", 900, &["ci-r-55-2"]))] },
            0,
        ),
        ("the job is younger than the minimum age", World { runs: RUNS.into(), runners: r#"{"runners":[]}"#.into(), jobs: vec![(55, job("build", 100, &["ci-r-55-2"]))] }, 0),
        ("only generic labels", World { runs: RUNS.into(), runners: r#"{"runners":[]}"#.into(), jobs: vec![(55, job("build", 900, &["self-hosted", "Linux", "ubuntu-latest"]))] }, 0),
        ("nothing is running", World { runs: r#"{"workflow_runs":[{"id":56,"status":"completed"}]}"#.into(), runners: r#"{"runners":[]}"#.into(), jobs: vec![] }, 0),
    ];
    for (name, world, stranded) in cases {
        let (ours, theirs) = both(world);
        assert_eq!(ours, theirs, "{name}: the two readings of the forge disagree");
        assert_eq!(ours.len(), stranded, "{name}: {ours:?}");
    }
}
