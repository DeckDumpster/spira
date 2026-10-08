//! Pure readings of text: the batch runner's suite lines, the gate string's `bash <path>`
//! words, the branch's tree key, and the attribution rule. Ported from gate-lib.sh
//! (`red_suites`, `ran_suites`, `timed_out_suites`, `gate_attribute`, `gate_tree_key`) and
//! gate.sh (the preflight's word scan, the harness-fault line).

/// Suites named on a line as `<x>.sh <STATUS>` (or `<x>.sh was killed`), first-seen order,
/// each once. `statuses` is the set of status words that count.
fn suites_where(out: &str, statuses: &[&str], killed: bool) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for line in out.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        for i in 0..w.len().saturating_sub(1) {
            if !w[i].ends_with(".sh") {
                continue;
            }
            let hit = statuses.contains(&w[i + 1])
                || (killed && w[i + 1] == "was" && w.get(i + 2) == Some(&"killed"));
            if hit && !seen.iter().any(|s| s == w[i]) {
                seen.push(w[i].to_string());
            }
        }
    }
    seen
}

/// `red_suites`: RED, TIMEOUT, FAILED, or killed.
pub fn red_suites(out: &str) -> Vec<String> {
    suites_where(out, &["RED", "TIMEOUT", "FAILED"], true)
}

/// `failed_tests`: the `<name>` of each cargo `test <name> ... FAILED` line, in order, unique.
pub fn failed_tests(out: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for l in out.lines() {
        let Some(rest) = l.trim().strip_prefix("test ") else { continue };
        let Some(name) = rest.strip_suffix(" ... FAILED") else { continue };
        if !name.is_empty() && !name.contains(char::is_whitespace) && !seen.iter().any(|x| x == name) {
            seen.push(name.to_string());
        }
    }
    seen
}

/// The fence named by the first `<fence>: REFUSED` line, e.g. `lifecycle-guard`.
pub fn fence_refusal(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let name = l.trim().split_once(": REFUSED")?.0;
        let ok = !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        ok.then(|| name.to_string())
    })
}

/// `ran_suites`: every suite the runner reported any status for.
pub fn ran_suites(out: &str) -> Vec<String> {
    suites_where(
        out,
        &[
            "ok",
            "SKIPPED",
            "SKIP-REQ",
            "TIMEOUT",
            "RED",
            "QUARANTINED-RED",
            "DISABLED",
            "UNREACHED",
            "FAILED",
            "FAULT",
        ],
        true,
    )
}

/// The `ran=<n>` of the runner's last `VERDICT` line, if it printed one.
pub fn verdict_ran(out: &str) -> Option<usize> {
    out.lines()
        .rev()
        .filter(|l| l.starts_with("VERDICT "))
        .find_map(|l| {
            l.split_whitespace()
                .find_map(|w| w.strip_prefix("ran="))
                .and_then(|n| n.parse().ok())
        })
}

/// `timed_out_suites`: only what the watchdog killed — never a genuine FAIL.
pub fn timed_out_suites(out: &str) -> Vec<String> {
    suites_where(out, &["TIMEOUT"], true)
}

/// `gate_tree_key`: `/` → `-`, anything outside `[A-Za-z0-9.-]` → `-`.
pub fn tree_key(branch: &str) -> String {
    branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The preflight's scan: every `bash <word>` in the gate string whose word is a relative path
/// (contains `/`, does not start with one). `\bbash [^[:space:];|&(){}$"]+`.
pub fn bash_paths(cmd: &str) -> Vec<String> {
    let b = cmd.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = cmd[i..].find("bash ") {
        let at = i + off;
        let boundary = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
        let start = at + 5;
        let end = cmd[start..]
            .find(|c: char| c.is_whitespace() || ";|&(){}$\"".contains(c))
            .map(|e| start + e)
            .unwrap_or(cmd.len());
        if boundary && end > start {
            let w = &cmd[start..end];
            if w.contains('/') && !w.starts_with('/') {
                out.push(w.to_string());
            }
        }
        i = start;
    }
    out
}

/// The last `batch: harness fault — container died mid-batch (<detail>)` detail, if any.
pub fn harness_fault_detail(out: &str) -> Option<String> {
    const MARK: &str = "batch: harness fault — container died mid-batch (";
    let mut last = None;
    for line in out.lines() {
        let Some(p) = line.find(MARK) else { continue };
        let rest = line[p + MARK.len()..].trim_end();
        if let Some(d) = rest.strip_suffix(')') {
            last = Some(d.to_string());
        }
    }
    last
}

/// The test runner's `VERDICT FAULT … reason=<word>` (its last line), when the suites step
/// ended in a harness fault. testenv prints exactly one VERDICT line per run.
pub fn testenv_fault_reason(out: &str) -> Option<String> {
    out.lines()
        .rev()
        .find(|l| l.starts_with("VERDICT "))
        .filter(|l| l.starts_with("VERDICT FAULT "))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("reason=")))
        .map(str::to_string)
}

