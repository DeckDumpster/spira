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
    /// What `symlinked_targets` answers, by the `dir` it is asked about; absent = none (an
    /// ordinary, non-symlinked `target/`).
    symlinked_targets: RefCell<BTreeMap<PathBuf, Vec<PathBuf>>>,
    /// What `current_exe()` answers; `None` (the default) is the "cannot resolve" case.
    current_exe: RefCell<Option<PathBuf>>,
    /// What `build_spira_config` answers; `None` (the default) is "the build failed".
    build_spira_config_result: RefCell<Option<PathBuf>>,
    /// Every `build_spira_config(repo_root)` call, in order.
    build_spira_config_calls: RefCell<Vec<PathBuf>>,
    /// Every `copy_file(from, to)` call, in order — so a test can see staging happened
    /// without inferring it from file presence alone (which `remove` would later erase).
    copies: RefCell<Vec<(PathBuf, PathBuf)>>,
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
    /// What `symlinked_targets(dir)` answers: the fake's stand-in for scanning `dir` on a
    /// real filesystem.
    fn symlink_target(&self, dir: &str, targets: &[&str]) {
        self.symlinked_targets.borrow_mut().insert(
            PathBuf::from(dir),
            targets.iter().map(PathBuf::from).collect(),
        );
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
    /// Simulates a build's own binary at `exe`, with a working `spira-config` already
    /// sitting next to it (sp-xjnzl) — the common case: the workspace was built first.
    fn with_spira_config_sibling(&self, exe: &str) {
        *self.current_exe.borrow_mut() = Some(PathBuf::from(exe));
        let sibling = Path::new(exe).parent().unwrap().join("spira-config");
        self.file(&sibling.display().to_string(), "#!/bin/sh\n");
    }
    /// No sibling of this process's own binary (current_exe stays None, the common case in
    /// a test binary) — but `cargo build -p spira-config` in `repo_root` succeeds and
    /// produces one at the usual path.
    fn with_spira_config_buildable(&self, repo_root: &str) {
        let bin = Path::new(repo_root).join("target/release/spira-config");
        self.file(&bin.display().to_string(), "#!/bin/sh\n");
        *self.build_spira_config_result.borrow_mut() = Some(bin);
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
    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), String> {
        let Some(data) = self.files.borrow().get(from).cloned() else {
            return Err("absent".into());
        };
        self.files.borrow_mut().insert(to.to_path_buf(), data);
        self.copies.borrow_mut().push((from.to_path_buf(), to.to_path_buf()));
        Ok(())
    }
    fn current_exe(&self) -> Option<PathBuf> {
        self.current_exe.borrow().clone()
    }
    fn build_spira_config(&self, repo_root: &Path) -> Option<PathBuf> {
        self.build_spira_config_calls.borrow_mut().push(repo_root.to_path_buf());
        self.build_spira_config_result.borrow().clone()
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
    fn symlinked_targets(&self, dir: &Path) -> Vec<PathBuf> {
        self.symlinked_targets
            .borrow()
            .get(dir)
            .cloned()
            .unwrap_or_default()
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
        cpus: None,
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
        "[[dep]]\nname = \"bd\"\ntier = \"runtime\"\n",
    );
    // sp-xjnzl: every ordinary build_image call needs a spira-config to stage for
    // doctor-check; tests of that staging itself (a_cold_build_*) set it up by hand instead.
    f.with_spira_config_buildable(root);
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
    // Containerfile and the bd pin hash in full; deps.toml hashes only the (name, tier)
    // rows doctor-check.sh's FATAL/WARN case arms actually consult — here just "bd
    // runtime" (sp-ehj2t: the raw file used to hash whole, so every release-tier addition
    // cold-built a new image).
    let line = |b: &str| format!("{}  -\n", crate::verdict::sha256_hex(b.as_bytes()));
    let closure = format!(
        "{}{}{}",
        line("FROM ubuntu:24.04\n"),
        "migrations=12\n",
        line("bd runtime\n")
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
    f.file(
        "/h/spira/deps.toml",
        "[[dep]]\nname = \"bd\"\ntier = \"optional\"\n",
    );
    let deps = tag_of(&f, &c).unwrap();
    assert_ne!(deps, base, "a checked-tier deps.toml change");
    f.file("/run/bd-pin", "migrations=13\n");
    let pin = tag_of(&f, &c).unwrap();
    assert_ne!(pin, deps, "the bd pin");
    f.file("/h/spira/testenv/Containerfile", "FROM ubuntu:26.04\n");
    assert_ne!(tag_of(&f, &c).unwrap(), pin, "the Containerfile");
}

// ---- tag: deps.toml only counts what doctor-check.sh's build-time check reads
// (sp-ehj2t) ---------------------------------------------------------------------------

#[test]
fn adding_a_release_tier_entry_leaves_the_tag_unchanged() {
    // Every rewrite branch adds its own new binary to deps.toml as tier "release"
    // (testenv stages Spira's own binaries per run; none of them are installed into the
    // image). doctor-check.sh's case statement has no arm for "release", so it can never
    // change whether the image build passes — the tag must not move either.
    let f = Fake::new();
    harness(&f, "/h");
    let c = conf("/h");
    let base = tag_of(&f, &c).unwrap();
    f.file(
        "/h/spira/deps.toml",
        "[[dep]]\nname = \"bd\"\ntier = \"runtime\"\n\n[[dep]]\nname = \"aeon\"\ntier = \"release\"\n",
    );
    assert_eq!(tag_of(&f, &c).unwrap(), base, "a release-tier addition");
}

#[test]
fn two_manifests_differing_only_in_release_entries_share_one_tag() {
    let f = Fake::new();
    harness(&f, "/a");
    f.file(
        "/a/spira/deps.toml",
        "[[dep]]\nname = \"bd\"\ntier = \"runtime\"\n\n[[dep]]\nname = \"aeon\"\ntier = \"release\"\n",
    );
    harness(&f, "/b");
    f.file(
        "/b/spira/deps.toml",
        "[[dep]]\nname = \"bd\"\ntier = \"runtime\"\n\n[[dep]]\nname = \"batcher\"\ntier = \"release\"\n\n[[dep]]\nname = \"gate\"\ntier = \"release\"\n",
    );
    assert_eq!(
        tag_of(&f, &conf("/a")),
        tag_of(&f, &conf("/b")),
        "two branches differing only in which release-tier binaries they declare"
    );
}

#[test]
fn changing_a_runtime_tier_entry_changes_the_tag() {
    let f = Fake::new();
    harness(&f, "/h");
    let c = conf("/h");
    let base = tag_of(&f, &c).unwrap();
    f.file(
        "/h/spira/deps.toml",
        "[[dep]]\nname = \"bd\"\ntier = \"optional\"\n",
    );
    assert_ne!(
        tag_of(&f, &c).unwrap(),
        base,
        "a runtime-tier entry's tier changed"
    );
}

#[test]
fn an_operator_tier_entry_moves_the_tag_like_optional_does() {
    // doctor-check.sh buckets "optional" and "operator" into the same WARN arm
    // (present-or-waived); only "dev" and "release" hit no arm. An operator-tier
    // addition can change whether the build needs a waiver, so it must move the tag —
    // unlike a release-tier addition, which never can.
    let f = Fake::new();
    harness(&f, "/h");
    let c = conf("/h");
    let base = tag_of(&f, &c).unwrap();
    f.file(
        "/h/spira/deps.toml",
        "[[dep]]\nname = \"bd\"\ntier = \"runtime\"\n\n[[dep]]\nname = \"go\"\ntier = \"operator\"\n",
    );
    assert_ne!(tag_of(&f, &c).unwrap(), base, "an operator-tier addition");
}

#[test]
fn a_dev_tier_entry_never_moves_the_tag() {
    // dev-tier programs (podman, bd-embedded, rustup) are what tests need to exist, not
    // what the image build verifies; doctor-check.sh's own "dev" arm is a deliberate
    // no-op, so the tag must not move either.
    let f = Fake::new();
    harness(&f, "/h");
    let c = conf("/h");
    let base = tag_of(&f, &c).unwrap();
    f.file(
        "/h/spira/deps.toml",
        "[[dep]]\nname = \"bd\"\ntier = \"runtime\"\n\n[[dep]]\nname = \"podman\"\ntier = \"dev\"\n",
    );
    assert_eq!(tag_of(&f, &c).unwrap(), base, "a dev-tier addition");
}

#[test]
fn an_unparseable_manifest_refuses_instead_of_naming_an_image() {
    let f = Fake::new();
    f.file("/h/spira/testenv/Containerfile", "FROM ubuntu:24.04\n");
    f.file("/h/spira/deps.toml", "not valid toml === [[[");
    let c = conf("/h");
    assert_eq!(tag_of(&f, &c), None, "an unparseable deps.toml");
    assert!(f.errs().contains("deps.toml"));
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

// ---- sp-xjnzl: staging spira-config into the build context for doctor-check -------------

#[test]
fn a_cold_build_stages_its_sibling_spira_config_and_removes_it_either_way() {
    let dest = PathBuf::from("/h/spira/testenv/.doctor-check-spira-config");

    // Present: staged for the build, then cleaned up on a GREEN build.
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    f.with_spira_config_sibling("/build/target/release/testenv");
    let c = conf("/h");
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 0);
    assert_eq!(
        *f.copies.borrow(),
        vec![(PathBuf::from("/build/target/release/testenv").parent().unwrap().join("spira-config"), dest.clone())]
    );
    assert!(!f.is_file(&dest), "left behind in the checkout after a green build");
    assert!(!f.errs().contains("no spira-config next to this binary"));

    // Present: also cleaned up on a RED build — staging must not leak on the failure path.
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    f.with_spira_config_sibling("/build/target/release/testenv");
    f.build_rc.set(1);
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 1);
    assert_eq!(f.copies.borrow().len(), 1);
    assert!(!f.is_file(&dest), "left behind in the checkout after a red build");
}

#[test]
fn a_cold_build_with_no_sibling_builds_spira_config_on_demand() {
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    // No with_spira_config_sibling: current_exe() answers None, as it would for a test
    // binary or any process that cannot resolve its own path. The gate's own host-side
    // container step builds only `-p testenv`, so this is the common case there, not a
    // corner case — stage_spira_config must not depend on the caller having remembered to
    // pre-build it.
    f.with_spira_config_buildable("/h");
    let c = conf("/h");
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 0);
    assert_eq!(*f.build_spira_config_calls.borrow(), vec![PathBuf::from("/h")]);
    assert_eq!(f.copies.borrow().len(), 1, "the on-demand build still gets staged into the build context");
    assert!(!f.is_file(&PathBuf::from("/h/spira/testenv/.doctor-check-spira-config")), "cleaned up after the build");
}

