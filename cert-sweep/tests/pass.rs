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
        write_exe(p.join("bin/bd"), "#!/bin/sh\ncat \"$FX/open.json\" 2>/dev/null || echo '[]'\n");
        write_exe(
            p.join("bin/bead.sh"),
            "#!/bin/sh\necho \"$@\" >> \"$FX/beads\"\nwhile [ $# -gt 0 ]; do [ \"$1\" = --body-file ] && cat \"$2\" >> \"$FX/beads\"; shift; done\n[ -e \"$FX/bead-fail\" ] && { echo no >&2; exit 1; }\necho '{\"id\":\"sp-fake1\"}'\n",
        );
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
            .env("SPIRA_DB", p.join("db"))
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
    assert_eq!(fx.beads().matches("--priority").count(), 1);
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

fn history_with_one_flip_and_one_regression() -> Fx {
    let fx = Fx::new();
    fx.commit("1");
    fx.seed("1");
    let repo = fx.d.path().join("repo");
    let g = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(a).status().unwrap().success());
    for (m, bead) in [("m1", "sp-aaa"), ("m2", "sp-bad"), ("m3", "sp-ccc")] {
        fs::write(repo.join(m), m).unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", &format!("round-q: merge {bead} ({m})")]);
    }
    g(&["update-ref", "refs/archive/rounds/2", "HEAD"]);
    g(&["branch", "-f", "local/main", "HEAD"]);
    write_exe(
        fx.d.path().join("bin/testenv"),
        "#!/bin/sh
suites=$(cat)
b=$(git -C \"$FX/repo\" rev-parse \"$3\")
for s in $suites; do
  case $s in
  test-a.sh) n=$(cat \"$FX/n\" 2>/dev/null || echo 0); echo $((n+1)) > \"$FX/n\"
    if [ \"$n\" = 0 ]; then echo '  test-a.sh   RED   rc=1 after 3s'; echo 'FAIL: a flaked'; else echo '  test-a.sh   ok   2s'; fi ;;
  test-b.sh) if git -C \"$FX/repo\" merge-base --is-ancestor \"$(git -C \"$FX/repo\" log --format=%H --grep='merge sp-bad' -1 local/main)\" \"$b\"; then echo '  test-b.sh   RED   rc=1 after 3s'; echo 'FAIL: b broke'; else echo '  test-b.sh   ok   2s'; fi ;;
  esac
done
",
    );
    fx
}

#[test]
fn a_flip_and_a_regression_file_exactly_two_beads_each_attributed() {
    let fx = history_with_one_flip_and_one_regression();
    let (rc, out, err) = fx.sample();
    assert_eq!(rc, 0, "{err}");
    let beads = fx.beads();
    assert_eq!(beads.matches("--priority").count(), 2, "{beads}\n{out}\n{err}");
    assert!(beads.contains("test-a.sh flips on one commit"), "{beads}");
    assert!(beads.contains("test-b.sh regressed at round 2 (member sp-bad)"), "{beads}");
    assert!(beads.contains("First FAIL line: FAIL: b broke"), "{beads}");
    assert!(out.contains("FLIP test-a.sh"), "{out}");
    let (_, out, _) = fx.sample();
    assert!(!out.contains("RED") && !out.contains("FLIP"), "nothing is reported twice: {out}");
    assert_eq!(fx.beads().matches("--priority").count(), 2);
}

#[test]
fn an_open_bead_for_the_suite_is_not_filed_again() {
    let fx = history_with_one_flip_and_one_regression();
    fs::write(
        fx.d.path().join("open.json"),
        r#"[{"id":"sp-old1","title":"test-a.sh flips"},{"id":"sp-old2","title":"basefail: test-b.sh is red"}]"#,
    )
    .unwrap();
    let (rc, out, err) = fx.sample();
    assert_eq!(rc, 0, "{err}");
    assert_eq!(fx.beads(), "", "{out}");
    assert!(out.contains("sp-old1 is already open") && out.contains("sp-old2 is already open"), "{out}");
}
