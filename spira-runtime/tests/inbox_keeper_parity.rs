//! For one fixture world, inbox-keeper.sh and the inbox-keeper handler type the same wake into
//! the Concierge. The script is the oracle: it runs unmodified against a recording stand-in for
//! `concierge.sh`; the handler runs on a virtual clock and reaches the same stand-in.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, Executor, MemJournal, Outcome};
use spira_runtime::watchers::inbox_keeper::InboxKeeper;
use spira_runtime::world::World;
use spira_runtime::Effects;

const STALL: u64 = 0;
const BACKOFF: u64 = 4321;

struct Scenario {
    name: &'static str,
    inbox_ahead: bool,
    triage: bool,
}

fn triage_running() -> bool {
    fs::read_dir("/proc").unwrap().flatten().any(|e| fs::read(e.path().join("cmdline")).is_ok_and(|c| String::from_utf8_lossy(&c).replace('\0', " ").contains("inbox-triage")))
}

fn debug_dir() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf()
}

fn build(s: &Scenario) -> testkit::TempDir {
    let d = testkit::TempDir::new(&format!("ik-{}", s.name));
    fs::create_dir_all(d.path().join("repo")).unwrap();
    testkit::write_exe(d.path().join("repo/concierge.sh"), &format!("#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> {}/wakes\n", d.path().display()));
    fs::write(
        d.path().join("layer.toml"),
        format!("[spira]\nconcierge_inbox = \"{0}/inbox\"\nconcierge_inbox_stall = {STALL}\nconcierge_inbox_backoff = {BACKOFF}\nrun = \"{0}/run\"\n", d.path().display()),
    )
    .unwrap();
    d
}

fn set_inbox(d: &Path, mtime: SystemTime) {
    let f = fs::File::create(d.join("inbox")).unwrap();
    f.set_modified(mtime).unwrap();
}

fn decoy(s: &Scenario) -> Option<Child> {
    s.triage.then(|| {
        use std::os::unix::process::CommandExt;
        let mut c = Command::new("sleep");
        c.arg0("inbox-triage").arg("30").stdout(Stdio::null());
        c.spawn().unwrap()
    })
}

fn wakes(d: &Path) -> Vec<String> {
    fs::read_to_string(d.join("wakes")).unwrap_or_default().lines().map(String::from).collect()
}

fn mtime(s: &Scenario, base: SystemTime) -> SystemTime {
    if s.inbox_ahead { base + Duration::from_secs(100) } else { SystemTime::UNIX_EPOCH + Duration::from_secs(100) }
}

fn old_wakes(s: &Scenario) -> Vec<String> {
    let d = build(s);
    set_inbox(d.path(), mtime(s, SystemTime::now()));
    let mut triage = decoy(s);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/inbox-keeper.sh");
    let path = format!("{}:/usr/bin:/bin", debug_dir().display());
    let mut child = Command::new("timeout")
        .args(["2", "bash"])
        .arg(&script)
        .env_clear()
        .env("PATH", path)
        .env("HOME", d.path())
        .env("SPIRA_TOML", format!("{}:{}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira-config/tests/fixtures/complete.toml").display(), d.path().join("layer.toml").display()))
        .env("SPIRA_REPO", d.path().join("repo"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.wait().unwrap();
    if let Some(t) = triage.as_mut() {
        t.kill().unwrap();
        t.wait().unwrap();
    }
    wakes(d.path())
}

fn new_wakes(s: &Scenario) -> Vec<String> {
    let d = build(s);
    let epoch_ms = 1_800_000_000_000u64;
    set_inbox(d.path(), mtime(s, SystemTime::UNIX_EPOCH + Duration::from_millis(epoch_ms)));
    let mut triage = decoy(s);
    fs::write(d.path().join("map"), format!("spira|{}|main\n", d.path().join("repo").display())).unwrap();
    let world = World { inbox: d.path().join("inbox"), repo_map: d.path().join("map"), home_repo: "spira".into(), proc_dir: PathBuf::from("/proc"), ..World::default() };
    let fx = Effects { world, ..Effects::default() };
    let clock = VirtualClock::new(epoch_ms);
    let keeper = InboxKeeper { interval_ms: 1000, stall_secs: STALL, backoff_secs: BACKOFF };
    let mut e = Engine::open(MemJournal::new(), Arc::new(clock.clone()), vec![keeper.registration()], 0).unwrap();
    let (id, stream, kind) = InboxKeeper::boot();
    e.submit(id, stream, kind).unwrap();
    e.run_until_idle().unwrap();
    loop {
        let pending = e.pending_effects();
        if pending.is_empty() {
            break;
        }
        for p in pending {
            let out = match fx.run(&p.key, &p.spec) {
                Ok(v) => Outcome::Succeeded(v),
                Err(v) => Outcome::Failed(v),
            };
            e.complete(&p.key, out).unwrap();
        }
        e.run_until_idle().unwrap();
    }
    if let Some(t) = triage.as_mut() {
        t.kill().unwrap();
        t.wait().unwrap();
    }
    wakes(d.path())
}

fn same_wakes(s: Scenario) -> Vec<String> {
    assert!(debug_dir().join("spira-config").exists(), "build spira-config first: inbox-keeper.sh resolves its config through it");
    if !s.triage && triage_running() {
        eprintln!("skipped {}: an inbox-triage process is running on this box", s.name);
        return vec![];
    }
    let old = old_wakes(&s);
    let new = new_wakes(&s);
    assert_eq!(new, old, "scenario {}: the handler and inbox-keeper.sh disagree", s.name);
    old
}

#[test]
fn a_stalled_monitor_with_events_waiting_is_woken_once() {
    let w = same_wakes(Scenario { name: "wake", inbox_ahead: true, triage: false });
    assert!(w.is_empty() && triage_running() || w.len() == 1 && w[0].contains("re-arm: Monitor inbox-triage"), "{w:?}");
}

#[test]
fn an_inbox_older_than_the_last_sighting_wakes_nobody() {
    assert!(same_wakes(Scenario { name: "old", inbox_ahead: false, triage: false }).is_empty());
}

#[test]
fn a_running_monitor_wakes_nobody() {
    assert!(same_wakes(Scenario { name: "alive", inbox_ahead: true, triage: true }).is_empty());
}
