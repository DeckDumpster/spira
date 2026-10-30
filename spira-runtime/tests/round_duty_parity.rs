//! For one fixture world, round-duty.sh and the round-duty handler leave the same rows in the
//! Concierge inbox. The script is the oracle: it runs unmodified against stubbed `spira-lc` and
//! `queue`; the handler runs on a virtual clock against a fake spira-lc socket.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal, Outcome};
use spira_runtime::watchers::round_duty::RoundDuty;
use spira_runtime::world::World;
use spira_runtime::Effects;

const REMIND: u64 = 777;
const IDLE_CUT: u64 = 888;

struct Fixture {
    dir: testkit::TempDir,
}

struct Scenario {
    name: &'static str,
    certified_spira: &'static str,
    cap: u64,
    open: usize,
    status_fails: bool,
    stopped: bool,
    results: Vec<&'static str>,
}

fn scenario(name: &'static str) -> Scenario {
    Scenario { name, certified_spira: r#"[{"bead_id": "sp-aaa", "holds": []}]"#, cap: 1, open: 0, status_fails: false, stopped: false, results: vec![] }
}

impl Fixture {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn build(s: &Scenario) -> Fixture {
        let f = Fixture { dir: testkit::TempDir::new(&format!("rd-{}", s.name)) };
        for d in ["bin", "run/rounds", "run/queue/spira", "run/scoped-stops", "repo"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        let repo = f.p("repo");
        fs::write(f.p("rows"), format!("spira | {0} | queue.local | local/main | |\ndeckdumpster | {0} | queue.forge | origin/main | |\n", repo.display())).unwrap();
        fs::write(f.p("certified.spira"), s.certified_spira).unwrap();
        fs::write(f.p("certified.deckdumpster"), r#"[{"bead_id": "dd-1"}, {"bead_id": "dd-2"}]"#).unwrap();
        let status = if s.open == 0 { "round=none\nrepo=spira\n".to_string() } else { format!("rounds={0}\nround=open\nbatch_id=r-1\n", s.open) };
        fs::write(f.p("qstatus"), status).unwrap();
        for i in 0..s.open {
            let name = if i == 0 { "round".to_string() } else { format!("round.r-{}", i + 1) };
            fs::write(f.p("run/queue/spira").join(name), format!("batch_id=r-{}\nopened={}\n", i + 1, i)).unwrap();
        }
        if !s.results.is_empty() {
            fs::write(f.p("run/rounds/300.result"), "").unwrap();
        }
        if s.stopped {
            fs::write(f.p("run/scoped-stops/repo-spira.json"), "{\n  \"repo\": \"spira\",\n  \"lanes\": [\n    \"rounds\"\n  ]\n}").unwrap();
        }
        let stub = |name: &str, body: String| {
            testkit::write_exe(f.p("bin").join(name), &body);
        };
        stub("spira-lc", format!("#!/usr/bin/env bash\n[ \"$1 $2 $3 $4\" = \"list --state CERTIFIED --repo\" ] && cat {}/certified.\"$5\"\nexit 0\n", f.dir.path().display()));
        let fail = if s.status_fails { "echo boom >&2; exit 1" } else { "" };
        stub("queue", format!("#!/usr/bin/env bash\n{fail}\ncat {}/qstatus\n", f.dir.path().display()));
        fs::write(
            f.p("layer.toml"),
            format!(
                "[spira]\nrun = \"{}\"\nround_min = 1\nround_max_open = {}\nround_due_remind = {REMIND}\nround_idle_cut = {IDLE_CUT}\nrepo_map = \"{}\"\nconcierge_inbox = \"{}\"\n",
                f.p("run").display(),
                s.cap,
                f.p("rows").display(),
                f.p("old-inbox").display()
            ),
        )
        .unwrap();
        f
    }

    fn append_result(&self, line: &str) {
        let mut h = fs::OpenOptions::new().create(true).append(true).open(self.p("run/rounds/300.result")).unwrap();
        writeln!(h, "{line}").unwrap();
    }

    fn serve_lc(&self) -> PathBuf {
        let sock = self.p("lc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let root = self.dir.path().to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let argv: Vec<String> = serde_json::from_str(&line).unwrap();
                let body = match argv.first().map(String::as_str) {
                    Some("list") => fs::read_to_string(root.join(format!("certified.{}", argv[4]))).unwrap_or_else(|_| "[]".into()),
                    Some("show-batch") => r#"{"state": "CI_RUNNING"}"#.into(),
                    _ => String::new(),
                };
                let mut w = &stream;
                writeln!(w, "{{\"exit_code\": 0, \"len\": {}}}", body.len()).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
        });
        sock
    }
}

fn debug_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn rows(inbox: &Path) -> Vec<String> {
    fs::read_to_string(inbox).unwrap_or_default().lines().map(|l| l.split_once(' ').unwrap().1.to_string()).collect()
}

fn old_rows(f: &Fixture, s: &Scenario, ticks: usize) -> Vec<String> {
    assert!(debug_dir().join("spira-config").exists(), "build spira-config first: round-duty.sh resolves the repo map through it");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/round-duty.sh");
    let path = format!("{}:{}:/usr/bin:/bin", f.p("bin").display(), debug_dir().display());
    let mut child = Command::new("bash")
        .arg(&script)
        .args(["watch", "--interval", "1", "--ticks", &ticks.to_string()])
        .env_clear()
        .env("PATH", path)
        .env("HOME", f.dir.path())
        .env("SPIRA_TOML", format!("{}:{}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/testlib/complete.toml").display(), f.p("layer.toml").display()))
        .env("SPIRA_REPO", f.p("repo"))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    if !s.results.is_empty() {
        let first_tick = f.p("run/watchd/round-duty.health");
        let mut waited = 0;
        while !first_tick.exists() {
            assert!(waited < 2000, "round-duty.sh never finished its first tick in {}", s.name);
            std::thread::sleep(std::time::Duration::from_millis(5)); // clock-safe: caps the wait for the script's first-tick health file
            waited += 1;
        }
        for r in &s.results {
            f.append_result(r);
        }
    }
    assert!(child.wait().unwrap().success(), "round-duty.sh failed in {}", s.name);
    rows(&f.p("old-inbox"))
}

fn new_rows(f: &Fixture, s: &Scenario, ticks: usize) -> Vec<String> {
    if !s.results.is_empty() {
        fs::write(f.p("run/rounds/300.result"), "").unwrap();
    }
    let world = World {
        run_dir: f.p("run"),
        queue_dir: f.p("run/queue"),
        repo_map: f.p("rows"),
        home_repo: "spira".into(),
        inbox: f.p("new-inbox"),
        proc_dir: PathBuf::from("/proc"),
        lc_socket: if s.status_fails { f.p("no-such.sock") } else { f.serve_lc() },
        ..World::default()
    };
    let fx = Effects { world, ..Effects::default() };
    let clock = VirtualClock::new(1_800_000_000_000);
    let duty = RoundDuty { interval_ms: 1000, min: 1, max_open: s.cap, remind_secs: REMIND, idle_cut_secs: IDLE_CUT };
    let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![duty.registration()], 0).unwrap();
    let (id, stream, kind) = RoundDuty::boot();
    e.submit(id, stream, kind).unwrap();
    for tick in 0..ticks {
        if tick == 1 {
            for r in &s.results {
                f.append_result(r);
            }
        }
        e.run_until_idle().unwrap();
        loop {
            let pending = e.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = match fx.run(&p.key, &p.spec) {
                    Ok(d) => Outcome::Succeeded(d),
                    Err(d) => Outcome::Failed(d),
                };
                e.complete(&p.key, out).unwrap();
            }
            e.run_until_idle().unwrap();
        }
        clock.advance(1000);
    }
    rows(&f.p("new-inbox"))
}

