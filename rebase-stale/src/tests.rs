//! Contract tests against real temp git repositories. git is real; the bead store
//! and the gate are a recording fake behind `Seam`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::engine::{self, Config};
use crate::git::{parse_worktree_list, Git};
use crate::record::Exit;
use crate::seam::Gate;
use crate::resolve::{Regen, ResolveKind, ResolveRule, Rules};
use crate::seam::{BeadStatus, RepoInfo, Seam};

static N: AtomicUsize = AtomicUsize::new(0);


struct Fx {
    root: testkit::TempDir,
    repo: PathBuf,
    run: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write(p: &Path, s: &str) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(p, s).unwrap();
}

impl Fx {
    fn new(tag: &str) -> Fx {
        let root = testkit::TempDir::new(&format!("rebase-stale-{tag}-{}", N.fetch_add(1, Ordering::SeqCst)));
        let repo = root.join("repo");
        let run = root.join("run");
        std::fs::create_dir_all(run.join("worktree")).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        write(
            &repo.join("spira-config/schema/spira-key-history.txt"),
            "a\nb\n",
        );
        write(&repo.join("f.txt"), "line1\n");
        write(
            &repo.join("x.rs"),
            "fn a() {}\n// original note\nfn b() {}\n",
        );
        write(&repo.join("derived.txt"), "v0\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        Fx { root, repo, run }
    }

    fn wt(&self, id: &str) -> PathBuf {
        self.run.join("worktree").join(id)
    }

    /// Cuts spira/<id> in a worktree named after the bead (the shape an aeon leaves) and
    /// commits `edit` there. The worktree is kept iff `keep`.
    fn branch(&self, id: &str, keep: bool, edit: impl Fn(&Path)) -> String {
        let w = self.wt(id);
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("spira/{id}"),
                w.to_str().unwrap(),
                "main",
            ],
        );
        edit(&w);
        git(&w, &["add", "-A"]);
        git(&w, &["commit", "-q", "-m", &format!("{id}: work")]);
        let tip = git(&w, &["rev-parse", "HEAD"]);
        if !keep {
            git(
                &self.repo,
                &["worktree", "remove", "--force", w.to_str().unwrap()],
            );
        }
        tip
    }

    fn advance_main(&self, edit: impl Fn(&Path)) {
        edit(&self.repo);
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", "advance main"]);
    }

    fn tip(&self, id: &str) -> String {
        git(&self.repo, &["rev-parse", &format!("spira/{id}")])
    }

    fn contains_main(&self, id: &str) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args([
                "merge-base",
                "--is-ancestor",
                "main",
                &format!("spira/{id}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    fn show(&self, id: &str, path: &str) -> String {
        git(&self.repo, &["show", &format!("spira/{id}:{path}")])
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.run.join("rebase-stale.log")).unwrap_or_default()
    }

    /// Registrations other than the private scratch tree: (path, branch).
    fn registrations(&self) -> Vec<(PathBuf, Option<String>)> {
        parse_worktree_list(&git(&self.repo, &["worktree", "list", "--porcelain"]))
            .into_iter()
            .filter(|e| !e.path.to_string_lossy().contains(".rebase-stale."))
            .map(|e| (e.path, e.branch))
            .collect()
    }

    fn cfg(&self) -> Config {
        Config {
            run: self.run.clone(),
            log: self.run.join("rebase-stale.log"),
            git_name: "spira".into(),
            git_email: "spira@spira.invalid".into(),
            lock_wait: Duration::from_secs(5),
        }
    }

    fn rules(&self) -> Rules {
        let mut r = Rules::standard(None);
        // A derived file whose "regeneration" is observable and needs no cargo.
        r.rules.push(ResolveRule {
            path: "derived.txt".into(),
            kind: ResolveKind::Regenerate(Regen {
                argv: vec!["sh".into(), "-c".into(), "echo regenerated".into()],
                fallback_argv: None,
                stdout_to_file: true,
                seed_from_ours: false,
            }),
        });
        r
    }

    fn seam(&self) -> Fake {
        Fake {
            repo: self.repo.clone(),
            status: RefCell::new(BTreeMap::new()),
            unreachable: false,
            green: true,
            no_verdict: false,
            calls: RefCell::new(Vec::new()),
        }
    }
}