/// Total seconds the trial's runner reports waiting for a slot: every `queue-wait=<n>s` word,
/// whichever pool said it.
pub fn queue_secs(out: &str) -> u64 {
    out.split_whitespace()
        .filter_map(|w| w.strip_prefix("queue-wait=")?.strip_suffix('s')?.parse::<u64>().ok())
        .sum()
}

/// Whether the suites step started at all: testenv always ends with a `VERDICT` line (its
/// DESIGN.md §2.3). A trial red before it (a fence, the selector) never reached the suites.
/// A per-suite line (a red, a ran suite) counts too: a runner that died before its VERDICT
/// line still ran suites, and their base must be run.
pub fn suites_step_ran(out: &str) -> bool {
    out.lines().any(|l| {
        l.starts_with("VERDICT GREEN")
            || l.starts_with("VERDICT RED")
            || l.starts_with("VERDICT FAULT")
    }) || !red_suites(out).is_empty()
        || !ran_suites(out).is_empty()
}

/// The `gate-step <ok|RED> rc=<n> :: <unit>` lines [`compose::run_all`](crate::compose::run_all)
/// prints: each step's unit and whether it was red, in order.
pub fn step_units(out: &str) -> Vec<(String, bool)> {
    out.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("gate-step ")?;
            let (head, unit) = rest.split_once(" :: ")?;
            let unit = unit.trim();
            (!unit.is_empty()).then(|| (unit.to_string(), head.starts_with("RED")))
        })
        .collect()
}

/// Every independent unit that is red in `out`: red suites, red fence steps (the suites step
/// only when it named no suite), and unit phases that failed.
pub fn red_units(out: &str) -> Vec<String> {
    let mut units = red_suites(out);
    let named = !units.is_empty();
    for (u, red) in step_units(out) {
        if red && !(named && u == "suites") && !units.contains(&u) {
            units.push(u);
        }
    }
    for l in out.lines() {
        let Some(name) = l.trim().strip_prefix("gate: phase '").and_then(|r| r.split('\'').next()) else {
            continue;
        };
        let u = format!("phase:{name}");
        if !units.contains(&u) {
            units.push(u);
        }
    }
    units
}

/// Whose fault a failed branch trial is (`gate_attribute`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribution {
    BranchRed(String),
    BaseRed(String),
    BaseTimeout(String),
    BaseUntestable,
}

