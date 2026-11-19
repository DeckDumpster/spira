//! T1–T6 for `sweep.reclaim`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-reclaim.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_reclaim::{self as task, CacheFile, Candidate, Carry, Phase, Snapshot, SweepReclaim as H, Verdict};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "3600"), (task::CACHE_CAP_KEY, "40"), (task::TMP_ROOT_KEY, "tmp"), (task::TMP_IDLE_KEY, "6")];
const HOUR: u64 = 3_600_000;
const GIB: u64 = 1 << 30;

/// The cache at 1 GiB under a 40 GiB cap, nothing to reap.
fn idle() -> Snapshot {
    Snapshot { repos: vec![], cache: vec![CacheFile { mtime: 1, bytes: GIB, path: "r/sccache-dav/a".into() }], tmp: vec![] }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_an_hourly_tick() {
    t::t1_registered_and_boots::<H>(SET, HOUR);
}

#[test]
fn t2_a_cache_under_its_cap_and_nothing_idle_is_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&idle()));
}

#[test]
fn t3_a_pass_reclaims_and_a_tick_while_reclaiming_is_refused() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let tmp = Snapshot { tmp: vec![Candidate { path: "tmp/tmp.a".into(), bytes: GIB, age: 9, verdict: Verdict::Reclaim }], ..idle() };
    let (c, _) = task::decide(&h.cfg, &observing, &Input::Observed(tmp), HOUR).unwrap();
    assert_eq!(c.phase, Phase::Reclaiming);
    let refused = task::decide(&h.cfg, &c, &Input::Fire, 2 * HOUR).unwrap_err();
    assert!(refused.contains("reclaiming"), "{refused}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_reclaim.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn low_free_space_and_a_fat_podman_request_only_the_kept_cleanups() {
    let world = r#"{"free_mib":1024,"candidates":[{"path":"r/worktree/a/target","bytes":53687091200,"age":9,"verdict":"reclaim"}],"podman_bytes":53687091200,"cache":[],"tmp":[{"path":"tmp/tmp.a","bytes":1,"age":9,"verdict":"reclaim"}]}"#;
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(world.to_string())).double(task::REMOVE_TMP, |_| Ok("reclaimed".into()));
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r.run_until_idle().unwrap();
    let asked: Vec<String> = r.requested().iter().map(|q| q.name.clone()).collect();
    assert_eq!(asked, [task::OBSERVE, task::REMOVE_TMP], "{asked:?}");
}

#[test]
fn a_cache_over_its_cap_is_trimmed_oldest_first() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let file = |mtime, path: &str| CacheFile { mtime, bytes: 30 * GIB, path: path.into() };
    let snap = Snapshot { cache: vec![file(1, "c/old"), file(2, "c/mid"), file(3, "c/new")], ..idle() };
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(snap), HOUR).unwrap();
    assert_eq!(names(&acts), [task::TRIM_CACHE]);
    assert_eq!(acts[0].input, r#"["c/old","c/mid"]"#, "90 GiB over a 40 GiB cap drops the two oldest");
}

#[test]
fn t5b_an_idle_tmp_directory_is_removed_at_any_free_space_and_a_held_one_is_not() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let cand = |p: &str, v| Candidate { path: p.into(), bytes: GIB, age: 9, verdict: v };
    let snap = Snapshot { tmp: vec![cand("tmp/tmp.a", Verdict::Reclaim), cand("tmp/tmp.b", Verdict::Held), cand("tmp/tmp-1000", Verdict::Special)], ..idle() };
    let (c, acts) = task::decide(&h.cfg, &observing, &Input::Observed(snap), HOUR).unwrap();
    assert_eq!(names(&acts), [task::REMOVE_TMP]);
    assert_eq!((acts[0].input.as_str(), c.pending), ("tmp/tmp.a", 1));
}

use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::tasks::b::kit::FIRE;

/// A pass that reaps one idle tmp directory.
fn reclaiming_one_tmp_dir() -> TaskRig {
    let world = json(&Snapshot { tmp: vec![Candidate { path: "tmp/tmp.a".into(), bytes: GIB, age: 9, verdict: Verdict::Reclaim }], ..idle() });
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(world.clone()));
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&reclaiming_one_tmp_dir(), task::NAME);
    for key in ["phase", "pending"] {
        assert!(rows.iter().any(|k| k == key), "{key} in {rows:?}");
    }
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(reclaiming_one_tmp_dir, || t::handler::<H>(SET).registration(), task::STREAM, runtime::event::EventKind::new(task::TICK, FIRE));
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn loose(dir: &std::path::Path) -> u64 {
    git(dir, &["count-objects", "-v"]).lines().find_map(|l| l.strip_prefix("count: ")?.parse().ok()).unwrap()
}

