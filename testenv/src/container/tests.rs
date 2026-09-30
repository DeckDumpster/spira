//! `testenv container` against a scripted host. These replace the stub-podman suites
//! (DESIGN.md §12.3 D20): test-testenv-image-tag.sh, test-testenv-registry.sh,
//! test-testenv-image-heartbeat.sh, test-testenv-resource-diagnosis.sh and
//! test-testenv-owner-guard.sh.

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

type Rule = Box<dyn Fn(&[String]) -> Option<(i32, String)>>;

#[derive(Default)]
struct Fake {
    files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    calls: RefCell<Vec<Vec<String>>>,
    rules: RefCell<Vec<Rule>>,
    err: RefCell<Vec<String>>,
    out: RefCell<Vec<String>>,
    slept: Cell<u64>,
    clock: Cell<u64>,
    /// pid -> ppid; a pid present here is live.
    procs: RefCell<BTreeMap<u32, u32>>,
    me: u32,
    /// The build: how many waits until it exits, its rc, and its log.
    build_waits: Cell<u32>,
    build_rc: Cell<i32>,
    build_log: RefCell<String>,
    inotify: Cell<u64>,
    sysctls: RefCell<BTreeMap<String, String>>,
    replaced: RefCell<Option<Vec<String>>>,
}

struct FakeBuild<'a>(&'a Fake);

impl Build for FakeBuild<'_> {
    fn wait(&mut self, d: Duration) -> Option<i32> {
        let n = self.0.build_waits.get();
        if n == 0 {
            return Some(self.0.build_rc.get());
        }
        self.0.build_waits.set(n - 1);
        self.0.clock.set(self.0.clock.get() + d.as_secs());
        None
    }
}

impl Fake {
    fn new() -> Fake {
        let f = Fake {
            me: 500,
            ..Default::default()
        };
        f.procs.borrow_mut().insert(500, 400);
        f.procs.borrow_mut().insert(400, 1);
        f.procs.borrow_mut().insert(1, 0);
        f
    }
    fn file(&self, p: &str, s: &str) {
        self.files
            .borrow_mut()
            .insert(PathBuf::from(p), s.as_bytes().to_vec());
    }
    fn on(&self, r: impl Fn(&[String]) -> Option<(i32, String)> + 'static) {
        self.rules.borrow_mut().push(Box::new(r));
    }
    /// A rule matching calls whose argv starts with `prefix`.
    fn when(&self, prefix: &[&str], rc: i32, out: &str) {
        let p: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
        let out = out.to_string();
        self.on(move |a| a.starts_with(&p).then(|| (rc, out.clone())));
    }
    fn calls_with(&self, first: &str) -> Vec<Vec<String>> {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c[0] == first)
            .cloned()
            .collect()
    }
    fn called(&self, argv: &[&str]) -> bool {
        self.calls.borrow().iter().any(|c| c == argv)
    }
    fn errs(&self) -> String {
        self.err.borrow().join("\n")
    }
}

impl Host for Fake {
    fn podman(&self, args: &[String], _io: Io) -> (i32, String) {
        self.calls.borrow_mut().push(args.to_vec());
        for r in self.rules.borrow().iter().rev() {
            if let Some(x) = r(args) {
                return x;
            }
        }
        (0, String::new())
    }
    fn build(&self, args: &[String], log: &Path) -> Box<dyn Build + '_> {
        let mut a = vec!["build".to_string()];
        a.extend(args.iter().skip(1).cloned());
        self.calls.borrow_mut().push(a);
        self.files.borrow_mut().insert(
            log.to_path_buf(),
            self.build_log.borrow().clone().into_bytes(),
        );
        Box::new(FakeBuild(self))
    }
    fn exec_replace(&self, args: &[String]) -> i32 {
        *self.replaced.borrow_mut() = Some(args.to_vec());
        0
    }
    fn read(&self, p: &Path) -> Option<Vec<u8>> {
        self.files.borrow().get(p).cloned()
    }
    fn is_file(&self, p: &Path) -> bool {
        self.files.borrow().contains_key(p)
    }
    fn write(&self, p: &Path, s: &str) {
        self.file(&p.display().to_string(), s)
    }
    fn remove(&self, p: &Path) {
        self.files.borrow_mut().remove(p);
    }
    fn temp_log(&self) -> PathBuf {
        PathBuf::from("/t/build.log")
    }
    fn sleep(&self, d: Duration) {
        self.slept.set(self.slept.get() + d.as_millis() as u64);
        self.clock.set(self.clock.get() + d.as_secs());
    }
    fn now(&self) -> u64 {
        self.clock.get()
    }
    fn pid(&self) -> u32 {
        self.me
    }
    fn parent_pid(&self) -> u32 {
        400
    }
    fn pid_live(&self, pid: u32) -> bool {
        self.procs.borrow().contains_key(&pid)
    }
    fn ppid_of(&self, pid: u32) -> Option<u32> {
        self.procs.borrow().get(&pid).copied()
    }
    fn inotify_used(&self) -> u64 {
        self.inotify.get()
    }
    fn sysctl(&self, path: &str) -> Option<String> {
        self.sysctls.borrow().get(path).cloned()
    }
    fn free_disk(&self, _: &Path) -> String {
        "3.1G".into()
    }
    fn free_mem(&self) -> String {
        "7.5Gi".into()
    }
    fn owner_dir(&self) -> PathBuf {
        PathBuf::from("/tmp")
    }
    fn err(&self, line: &str) {
        self.err.borrow_mut().push(line.to_string());
    }
    fn out(&self, line: &str) {
        self.out.borrow_mut().push(line.to_string());
    }
}

