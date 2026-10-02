//! The CLI end to end against a real git repo, with testenv and bead.sh stood in by scripts
//! that print what the real ones print: a planted red is reported with the right window,
//! a fault is its own event, and a bead that cannot be filed is retried, not lost.

use std::fs;
use std::path::Path;
use std::process::Command;
use testkit::{write_exe, TempDir};

struct Fx {
    d: TempDir,
}

impl Fx {
    fn new() -> Fx {
        let d = TempDir::new("cert-sweep");
        let p = d.path();
        let repo = p.join("repo");
        fs::create_dir_all(repo.join("spira")).unwrap();
        for s in ["test-a.sh", "test-b.sh"] {
            fs::write(repo.join("spira").join(s), "#!/bin/sh\n").unwrap();
        }
        let g = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(a).status().unwrap().success());
        g(&["init", "-q", "-b", "main"]);
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "one"]);
        fs::create_dir_all(p.join("bin")).unwrap();
        fs::create_dir_all(p.join("run")).unwrap();
        write_exe(p.join("bin/bead.sh"), "#!/bin/sh\necho \"$@\" >> \"$FX/beads\"\n[ -e \"$FX/bead-fail\" ] && { echo no >&2; exit 1; }\necho '{\"id\":\"sp-fake1\"}'\n");
        Fx { d }
    }

    fn commit(&self, round: &str) {
        let repo = self.d.path().join("repo");
        let g = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(a).status().unwrap().success());
        g(&["commit", "-q", "--allow-empty", "-m", round]);
        g(&["update-ref", &format!("refs/archive/rounds/{round}"), "HEAD"]);
        g(&["branch", "-f", "local/main", "HEAD"]);
    }

    fn testenv(&self, b_verdict: &str) {
        write_exe(
            self.d.path().join("bin/testenv"),
            &format!("#!/bin/sh\ncat >/dev/null\nprintf '  test-a.sh   ok   2s\\n  test-b.sh   {b_verdict}   rc=1 after 3s\\n'\n"),
        );
    }

    fn cert(&self, args: &[&str]) -> (i32, String, String) {
        let p = self.d.path();
        let o = Command::new(env!("CARGO_BIN_EXE_cert-sweep"))
            .args(args)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", p.join("bin").display()))
            .env("FX", p)
            .env("SPIRA_RUN", p.join("run"))
            .env("SPIRA_REPO", p.join("repo"))
            .output()
            .unwrap();
        (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
    }

    fn sample(&self) -> (i32, String, String) {
        self.cert(&["pass", "--mode", "subset", "--subset-div", "1"])
    }

    fn seed(&self, round: &str) {
        let rd = self.d.path().join("seed-results");
        fs::create_dir_all(&rd).unwrap();
        for s in ["test-a.sh", "test-b.sh"] {
            fs::write(rd.join(format!("{s}.result")), "ok 1 2 - p e 0\n").unwrap();
        }
        let sha = String::from_utf8(Command::new("git").arg("-C").arg(self.d.path().join("repo")).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap();
        let (rc, _, e) = self.cert(&["seed", "--results-dir", rd.to_str().unwrap(), "--commit", sha.trim(), "--round", round]);
        assert_eq!(rc, 0, "{e}");
    }

    fn beads(&self) -> String {
        fs::read_to_string(self.d.path().join("beads")).unwrap_or_default()
    }
}

#[test]
fn a_planted_red_is_reported_with_its_window_and_a_raised_priority_bead() {
    let fx = Fx::new();
    fx.commit("183");
    fx.seed("183");
    fx.commit("184");
    fx.testenv("ok");
    let (rc, out, err) = fx.sample();
    assert_eq!(rc, 0, "{err}");
    assert!(!out.contains("RED"), "{out}");
    fx.commit("185");
    fx.testenv("RED");
    let (rc, out, err) = fx.sample();
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("NEW RED test-b.sh: last green round 184"), "{out}");
    assert!(out.contains("first red round 185"), "{out}");
    assert!(fx.beads().contains("--priority 1"), "{}", fx.beads());
    assert!(fx.beads().contains("went red at round 185"), "{}", fx.beads());
    let (_, out, _) = fx.sample();
    assert!(!out.contains("NEW RED"), "a suite already red is not reported again: {out}");
    assert_eq!(fx.beads().lines().count(), 1);
}

