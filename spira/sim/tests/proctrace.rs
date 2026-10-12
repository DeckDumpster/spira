use spira_sim::proctrace::{render, start, summarize, TRACE_FILE};
use spira_sim::world::run_capture_traced;
use std::process::Command;
use std::time::Duration;

#[test]
fn the_trace_holds_the_command_and_the_processes_it_started() {
    let t = testkit::TempDir::new("simtrace");
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg("sleep 0.3; sleep 0.3");
    let (status, _, _) = run_capture_traced(&mut cmd, Duration::from_secs(30), Some((t.path(), "two sleeps"))).unwrap();
    assert!(status.success());
    let text = std::fs::read_to_string(t.join(TRACE_FILE)).unwrap();
    assert!(text.starts_with("=== two sleeps\n"), "{text}");
    let argvs: Vec<&str> = text.lines().skip(1).filter_map(|l| l.splitn(7, '\t').nth(6)).collect();
    assert!(argvs.iter().any(|a| a.starts_with("sh -c")), "{argvs:?}");
    assert!(argvs.iter().filter(|a| a.starts_with("sleep 0.3")).count() >= 1, "{argvs:?}");
}

#[test]
fn a_run_without_a_trace_target_writes_nothing() {
    let t = testkit::TempDir::new("simtrace");
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg("true");
    run_capture_traced(&mut cmd, Duration::from_secs(30), None).unwrap();
    assert!(!t.join(TRACE_FILE).exists());
}

#[test]
fn rows_carry_start_life_and_cpu_before_the_argv() {
    let child = Command::new("sh").arg("-c").arg("sleep 0.2").spawn().unwrap();
    let tracer = start(child.id());
    let mut child = child;
    child.wait().unwrap();
    let seen = tracer.finish();
    let text = render("x", &seen);
    let row = text.lines().nth(1).expect("the root process is seen");
    let cols: Vec<&str> = row.splitn(7, '\t').collect();
    assert_eq!(cols.len(), 7, "{row}");
    assert!(cols[..6].iter().all(|c| c.parse::<u64>().is_ok()), "{row}");
}

#[test]
fn the_summary_ranks_programs_by_cpu_and_charges_unseen_children_to_their_parent() {
    let trace = "=== queue step\n\
        0\t900\t100\t500\t10\t1\tqueue step sim\n\
        5\t300\t100\t0\t11\t10\t/usr/bin/git -C /w log\n\
        6\t200\t30\t0\t12\t10\tsh -c /x/commit-cite.sh land\n\
        7\t100\t0\t0\t13\t10\t/usr/bin/timeout 5 spira-lc list\n\
        === next\n\
        0\t50\t5\t0\t20\t1\tgit status\n";
    let out = summarize(trace);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].ends_with(": queue"), "the parent whose children were too short to see leads: {out}");
    assert!(lines[0].contains(" 100 ms cpu") && lines[0].contains("  370 ms cpu in children"), "{}", lines[0]);
    let git = lines.iter().find(|l| l.ends_with(": git")).unwrap();
    assert!(git.contains("    2 procs") && git.contains("105 ms cpu"), "programs of the same name add up: {git}");
    assert!(lines.iter().any(|l| l.ends_with(": sh commit-cite.sh")), "a shell is named for the script it runs: {out}");
    assert!(lines.iter().any(|l| l.ends_with(": timeout spira-lc")), "a wrapper is named for what it wraps: {out}");
}