fn conf(root: &str) -> Conf {
    Conf {
        harness: Some(PathBuf::from(root)),
        bd_pin: Some(PathBuf::from("/run/bd-pin")),
        registry: String::new(),
        max_concurrent: 8,
        queue_timeout: 900,
        queue_poll: 5,
        heartbeat: 60,
        basic_wait_ticks: 20,
        basic_retry_sleep: 2,
    }
}

fn harness(f: &Fake, root: &str) {
    f.file(
        &format!("{root}/spira/testenv/Containerfile"),
        "FROM ubuntu:24.04\n",
    );
    f.file(
        &format!("{root}/spira/deps.toml"),
        "[bd]\ntier = \"fatal\"\n",
    );
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn tag_of(f: &Fake, c: &Conf) -> Option<String> {
    Driver { host: f, conf: c }.image_tag()
}

// ---- tag: the build closure (was test-testenv-image-tag.sh) ------------------------------

#[test]
fn the_tag_is_the_scripts_hash_of_the_closure_by_content() {
    let f = Fake::new();
    harness(&f, "/h");
    f.file("/run/bd-pin", "migrations=12\n");
    let c = conf("/h");
    // What testenv.sh computed: sha256sum < Containerfile; cat pin; sha256sum < deps.toml,
    // all through sha256sum, first 12 hex.
    let line = |b: &str| format!("{}  -\n", crate::verdict::sha256_hex(b.as_bytes()));
    let closure = format!(
        "{}{}{}",
        line("FROM ubuntu:24.04\n"),
        "migrations=12\n",
        line("[bd]\ntier = \"fatal\"\n")
    );
    let want = crate::verdict::sha256_hex(closure.as_bytes())[..12].to_string();
    assert_eq!(tag_of(&f, &c), Some(want));
}

#[test]
fn every_closure_input_moves_the_tag_and_nothing_else_does() {
    let f = Fake::new();
    harness(&f, "/h");
    let c = conf("/h");
    let base = tag_of(&f, &c).unwrap();
    assert_eq!(base.len(), 12);
    f.file("/h/spira/unrelated.sh", "echo hi");
    assert_eq!(tag_of(&f, &c).unwrap(), base, "a file outside the closure");
    f.file("/h/spira/deps.toml", "[bd]\ntier = \"warn\"\n");
    let deps = tag_of(&f, &c).unwrap();
    assert_ne!(deps, base, "deps.toml");
    f.file("/run/bd-pin", "migrations=13\n");
    let pin = tag_of(&f, &c).unwrap();
    assert_ne!(pin, deps, "the bd pin");
    f.file("/h/spira/testenv/Containerfile", "FROM ubuntu:26.04\n");
    assert_ne!(tag_of(&f, &c).unwrap(), pin, "the Containerfile");
}

#[test]
fn two_checkouts_of_one_commit_compute_one_tag() {
    let f = Fake::new();
    harness(&f, "/a");
    harness(&f, "/b/deeper");
    assert_eq!(tag_of(&f, &conf("/a")), tag_of(&f, &conf("/b/deeper")));
}

#[test]
fn an_unreadable_closure_refuses_instead_of_naming_an_image() {
    let f = Fake::new();
    f.file("/h/spira/deps.toml", "x");
    let c = conf("/h");
    let d = Driver { host: &f, conf: &c };
    assert_eq!(d.cmd_tag(), 1);
    assert!(f.out.borrow().is_empty());
    assert!(f.errs().contains("Containerfile"));
    let f = Fake::new();
    f.file("/h/spira/testenv/Containerfile", "x");
    assert_eq!(tag_of(&f, &c), None);
    assert!(f.errs().contains("deps.toml"));
    let none = Conf {
        harness: None,
        ..conf("/h")
    };
    let f = Fake::new();
    assert_eq!(
        Driver {
            host: &f,
            conf: &none
        }
        .cmd_tag(),
        1
    );
    assert!(f.errs().contains("SPIRA_TESTENV_HARNESS"));
}

// ---- image acquisition and publish (was test-testenv-registry.sh) ------------------------

#[test]
fn a_local_image_is_used_as_is() {
    let f = Fake::new();
    harness(&f, "/h");
    let c = conf("/h");
    let d = Driver { host: &f, conf: &c };
    assert_eq!(d.cmd_image(), 0);
    let img = f.out.borrow()[0].clone();
    assert!(img.starts_with("localhost/spira-testenv:"));
    assert!(f.calls_with("pull").is_empty() && f.calls_with("build").is_empty());
}

#[test]
fn no_registry_means_nothing_is_pulled() {
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    let c = conf("/h");
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 0);
    assert!(f.calls_with("pull").is_empty());
    let b = f.calls_with("build");
    assert_eq!(b.len(), 1);
    assert_eq!(
        b[0][1..],
        args(&[
            "-t",
            &f.out.borrow()[0],
            "-f",
            "/h/spira/testenv/Containerfile",
            "/h/spira"
        ])[..]
    );
}

