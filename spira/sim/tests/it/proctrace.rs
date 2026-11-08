use spira_sim::proctrace::summarize;

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