#[test]
fn a_cold_build_refuses_rather_than_run_podman_when_spira_config_cannot_be_had_at_all() {
    let f = Fake::new();
    harness(&f, "/h");
    f.when(&["image", "exists"], 1, "");
    // No sibling AND the on-demand build fails too — undo harness()'s own default fixture,
    // which assumes the ordinary case: every avenue is exhausted here on purpose.
    *f.build_spira_config_result.borrow_mut() = None;
    let c = conf("/h");
    assert_eq!(Driver { host: &f, conf: &c }.cmd_image(), 1, "a doomed podman build is never attempted");
    assert!(f.copies.borrow().is_empty());
    assert!(f.calls_with("build").is_empty(), "podman build must not even be invoked");
    assert!(f.errs().contains("could not build spira-config"));
    assert!(f.errs().contains("refusing the image build"));
}

// ---- boot failure diagnosis and admission (was test-testenv-resource-diagnosis.sh) -------

fn booting(f: &Fake) {
    harness(f, "/h");
    f.when(&["info", "--format"], 0, "true\n");
    f.when(&["container", "exists"], 1, "");
}

#[test]
fn up_passes_the_configured_cpu_cap_to_podman_run() {
    let f = Fake::new();
    booting(&f);
    let c = Conf { cpus: Some("12.5".into()), ..conf("/h") };
    assert_eq!(Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "n1"])), 0);
    let run = &f.calls_with("run")[0];
    let i = run.iter().position(|a| a == "--cpus").expect("--cpus passed");
    assert_eq!(run[i + 1], "12.5");
}