#[test]
fn a_registry_hit_pulls_the_closure_tag_retags_it_and_builds_nothing() {
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    let c = Conf {
        registry: "ghcr.io/org/".into(),
        ..conf("/h")
    };
    let tag = tag_of(&f, &c).unwrap();
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 0);
    let remote = format!("ghcr.io/org/spira-testenv:{tag}");
    let local = format!("localhost/spira-testenv:{tag}");
    assert!(f.called(&["pull", "-q", &remote]));
    assert!(f.called(&["tag", &remote, &local]));
    assert!(f.calls_with("build").is_empty());
    assert_eq!(
        f.out.borrow()[0],
        local,
        "callers get the local ref either way"
    );
}

#[test]
fn a_registry_miss_is_slow_never_fatal() {
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    f.when(&["pull"], 125, "");
    let c = Conf {
        registry: "ghcr.io/org".into(),
        ..conf("/h")
    };
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 0);
    assert_eq!(f.calls_with("build").len(), 1);
    assert!(f.errs().contains("is not in the registry — building it"));
}

#[test]
fn publish_pushes_the_closure_tag_and_nothing_floating() {
    let f = Fake::new();
    harness(&f, "/h");
    let c = Conf {
        registry: "ghcr.io/org".into(),
        ..conf("/h")
    };
    let tag = tag_of(&f, &c).unwrap();
    assert_eq!(Driver { host: &f, conf: &c }.cmd_publish(), 0);
    let remote = format!("ghcr.io/org/spira-testenv:{tag}");
    assert!(f.called(&["tag", &format!("localhost/spira-testenv:{tag}"), &remote]));
    assert_eq!(f.calls_with("push"), vec![args(&["push", &remote])]);
    assert!(!f
        .calls
        .borrow()
        .iter()
        .flatten()
        .any(|a| a.contains(":latest")));
    assert_eq!(*f.out.borrow(), vec![remote]);
}

#[test]
fn publish_without_a_registry_or_after_a_failed_push_fails() {
    let f = Fake::new();
    harness(&f, "/h");
    let c = conf("/h");
    assert_eq!(Driver { host: &f, conf: &c }.cmd_publish(), 1);
    assert!(f.errs().contains("SPIRA_TESTENV_REGISTRY is unset"));
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["push"], 1, "");
    let c = Conf {
        registry: "r".into(),
        ..conf("/h")
    };
    assert_eq!(Driver { host: &f, conf: &c }.cmd_publish(), 1);
    assert!(f.errs().contains("push failed"));
}