struct Fake {
    repo: PathBuf,
    status: RefCell<BTreeMap<String, String>>,
    unreachable: bool,
    green: bool,
    no_verdict: bool,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn calls(&self, prefix: &str) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .cloned()
            .collect()
    }
}

impl Seam for Fake {
    fn home_repo(&self) -> Result<String, String> {
        Ok("fixture".into())
    }
    fn repo(&self, name: &str) -> Result<RepoInfo, String> {
        if name != "fixture" {
            return Err(format!("cannot resolve repo {name}"));
        }
        Ok(RepoInfo {
            name: name.into(),
            path: self.repo.clone(),
            landref: "main".into(),
        })
    }
    fn bead_status(&self, id: &str) -> BeadStatus {
        if self.unreachable {
            return BeadStatus::Unreachable;
        }
        BeadStatus::Known(
            self.status
                .borrow()
                .get(id)
                .cloned()
                .unwrap_or_else(|| "open".into()),
        )
    }
    fn reopen(&self, id: &str, cause: &str, note: &str) {
        self.calls
            .borrow_mut()
            .push(format!("reopen {id} {cause}\n{note}"));
    }
    fn bump_requeue(&self, id: &str, reason: &str) {
        self.calls.borrow_mut().push(format!("bump {id} {reason}"));
    }
    fn note(&self, id: &str, text: &str) {
        self.calls.borrow_mut().push(format!("note {id} {text}"));
    }
    fn submit(&self, branch: &str, repo_name: &str) -> (Gate, String) {
        // The gate must see the REBASED tip on the real branch ref.
        let tip = git(&self.repo, &["rev-parse", branch]);
        self.calls
            .borrow_mut()
            .push(format!("submit {branch} {repo_name} {tip}"));
        if self.no_verdict {
            (
                Gate::NoVerdict,
                "gate: VERDICT=NO_VERDICT reason=budget suite=-".into(),
            )
        } else if self.green {
            (Gate::Green, "queue submit: certified".into())
        } else {
            (
                Gate::Red,
                "gate: VERDICT=FAIL reason=suite-red suite=test-stub".into(),
            )
        }
    }
}

fn run(fx: &Fx, seam: &Fake, id: &str) -> engine::Report {
    engine::run(id, Some("fixture"), &fx.cfg(), seam, &fx.rules())
}

fn append(p: PathBuf, s: &str) {
    let mut t = std::fs::read_to_string(&p).unwrap();
    t.push_str(s);
    std::fs::write(p, t).unwrap();
}

#[test]
fn stale_only_by_appended_key_history_rebases_mechanically_with_no_aeon() {
    let fx = Fx::new("keyhist");
    let kh = "spira-config/schema/spira-key-history.txt";
    fx.branch("sp-kh", false, |w| append(w.join(kh), "c-from-branch\n"));
    fx.advance_main(|r| append(r.join(kh), "c-from-main\n"));
    let before = fx.registrations();
    let seam = fx.seam();

    let r = run(&fx, &seam, "sp-kh");
    assert_eq!(r.exit, Exit::Ok, "{r:?}");
    assert!(
        r.stdout
            .as_deref()
            .unwrap()
            .contains("rebased (mechanical)"),
        "{r:?}"
    );
    assert!(fx.contains_main("sp-kh"));
    assert_eq!(fx.show("sp-kh", kh), "a\nb\nc-from-main\nc-from-branch");
    let submits = seam.calls("submit");
    assert_eq!(submits.len(), 1, "re-certified exactly once");
    assert!(
        submits[0].ends_with(&fx.tip("sp-kh")),
        "the gate saw the rebased tip"
    );
    assert!(seam.calls("reopen").is_empty(), "no aeon");
    assert!(seam.calls("note sp-kh")[0].contains("(mechanical)"));
    assert!(
        fx.log()
            .contains("id=sp-kh repo=fixture outcome=mechanical reason=tip="),
        "{}",
        fx.log()
    );
    assert_eq!(
        fx.registrations(),
        before,
        "no bead worktree created or removed"
    );
}

