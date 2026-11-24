//! The gate's three helper scripts, run for real under `bash`: each is the subject, so no Rust
//! seam can stand in for it. `spira-config` is a stub answering only `resolve`, because an
//! integration target of this crate is not built with that binary; flock, pgrep, git and
//! python3 are the real ones.

#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use testkit::{git, ChildGuard, TempDir};

fn sys_path() -> String {
    format!("{}:{}", system_paths::USR_BIN, system_paths::BIN)
}

fn spira() -> PathBuf {
    testkit::manifest_dir().join("../spira").canonicalize().unwrap()
}

const SPIRA_CONFIG: &str = "#!/bin/bash\ncase \"$1\" in resolve) cat \"$STUB_RESOLVED\" ;; env-bootstrap|check-bd) ;; *) exit 1 ;; esac\n";

struct Rig {
    dir: TempDir,
    toml: PathBuf,
}

impl Rig {
    fn new(declare: &[(&str, &str)]) -> Rig {
        let dir = TempDir::new("gate-scripts");
        let run = dir.join("run").display().to_string();
        let declare: Vec<(&str, &str)> = declare.iter().copied().chain([("SPIRA_RUN", run.as_str())]).collect();
        let toml = spira_config::process::fixture_toml(&dir, &declare);
        let doc = spira_config::validate(&std::fs::read_to_string(&toml).unwrap()).unwrap();
        let home = spira();
        let env: BTreeMap<String, String> = [("HOME".to_string(), dir.display().to_string())].into();
        let resolved = spira_config::resolve::resolve(spira_config::resolve::ResolveInput {
            env: &env,
            home: &home,
            repo: home.parent().unwrap(),
            toml: Some(&doc),
            conf_d: &home.join("conf.d"),
        })
        .unwrap_or_else(|_| panic!("the fixture config resolves"));
        std::fs::write(dir.join("resolved.sh"), resolved.to_sh_all()).unwrap();
        std::fs::create_dir(dir.join("bin")).unwrap();
        testkit::write_exe(dir.join("bin/spira-config"), SPIRA_CONFIG);
        Rig { dir, toml }
    }

    fn script(&self, name: &str, args: &[&str], env: &[(&str, &Path)]) -> String {
        self.script_rc(name, args, env).1
    }

    fn script_rc(&self, name: &str, args: &[&str], env: &[(&str, &Path)]) -> (i32, String) {
        let mut c = Command::new("bash");
        c.arg(spira().join(name)).args(args).env_clear();
        c.env("PATH", format!("{}:{}", self.dir.join("bin").display(), sys_path()))
            .env("HOME", &*self.dir)
            .env("SPIRA_TOML", &self.toml)
            .env("STUB_RESOLVED", self.dir.join("resolved.sh"))
            .env("GIT_CONFIG_GLOBAL", system_paths::DEV_NULL)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for (k, v) in env {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }
}

/// A process in a group of its own holding an exclusive flock on `lock`, returned once it has
/// said so through a fifo: the lock is taken before the test looks.
fn hold(dir: &Path, lock: &Path) -> ChildGuard {
    let fifo = dir.join(format!("{}.up", lock.file_name().unwrap().to_string_lossy()));
    let path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: a plain mkfifo(3) on a path this test just built.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0, "mkfifo {}", fifo.display());
    let g = ChildGuard::spawn(Command::new("flock").arg("-x").arg(lock).args(["sh", "-c", "echo held > \"$1\"; exec sleep 3600", "sh"]).arg(&fifo));
    let mut line = String::new();
    BufReader::new(std::fs::File::open(&fifo).unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "held", "the holder took {}", lock.display());
    g
}

fn dead_pid() -> u32 {
    let mut c = Command::new("true").spawn().unwrap();
    let pid = c.id();
    c.wait().unwrap();
    pid
}

fn row<'a>(out: &'a str, label: &str) -> &'a str {
    out.lines().find(|l| l.starts_with(&format!("harness.{label} "))).unwrap_or_else(|| panic!("no row for {label} in:\n{out}"))
}