// ---- cold build heartbeat (was test-testenv-image-heartbeat.sh) --------------------------

#[test]
fn a_slow_build_beats_every_interval_naming_its_multistage_step() {
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    f.build_waits.set(3);
    *f.build_log.borrow_mut() =
        "[1/3] STEP 1/4: FROM golang AS bd\n[2/3] STEP 3/4: RUN go build ./cmd/bd\ncompiling dolt\n\n".into();
    let c = Conf {
        heartbeat: 7,
        ..conf("/h")
    };
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 0);
    let beats: Vec<String> = f
        .err
        .borrow()
        .iter()
        .filter(|l| l.starts_with("testenv: building — "))
        .cloned()
        .collect();
    assert_eq!(beats.len(), 3);
    assert!(
        beats[0].contains("[2/3] STEP 3/4: RUN go build"),
        "{}",
        beats[0]
    );
    assert!(beats[0].contains("elapsed 7s") && beats[2].contains("elapsed 21s"));
    assert!(beats[0].contains("disk 3.1G free, memory 7.5Gi free"));
    assert!(f.errs().contains("testenv:   last output: compiling dolt"));
    assert!(f.errs().contains("at least every 7s"));
    assert!(
        !f.is_file(Path::new("/t/build.log")),
        "the build log is removed"
    );
}

#[test]
fn a_silent_build_says_so_rather_than_naming_no_step() {
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    f.build_waits.set(1);
    let c = conf("/h");
    Driver { host: &f, conf: &c }.cmd_image();
    assert!(f.errs().contains("building — (no build output yet) —"));
}

#[test]
fn a_failed_build_names_disk_exhaustion_or_shows_its_tail() {
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    f.build_rc.set(1);
    *f.build_log.borrow_mut() =
        "STEP 2/9: COPY . .\nError: write /var/tmp/x: no space left on device\n".into();
    let c = conf("/h");
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 1);
    assert!(f.errs().contains("disk exhausted on this VM (3.1G free)"));
    assert!(f.out.borrow().is_empty());

    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    f.build_rc.set(2);
    *f.build_log.borrow_mut() = (0..50)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 1);
    let e = f.errs();
    assert!(e.contains("check Containerfile in /h/spira/testenv"));
    assert!(e.contains("line 49") && e.contains("line 10") && !e.contains("line 9\n"));
}

// ---- boot failure diagnosis and admission (was test-testenv-resource-diagnosis.sh) -------

fn booting(f: &Fake) {
    harness(f, "/h");
    f.when(&["container", "exists"], 1, "");
}

#[test]
fn up_boots_with_the_label_limit_and_volumes_and_records_its_caller() {
    let f = Fake::new();
    booting(&f);
    let c = conf("/h");
    let tag = tag_of(&f, &c).unwrap();
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "n1", "--checkout", "/wt"])),
        0
    );
    let run = f.calls_with("run");
    assert_eq!(
        run[0],
        args(&[
            "run",
            "-d",
            "--name",
            "n1",
            "--systemd=true",
            "--pids-limit",
            "8192",
            "--label",
            "spira.testenv=1",
            "--volume",
            "/wt:/workspace:z",
            "--volume",
            "n1-cargo-reg:/var/spira/cargo/registry",
            "--volume",
            "n1-cargo-git:/var/spira/cargo/git",
            &format!("localhost/spira-testenv:{tag}"),
        ])
    );
    assert_eq!(f.read(Path::new("/tmp/n1.owner")), Some(b"400\n".to_vec()));
    assert!(f.called(&["exec", "n1", "loginctl", "enable-linger", "spirauser"]));
    assert!(f.called(&[
        "exec",
        "n1",
        "git",
        "config",
        "--system",
        "--add",
        "safe.directory",
        "/workspace"
    ]));
    assert!(f.errs().contains("user@1001.service active"));
}

#[test]
fn up_defaults_to_the_harness_checkout_and_keeps_an_existing_owner() {
    let f = Fake::new();
    booting(&f);
    f.file("/tmp/spira-testenv.owner", "77\n");
    let c = conf("/h");
    assert_eq!(Driver { host: &f, conf: &c }.cmd_up(&[]), 0);
    assert!(f.calls_with("run")[0].contains(&"/h:/workspace:z".to_string()));
    assert_eq!(
        f.read(Path::new("/tmp/spira-testenv.owner")),
        Some(b"77\n".to_vec())
    );
}