/// `base_rc` is only read when `base_ran`. `absent_on_base`: branch reds whose suite file
/// the base does not have — the branch's own by construction.
///
/// EACH RED IS JUDGED AGAINST THE BASE ON ITS OWN UNIT (sp-hh5h0; every fence and suite is a
/// unit). A branch red is the branch's when the base ran that unit and it was not red there,
/// or the base lacks it. A branch red the base never ran is not evidence either way:
/// base-untestable, never branch-red on a base that merely did not look. Only when every
/// branch red is red on the base too does the base's own rule apply (all timeouts →
/// base-timeout, else base-red: the caller subtracts those units rather than holding the branch).
pub fn attribute(
    branch_out: &str,
    base_ran: bool,
    base_rc: i32,
    base_out: &str,
    absent_on_base: &[String],
) -> Attribution {
    if !base_ran {
        return Attribution::BaseUntestable;
    }
    let branch_reds = red_units(branch_out);
    let base_reds = red_units(base_out);
    if branch_reds.is_empty() {
        // A red that names no unit (a legacy string, an unnamed phase): judged on the command whole.
        if base_rc == 0 {
            return Attribution::BranchRed("-".into());
        }
    } else {
        let base_ran_set = ran_suites(base_out);
        let base_steps = step_units(base_out);
        let own = |u: &String| {
            if absent_on_base.contains(u) {
                return Some(true);
            }
            if base_reds.contains(u) {
                return Some(false);
            }
            if u.ends_with(".sh") {
                return base_ran_set.contains(u).then_some(true);
            }
            if u.starts_with("phase:") {
                return (base_rc == 0).then_some(true);
            }
            if base_steps.is_empty() {
                return (base_rc == 0).then_some(true);
            }
            Some(true)
        };
        if let Some(s) = branch_reds.iter().find(|u| own(u) == Some(true)) {
            return Attribution::BranchRed(s.clone());
        }
        if branch_reds.iter().any(|u| own(u).is_none()) {
            return Attribution::BaseUntestable;
        }
    }
    let timeouts = timed_out_suites(base_out);
    let genuine = base_reds.iter().filter(|s| !timeouts.contains(s)).count();
    if !timeouts.is_empty() && genuine == 0 {
        return Attribution::BaseTimeout(timeouts[0].clone());
    }
    Attribution::BaseRed(base_reds.into_iter().next().unwrap_or_else(|| "-".into()))
}

