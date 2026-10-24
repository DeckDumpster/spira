use acceptance_daily::{pass, real::{slot_locked, Real}, State};
use spira_otel::metrics::MetricsSink;
use std::process::Command;

fn sh(dir: &std::path::Path, args: &[&str]) {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

struct Rig {
    dir: testkit::TempDir,
    real: Real,
}

impl Rig {
    fn new(accept_body: &str) -> Rig {
        let dir = testkit::TempDir::new("acceptance-daily-stub");
        let p = dir.path();
        let repo = p.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);
        sh(&repo, &["branch", "local/main", "main"]);
        std::fs::create_dir_all(p.join("run/gate-worker")).unwrap();
        testkit::write_exe(p.join("accept.sh"), accept_body);
        let log = p.join("bead.log");
        testkit::write_exe(
            p.join("bead.sh"),
            &format!(
                "#!/bin/bash\nn=$(cat {c} 2>/dev/null || echo 0)\nverb=\"$1\"\nprev=\"\"\nfor a; do [ \"$prev\" = --body-file -o \"$prev\" = --append-body ] && body=\"$a\"; prev=\"$a\"; done\n{{ echo \"$verb $*\"; cat \"$body\"; echo ---; }} >> {l}\nif [ \"$verb\" = file ]; then echo $((n+1)) > {c}; echo '{{\"id\":\"sp-stub'$((n+1))'\"}}'; fi\n",
                c = p.join("count").display(),
                l = log.display()
            ),
        );
        testkit::write_exe(p.join("lc"), "#!/bin/sh\necho '{\"bead\":{\"state\":\"OPEN\"}}'\n");
        let real = Real {
            repo: repo.to_string_lossy().into(),
            land_ref: "local/main".into(),
            run: p.join("run"),
            tree: p.join("run/acceptance-daily/tree"),
            accept: p.join("accept.sh").to_string_lossy().into(),
            bead: p.join("bead.sh").to_string_lossy().into(),
            lc: p.join("lc").to_string_lossy().into(),
            sink: MetricsSink::for_component(None, "acceptance-daily"),
        };
        Rig { dir, real }
    }

    fn beads(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("bead.log")).unwrap_or_default()
    }
}

const RED: &str = "#!/bin/sh\necho '  ok    install'\necho '  FAIL  work summoned: no read grant'\nexit 1\n";

#[test]
fn a_stubbed_red_run_files_one_bead_with_the_fail_line_and_a_repeat_appends() {
    let rig = Rig::new(RED);
    let mut st = State::default();
    let first = pass(&rig.real, &mut st, &mut |_| Ok(()), 0);
    assert_eq!(first.filed.len(), 1, "{first:?}");
    assert!(rig.beads().contains("FAIL  work summoned: no read grant"));
    assert!(!rig.dir.path().join("run/acceptance-daily/tree").exists(), "the worktree is removed");
    let second = pass(&rig.real, &mut st, &mut |_| Ok(()), 0);
    assert!(second.filed.is_empty() && second.amended.len() == 1, "{second:?}");
    assert_eq!(rig.beads().matches("\nfile ").count() + usize::from(rig.beads().starts_with("file ")), 1);
    assert!(rig.beads().contains("amend sp-stub1"));
}

#[test]
fn a_stubbed_green_run_files_nothing() {
    let rig = Rig::new("#!/bin/sh\nexit 0\n");
    let o = pass(&rig.real, &mut State::default(), &mut |_| Ok(()), 0);
    assert!(o.failed.is_empty() && o.filed.is_empty());
    assert_eq!(rig.beads(), "");
}

#[test]
fn a_held_gate_worker_slot_reads_as_a_round() {
    let dir = testkit::TempDir::new("acceptance-daily-lock");
    assert!(!slot_locked(dir.path()), "positive control: no lock file, no round");
    let p = dir.path().join("worker.lock.1");
    std::fs::write(&p, "").unwrap();
    assert!(!slot_locked(dir.path()), "a free lock file is no round");
    let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    use std::os::unix::io::AsRawFd;
    assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    assert!(slot_locked(dir.path()), "a held slot is a round");
}