/// A repo whose history reaches `n` loose blobs, none packed.
fn repo_with_loose_blobs(dir: &std::path::Path, n: usize) {
    git(dir, &["init", "-q"]);
    let mut tree = String::new();
    for i in 0..n {
        let f = dir.join(format!("blob{i}"));
        std::fs::write(&f, format!("blob {i}\n")).unwrap();
        let sha = git(dir, &["hash-object", "-w", f.to_str().unwrap()]);
        std::fs::remove_file(&f).unwrap();
        tree.push_str(&format!("100644 blob {sha}\tb{i}\n"));
    }
    let mut mk = std::process::Command::new("git").arg("-C").arg(dir).arg("mktree").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    std::io::Write::write_all(mk.stdin.as_mut().unwrap(), tree.as_bytes()).unwrap();
    let tree = String::from_utf8(mk.wait_with_output().unwrap().stdout).unwrap().trim().to_string();
    let commit = git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit-tree", &tree, "-m", "c"]);
    git(dir, &["update-ref", "refs/heads/main", &commit]);
}

fn input(name: &str, path: &std::path::Path) -> String {
    serde_json::to_string(&task::Repo { name: name.into(), path: path.display().to_string(), ..Default::default() }).unwrap()
}

#[test]
fn a_maintained_repo_reports_its_loose_objects_before_and_after_and_git_agrees() {
    let d = testkit::TempDir::new("reclaim-maintain");
    repo_with_loose_blobs(d.path(), 200);
    let before = loose(d.path());
    assert!(before >= 200, "the fixture holds the blobs loose: {before}");
    let said = task::maintain(&runtime::ports::SysProc, &input("alpha", d.path())).unwrap();
    assert_eq!(said, format!("alpha: loose objects {before} before, 0 after"));
    assert_eq!(loose(d.path()), 0);
    assert_eq!(git(d.path(), &["rev-list", "--objects", "main"]).lines().count() as u64, before, "every object is still reachable");
}

#[test]
fn a_row_that_is_not_a_repo_is_named_with_gits_stderr() {
    let d = testkit::TempDir::new("reclaim-maintain");
    let err = task::maintain(&runtime::ports::SysProc, &input("ghost", d.path())).unwrap_err();
    assert!(err.starts_with("ghost (") && err.contains("not a git repository"), "{err}");
}

#[test]
fn every_row_of_the_map_is_a_maintain_of_its_own_so_one_failure_stops_no_other() {
    let world = json(&Snapshot { repos: vec![task::Repo { name: "ghost".into(), path: "nowhere".into(), ..Default::default() }, task::Repo { name: "alpha".into(), path: "a".into(), ..Default::default() }], ..idle() });
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(world.clone())).double(task::MAINTAIN, |i| if i.contains("ghost") { Err("ghost: not a repo".into()) } else { Ok("alpha: ok".into()) });
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r.run_until_idle().unwrap();
    let asked: Vec<&str> = r.requested().iter().filter(|q| q.name == task::MAINTAIN).map(|q| q.key.as_str()).collect();
    assert_eq!(asked, ["maintain:ghost", "maintain:alpha"], "{:?}", r.requested());
    r.tick(stream, runtime::event::EventKind::new(task::TICK, FIRE)).expect("a failed row leaves the pass settled, so the next tick is not refused");
}

#[test]
fn the_world_reads_its_repos_from_the_map_and_the_leg_is_registered_on_the_task() {
    let d = testkit::TempDir::new("reclaim-maintain");
    let map = d.path().join("repos");
    std::fs::write(&map, "# name|path|land\nalpha|a|main\nbeta|b|main\n").unwrap();
    let w = spira_runtime::world::World { repo_map: map, ..Default::default() };
    assert_eq!(w.repos().iter().map(|r| (r.name.as_str(), r.path.as_str())).collect::<Vec<_>>(), [("alpha", "a"), ("beta", "b")]);
    let lost = spira_runtime::world::World { repo_map: d.path().join("missing"), ..Default::default() }.repos();
    let err = task::maintain(&runtime::ports::SysProc, &serde_json::to_string(&lost[0]).unwrap()).unwrap_err();
    assert!(err.starts_with("repo map") && err.contains("missing"), "an unreadable map is one reported row, not a dead observe: {err}");
    assert!(spira_runtime::tasks::routes().contains_key(task::MAINTAIN));
    assert!(task::EFFECTS.contains(&task::MAINTAIN));
}

fn plant_lock(dir: &std::path::Path, age_s: u64) -> std::path::PathBuf {
    let lock = dir.join(".git/objects/maintenance.lock");
    let f = std::fs::File::create(&lock).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(age_s)).unwrap();
    lock
}

#[test]
fn a_stale_maintenance_lock_is_removed_and_said_so_rather_than_a_silent_no_op() {
    let d = testkit::TempDir::new("reclaim-lock");
    repo_with_loose_blobs(d.path(), 20);
    let lock = plant_lock(d.path(), 7 * 86_400);
    let said = task::maintain(&runtime::ports::SysProc, &input("alpha", d.path())).unwrap();
    assert!(said.ends_with("0 after (removed a stale maintenance.lock)"), "{said}");
    assert!(!lock.exists());
}

#[test]
fn a_fresh_maintenance_lock_is_an_error_naming_the_repo_and_is_left_alone() {
    let d = testkit::TempDir::new("reclaim-lock");
    repo_with_loose_blobs(d.path(), 20);
    let lock = plant_lock(d.path(), 5);
    let err = task::maintain(&runtime::ports::SysProc, &input("alpha", d.path())).unwrap_err();
    assert!(err.starts_with("alpha (") && err.contains("is held"), "{err}");
    assert!(lock.exists());
}