#[test]
fn up_on_an_existing_container_does_nothing() {
    let f = Fake::new();
    harness(&f, "/h");
    let c = conf("/h");
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "x"])),
        0
    );
    assert!(f.calls_with("run").is_empty());
    assert!(f.errs().contains("x already exists; nothing to do"));
}

#[test]
fn a_failed_podman_run_refuses_at_once() {
    let f = Fake::new();
    booting(&f);
    f.when(&["run"], 125, "");
    let c = conf("/h");
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "n"])),
        1
    );
    assert!(!f.is_file(Path::new("/tmp/n.owner")));
    assert!(
        f.calls_with("exec").is_empty(),
        "no basic.target wait on a container never created"
    );
}

#[test]
fn a_boot_failure_retries_once_then_names_the_exhausted_resource_and_removes_it() {
    let f = Fake::new();
    booting(&f);
    f.when(
        &["exec", "n", "systemctl", "is-active", "basic.target"],
        3,
        "",
    );
    f.when(
        &["exec", "n", "cat", "/sys/fs/cgroup/pids.current"],
        0,
        "8100\n8192\n",
    );
    f.when(
        &["exec", "n", "systemctl", "show"],
        0,
        "TasksCurrent=40\nTasksMax=infinity\n",
    );
    f.when(&["exec", "n", "sh", "-c"], 0, "12\n");
    f.when(
        &["exec", "n", "cat", "/proc/sys/kernel/keys/maxkeys"],
        0,
        "200\n",
    );
    f.inotify.set(100);
    f.sysctls.borrow_mut().insert(
        "/proc/sys/fs/inotify/max_user_instances".into(),
        "1024\n".into(),
    );
    f.sysctls.borrow_mut().insert(
        "/proc/sys/user/max_inotify_instances".into(),
        "512\n".into(),
    );
    let c = Conf {
        basic_wait_ticks: 3,
        ..conf("/h")
    };
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "n"])),
        1
    );
    let basic = f
        .calls
        .borrow()
        .iter()
        .filter(|c| c.contains(&"basic.target".to_string()))
        .count();
    assert_eq!(basic, 6, "two attempts of three ticks");
    let e = f.errs();
    assert!(e.contains("basic.target startup attempt 1 failed; retrying after 2s"));
    assert!(e.contains("did not reach basic.target after 2 attempt(s)"));
    assert!(e.contains(
        "resource check for n — inotify instances 100/512, pids 8100/8192, user-1001.slice tasks ?/?, keyring 12/200"
    ), "{e}");
    assert!(e.contains("exhausted resource for n — pids (8100/8192, 98%)"));
    assert!(f.called(&["stop", "n"]) && f.called(&["rm", "n"]));
}

#[test]
fn the_bare_failure_message_is_not_mistaken_for_an_attribution() {
    // Seen red first: the pre-sp-cvle7 output names no resource.
    let bare = "testenv: system systemd did not reach basic.target after 2 attempt(s)";
    let named = Regex::new(r"exhausted resource for \S+ — \S+ \([0-9]+/[0-9]+, [0-9]+%\)").unwrap();
    assert!(!named.is_match(bare));
    assert!(named.is_match("testenv: exhausted resource for n — pids (8100/8192, 98%)"));
}

#[test]
fn an_unreachable_container_reads_question_marks_and_inotify_still_runs() {
    let f = Fake::new();
    f.when(&["exec"], 125, "");
    f.inotify.set(10);
    f.sysctls.borrow_mut().insert(
        "/proc/sys/fs/inotify/max_user_instances".into(),
        "128".into(),
    );
    let c = conf("/h");
    Driver { host: &f, conf: &c }.diagnose_boot_failure("gone");
    let e = f.errs();
    assert!(
        e.contains("inotify instances 10/128, pids ?/?, user-1001.slice tasks ?/?, keyring ?/?")
    );
    assert!(e.contains("no candidate resource for gone is near its cap (closest: inotify (10/128, 7%)) — inconclusive"));
    let f = Fake::new();
    f.when(&["exec"], 125, "");
    Driver { host: &f, conf: &c }.diagnose_boot_failure("gone");
    assert!(f
        .errs()
        .contains("could not measure any candidate resource for gone — container unreachable"));
}