#[test]
fn real_same_line_conflict_is_returned_with_the_hunk_quoted() {
    let fx = Fx::new("conflict");
    let old = fx.branch("sp-cf", false, |w| {
        write(&w.join("f.txt"), "line1-branch\n")
    });
    fx.advance_main(|r| write(&r.join("f.txt"), "line1-main\n"));
    let seam = fx.seam();

    let r = run(&fx, &seam, "sp-cf");
    assert_eq!(r.exit, Exit::Conflict, "{r:?}");
    assert_eq!(fx.tip("sp-cf"), old, "branch untouched");
    assert!(seam.calls("submit").is_empty(), "gate never ran");
    let reopen = seam.calls("reopen sp-cf rebase-conflict");
    assert_eq!(reopen.len(), 1);
    for want in [
        "<<<<<<<",
        "line1-branch",
        "line1-main",
        ">>>>>>>",
        "Conflicting file(s): f.txt",
    ] {
        assert!(
            reopen[0].contains(want),
            "note lacks {want:?}: {}",
            reopen[0]
        );
    }
    assert_eq!(seam.calls("bump sp-cf merge-conflict").len(), 1);
    assert!(
        fx.log().contains("outcome=conflict reason=files: f.txt"),
        "{}",
        fx.log()
    );
    let sg = Git::new(fx.run.join("worktree/.rebase-stale.repo"), "t", "t@t");
    assert!(!sg.mid_rebase(), "rebase aborted");
}

#[test]
fn a_mixed_stop_resolves_nothing_and_quotes_only_the_real_conflict() {
    let fx = Fx::new("mixed");
    let kh = "spira-config/schema/spira-key-history.txt";
    let old = fx.branch("sp-mx", false, |w| {
        append(w.join(kh), "k-branch\n");
        write(&w.join("f.txt"), "branch\n");
    });
    fx.advance_main(|r| {
        append(r.join(kh), "k-main\n");
        write(&r.join("f.txt"), "main\n");
    });
    let seam = fx.seam();
    let r = run(&fx, &seam, "sp-mx");
    assert_eq!(r.exit, Exit::Conflict);
    assert_eq!(fx.tip("sp-mx"), old);
    let note = &seam.calls("reopen")[0];
    assert!(
        note.contains("Conflicting file(s): f.txt") && !note.contains("--- spira-config"),
        "{note}"
    );
}

#[test]
fn red_gate_restores_the_pre_rebase_tip_and_quotes_the_gate() {
    let fx = Fx::new("gatered");
    let old = fx.branch("sp-gr", false, |w| write(&w.join("branch-only.txt"), "b\n"));
    fx.advance_main(|r| write(&r.join("main-only.txt"), "m\n"));
    let mut seam = fx.seam();
    seam.green = false;

    let r = run(&fx, &seam, "sp-gr");
    assert_eq!(r.exit, Exit::GateRed, "{r:?}");
    assert_eq!(fx.tip("sp-gr"), old, "restored");
    assert_eq!(seam.calls("submit").len(), 1);
    let reopen = &seam.calls("reopen sp-gr rebase-gate-red")[0];
    assert!(
        reopen.contains("VERDICT=FAIL") && reopen.contains(&old),
        "{reopen}"
    );
    assert!(
        fx.log().contains("outcome=gate-red reason=tip="),
        "{}",
        fx.log()
    );
}