#[test]
fn cpu_cap_is_unset_unless_a_positive_number_is_configured() {
    let load = |v: Option<&'static str>| {
        let env = move |k: &str| (k == "SPIRA_TESTENV_CPUS").then(|| v.map(String::from)).flatten();
        Conf::load(&Source { env: &env, config: None }, None).cpus
    };
    assert_eq!(load(None), None);
    assert_eq!(load(Some("")), None);
    assert_eq!(load(Some("0")), None);
    assert_eq!(load(Some("many")), None);
    assert_eq!(load(Some("16")), Some("16".into()));
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
            "--network",
            "pasta:-T,none,--no-map-gw",
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

fn network_of_run(f: &Fake) -> String {
    let c = conf("/h");
    assert_eq!(
        Driver { host: f, conf: &c }.cmd_up(&args(&["--name", "n1", "--checkout", "/wt"])),
        0
    );
    let run = &f.calls_with("run")[0];
    let i = run.iter().position(|a| a == "--network").unwrap();
    run[i + 1].clone()
}

#[test]
fn up_uses_the_bridge_when_podman_is_rootful() {
    let f = Fake::new();
    booting(&f);
    f.when(&["info", "--format"], 0, "false\n");
    assert_eq!(network_of_run(&f), "bridge");
}

#[test]
fn up_refuses_to_start_when_the_podman_mode_is_unknown() {
    let f = Fake::new();
    booting(&f);
    f.when(&["info", "--format"], 1, "");
    let c = conf("/h");
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "n1", "--checkout", "/wt"])),
        1
    );
    assert!(f.calls_with("run").is_empty());
}

