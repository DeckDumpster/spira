use exec_census::{unit_of_cgroup, Probe, ProcFs, Sampler, Window, COVERAGE, EXECS, FORKS, NO_UNIT};
use std::path::Path;
use std::time::SystemTime;
use testkit::TempDir;

fn put(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn counters(root: &Path, last_pid: u32, forks: u64) {
    put(root, "sys/kernel/ns_last_pid", format!("{last_pid}\n").as_bytes());
    put(root, "sys/kernel/pid_max", b"4194304\n");
    put(root, "stat", format!("cpu  1 2 3\nprocesses {forks}\nprocs_running 1\n").as_bytes());
}

fn proc_(root: &Path, pid: u32, comm: &str, argv: &[&str], cgroup: &str) {
    put(root, &format!("{pid}/status"), format!("Name:\t{comm}\nTgid:\t{pid}\nPid:\t{pid}\n").as_bytes());
    put(root, &format!("{pid}/comm"), format!("{comm}\n").as_bytes());
    put(root, &format!("{pid}/cmdline"), argv.join("\0").as_bytes());
    put(root, &format!("{pid}/cgroup"), format!("0::{cgroup}\n").as_bytes());
}

const SVC: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice/spira-perf-watch.service";

#[test]
fn a_fixture_tree_attributes_each_new_pid_to_its_unit() {
    let t = TempDir::new("exec-census");
    counters(t.path(), 100, 1000);
    let fs = ProcFs::new(t.path());
    let mut s = Sampler::new();
    assert_eq!(s.tick(&fs).forks, 0, "the first tick only baselines");

    counters(t.path(), 104, 1004);
    proc_(t.path(), 101, "sh", &["/bin/sh", "-c", "secret"], SVC);
    proc_(t.path(), 102, "git", &["git", "status"], SVC);
    proc_(t.path(), 103, "dolt", &["dolt", "sql"], "/system.slice/dolt.service");
    proc_(t.path(), 104, "bash", &["bash"], "/user.slice/user-1000.slice/session-3.scope");
    proc_(t.path(), 99, "old", &["old"], SVC);
    let tick = s.tick(&fs);
    assert_eq!(tick.forks, 4);
    let mut w = Window::default();
    w.add(tick);
    assert_eq!(w.by_unit["spira-perf-watch.service"], 2);
    assert_eq!(w.by_unit["dolt.service"], 1);
    assert_eq!(w.by_unit["session.scope"], 1);
    assert_eq!(w.execs(), 4, "pid 99 predates the window and is not counted");
    assert_eq!(w.coverage(), 1.0);
}

#[test]
fn coverage_reports_what_the_probes_could_not_reach() {
    let t = TempDir::new("exec-census");
    counters(t.path(), 100, 1000);
    let fs = ProcFs::new(t.path());
    let mut s = Sampler::new();
    s.tick(&fs);
    counters(t.path(), 110, 1010);
    proc_(t.path(), 105, "a", &["a"], SVC);
    proc_(t.path(), 106, "b", &["b"], SVC);
    let mut w = Window::default();
    w.add(s.tick(&fs));
    assert_eq!((w.execs(), w.gone, w.forks), (2, 8, 10));
    assert!((w.coverage() - 0.2).abs() < 1e-9, "{}", w.coverage());
    let out = w.series(60.0, SystemTime::now());
    let val = |n: &str| out.iter().find(|s| s.name == n).unwrap().value;
    assert_eq!(val(COVERAGE), w.coverage());
    assert_eq!(val(FORKS), 10.0);
    let e = out.iter().find(|s| s.name == EXECS).unwrap();
    assert_eq!((e.value, e.labels.clone()), (2.0, vec![("unit".to_string(), "spira-perf-watch.service".to_string())]));
}

#[test]
fn rates_are_per_minute_whatever_the_window() {
    let mut w = Window::default();
    w.by_unit.insert("x.service".into(), 30);
    let out = w.series(30.0, SystemTime::now());
    assert_eq!(out.iter().find(|s| s.name == EXECS).unwrap().value, 60.0);
}

#[test]
fn a_thread_is_counted_as_a_launch_but_not_as_an_exec() {
    let t = TempDir::new("exec-census");
    counters(t.path(), 100, 1000);
    let fs = ProcFs::new(t.path());
    let mut s = Sampler::new();
    s.tick(&fs);
    counters(t.path(), 102, 1002);
    proc_(t.path(), 101, "sh", &["sh"], SVC);
    put(t.path(), "102/status", b"Name:\tworker\nTgid:\t101\nPid:\t102\n");
    assert_eq!(fs.probe(102), Probe::Thread);
    let mut w = Window::default();
    w.add(s.tick(&fs));
    assert_eq!((w.execs(), w.threads), (1, 1));
    assert_eq!(w.coverage(), 1.0);
}

#[test]
fn argv_is_cut_after_two_entries_and_environ_is_never_opened() {
    let t = TempDir::new("exec-census");
    proc_(t.path(), 7, "tool", &["/usr/bin/tool", "sub", "--token=hunter2", "x"], SVC);
    put(t.path(), "7/environ", b"SECRET=1\0");
    std::fs::set_permissions(t.path().join("7/environ"), std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let Probe::Process(p) = ProcFs::new(t.path()).probe(7) else { panic!("not a process") };
    assert_eq!(p.argv, ["/usr/bin/tool", "sub"]);
    assert!(!format!("{p:?}").contains("hunter2"));
    let named: Vec<&str> = include_str!("../src/lib.rs").lines().filter(|l| l.contains(&["env", "iron"].concat())).collect();
    assert!(named.iter().all(|l| l.starts_with("//!")), "code names the file: {named:?}");
}

#[test]
fn a_wrapped_pid_counter_probes_both_ends_of_the_range() {
    let t = TempDir::new("exec-census");
    put(t.path(), "sys/kernel/pid_max", b"12\n");
    put(t.path(), "sys/kernel/ns_last_pid", b"11\n");
    put(t.path(), "stat", b"processes 50\n");
    let fs = ProcFs::new(t.path());
    let mut s = Sampler::new();
    s.tick(&fs);
    put(t.path(), "sys/kernel/ns_last_pid", b"3\n");
    put(t.path(), "stat", b"processes 54\n");
    for pid in [12, 2, 3] {
        proc_(t.path(), pid, "p", &["p"], SVC);
    }
    let tick = s.tick(&fs);
    assert_eq!((tick.procs.len(), tick.gone, tick.forks), (3, 0, 4));
}

#[test]
fn cgroup_paths_name_their_innermost_unit() {
    assert_eq!(unit_of_cgroup(&format!("0::{SVC}\n")), "spira-perf-watch.service");
    assert_eq!(unit_of_cgroup("0::/system.slice/spira-watch@alpha.service\n"), "spira-watch@alpha.service");
    assert_eq!(unit_of_cgroup("0::/user.slice/run-r5f3a9.scope\n"), "run.scope");
    assert_eq!(unit_of_cgroup("0::/user.slice/app-gnome-term-1234.scope\n"), "app-gnome-term.scope");
    assert_eq!(unit_of_cgroup("0::/\n"), NO_UNIT);
    assert_eq!(unit_of_cgroup("12:cpu:/system.slice/x.service\n"), "x.service");
}