// covers: UC-gate-verdict-19
#[test_id(thtdezk)]
#[test]
fn gate_locks_reads_a_dead_pid_whose_group_lives_as_held_and_a_dead_group_as_stale() {
    let rig = Rig::new(&[]);
    let run = rig.dir.join("run");
    let wt = run.join("worktree");
    std::fs::create_dir_all(&wt).unwrap();
    let lock = |name: &str| {
        let p = wt.join(format!(".gate.harness.{name}.lock"));
        std::fs::write(&p, "").unwrap();
        p
    };
    let _free = lock("free");
    let held = lock("held");
    let inherit = lock("inherit");
    let stale = lock("stale");
    let (g_held, g_inherit, _g_stale) = (hold(&rig.dir, &held), hold(&rig.dir, &inherit), hold(&rig.dir, &stale));
    let gone = dead_pid();
    std::fs::write(format!("{}.holder", held.display()), format!("{} {}\n", g_held.id(), g_held.id())).unwrap();
    std::fs::write(format!("{}.holder", inherit.display()), format!("{gone} {}\n", g_inherit.id())).unwrap();
    std::fs::write(format!("{}.holder", stale.display()), format!("{gone} {gone}\n")).unwrap();

    let out = rig.script("gate-locks.sh", &[run.to_str().unwrap()], &[]);

    assert!(row(&out, "free").contains("FREE"), "{out}");
    let r = row(&out, "held");
    assert!(r.contains("HELD") && r.contains(&g_held.id().to_string()), "{r}");
    let r = row(&out, "inherit");
    assert!(r.contains("HELD") && !r.contains("STALE") && r.contains("inherited fd"), "a dead pid whose group lives is held: {r}");
    assert!(row(&out, "stale").contains("STALE"), "{out}");
    assert!(out.contains("1 stale lock(s)"), "{out}");
    assert!(!out.contains("harness.absent"), "a lock that does not exist is not listed: {out}");
}

fn age(path: &Path, secs: u64) {
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    std::fs::File::open(path).unwrap().set_modified(then).unwrap();
}

const TWO_HOURS: u64 = 7200;

// covers: UC-gate-verdict-20
#[test_id(tnubg8o)]
#[test]
fn gate_sweep_removes_old_gate_trees_and_orphaned_batch_homes_but_keeps_a_held_or_young_one() {
    let rig = Rig::new(&[]);
    let (repo, wt) = (rig.dir.join("repo"), rig.dir.join("run/worktree"));
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let tree = |name: &str, old: bool| {
        let p = wt.join(name);
        git(&repo, &["worktree", "add", "-q", "--detach", p.to_str().unwrap(), "HEAD"]);
        if old {
            age(&p, TWO_HOURS);
        }
        p
    };
    let stale = tree(".gate.repo.stale", true);
    let young = tree(".gate.repo.young", false);
    let locked = tree(".gate.repo.locked", true);
    let other = tree(".gate.repository", true);
    let _holder = hold(&rig.dir, &PathBuf::from(format!("{}.lock", locked.display())));

    let homes = rig.dir.join("homes");
    std::fs::create_dir(&homes).unwrap();
    let home = |name: &str, old: bool| {
        let p = homes.join(name);
        std::fs::create_dir(&p).unwrap();
        if old {
            age(&p, TWO_HOURS);
        }
        p
    };
    let (orphan, live, fresh) = (home("spira-batch-orphan", true), home("spira-batch-live", true), home("spira-batch-fresh", false));
    let ps = rig.dir.join("podman-ps");
    std::fs::write(&ps, "spira-batch-live\n").unwrap();

    let out = rig.script(
        "gate-sweep.sh",
        &[repo.to_str().unwrap(), "3600"],
        &[("SPIRA_BATCH_HOME_GLOB", &homes.join("spira-batch-*")), ("SPIRA_PODMAN_PS_FILE", &ps)],
    );

    assert!(!stale.exists(), "an old gate tree is removed: {out}");
    assert!(!git(&repo, &["worktree", "list", "--porcelain"]).contains(".gate.repo.stale"), "and deregistered");
    assert!(young.exists() && locked.exists() && other.exists(), "young, held and another repo's trees stay: {out}");
    assert!(out.contains(&format!("skipping {} — gate lock is held", locked.display())), "{out}");
    assert!(!orphan.exists(), "a batch home with no container is removed: {out}");
    assert!(live.exists() && fresh.exists(), "a home with a live container, or a young one, stays: {out}");
}

