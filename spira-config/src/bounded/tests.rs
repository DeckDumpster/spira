    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test_id(ttb7fhc)]
    #[test]
    fn the_program_is_the_callers_direct_child() {
        let out = bounded("sh").args(["-c", "echo $PPID"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), std::process::id().to_string());
    }

    #[test_id(tglv5z1)]
    #[test]
    fn get_program_names_the_real_program() {
        assert_eq!(bounded("git").get_program(), "git");
    }

    #[test_id(tunxd7c)]
    #[test]
    fn a_deadline_hit_reads_124() {
        let mk = || {
            let mut b = bounded_for("sleep", "0.05");
            b.arg("5");
            b
        };
        assert_eq!(mk().status().unwrap().code(), Some(124));
        assert_eq!(mk().output().unwrap().status.code(), Some(124));
        let child = mk().spawn().unwrap();
        assert_eq!(child.wait_with_output().unwrap().status.code(), Some(124));
    }

    #[test_id(tzihn38)]
    #[test]
    fn expiry_kills_the_process_group() {
        let pid = expire_when_ready("sleep 300 & echo $!; wait", false);
        let gone = (0..3000).any(|_| {
            let alive = std::fs::read_to_string(format!("{}/{pid}/stat", system_paths::PROC))
                .map(|s| !s.rsplit(") ").next().unwrap_or("").starts_with('Z'))
                .unwrap_or(false);
            if alive {
                std::thread::sleep(Duration::from_millis(10)); // clock-safe: cap on waiting for init to reap the killed group
            }
            !alive
        });
        assert!(gone, "backgrounded sleep {pid} survived the group kill");
    }

    #[test_id(t1i7rzk)]
    #[test]
    fn a_child_killed_by_a_signal_reads_128_plus_n() {
        let out = bounded("sh").args(["-c", "kill -9 $$"]).output().unwrap();
        assert_eq!(out.status.code(), Some(137));
    }

    #[test_id(t6d1tw1)]
    #[test]
    fn a_missing_program_reads_127() {
        let out = bounded("nonexistent/spira-no-such-program").output().unwrap();
        assert_eq!(out.status.code(), Some(127));
    }

    #[test_id(t7kzokr)]
    #[test]
    fn a_program_that_cannot_execute_reads_126() {
        let out = bounded(system_paths::ETC_PASSWD).output().unwrap();
        assert_eq!(out.status.code(), Some(126));
    }

    #[test_id(ta4ilqq)]
    #[test]
    fn a_blocked_stdin_write_is_released_by_the_deadline() {
        let mut child = bounded_for("sleep", "0.05").arg("5").stdin(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        assert!(stdin.write_all(&vec![0u8; 1 << 20]).is_err());
        assert_eq!(child.wait().unwrap().code(), Some(124));
    }

    #[test_id(tb85lhd)]
    #[test]
    fn the_duration_grammar_is_timeouts() {
        let secs = Duration::from_secs;
        assert_eq!(parse_deadline("5"), Ok(Some(secs(5))));
        assert_eq!(parse_deadline("0.5"), Ok(Some(Duration::from_millis(500))));
        assert_eq!(parse_deadline("2m"), Ok(Some(secs(120))));
        assert_eq!(parse_deadline("1h"), Ok(Some(secs(3600))));
        assert_eq!(parse_deadline("0"), Ok(None));
        assert_eq!(bounded_for("true", "x").status().unwrap().code(), Some(125));
        assert_eq!(bounded_for("true", "x").output().unwrap().status.code(), Some(125));
        assert_eq!(bounded_for("true", "x").spawn().err().unwrap().kind(), io::ErrorKind::InvalidInput);
    }

    fn alive(pid: &str) -> bool {
        std::fs::read_to_string(format!("{}/{pid}/stat", system_paths::PROC)).map(|s| !s.rsplit(") ").next().unwrap_or("").starts_with('Z')).unwrap_or(false)
    }

    /// Runs `script`, waits for the pid it prints once its processes are in place, then expires it.
    fn expire_when_ready(script: &str, tree: bool) -> String {
        let mut child = bounded_for("sh", "0").args(["-c", script]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        // One line only: the sleepers hold the pipe open, so reading to EOF would wait for them.
        // Each sleeps 300 s to outlast the 30 s poll caps, so only a kill can end it in time.
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let pid = line.trim().to_string();
        assert!(!pid.is_empty(), "the child exited without printing its readiness pid");
        expire(child.id() as libc::pid_t, tree);
        assert_eq!(child.wait().unwrap().code(), Some(137));
        pid
    }

    // These tests spawn `sh` because a session and group kill is only provable on real processes
    // (law-a-unit-test-calls-no-binary's written exception).
    fn setsid_grandchild(kill_tree: bool) -> (String, bool) {
        let pid = expire_when_ready("setsid sh -c 'echo $$; exec sleep 300' & sleep 300", kill_tree);
        let gone = if kill_tree {
            (0..3000).any(|_| {
                let a = alive(&pid);
                if a {
                    std::thread::sleep(Duration::from_millis(10)); // clock-safe: cap on waiting for the killed process to be reaped
                }
                !a
            })
        } else {
            std::thread::sleep(Duration::from_millis(300)); // clock-safe: lower bound on the survivor's life; a slow box only lengthens it
            !alive(&pid)
        };
        if !gone {
            unsafe { libc::kill(pid.parse().unwrap(), libc::SIGKILL) };
        }
        (pid, gone)
    }

    #[test_id(ta8fb56)]
    #[test]
    fn kill_tree_kills_a_grandchild_that_took_its_own_session() {
        let (pid, gone) = setsid_grandchild(true);
        assert!(gone, "grandchild {pid} survived kill_tree");
    }

    #[test_id(tf8e0f2)]
    #[test]
    fn without_kill_tree_a_grandchild_in_its_own_session_survives() {
        let (pid, gone) = setsid_grandchild(false);
        assert!(!gone, "grandchild {pid} died without kill_tree: the opt-in is not pinned");
    }

#[test_id(tfr35id)]
#[test]
fn started_counts_each_child_this_process_starts() {
    let before = started();
    bounded("true").status().unwrap();
    assert!(started() > before);
}

#[test_id(tpopfef)]
#[test]
fn counting_names_each_program_started_on_this_thread_and_on_carried_threads() {
    let (_, outside) = counting(|| ());
    assert!(outside.is_empty());
    bounded("true").status().unwrap();
    let (_, got) = counting(|| {
        bounded("true").status().unwrap();
        bounded("true").output().unwrap();
        let t = tally();
        std::thread::spawn(move || in_tally(t, || bounded("true").status().unwrap())).join().unwrap();
        std::thread::spawn(|| bounded("true").status().unwrap()).join().unwrap();
    });
    assert_eq!(got, BTreeMap::from([("true".to_string(), 3)]));
}
