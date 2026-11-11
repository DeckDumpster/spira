//! A program whose output outgrows the pipe buffer still finishes inside its deadline.

use runtime::ports::{Proc, ProcSpec, SysProc};
use std::time::Duration;

fn spec(script: &str) -> ProcSpec {
    ProcSpec { prog: "sh".into(), args: vec!["-c".into(), script.into()], stdin: vec![], deadline: Duration::from_secs(10) }
}

#[test]
fn a_program_writing_past_the_pipe_buffer_on_stdout_is_not_reported_as_a_missed_deadline() {
    let out = SysProc.run(&spec("head -c 300000 /dev/zero")).unwrap();
    assert_eq!(out.stdout.len(), 300_000);
    assert_eq!(out.code, Some(0));
}

#[test]
fn a_program_writing_past_the_pipe_buffer_on_stderr_is_not_reported_as_a_missed_deadline() {
    let out = SysProc.run(&spec("head -c 300000 /dev/zero >&2; echo done")).unwrap();
    assert_eq!(out.stderr.len(), 300_000);
    assert_eq!(out.stdout, b"done\n");
}

#[test]
fn a_program_that_outlives_its_deadline_is_killed_and_named() {
    let mut s = spec("sleep 30");
    s.deadline = Duration::from_millis(200);
    let e = SysProc.run(&s).unwrap_err();
    assert!(e.contains("exceeded"), "{e}");
}

#[test]
fn a_grandchild_holding_the_pipe_open_does_not_outlive_the_deadline() {
    let mut s = spec("sleep 30 & echo started");
    s.deadline = Duration::from_millis(300);
    let t0 = std::time::Instant::now();
    let e = SysProc.run(&s).unwrap_err();
    assert!(e.contains("exceeded"), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(5));
}
