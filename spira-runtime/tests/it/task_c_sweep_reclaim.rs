//! T1–T6 for `sweep.reclaim`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-reclaim.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_reclaim::{self as task, CacheFile, Candidate, Carry, Phase, Snapshot, SweepReclaim as H, Verdict};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "3600"), (task::MIN_FREE_KEY, "70"), (task::TARGET_FREE_KEY, "100"), (task::PODMAN_CAP_KEY, "40"), (task::CACHE_CAP_KEY, "40"), ("SPIRA_RECLAIM_IDLE_DAYS", "2"), (task::TMP_ROOT_KEY, "/tmp"), (task::TMP_IDLE_KEY, "6")];
const HOUR: u64 = 3_600_000;
const GIB: u64 = 1 << 30;

/// 90 GiB free, podman at 10 GiB, the cache at 1 GiB.
fn idle() -> Snapshot {
    Snapshot { repos: vec![], require_ok: vec![("r/worktree".into(), true)], free_mib: 90 * 1024, candidates: vec![], podman_bytes: 10 * GIB, cache: vec![CacheFile { mtime: 1, bytes: GIB, path: "r/sccache-dav/a".into() }], tmp: vec![] }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_an_hourly_tick() {
    t::t1_registered_and_boots::<H>(SET, HOUR);
}

#[test]
fn t2_plenty_free_and_everything_under_its_cap_is_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&idle()));
}

#[test]
fn t3_a_pass_reclaims_or_refuses_and_a_tick_while_reclaiming_is_refused() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let low = Snapshot { free_mib: 60 * 1024, candidates: vec![Candidate { path: "r/worktree/a/target".into(), bytes: 50 * GIB, age: 9, verdict: Verdict::Reclaim }], ..idle() };
    let (c, _) = task::decide(&h.cfg, &observing, &Input::Observed(low), HOUR).unwrap();
    assert_eq!(c.phase, Phase::Reclaiming);
    let refused = task::decide(&h.cfg, &c, &Input::Fire, 2 * HOUR).unwrap_err();
    assert!(refused.contains("reclaiming"), "{refused}");
    let (c, acts) = task::decide(&h.cfg, &observing, &Input::Observed(Snapshot { require_ok: vec![("r/worktree".into(), false)], ..idle() }), HOUR).unwrap();
    assert_eq!((c.phase, acts.len()), (Phase::Refused, 0));
    assert!(c.refused_cause.is_some_and(|w| w.contains("r/worktree")));
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_reclaim.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_low_space_prunes_podman_then_the_oldest_targets_and_a_fat_podman_alone_prunes() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let cand = |p: &str, gib: u64, v| Candidate { path: p.into(), bytes: gib * GIB, age: 9, verdict: v };
    let low = Snapshot { free_mib: 60 * 1024, candidates: vec![cand("old", 30, Verdict::Reclaim), cand("held", 30, Verdict::Held), cand("newer", 30, Verdict::Reclaim), cand("newest", 30, Verdict::Reclaim)], ..idle() };
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(low), HOUR).unwrap();
    assert_eq!(names(&acts), [task::PODMAN_PRUNE, task::REMOVE_TARGET, task::REMOVE_TARGET], "reclaimed old, newer (dry); kept held; target 100 GiB reached");
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(Snapshot { podman_bytes: 50 * GIB, ..idle() }), HOUR).unwrap();
    assert_eq!(names(&acts), [task::PODMAN_PRUNE], "podman: system prune -f would run");
}

#[test]
fn t5b_an_idle_tmp_directory_is_removed_at_any_free_space_and_a_held_one_is_not() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let cand = |p: &str, v| Candidate { path: p.into(), bytes: GIB, age: 9, verdict: v };
    let snap = Snapshot { tmp: vec![cand("/tmp/tmp.a", Verdict::Reclaim), cand("/tmp/tmp.b", Verdict::Held), cand("/tmp/tmp-1000", Verdict::Special)], ..idle() };
    let (c, acts) = task::decide(&h.cfg, &observing, &Input::Observed(snap), HOUR).unwrap();
    assert_eq!(names(&acts), [task::REMOVE_TMP]);
    assert_eq!((acts[0].input.as_str(), c.pending), ("/tmp/tmp.a", 1));
}

use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::tasks::b::kit::FIRE;

/// A pass refused because a positive control matched nothing.
fn refused_by_a_failed_control() -> TaskRig {
    let world = json(&Snapshot { require_ok: vec![("r/worktree".into(), false)], ..idle() });
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(world.clone()));
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&refused_by_a_failed_control(), task::NAME);
    for key in ["phase", "refused_cause", "pending"] {
        assert!(rows.iter().any(|k| k == key), "{key} in {rows:?}");
    }
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(refused_by_a_failed_control, || t::handler::<H>(SET).registration(), task::STREAM, runtime::event::EventKind::new(task::TICK, FIRE));
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