/// THE PRODUCTION BUG (sp-e5v53-3, 2026-10-01): a gate tree's `target/{aeon,release,debug,
/// gate-tools}` are symlinks to a tmpfs root outside the checkout (gate/src/target.rs,
/// sp-z61hj). `--checkout /wt` bind-mounts `/wt` at `/workspace`, preserving the symlink as
/// a symlink — `/workspace/target/aeon`, from inside the container, points to a host path
/// that was never mounted there, so a binary built right through it is invisible to `stage`
/// ("testenv: stage: aeon was not built into /workspace/target/aeon"), even though it
/// genuinely was built. Reproduced directly against this repository before this fix: `cargo
/// build --profile aeon --workspace` in such a tree succeeds, `stage` still faults rc=1.
///
/// Fix: `up` now also mounts whatever `symlinked_targets` names, at the same absolute path,
/// so the symlink resolves inside the container too.
#[test]
fn up_also_mounts_a_checkouts_symlinked_target_directories() {
    let f = Fake::new();
    booting(&f);
    f.symlink_target(
        "/wt/target",
        &["/tmp/spira-gate-target-abc123/.gate.harness.concierge-sp-8itaf"],
    );
    let c = conf("/h");
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "n1", "--checkout", "/wt"])),
        0
    );
    let run = f.calls_with("run");
    assert!(
        run[0].windows(2).any(|w| w
            == [
                "--volume".to_string(),
                "/tmp/spira-gate-target-abc123/.gate.harness.concierge-sp-8itaf:/tmp/spira-gate-target-abc123/.gate.harness.concierge-sp-8itaf:z".to_string()
            ]),
        "{:?}",
        run[0]
    );
    // Still named, same as always — the extra mount is additive, not a replacement.
    assert!(run[0].windows(2).any(|w| w == ["--volume".to_string(), "/wt:/workspace:z".to_string()]));
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
        RC_QUEUE
    );
    assert!(f.calls_with("run").is_empty());
    assert!(f
        .errs()
        .contains("gave up waiting for a slot after 12s (still 2/2 running)"));
    assert!(f.errs().contains("testenv: queue-wait=15s pool=container"));

    // The caller's bound outranks the configured one.
    let f = Fake::new();
    booting(&f);
    f.when(&["ps"], 0, "a\nb\n");
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "q", "--queue-timeout", "4"])),
        RC_QUEUE
    );
    assert!(f.errs().contains("gave up waiting for a slot after 4s"));

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