#[test]
fn up_queues_while_the_host_is_full_and_starts_when_a_slot_frees() {
    let f = Fake::new();
    booting(&f);
    let polls = std::rc::Rc::new(Cell::new(0));
    let p = polls.clone();
    f.on(move |a| {
        (a[0] == "ps").then(|| {
            p.set(p.get() + 1);
            let n = if p.get() <= 3 { 2 } else { 1 };
            (0, "c\n".repeat(n))
        })
    });
    let c = Conf {
        max_concurrent: 2,
        ..conf("/h")
    };
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "q"])),
        0
    );
    assert_eq!(polls.get(), 4);
    assert_eq!(f.calls_with("run").len(), 1);
    let e = f.errs();
    assert!(e.contains("2 testenv containers already running (limit 2) — queueing"));
    assert!(e.contains("slot free (1/2 running) — starting"));
    assert_eq!(
        f.calls_with("ps")[0],
        args(&["ps", "-q", "--filter", "label=spira.testenv=1"])
    );
}

#[test]
fn a_full_host_gives_up_after_the_queue_timeout_and_zero_disables_the_gate() {
    let f = Fake::new();
    booting(&f);
    f.when(&["ps"], 0, "a\nb\n");
    let c = Conf {
        max_concurrent: 2,
        queue_timeout: 12,
        queue_poll: 5,
        ..conf("/h")
    };
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "q"])),
        1
    );
    assert!(f.calls_with("run").is_empty());
    assert!(f
        .errs()
        .contains("gave up waiting for a slot after 12s (still 2/2 running)"));

    let f = Fake::new();
    booting(&f);
    f.when(&["ps"], 0, "a\nb\nc\n");
    let c = Conf {
        max_concurrent: 0,
        ..conf("/h")
    };
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "q"])),
        0
    );
    assert!(f.calls_with("ps").is_empty());
}

// ---- the owner-file guard (was test-testenv-owner-guard.sh) ------------------------------

fn down(f: &Fake, a: &[&str]) -> i32 {
    let c = conf("/h");
    Driver { host: f, conf: &c }.cmd_down(&args(a))
}

#[test]
fn a_live_foreign_owner_blocks_down() {
    let f = Fake::new();
    f.procs.borrow_mut().insert(900, 1);
    f.file("/tmp/c.owner", "900\n");
    assert_eq!(down(&f, &["--name", "c"]), 1);
    assert!(f.calls_with("stop").is_empty() && f.calls_with("rm").is_empty());
    assert!(f
        .errs()
        .contains("c is owned by pid 900 (not this process or an ancestor of it)"));
    assert!(f.is_file(Path::new("/tmp/c.owner")));
}

#[test]
fn the_owner_or_its_descendant_tears_down_and_releases_the_owner_file() {
    for owner in ["500", "400", "1"] {
        let f = Fake::new();
        f.file("/tmp/c.owner", owner);
        f.when(&["container", "exists"], 1, "");
        assert_eq!(down(&f, &["--name", "c"]), 0, "owner {owner}");
        assert!(f.called(&["stop", "c"]) && f.called(&["rm", "c"]));
        assert!(!f.is_file(Path::new("/tmp/c.owner")));
    }
}

#[test]
fn a_dead_owner_force_foreign_and_no_owner_file_do_not_block() {
    let f = Fake::new();
    f.file("/tmp/c.owner", "31337\n");
    assert_eq!(down(&f, &["--name", "c"]), 0);
    let f = Fake::new();
    f.procs.borrow_mut().insert(900, 1);
    f.file("/tmp/c.owner", "900\n");
    assert_eq!(down(&f, &["--name", "c", "--force-foreign"]), 0);
    assert!(f.called(&["stop", "c"]));
    let f = Fake::new();
    assert_eq!(down(&f, &["--name", "c"]), 0);
    assert!(f.called(&["rm", "c"]));
}

#[test]
fn a_failed_teardown_keeps_the_owner_file_and_volumes_go_only_when_asked() {
    let f = Fake::new();
    f.file("/tmp/c.owner", "500");
    assert_eq!(down(&f, &["--name", "c"]), 0);
    assert!(
        f.is_file(Path::new("/tmp/c.owner")),
        "container still exists"
    );
    assert!(f.calls_with("volume").is_empty());
    let f = Fake::new();
    assert_eq!(down(&f, &["--name", "c", "--volumes"]), 0);
    let v: BTreeSet<Vec<String>> = f.calls_with("volume").into_iter().collect();
    assert!(
        v.contains(&args(&["volume", "rm", "c-cargo-reg"]))
            && v.contains(&args(&["volume", "rm", "c-cargo-git"]))
    );
    assert_eq!(down(&f, &["--bogus"]), 1);
}