#[test]
fn a_fault_is_reported_as_a_fault_never_as_a_red_and_files_nothing() {
    let fx = Fx::new();
    fx.commit("1");
    fx.seed("1");
    fx.commit("2");
    fx.testenv("FAULT");
    let (rc, out, _) = fx.sample();
    assert_eq!(rc, 0);
    assert!(out.contains("FAULT test-b.sh at round 2"), "{out}");
    assert!(!out.contains("RED test-b"), "{out}");
    assert_eq!(fx.beads(), "");
    let hist = fs::read_to_string(Path::new(fx.d.path()).join("run/tsd/cert-history.jsonl")).unwrap();
    assert!(hist.contains("\"verdict\":\"fault\""), "{hist}");
}

#[test]
fn a_bead_that_cannot_be_filed_leaves_the_red_to_be_found_again() {
    let fx = Fx::new();
    fx.commit("1");
    fx.seed("1");
    fx.commit("2");
    fx.testenv("RED");
    fs::write(fx.d.path().join("bead-fail"), "").unwrap();
    let (_, out, err) = fx.sample();
    assert!(!out.contains("NEW RED"), "{out}");
    assert!(err.contains("held for the next pass"), "{err}");
    fs::remove_file(fx.d.path().join("bead-fail")).unwrap();
    let (_, out, _) = fx.sample();
    assert!(out.contains("NEW RED test-b.sh: last green round 1"), "{out}");
}

#[test]
fn a_runner_that_returns_nothing_is_a_sweep_fault() {
    let fx = Fx::new();
    fx.commit("1");
    write_exe(fx.d.path().join("bin/testenv"), "#!/bin/sh\ncat >/dev/null\necho boom\n");
    let (rc, out, _) = fx.sample();
    assert_eq!(rc, 1);
    assert!(out.contains("SWEEP FAULT"), "{out}");
}

#[test]
fn the_bead_bound_holds_the_remainder() {
    let fx = Fx::new();
    fx.commit("1");
    fx.seed("1");
    fx.commit("2");
    write_exe(fx.d.path().join("bin/testenv"), "#!/bin/sh\ncat >/dev/null\nprintf '  test-a.sh   RED   rc=1 after 3s\\n  test-b.sh   RED   rc=1 after 3s\\n'\n");
    let (_, out, err) = fx.cert(&["pass", "--mode", "subset", "--subset-div", "1", "--max-beads", "1"]);
    assert_eq!(out.matches("NEW RED").count(), 1, "{out}");
    assert!(err.contains("over the 1-bead bound"), "{err}");
    let (_, out, _) = fx.cert(&["pass", "--mode", "subset", "--subset-div", "1", "--max-beads", "1"]);
    assert_eq!(out.matches("NEW RED").count(), 1, "the held one is reported next: {out}");
}

/// Every shipped cert-sweep unit must pass the args `pass` requires (--mode, --tree);
/// a unit that omits --tree exits 2 on every tick.
#[test]
fn shipped_units_satisfy_usage() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd");
    for u in ["spira-cert-sweep-full.service", "spira-cert-sweep-sample.service"] {
        let t = fs::read_to_string(dir.join(u)).unwrap();
        let l = t.lines().find(|l| l.starts_with("ExecStart=")).unwrap();
        assert!(l.contains(" pass "), "{u}: {l}");
        assert!(l.contains(" --mode "), "{u}: missing --mode");
        assert!(l.contains(" --tree "), "{u}: missing --tree");
    }
}