// ---- RealHost::symlinked_targets, against a real filesystem (sp-e5v53-3) -----------------

#[test]
fn symlinked_targets_finds_a_real_symlinked_build_dir_and_collapses_a_shared_root() {
    let d = testkit::TempDir::new("testenv-symlinked-targets");
    let root = d.join("tmpfs-root").join("tree-x");
    for p in ["aeon", "release", "debug", "gate-tools"] {
        std::fs::create_dir_all(root.join(p)).unwrap();
    }
    let target = d.join("checkout").join("target");
    std::fs::create_dir_all(&target).unwrap();
    for p in ["aeon", "release", "debug", "gate-tools"] {
        std::os::unix::fs::symlink(root.join(p), target.join(p)).unwrap();
    }
    // An ordinary, non-symlinked entry alongside them (e.g. ".rustc_info.json" or any real
    // file testenv's own build leaves) must never be reported as something to mount.
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d").unwrap();

    let got = RealHost.symlinked_targets(&target);
    assert_eq!(
        got,
        vec![std::fs::canonicalize(&root).unwrap()],
        "all four symlinks share one root (gate/src/target.rs) — one mount, not four"
    );
}

#[test]
fn symlinked_targets_is_empty_for_an_ordinary_non_symlinked_target_dir() {
    let d = testkit::TempDir::new("testenv-symlinked-targets-plain");
    let target = d.join("checkout").join("target");
    std::fs::create_dir_all(target.join("aeon")).unwrap();
    assert_eq!(RealHost.symlinked_targets(&target), Vec::<PathBuf>::new());
}

#[test]
fn symlinked_targets_is_empty_for_a_target_dir_that_does_not_exist() {
    assert_eq!(
        RealHost.symlinked_targets(Path::new("/nonexistent/target")),
        Vec::<PathBuf>::new()
    );
}

#[test]
fn real_copy_file_follows_a_symlinked_source_and_refuses_a_dangling_one() {
    let tmp = testkit::TempDir::new("testenv-copy");
    let root = tmp.join("root");
    std::fs::create_dir_all(root.join("store")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("store/spira-config"), b"binary").unwrap();
    std::os::unix::fs::symlink("../store/spira-config", root.join("bin/spira-config")).unwrap();
    let dest = root.join("ctx/testenv/.stage");
    assert_eq!(RealHost.copy_file(&root.join("bin/spira-config"), &dest), Ok(()));
    assert_eq!(std::fs::read(&dest).unwrap(), b"binary");
    assert!(!dest.symlink_metadata().unwrap().file_type().is_symlink());
    // a stale read-only file at the destination is replaced, not a reason to refuse
    std::fs::set_permissions(&dest, std::os::unix::fs::PermissionsExt::from_mode(0o444)).unwrap();
    assert_eq!(RealHost.copy_file(&root.join("bin/spira-config"), &dest), Ok(()));
    std::os::unix::fs::symlink("../store/absent", root.join("bin/dangling")).unwrap();
    assert!(RealHost.copy_file(&root.join("bin/dangling"), &dest).is_err());
}

#[test]
fn up_no_build_with_an_absent_image_is_image_not_ready_and_never_builds() {
    let f = Fake::new();
    booting(&f);
    f.when(&["image", "exists"], 1, "");
    let c = conf("/h");
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "n", "--checkout", "/wt", "--no-build"])),
        RC_IMAGE_NOT_READY
    );
    assert!(f.calls_with("build").is_empty(), "a bounded trial never builds");
    assert!(f.calls_with("run").is_empty());
}

#[test]
fn up_no_build_with_the_image_present_boots() {
    let f = Fake::new();
    booting(&f);
    let c = conf("/h");
    assert_eq!(
        Driver { host: &f, conf: &c }.cmd_up(&args(&["--name", "n", "--checkout", "/wt", "--no-build"])),
        0
    );
}