// ---- exec, probe, usage ------------------------------------------------------------------

#[test]
fn exec_as_spirauser_carries_the_session_and_cargo_environment() {
    let f = Fake::new();
    let c = conf("/h");
    let d = Driver { host: &f, conf: &c };
    assert_eq!(
        d.cmd_exec(&args(&[
            "--name",
            "n",
            "--user",
            "spirauser",
            "--",
            "cargo",
            "build"
        ])),
        0
    );
    assert_eq!(
        f.replaced.borrow().clone().unwrap(),
        args(&[
            "exec",
            "--user",
            "spirauser",
            "-e",
            "XDG_RUNTIME_DIR=/run/user/1001",
            "-e",
            "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus",
            "-e",
            "CARGO_HOME=/var/spira/cargo",
            "-e",
            "CARGO_TARGET_DIR=/var/spira/cargo/target",
            "n",
            "cargo",
            "build",
        ])
    );
    assert_eq!(d.cmd_exec(&args(&["id"])), 0);
    assert_eq!(
        f.replaced.borrow().clone().unwrap(),
        args(&["exec", "--user", "root", "spira-testenv", "id"])
    );
    assert_eq!(d.cmd_exec(&args(&["--name", "n"])), 1);
    assert!(f.errs().contains("testenv exec: command required"));
    assert_eq!(d.cmd_exec(&args(&["-x", "id"])), 1);
    assert!(f.errs().contains("testenv exec: unknown option: -x"));
}

#[test]
fn probe_needs_the_user_manager_and_its_bus() {
    let f = Fake::new();
    let c = conf("/h");
    let d = Driver { host: &f, conf: &c };
    assert_eq!(d.cmd_probe(&args(&["--name", "p"])), 0);
    assert!(f.called(&[
        "exec",
        "--user",
        "spirauser",
        "-e",
        "XDG_RUNTIME_DIR=/run/user/1001",
        "p",
        "systemctl",
        "--user",
        "is-active",
        "default.target"
    ]));
    f.when(&["exec", "--user"], 1, "");
    assert_eq!(d.cmd_probe(&args(&["--name", "p"])), 1);
    f.when(&["exec", "p", "systemctl", "is-active"], 3, "");
    assert_eq!(d.cmd_probe(&args(&["--name", "p"])), 1);
}

#[test]
fn an_unknown_subcommand_prints_usage_and_fails() {
    let f = Fake::new();
    let c = conf("/h");
    let d = Driver { host: &f, conf: &c };
    assert_eq!(d.dispatch(&args(&["scratch"])), 1);
    assert_eq!(d.dispatch(&[]), 1);
    assert!(f
        .errs()
        .starts_with("usage: testenv container up|down|exec|probe|tag|image|publish"));
}

#[test]
fn settings_come_from_the_environment_then_the_config_then_conf_sh_defaults() {
    let env = |k: &str| match k {
        "SPIRA_RUN" => Some("/r".to_string()),
        "SPIRA_TESTENV_MAX_CONCURRENT" => Some("3".to_string()),
        _ => None,
    };
    let src = Source {
        env: &env,
        config: None,
    };
    let c = Conf::load(&src, Some(PathBuf::from("/h")));
    assert_eq!(c.bd_pin, Some(PathBuf::from("/r/bd-pin")));
    assert_eq!(
        (c.max_concurrent, c.queue_timeout, c.queue_poll, c.heartbeat),
        (3, 900, 5, 60)
    );
    assert_eq!((c.basic_wait_ticks, c.basic_retry_sleep), (20, 2));
    assert_eq!(c.registry, "");
    let env = |k: &str| (k == "SPIRA_BD_PIN").then(|| "/pin".to_string());
    let c = Conf::load(
        &Source {
            env: &env,
            config: None,
        },
        None,
    );
    assert_eq!(c.bd_pin, Some(PathBuf::from("/pin")));
}

#[test]
fn sizes_read_like_df_h() {
    assert_eq!(human(512), "512B");
    assert_eq!(human(3 * 1024 * 1024 * 1024 + 100 * 1024 * 1024), "3.1G");
    assert_eq!(human(45 * 1024 * 1024 * 1024), "45G");
}
