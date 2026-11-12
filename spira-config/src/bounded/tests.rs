    use super::*;
    use std::io::Write;

    #[test]
    fn the_program_is_the_callers_direct_child() {
        let out = bounded("sh").args(["-c", "echo $PPID"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), std::process::id().to_string());
    }

    #[test]
    fn get_program_names_the_real_program() {
        assert_eq!(bounded("git").get_program(), "git");
    }

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

    #[test]
    fn expiry_kills_the_process_group() {
        let out = bounded_for("sh", "0.1").args(["-c", "sleep 30 & echo $!; wait"]).output().unwrap();
        assert_eq!(out.status.code(), Some(124));
        let pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let gone = (0..200).any(|_| {
            let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|s| !s.rsplit(") ").next().unwrap_or("").starts_with('Z'))
                .unwrap_or(false);
            if alive {
                std::thread::sleep(Duration::from_millis(10)); // clock-safe: cap on waiting for init to reap the killed group
            }
            !alive
        });
        assert!(gone, "backgrounded sleep {pid} survived the group kill");
    }

    #[test]
    fn a_child_killed_by_a_signal_reads_128_plus_n() {
        let out = bounded("sh").args(["-c", "kill -9 $$"]).output().unwrap();
        assert_eq!(out.status.code(), Some(137));
    }

    #[test]
    fn a_missing_program_reads_127() {
        let out = bounded("/nonexistent/spira-no-such-program").output().unwrap();
        assert_eq!(out.status.code(), Some(127));
    }

    #[test]
    fn a_program_that_cannot_execute_reads_126() {
        let out = bounded("/etc/hostname").output().unwrap();
        assert_eq!(out.status.code(), Some(126));
    }

    #[test]
    fn a_blocked_stdin_write_is_released_by_the_deadline() {
        let mut child = bounded_for("sleep", "0.05").arg("5").stdin(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        assert!(stdin.write_all(&vec![0u8; 1 << 20]).is_err());
        assert_eq!(child.wait().unwrap().code(), Some(124));
    }

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