#[test]
fn a_finished_sessions_leftover_worktree_does_not_block() {
    let fx = Fx::new("leftover");
    fx.branch("sp-lo", true, |w| write(&w.join("leftover.txt"), "x\n"));
    fx.advance_main(|r| write(&r.join("main-advance.txt"), "m\n"));
    let before = fx.registrations();
    let seam = fx.seam(); // status "open", no pidfiles: the session is gone

    let r = run(&fx, &seam, "sp-lo");
    assert_eq!(r.exit, Exit::Ok, "{r:?}");
    assert!(r.stdout.as_deref().unwrap().contains("rebased (clean)"));
    assert!(fx.contains_main("sp-lo"));
    assert!(!fx.log().contains("outcome=busy"), "{}", fx.log());
    // Collision guarantee: the leftover keeps its registration, path and branch …
    assert_eq!(fx.registrations(), before);
    // … and it follows its branch, clean, to the new tip.
    let w = Git::new(fx.wt("sp-lo"), "t", "t@t");
    assert_eq!(w.rev("HEAD").unwrap(), fx.tip("sp-lo"));
    assert_eq!(w.porcelain().unwrap().trim(), "");
    assert!(fx.wt("sp-lo").join("main-advance.txt").exists());
    // The scratch tree never holds a branch.
    let sg = Git::new(fx.run.join("worktree/.rebase-stale.repo"), "t", "t@t");
    assert!(
        sg.out(["symbolic-ref", "-q", "HEAD"]).is_none(),
        "scratch is detached"
    );
}

#[test]
fn a_red_gate_brings_the_leftover_back_to_the_old_tip() {
    let fx = Fx::new("leftover-red");
    let old = fx.branch("sp-lr", true, |w| write(&w.join("leftover.txt"), "x\n"));
    fx.advance_main(|r| write(&r.join("main-advance.txt"), "m\n"));
    let mut seam = fx.seam();
    seam.green = false;
    assert_eq!(run(&fx, &seam, "sp-lr").exit, Exit::GateRed);
    let w = Git::new(fx.wt("sp-lr"), "t", "t@t");
    assert_eq!(w.rev("HEAD").unwrap(), old);
    assert_eq!(w.porcelain().unwrap().trim(), "");
}

#[test]
fn a_gate_with_no_verdict_is_not_a_red() {
    let fx = Fx::new("noverdict");
    let old = fx.branch("sp-nv", false, |w| write(&w.join("branch-only.txt"), "b\n"));
    fx.advance_main(|r| write(&r.join("main-only.txt"), "m\n"));
    let mut seam = fx.seam();
    seam.green = false;
    seam.no_verdict = true;

    let r = run(&fx, &seam, "sp-nv");
    assert_eq!(r.exit, Exit::NotAttempted, "{r:?}");
    assert_eq!(fx.tip("sp-nv"), old, "restored");
    assert_eq!(seam.calls("submit").len(), 1);
    assert!(seam.calls("reopen").is_empty(), "{:?}", seam.calls("reopen"));
}

