//! The restricted personas' tool allowlists, read from the shipped runes.

use spira_config::convert::persona_section;
use spira_config::convert::ConvertWarnings;
use spira_config::SpiraSection;
use std::fs;

fn tools_of(persona: &str) -> Vec<String> {
    let path = spira_config::resolve::locate_home_for_process().unwrap().join("runes").display().to_string() + "/";
    let text = fs::read_to_string(format!("{path}{persona}.rune"))
        .unwrap_or_else(|e| panic!("{persona}.rune: {e}"));
    let (_, section) = persona_section(
        &text,
        &SpiraSection::default(),
        &mut ConvertWarnings::default(),
    );
    section.tools
}

fn glob(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((head, rest)) => s.strip_prefix(head).is_some_and(|tail| {
            (0..=tail.len()).any(|i| tail.is_char_boundary(i) && glob(rest, &tail[i..]))
        }),
    }
}

fn allows(tools: &[String], cmd: &str) -> bool {
    tools
        .iter()
        .filter_map(|t| t.strip_prefix("Bash(")?.strip_suffix(')'))
        .any(|p| glob(p, cmd))
}

fn check(persona: &str, allowed: &[&str], refused: &[&str]) {
    let tools = tools_of(persona);
    assert!(
        !tools.iter().any(|t| t == "Bash"),
        "{persona}: bare Bash in {tools:?}"
    );
    assert!(
        tools.iter().any(|t| t.starts_with("Bash(")),
        "{persona}: no Bash(pattern) entry"
    );
    for c in allowed {
        assert!(allows(&tools, c), "{persona}: diagnostic refused: {c}");
    }
    for c in refused {
        assert!(!allows(&tools, c), "{persona}: mutation allowed: {c}");
    }
}

const MUTATIONS: &[&str] = &[
    "bd -C /srv/db show sp-xyz",
    "bd -C /srv/db note sp-xyz 'diagnosis'",
    "rm -rf /srv/run/worktree",
    "systemctl restart spira-sentinel.service",
    "systemctl start spira-sentinel.service",
    "systemctl stop spira-sentinel.service",
    "git branch -D spira/sp-xyz",
];

// covers: UC-safety-fences-30 UC-dispatch-24
#[test_id(tu1rplq)]
#[test]
fn ops_runs_diagnostics_and_nothing_that_mutates_and_has_no_bare_bash() {
    check(
        "ops",
        &[
            "systemctl status x.service",
            "systemctl show x.service",
            "cat /var/log/ops.log",
            "grep -r 'error' /var/log/",
            "ls -la /srv/run",
            "git log --oneline -10",
            "git -C /srv/h log --oneline -5",
            "git status",
        ],
        MUTATIONS,
    );
}

// covers: UC-safety-fences-30 UC-dispatch-24
#[test_id(txbdix1)]
#[test]
fn czar_runs_its_fence_and_diagnostics_and_nothing_that_mutates_and_has_no_bare_bash() {
    check(
        "czar",
        &[
            "bash spira/czar-fence.sh deadlock",
            "czar-pass --lint-default",
            "srv/bin/queue eject sp-xyz",
            "srv/bin/landing-pass halt --reason-file -",
            "git log --oneline -10",
            "cat /var/log/czar.log",
            "ls -la /srv/run",
        ],
        MUTATIONS,
    );
}

#[test_id(tz18vju)]
#[test]
fn the_matcher_refuses_what_no_pattern_names_so_a_pass_means_something() {
    let t = vec!["Bash(git log*)".to_string(), "Read".to_string()];
    assert!(allows(&t, "git log -5") && !allows(&t, "git commit -a") && !allows(&t, "Read"));
}