/// `$(printf '%s\n' "$s" | tail -c N)`: the last N bytes of s + "\n", trailing newlines
/// stripped, never splitting a UTF-8 character (the bash could; a reader cannot use half one).
pub fn tail_bytes(s: &str, n: usize) -> String {
    let full = format!("{s}\n");
    let mut start = full.len().saturating_sub(n);
    while !full.is_char_boundary(start) {
        start += 1;
    }
    full[start..].trim_end_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    const NONE: &[String] = &[];
    const STEPS_BASE: &str = "gate-step RED rc=1 :: spira-lint\ngate-step ok rc=0 :: build-fence\n  test-s1.sh RED rc=1";

    #[test]
    fn a_unit_red_on_both_is_inherited_and_the_branchs_own_is_named() {
        let branch = "gate-step RED rc=1 :: spira-lint\ngate-step RED rc=1 :: lifecycle-guard\n  test-s1.sh RED rc=1\n  test-s2.sh RED rc=1";
        assert_eq!(attribute(branch, true, 1, STEPS_BASE, NONE), Attribution::BranchRed("lifecycle-guard".into()));
        assert_eq!(red_units(branch), vec!["test-s1.sh", "test-s2.sh", "spira-lint", "lifecycle-guard"]);
        let inherited = "gate-step RED rc=1 :: spira-lint\n  test-s1.sh RED rc=1";
        assert_eq!(attribute(inherited, true, 1, STEPS_BASE, NONE), Attribution::BaseRed("test-s1.sh".into()));
    }

    #[test]
    fn a_fence_red_does_not_hide_the_suites_that_ran() {
        let out = "gate-step RED rc=1 :: lifecycle-guard\n  test-s2.sh RED rc=1\ngate-step RED rc=1 :: suites";
        assert_eq!(red_units(out), vec!["test-s2.sh", "lifecycle-guard"]);
    }

    use super::*;

    #[test]
    fn failed_tests_reads_cargo_failure_lines_only() {
        let out = "test a::ok ... ok\ntest a::bad ... FAILED\ntest a::bad ... FAILED\ntest result: FAILED. 1 passed\n";
        assert_eq!(failed_tests(out), vec!["a::bad".to_string()]);
        assert!(failed_tests("test result: FAILED\n").is_empty());
    }

    #[test]
    fn fence_refusal_names_the_fence() {
        let out = "x\nlifecycle-guard: REFUSED ... bead/src/main.rs:717\n";
        assert_eq!(fence_refusal(out), Some("lifecycle-guard".to_string()));
        assert_eq!(fence_refusal("some text: REFUSED\n"), None);
        assert_eq!(fence_refusal("all green"), None);
    }

    #[test]
    fn red_suites_reads_every_red_form_once_in_order() {
        let out = "test-a.sh ok\ntest-b.sh RED\n  test-c.sh TIMEOUT\ntest-d.sh was killed\ntest-b.sh RED\nx test-e.sh FAILED";
        assert_eq!(
            red_suites(out),
            ["test-b.sh", "test-c.sh", "test-d.sh", "test-e.sh"]
        );
    }

    #[test]
    fn ran_suites_includes_passes_and_skips() {
        let out = "test-a.sh ok\ntest-b.sh SKIPPED\ntest-c.sh QUARANTINED-RED\ntest-d.sh RED\nnoise.sh words";
        assert_eq!(
            ran_suites(out),
            ["test-a.sh", "test-b.sh", "test-c.sh", "test-d.sh"]
        );
    }

    #[test]
    fn verdict_ran_reads_the_runners_count() {
        assert_eq!(verdict_ran("  a.sh ok\nVERDICT GREEN ran=66 skipped=2"), Some(66));
        assert_eq!(verdict_ran("VERDICT RED ran=3 red=1"), Some(3));
        assert_eq!(verdict_ran("a.sh ok\nran=5 but no verdict"), None);
    }

    #[test]
    fn a_fault_line_counts_as_ran() {
        assert_eq!(ran_suites("  test-f.sh   FAULT   podman lost the exit status"), ["test-f.sh"]);
    }

    #[test]
    fn timed_out_is_only_the_watchdogs() {
        let out = "test-a.sh TIMEOUT\ntest-b.sh RED\ntest-c.sh was killed";
        assert_eq!(timed_out_suites(out), ["test-a.sh", "test-c.sh"]);
    }

    /// sp-gjx1b (testenv DESIGN.md §3.7): an undeclared SKIP/SKIP-REQ is reclassified to a
    /// plain `RED` — the word every consumer here already keys on — with the reason appended
    /// after it (`RED     undeclared skip — <requirement>`), never a new status word. This
    /// proves the gate's own suite-line reader (unchanged) still catches it as red and as ran,
    /// exactly like any other red, so the new contract propagates without a gate code change.
    #[test]
    fn an_undeclared_skip_reclassified_by_testenv_still_reads_as_a_red_suite() {
        let out = "  test-f.sh                        RED     undeclared skip — skip:server testdb not available\nVERDICT RED ran=1 red=1";
        assert_eq!(red_suites(out), ["test-f.sh"]);
        assert_eq!(ran_suites(out), ["test-f.sh"]);
        let out_req = "  test-e.sh                        RED     undeclared skip — requires:claude\nVERDICT RED ran=0 red=1";
        assert_eq!(red_suites(out_req), ["test-e.sh"]);
    }

    #[test]
    fn a_word_not_ending_in_sh_is_not_a_suite() {
        assert!(red_suites("cargo RED\nfoo.shx RED").is_empty());
    }

    #[test]
    fn tree_key_is_filesystem_safe() {
        assert_eq!(tree_key("spira/sp-a1"), "spira-sp-a1");
        assert_eq!(tree_key("concierge/x_y z"), "concierge-x-y-z");
        assert_eq!(tree_key("a.b-c"), "a.b-c");
    }

    #[test]
    fn bash_paths_takes_only_relative_paths_after_a_word_boundary() {
        let cmd = r#"bash spira/a.sh && bash /abs/b.sh && bash -c 'x' && mybash spira/c.sh && { bash spira/d.sh; } && "$X" --only y && bash "$Q" && bash spira/e.sh|cat"#;
        assert_eq!(bash_paths(cmd), ["spira/a.sh", "spira/d.sh", "spira/e.sh"]);
        assert!(bash_paths("").is_empty());
        assert!(bash_paths("cargo test").is_empty());
    }

    #[test]
    fn harness_fault_detail_takes_the_last() {
        let out = "x\nbatch: harness fault — container died mid-batch (one)\nbatch: harness fault — container died mid-batch (two)  \n";
        assert_eq!(harness_fault_detail(out).as_deref(), Some("two"));
        assert_eq!(harness_fault_detail("nothing"), None);
    }



    #[test]
    fn attribution_base_untestable_when_the_base_did_not_run() {
        assert_eq!(
            attribute("test-a.sh RED", false, 1, "", NONE),
            Attribution::BaseUntestable
        );
    }

    #[test]
    fn attribution_branch_red_when_the_base_ran_the_suite_green() {
        assert_eq!(
            attribute("test-a.sh RED", true, 0, "test-a.sh ok", NONE),
            Attribution::BranchRed("test-a.sh".into())
        );
        assert_eq!(
            attribute("silent", true, 0, "", NONE),
            Attribution::BranchRed("-".into())
        );
    }

    #[test]
    fn attribution_branch_red_on_a_suite_red_only_on_the_branch() {
        let a = attribute(
            "test-a.sh RED\ntest-b.sh RED",
            true,
            1,
            "test-a.sh RED\ntest-b.sh ok",
            NONE,
        );
        assert_eq!(a, Attribution::BranchRed("test-b.sh".into()));
    }

    #[test]
    fn attribution_base_red_when_the_base_has_the_same_reds() {
        let a = attribute(
            "test-a.sh RED",
            true,
            1,
            "test-a.sh RED\ntest-z.sh RED",
            NONE,
        );
        assert_eq!(a, Attribution::BaseRed("test-a.sh".into()));
        assert_eq!(
            attribute("x", true, 1, "no suite named", NONE),
            Attribution::BaseRed("-".into())
        );
    }

    /// sp-hh5h0: the branch and the base are both red on the same suite. Base-red, whatever
    /// else the base did — never branch-red.
    #[test]
    fn attribution_the_same_suite_red_on_both_is_the_bases() {
        let a = attribute(
            "  test-gate-verdict.sh           RED     rc=1 after 31s",
            true,
            1,
            "  test-other.sh ok\n  test-gate-verdict.sh           RED     rc=1 after 30s",
            NONE,
        );
        assert_eq!(a, Attribution::BaseRed("test-gate-verdict.sh".into()));
    }

    /// sp-hh5h0: a base that exited 0 without running the branch's red suite (a different
    /// selection, or --deadline deferred it) proves nothing about that suite.
    #[test]
    fn attribution_a_red_the_base_never_ran_is_untestable_not_branch_red() {
        for base in [
            "test-other.sh ok",
            "  test-a.sh                        DEFERRED deadline",
            "",
        ] {
            assert_eq!(
                attribute("test-a.sh RED", true, 0, base, NONE),
                Attribution::BaseUntestable,
                "{base:?}"
            );
        }
        // One red judged green on the base is enough to name the branch.
        assert_eq!(
            attribute(
                "test-a.sh RED\ntest-b.sh RED",
                true,
                0,
                "test-b.sh ok",
                NONE
            ),
            Attribution::BranchRed("test-b.sh".into())
        );
    }

    #[test]
    fn attribution_a_red_suite_the_base_lacks_is_the_branchs() {
        assert_eq!(
            attribute("test-new.sh RED", true, 0, "", &["test-new.sh".to_string()]),
            Attribution::BranchRed("test-new.sh".into())
        );
    }

    #[test]
    fn attribution_base_timeout_when_every_base_red_was_killed() {
        let a = attribute(
            "test-a.sh TIMEOUT",
            true,
            1,
            "test-a.sh TIMEOUT\ntest-b.sh was killed",
            NONE,
        );
        assert_eq!(a, Attribution::BaseTimeout("test-a.sh".into()));
        let mixed = attribute(
            "test-a.sh TIMEOUT",
            true,
            1,
            "test-a.sh TIMEOUT\ntest-b.sh RED",
            NONE,
        );
        assert_eq!(mixed, Attribution::BaseRed("test-a.sh".into()));
    }

    #[test]
    fn tail_bytes_keeps_the_end_and_whole_characters() {
        assert_eq!(tail_bytes("abcdef", 4), "def");
        assert_eq!(tail_bytes("ab", 100), "ab");
        assert_eq!(tail_bytes("a—b", 3), "b");
    }
}