fn same_rows(s: Scenario, ticks: usize) -> Vec<String> {
    let f = Fixture::build(&s);
    let old = old_rows(&f, &s, ticks);
    let new = new_rows(&f, &s, ticks);
    assert_eq!(new, old, "scenario {}: the handler and round-duty.sh disagree", s.name);
    old
}

#[test]
fn a_free_slot_with_certified_work_is_due() {
    let r = same_rows(scenario("due"), 1);
    assert!(r.iter().any(|l| l.contains("ROUND DUE: 1 certified waiting [spira=1]")), "{r:?}");
    assert!(r.iter().all(|l| !l.contains("deckdumpster")), "{r:?}");
}

#[test]
fn a_held_certified_bead_is_not_due() {
    let r = same_rows(Scenario { certified_spira: r#"[{"bead_id": "sp-aaa", "holds": ["ask"]}]"#, ..scenario("held") }, 1);
    assert!(r.is_empty(), "{r:?}");
}

#[test]
fn an_open_round_suppresses_due_unless_the_cap_allows_another() {
    assert!(same_rows(Scenario { open: 1, ..scenario("capped") }, 1).is_empty());
    let r = same_rows(Scenario { open: 1, cap: 2, ..scenario("slot") }, 1);
    assert!(r.iter().any(|l| l.contains("1 of 2 rounds open")), "{r:?}");
}

#[test]
fn a_stopped_lane_is_announced_and_never_due() {
    let r = same_rows(Scenario { stopped: true, ..scenario("stopped") }, 1);
    assert!(r.iter().any(|l| l.contains("ROUNDS STOPPED ON PURPOSE: spira")), "{r:?}");
    assert!(r.iter().all(|l| !l.contains("ROUND DUE")), "{r:?}");
}

#[test]
fn an_unanswered_round_status_is_unknown_and_pages_nothing() {
    let r = same_rows(Scenario { status_fails: true, ..scenario("unknown") }, 1);
    assert!(r.iter().any(|l| l.contains("ROUND STATE UNKNOWN")), "{r:?}");
    assert!(r.iter().all(|l| !l.contains("ROUND DUE")), "{r:?}");
}

#[test]
fn result_lines_are_relayed_once_and_a_verdictless_round_is_named() {
    let r = same_rows(Scenario { open: 1, results: vec!["red sp-aaa", "round 300: no verdict from any VM"], ..scenario("results") }, 3);
    assert_eq!(r.iter().filter(|l| l.contains("ROUND RESULT 300: red sp-aaa")).count(), 1, "{r:?}");
    assert_eq!(r.iter().filter(|l| l.contains("ROUND NO VERDICTS 300")).count(), 1, "{r:?}");
}

#[test]
fn the_migrated_handlers_launch_no_process() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<_> = fs::read_dir(src.join("watchers")).unwrap().flatten().map(|e| e.path()).collect();
    files.extend([src.join("world.rs"), src.join("forge.rs")]);
    let mut checked = 0;
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains("Command::new") && !code.contains("std::process"), "{} launches a process", path.display());
        checked += 1;
    }
    assert!(checked >= 2, "the scan found no handlers to check");
    let planted = "fn f() { Command::new(\"x\"); }";
    assert!(planted.contains("Command::new"), "the matcher must be able to see an offender");
}