const WINDOW: u64 = 7200;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn utc(secs: u64) -> String {
    let (days, rem) = (secs / 86400, secs % 86400);
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// yield.sh is copied beside a `lib.sh` that only names the run directory: configuration
/// resolution is a hundred milliseconds a call, and nothing here reads a configured value.
struct Yield {
    script: PathBuf,
    dir: PathBuf,
    log: PathBuf,
}

impl Yield {
    fn new(root: &Path, name: &str) -> Yield {
        let sh = root.join(format!("{name}-sh"));
        std::fs::create_dir_all(&sh).unwrap();
        std::fs::copy(spira().join("yield.sh"), sh.join("yield.sh")).unwrap();
        std::fs::write(sh.join("lib.sh"), "SPIRA_RUN=${SPIRA_RUN:-/nonexistent}\n").unwrap();
        Yield { script: sh.join("yield.sh"), dir: root.join(format!("{name}-yield")), log: root.join("gate.log") }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let o = Command::new("bash")
            .arg(&self.script)
            .args(args)
            .env_clear()
            .env("PATH", sys_path())
            .env("SPIRA_YIELD", &self.dir)
            .env("SPIRA_GATE_LOG", &self.log)
            .env("SPIRA_YIELD_WINDOW", WINDOW.to_string())
            .output()
            .unwrap();
        (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn report(&self, args: &[&str]) -> BTreeMap<String, String> {
        let mut a = vec!["report"];
        a.extend(args);
        self.run(&a).1.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn meter(&self, rows: &[(u64, &str, u64, u64, &str, &str)]) {
        let text: String = rows
            .iter()
            .map(|(ago, branch, waited, ran, rc, note)| format!("{} repo {branch} waited={waited}s ran={ran}s rc={rc} {note}\n", utc(now() - ago)))
            .collect();
        std::fs::write(&self.log, text).unwrap();
    }
}

// covers: UC-gate-verdict-22
// covers: UC-gate-verdict-23
#[test_id(tu369oz)]
#[test]
fn yield_reports_an_absent_record_as_unknown_and_classifies_what_the_gate_and_a_person_say() {
    let root = TempDir::new("yield");
    let y = Yield::new(&root, "main");

    let r = y.report(&[]);
    assert_eq!((r["YIELD_REDS"].as_str(), r["YIELD_FAULT"].as_str(), r["YIELD_UNKNOWN"].as_str()), ("?", "?", "?"), "{r:?}");
    assert_eq!((r["YIELD_RECORDER"].as_str(), r["YIELD_SOLO_MED"].as_str(), r["YIELD_CONC_MAX"].as_str()), ("?", "?", "?"));
    assert_eq!(r["YIELD_WINDOW"], WINDOW.to_string(), "the configured window is the one reported");

    y.run(&["record", "repo", "spira/sp-guilty", "1", "branch-red", "tree1", "test-guilty.sh"]);
    y.run(&["record", "repo", "spira/sp-guilty", "1", "branch-red", "tree1", "test-guilty.sh"]);
    let rec = std::fs::read_to_string(std::fs::read_dir(&y.dir).unwrap().next().unwrap().unwrap().path()).unwrap();
    for want in ["branch=spira/sp-guilty", "bead=sp-guilty", "suite=test-guilty.sh", "tree=tree1", "seen=2"] {
        assert!(rec.contains(want), "{want} in {rec}");
    }
    let r = y.report(&[]);
    assert_eq!((r["YIELD_REDS"].as_str(), r["YIELD_UNKNOWN"].as_str()), ("1", "1"), "gating one tree twice is one red: {r:?}");

    y.run(&["pass", "repo", "spira/sp-guilty", "tree2"]);
    let r = y.report(&[]);
    assert_eq!((r["YIELD_DEFECT"].as_str(), r["YIELD_DEFECT_INFERRED"].as_str(), r["YIELD_UNKNOWN"].as_str()), ("1", "1", "0"), "a red whose tree changed then passed: {r:?}");

    y.run(&["record", "repo", "spira/sp-base", "76", "base-red", "-", "test-boxreader.sh"]);
    let r = y.report(&[]);
    assert_eq!((r["YIELD_FAULT"].as_str(), r["YIELD_TOP_FAULT"].as_str()), ("1", "test-boxreader.sh x1"), "BASE_FAIL lands as a gate fault: {r:?}");

    assert_eq!(y.run(&["classify", "spira/sp-guilty", "GATE_FAULT", "the suite read the box"]).0, 0);
    let r = y.report(&[]);
    assert_eq!((r["YIELD_DEFECT"].as_str(), r["YIELD_FAULT"].as_str(), r["YIELD_DEFECT_INFERRED"].as_str()), ("0", "2", "0"), "a stated verdict replaces the inferred one: {r:?}");
    assert!(y.run(&["list"]).1.contains("the suite read the box"));
    assert_eq!(y.run(&["classify", "spira/sp-nosuch", "DEFECT"]).0, 1, "an unknown branch is refused");
    assert_eq!(y.run(&["classify", "spira/sp-guilty", "NOT_A_VERDICT"]).0, 1, "an invalid verdict is refused");

    let ancient = now() - WINDOW * 3;
    std::fs::write(
        y.dir.join("repo.spira_sp-ancient.-.branch-red"),
        format!("at={ancient}\nlast={ancient}\nseen=1\nrepo=repo\nbranch=spira/sp-ancient\nbead=sp-ancient\ntree=-\noutcome=FAIL\nreason=branch-red\nsuite=-\nverdict=UNKNOWN\nby=\nwhy=\n"),
    )
    .unwrap();
    assert_eq!(y.report(&[])["YIELD_REDS"], "2", "a red older than the window is not counted");
    let wide = (WINDOW * 4).to_string();
    assert_eq!(y.report(&["--window", &wide])["YIELD_REDS"], "3", "a wide enough window finds it");

    y.meter(&[(300, "spira/sp-x", 0, 100, "0", "pass"), (310, "spira/sp-y", 0, 40, "0", "pass"), (40, "spira/sp-z", 0, 60, "0", "pass"), (30, "spira/sp-c", 0, 0, "0", "cached")]);
    let r = y.report(&[]);
    let cost = |k: &str| r[k].as_str();
    assert_eq!((cost("YIELD_SOLO_N"), cost("YIELD_SOLO_MED")), ("1", "60"), "a cached pass is no cost: {r:?}");
    assert_eq!((cost("YIELD_CONC_N"), cost("YIELD_CONC_MAX")), ("2", "100"), "overlapping runs are contended: {r:?}");
    std::fs::remove_file(&y.log).unwrap();
    let r = y.report(&[]);
    assert_eq!((r["YIELD_SOLO_MED"].as_str(), r["YIELD_CONC_MAX"].as_str()), ("?", "?"), "a missing log is not zero seconds");

    let silent = Yield::new(&root, "silent");
    std::fs::create_dir(&silent.dir).unwrap();
    silent.meter(&[(200, "spira/sp-q", 0, 90, "1", "branch-red")]);
    let r = silent.report(&[]);
    assert_eq!((r["YIELD_RECORDER"].as_str(), r["YIELD_REDS"].as_str(), r["YIELD_FAULT"].as_str()), ("silent", "?", "?"), "{r:?}");
    assert!(silent.run(&["show"]).1.contains("THE RECORDER IS NOT RUNNING"));
    assert_eq!(y.report(&[])["YIELD_RECORDER"], "ok", "a record holding reds is not withheld by the same log");
}