/// `/proc/<pid>/cmdline`, NUL-joined argv rendered as spaces — for polling a just-spawned
/// child past its own `exec()` (sp-os3of): empty once the pid is gone.
fn cmdline_of(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

fn assert_busy(fx: &Fx, seam: &Fake, id: &str, old: &str, want: &str) {
    let r = run(fx, seam, id);
    assert_eq!(r.exit, Exit::NotAttempted, "{r:?}");
    assert_eq!(fx.tip(id), old, "branch untouched");
    assert!(seam.calls("submit").is_empty() && seam.calls("reopen").is_empty());
    assert!(fx.wt(id).join(".git").exists(), "worktree untouched");
    assert!(fx.log().contains("outcome=busy"), "{}", fx.log());
    assert!(fx.log().contains(want), "log lacks {want:?}: {}", fx.log());
}

fn live_holder_fixture(tag: &str, id: &str) -> (Fx, String) {
    let fx = Fx::new(tag);
    let old = fx.branch(id, true, |w| write(&w.join("busy.txt"), "x\n"));
    fx.advance_main(|r| write(&r.join("main-advance.txt"), "m\n"));
    (fx, old)
}

#[test]
fn a_live_holder_blocks_hold_pidfile() {
    let (fx, old) = live_holder_fixture("live-hold", "sp-lh");
    write(
        &fx.run.join("hold-sp-lh.pid"),
        &format!("{}\n", std::process::id()),
    );
    assert_busy(
        &fx,
        &fx.seam(),
        "sp-lh",
        &old,
        "hold-sp-lh.pid names live pid",
    );
}

#[test]
fn a_live_holder_blocks_aeon_pidfile() {
    let (fx, old) = live_holder_fixture("live-aeon", "sp-la");
    // Kill-on-drop (sp-r70dc): a failed assertion between spawn and the explicit kill
    // below used to leave this fixture running for its full 30s as an orphan.
    let mut child = testkit::ChildGuard::spawn(
        Command::new("bash").args(["-c", "exec -a aeon.sh-stub sleep 30"]),
    );
    // bash's own exec() is a second step after fork(), and /proc/<pid>/cmdline can still
    // read as bash's (or briefly empty, mid-transition) the instant after spawn()
    // returns — green in isolation, red under a loaded gate (sp-os3of, aeon/src/trace.rs
    // had the same shape). Poll (bounded) until the exec has actually landed, rather than
    // a fixed sleep that is merely usually enough.
    //
    // Checked by argv[0] alone, not "contains aeon.sh-stub": bash's OWN pre-exec cmdline
    // is `bash -c "exec -a aeon.sh-stub sleep 30"`, which already contains the substring
    // "aeon.sh-stub" in its `-c` argument — a naive `.contains(...)` poll would pass
    // instantly, during the bash phase, defeating the wait entirely.
    let mut tries = 0;
    while cmdline_of(child.id()).split(' ').next() != Some("aeon.sh-stub") {
        assert!(tries < 500, "the child never finished exec'ing into aeon.sh-stub");
        tries += 1;
        std::thread::sleep(Duration::from_millis(10));
    }
    write(
        &fx.run.join("aeon-builder-sp-la.pid"),
        &format!("{}\n", child.id()),
    );
    assert_busy(&fx, &fx.seam(), "sp-la", &old, "live aeon pid");
    child.kill();
}

#[test]
fn a_dead_aeon_pidfile_is_not_a_holder() {
    let fx = Fx::new("dead-aeon");
    fx.branch("sp-da", true, |w| write(&w.join("x.txt"), "x\n"));
    fx.advance_main(|r| write(&r.join("m.txt"), "m\n"));
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    write(&fx.run.join("aeon-builder-sp-da.pid"), &format!("{pid}\n"));
    assert_eq!(run(&fx, &fx.seam(), "sp-da").exit, Exit::Ok);
}

#[test]
fn a_live_holder_blocks_in_progress_lease() {
    let (fx, old) = live_holder_fixture("live-lease", "sp-ll");
    let seam = fx.seam();
    seam.status
        .borrow_mut()
        .insert("sp-ll".into(), "in_progress".into());
    assert_busy(&fx, &seam, "sp-ll", &old, "in_progress");
}

#[test]
fn a_live_holder_blocks_unreachable_database() {
    let (fx, old) = live_holder_fixture("live-db", "sp-ld");
    let mut seam = fx.seam();
    seam.unreachable = true;
    assert_busy(&fx, &seam, "sp-ld", &old, "did not answer");
}

#[test]
fn a_live_holder_blocks_process_working_in_it() {
    let (fx, old) = live_holder_fixture("live-cwd", "sp-lc");
    // Kill-on-drop (sp-r70dc).
    let mut child = testkit::ChildGuard::spawn(
        Command::new("sleep").arg("30").current_dir(fx.wt("sp-lc")),
    );
    assert_busy(&fx, &fx.seam(), "sp-lc", &old, "is working in");
    child.kill();
}

#[test]
fn a_dirty_leftover_is_not_touched() {
    let (fx, old) = live_holder_fixture("dirty", "sp-dy");
    write(&fx.wt("sp-dy").join("uncommitted.txt"), "wip\n");
    assert_busy(&fx, &fx.seam(), "sp-dy", &old, "uncommitted changes");
    assert!(fx.wt("sp-dy").join("uncommitted.txt").exists());
}

#[test]
fn comment_only_conflict_unions() {
    let fx = Fx::new("comment");
    fx.branch("sp-cm", false, |w| {
        write(
            &w.join("x.rs"),
            "fn a() {}\n// note from the branch\nfn b() {}\n",
        )
    });
    fx.advance_main(|r| write(&r.join("x.rs"), "fn a() {}\n// note from main\nfn b() {}\n"));
    let seam = fx.seam();
    let r = run(&fx, &seam, "sp-cm");
    assert_eq!(r.exit, Exit::Ok, "{r:?}");
    assert!(fx.log().contains("outcome=mechanical"));
    assert_eq!(
        fx.show("sp-cm", "x.rs"),
        "fn a() {}\n// note from main\n// note from the branch\nfn b() {}"
    );
}

#[test]
fn a_derived_file_is_regenerated_not_merged() {
    let fx = Fx::new("regen");
    fx.branch("sp-rg", false, |w| {
        write(&w.join("derived.txt"), "v-branch\n")
    });
    fx.advance_main(|r| write(&r.join("derived.txt"), "v-main\n"));
    let seam = fx.seam();
    assert_eq!(run(&fx, &seam, "sp-rg").exit, Exit::Ok);
    assert_eq!(fx.show("sp-rg", "derived.txt"), "regenerated");
}

#[test]
fn an_already_current_branch_is_nothing_to_do() {
    let fx = Fx::new("current");
    fx.advance_main(|r| write(&r.join("m.txt"), "m\n"));
    let old = fx.branch("sp-cu", false, |w| write(&w.join("b.txt"), "b\n"));
    let seam = fx.seam();
    let r = run(&fx, &seam, "sp-cu");
    assert_eq!(r.exit, Exit::Ok);
    assert!(r.stdout.unwrap().contains("nothing to do"));
    assert_eq!(fx.tip("sp-cu"), old);
    assert!(seam.calls("submit").is_empty());
    assert!(fx.log().contains("outcome=current reason="));
}

#[test]
fn unknown_branch_or_repo_is_not_attempted() {
    let fx = Fx::new("missing");
    let seam = fx.seam();
    assert_eq!(run(&fx, &seam, "sp-none").exit, Exit::NotAttempted);
    assert_eq!(
        engine::run("sp-none", Some("nope"), &fx.cfg(), &seam, &fx.rules()).exit,
        Exit::NotAttempted
    );
    assert_eq!(fx.log(), "", "no log line before the branch resolves");
}

#[test]
fn a_foreign_checkout_is_never_touched() {
    let fx = Fx::new("foreign");
    let w = fx.root.join("elsewhere");
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "spira/sp-fo",
            w.to_str().unwrap(),
            "main",
        ],
    );
    write(&w.join("b.txt"), "b\n");
    git(&w, &["add", "-A"]);
    git(&w, &["commit", "-q", "-m", "w"]);
    fx.advance_main(|r| write(&r.join("m.txt"), "m\n"));
    let old = fx.tip("sp-fo");
    let r = run(&fx, &fx.seam(), "sp-fo");
    assert_eq!(r.exit, Exit::NotAttempted);
    assert_eq!(fx.tip("sp-fo"), old);
    assert!(fx.log().contains("outside the sanctioned root"));
}

#[test]
fn the_scratch_tree_survives_reuse_and_a_dangling_registration() {
    let fx = Fx::new("reuse");
    fx.branch("sp-r1", false, |w| write(&w.join("r1.txt"), "1\n"));
    fx.branch("sp-r2", false, |w| write(&w.join("r2.txt"), "2\n"));
    fx.advance_main(|r| write(&r.join("m.txt"), "m\n"));
    let seam = fx.seam();
    assert_eq!(run(&fx, &seam, "sp-r1").exit, Exit::Ok);
    // A crash that deleted the scratch directory but not its registration.
    std::fs::remove_dir_all(fx.run.join("worktree/.rebase-stale.repo")).unwrap();
    assert_eq!(run(&fx, &seam, "sp-r2").exit, Exit::Ok);
    assert!(fx.contains_main("sp-r1") && fx.contains_main("sp-r2"));
}

#[test]
fn lib_seam_passes_payloads_on_stdin_and_reads_status_with_a_positive_control() {
    use crate::seam::LibSeam;
    let fx = Fx::new("libseam");
    let home = fx.root.join("home");
    let out = fx.root.join("calls");
    // A stand-in lib.sh / queue / bd: each records its argv, one per line, and any stdin.
    write(
        &home.join("lib.sh"),
        &format!(
            "rec() {{ printf '%s|' \"$@\" >> '{o}'; echo >> '{o}'; }}\n\
             bead_reopen() {{ rec bead_reopen \"$@\"; }}\n\
             bdq() {{ rec bdq \"$@\"; }}\n\
             repo_root() {{ printf '/r/%s' \"$1\"; }}\n\
             spira_landref() {{ printf 'local/main'; }}\n",
            o = out.display()
        ),
    );
    let queue = home.join("queue");
    // testkit::write_exe, never write + chmod: a write descriptor held while another test
    // thread forks makes the exec fail with ETXTBSY (testkit/DESIGN.md).
    testkit::write_exe(&queue, "#!/bin/sh\necho \"out:$1 $2 $3\"; echo err >&2; exit 1\n");
    let bd = fx.root.join("bd");
    testkit::write_exe(&bd, "#!/bin/sh\n[ \"$3\" = list ] && { echo '[{\"id\":\"sp-any\"}]'; exit 0; }\ncase \"$4\" in sp-ip) echo '{\"status\":\"in_progress\"}';; *) exit 1;; esac\n");

    let mut s = LibSeam::new(home.clone(), Some(fx.root.to_path_buf()), bd.to_string_lossy().into(), fx.run.clone());
    s.queue_bin = queue;
    let note = "multi\nline $(not expanded) 'quoted'";
    s.reopen("sp-1", "rebase-conflict", note);
    s.note("sp-1", "hello");
    let calls = std::fs::read_to_string(&out).unwrap();
    assert!(calls.contains(&format!("bead_reopen|sp-1|rebase-conflict|{note}|")), "{calls}");
    assert!(calls.contains("bdq|note|sp-1|hello|"), "{calls}");

    let info = s.repo("fixture").unwrap();
    assert_eq!((info.path, info.landref.as_str()), (PathBuf::from("/r/fixture"), "local/main"));
    let (gate, text) = s.submit("spira/sp-1", "fixture");
    assert!(gate == Gate::Red && text.contains("out:submit spira/sp-1 fixture") && text.contains("err"), "{text}");

    testkit::write_exe(&s.queue_bin, "#!/bin/sh\nexit 75\n");
    assert_eq!(s.submit("spira/sp-1", "fixture").0, Gate::NoVerdict);
    testkit::write_exe(&s.queue_bin, "#!/bin/sh\nexit 0\n");
    assert_eq!(s.submit("spira/sp-1", "fixture").0, Gate::Green);

    assert_eq!(s.bead_status("sp-ip"), BeadStatus::Known("in_progress".into()));
    assert_eq!(s.bead_status("sp-unknown"), BeadStatus::Known(String::new()));
    let dead = LibSeam::new(home, Some(fx.root.to_path_buf()), "/nonexistent/bd".into(), fx.run.clone());
    assert_eq!(dead.bead_status("sp-ip"), BeadStatus::Unreachable, "no positive control, no absence");
}
