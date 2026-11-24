//! Pure readings of text: the batch runner's suite lines, the gate string's `bash <path>`
//! words, the branch's tree key, and the attribution rule. Ported from gate-lib.sh
//! (`red_suites`, `ran_suites`, `timed_out_suites`, `gate_attribute`, `gate_tree_key`) and
//! gate.sh (the preflight's word scan, the harness-fault line).

use crate::tier::{Status, Tier};

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

fn with_tiers(mut seen: Vec<String>, tiers: &[Tier], counts: impl Fn(Status) -> bool) -> Vec<String> {
    for t in tiers.iter().filter(|t| counts(t.status)) {
        if !seen.contains(&t.name) {
            seen.push(t.name.clone());
        }
    }
    seen
}

/// `red_suites`: RED, TIMEOUT, FAILED, or killed; and each tier recorded red.
pub fn red_suites(out: &str, tiers: &[Tier]) -> Vec<String> {
    with_tiers(suites_where(out, &["RED", "TIMEOUT", "FAILED"], true), tiers, |s| s == Status::Red)
}

/// The backticked argument of each `error: test failed, to rerun pass `…`` line, once each.
pub fn cargo_reruns(out: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for l in out.lines() {
        let Some(rest) = l.trim().strip_prefix("error: test failed, to rerun pass `") else { continue };
        let Some(arg) = rest.strip_suffix('`') else { continue };
        if !v.iter().any(|x| x == arg) {
            v.push(arg.to_string());
        }
    }
    v
}

/// `failed_tests`: the `<name>` of each failing test, in order, unique: a cargo `test <name> ...
/// FAILED` line, or a nextest `FAIL|TIMEOUT|SIG*|LEAK-FAIL|EXEC-FAIL [ <secs>] (<k>/<n>) <binary>
/// <name>` line.
pub fn failed_tests(out: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for l in out.lines() {
        let l = l.trim();
        let name = match l.strip_prefix("test ") {
            Some(rest) => rest.strip_suffix(" ... FAILED"),
            None => spira_config::unit_tier::nextest_red(l),
        };
        let Some(name) = name else { continue };
        if !name.is_empty() && !name.contains(char::is_whitespace) && !seen.iter().any(|x| x == name) {
            seen.push(name.to_string());
        }
    }
    seen
}

/// The fence named by the first `<fence>: REFUSED` line, e.g. `lifecycle-guard`; failing that,
/// the first line opening with a hyphenated lowercase rule token and `: `, e.g. a lint's
/// `process-exit-in-library: ...`.
pub fn fence_refusal(out: &str) -> Option<String> {
    let ident = |n: &str| {
        !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    out.lines()
        .find_map(|l| {
            let name = l.trim().split_once(": REFUSED")?.0;
            ident(name).then(|| name.to_string())
        })
        .or_else(|| {
            out.lines().find_map(|l| {
                let name = l.split_once(": ")?.0;
                let rule = name.contains('-')
                    && !name.starts_with('-')
                    && !name.ends_with('-')
                    && name.chars().all(|c| c.is_ascii_lowercase() || c == '-');
                rule.then(|| name.to_string())
            })
        })
}

/// `ran_suites`: every suite the runner reported any status for, and every recorded tier.
pub fn ran_suites(out: &str, tiers: &[Tier]) -> Vec<String> {
    with_tiers(suites_where(
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
    ), tiers, |_| true)
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

/// What a harness fault's output says about the cause, on one line for a yield record: the
/// runner's own words (what it refused and why), without the passing steps, the fences' summary
/// lines or the gate's restatement of the command.
pub fn fault_detail(msg: &str) -> String {
    const NOISE: &[&str] = &["gate: command:", "gate: phase ", "gate: VERDICT", "gate-step ok", "fence: "];
    let kept: Vec<&str> = msg
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !NOISE.iter().any(|n| l.starts_with(n)))
        .collect();
    tail_bytes(&kept.join(" | "), 600)
}

/// The seconds testenv's `phases:` line reports for its suites phase.
pub fn testenv_suites_secs(out: &str) -> Option<u64> {
    let line = out.lines().rev().find(|l| l.trim_start().starts_with("batch: phases: ") || l.trim_start().starts_with("phases: "))?;
    let list = line.split("phases: ").nth(1)?.split_whitespace().next()?;
    list.split(',').find_map(|p| p.strip_prefix("suites:")?.parse().ok())
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
pub fn suites_step_ran(out: &str, tiers: &[Tier]) -> bool {
    out.lines().any(|l| {
        l.starts_with("VERDICT GREEN")
            || l.starts_with("VERDICT RED")
            || l.starts_with("VERDICT FAULT")
            || l.starts_with("VERDICT EMPTY")
    }) || !red_suites(out, tiers).is_empty()
        || !ran_suites(out, tiers).is_empty()
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
pub fn red_units(out: &str, tiers: &[Tier]) -> Vec<String> {
    let mut units = red_suites(out, tiers);
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

/// The compiler-cache server could not serve the compile (lost connection, or could not
/// create its temporary folder): the cache's fault, never a red on either side of the base
/// comparison.
pub fn cache_transport_failure(out: &str) -> bool {
    let lower = out.to_lowercase();
    lower.contains("sccache: error")
        && ["connection reset", "connection refused", "failed to execute compile", "failed to create temp dir"]
            .iter()
            .any(|m| lower.contains(m))
}

/// The server's own words for a cache fault: the first `caused by:` line after the first
/// `sccache: error` line that names a path or an os error, else the error line itself.
pub fn cache_failure_line(out: &str) -> Option<&str> {
    let mut lines = out.lines().map(str::trim).skip_while(|l| !l.to_lowercase().starts_with("sccache: error"));
    let head = lines.next()?;
    let cause = lines.find(|l| {
        let l = l.to_lowercase();
        l.contains("caused by:") && (l.contains("os error") || l.contains("path "))
    });
    Some(cause.unwrap_or(head))
}

/// A test that could not find a binary cargo built for it: the output names a
/// `CARGO_BIN_EXE_*` path that is missing. Never the branch's red.
pub fn missing_built_binary(out: &str) -> bool {
    out.lines().any(|l| l.contains("CARGO_BIN_EXE_") && (l.contains("is_file()") || l.contains("NotFound") || l.contains("No such file")))
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
    (branch_out, branch_tiers): (&str, &[Tier]),
    base_ran: bool,
    base_rc: i32,
    (base_out, base_tiers): (&str, &[Tier]),
    absent_on_base: &[String],
) -> Attribution {
    if !base_ran {
        return Attribution::BaseUntestable;
    }
    let branch_reds = red_units(branch_out, branch_tiers);
    let base_reds = red_units(base_out, base_tiers);
    if branch_reds.is_empty() {
        // A red that names no unit (a legacy string, an unnamed phase): judged on the command whole.
        if base_rc == 0 {
            return Attribution::BranchRed("-".into());
        }
    } else {
        let base_ran_set = ran_suites(base_out, base_tiers);
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
    fn attribute(branch: &str, base_ran: bool, base_rc: i32, base: &str, absent: &[String]) -> Attribution {
        super::attribute((branch, &[]), base_ran, base_rc, (base, &[]), absent)
    }

    fn tier(name: &str, status: crate::tier::Status) -> crate::tier::Tier {
        crate::tier::Tier { name: name.into(), status }
    }

    #[test_id(t9daavf)]
    #[test]
    fn a_recorded_tier_is_a_ran_suite_and_a_red_one_when_red() {
        use crate::tier::Status;
        let red = [tier("gate", Status::Red)];
        assert_eq!(super::ran_suites("", &red), ["gate"]);
        assert_eq!(super::red_suites("", &red), ["gate"]);
        let ok = [tier("gate", Status::Ok), tier("build", Status::Fault)];
        assert_eq!(super::ran_suites("", &ok), ["gate", "build"]);
        assert!(super::red_suites("", &ok).is_empty());
    }

    #[test_id(t39c9o9)]
    #[test]
    fn a_printed_tier_row_names_nothing_without_the_record() {
        for l in ["  gate                             RED     after 12s", "  gate ok 3s", "  build FAULT x after 3s"] {
            assert!(super::ran_suites(l, &[]).is_empty(), "{l}");
            assert!(super::red_suites(l, &[]).is_empty(), "{l}");
        }
    }

    #[test_id(t5ljzu4)]
    #[test]
    fn cargo_reruns_returns_each_distinct_argument_once() {
        let o = "error: test failed, to rerun pass `-p a --lib`\nnoise\nerror: test failed, to rerun pass `-p b --test t`\nerror: test failed, to rerun pass `-p a --lib`\n";
        assert_eq!(super::cargo_reruns(o), ["-p a --lib", "-p b --test t"]);
    }

    const NONE: &[String] = &[];
    const STEPS_BASE: &str = "gate-step RED rc=1 :: spira-lint\ngate-step ok rc=0 :: build-fence\n  test-s1.sh RED rc=1";

    #[test_id(tsb06ud)]
    #[test]
    fn a_unit_red_on_both_is_inherited_and_the_branchs_own_is_named() {
        let branch = "gate-step RED rc=1 :: spira-lint\ngate-step RED rc=1 :: lifecycle-guard\n  test-s1.sh RED rc=1\n  test-s2.sh RED rc=1";
        assert_eq!(attribute(branch, true, 1, STEPS_BASE, NONE), Attribution::BranchRed("lifecycle-guard".into()));
        assert_eq!(red_units(branch, &[]), vec!["test-s1.sh", "test-s2.sh", "spira-lint", "lifecycle-guard"]);
        let inherited = "gate-step RED rc=1 :: spira-lint\n  test-s1.sh RED rc=1";
        assert_eq!(attribute(inherited, true, 1, STEPS_BASE, NONE), Attribution::BaseRed("test-s1.sh".into()));
    }

    #[test_id(tc8uddx)]
    #[test]
    fn a_fence_red_does_not_hide_the_suites_that_ran() {
        let out = "gate-step RED rc=1 :: lifecycle-guard\n  test-s2.sh RED rc=1\ngate-step RED rc=1 :: suites";
        assert_eq!(red_units(out, &[]), vec!["test-s2.sh", "lifecycle-guard"]);
    }

    use super::*;

    #[test_id(ttv919p)]
    #[test]
    fn a_cache_transport_error_is_found_and_a_compile_error_is_not() {
        let wedged = "error: could not compile `x`\nsccache: error: failed to execute compile\ncaused by: Connection reset by peer (os error 104)\n";
        assert!(cache_transport_failure(wedged));
        assert!(!cache_transport_failure("error[E0308]: mismatched types\n --> src/lib.rs:1:1\n"));
        assert!(!cache_transport_failure("test result: FAILED. Connection reset by peer in a test\n"));
        let tmp = "sccache: error: Failed to create temp dir\nsccache: caused by: Failed to create temp dir\nsccache: caused by: No such file or directory (os error 2) at path \"/w/.runtime/session-tmp-1/sccache29LXwu\"\n";
        assert!(cache_transport_failure(tmp));
        assert!(!cache_transport_failure("test x ... FAILED: Failed to create temp dir\n"));
        assert!(cache_failure_line(tmp).unwrap().ends_with("os error 2) at path \"/w/.runtime/session-tmp-1/sccache29LXwu\""));
        assert_eq!(cache_failure_line("sccache: error: failed to execute compile\n"), Some("sccache: error: failed to execute compile"));
        assert_eq!(cache_failure_line("error[E0308]\n"), None);
        assert_eq!(cache_failure_line("SCCACHE: Error: Failed to create temp dir\n"), Some("SCCACHE: Error: Failed to create temp dir"));
    }

    #[test_id(tkkxi4j)]
    #[test]
    fn failed_tests_reads_cargo_failure_lines_only() {
        let out = "test a::ok ... ok\ntest a::bad ... FAILED\ntest a::bad ... FAILED\ntest result: FAILED. 1 passed\n";
        assert_eq!(failed_tests(out), vec!["a::bad".to_string()]);
        assert!(failed_tests("test result: FAILED\n").is_empty());
        let nx = "        PASS [   0.007s] (1/2) fx t::quick\n     TIMEOUT [   4.002s] (2/2) fx t::sleeper\n        FAIL [   0.1s] fx t::bad\n     TIMEOUT [   4.002s] (2/2) fx t::sleeper\n";
        assert_eq!(failed_tests(nx), vec!["t::sleeper".to_string(), "t::bad".to_string()]);
        let padded = "        FAIL [   0.1s] ( 3/12) fx t::padded\n  SIGSEGV [   0.1s] (12/12) fx t::crashed\n";
        assert_eq!(failed_tests(padded), vec!["t::padded".to_string(), "t::crashed".to_string()]);
    }

    #[test_id(tcccka8)]
    #[test]
    fn fence_refusal_names_the_fence() {
        let out = "x\nlifecycle-guard: REFUSED ... bead/src/main.rs:717\n";
        assert_eq!(fence_refusal(out), Some("lifecycle-guard".to_string()));
        assert_eq!(fence_refusal("some text: REFUSED\n"), None);
        assert_eq!(fence_refusal("all green"), None);
        let lint = "scanning\nprocess-exit-in-library: spira/x.rs:13: lists a path that no longer needs an exception\n";
        assert_eq!(fence_refusal(lint), Some("process-exit-in-library".to_string()));
        assert_eq!(fence_refusal("error: boom\ngate: x\nsome-text here: y\n"), None);
    }

    #[test_id(t6hn493)]
    #[test]
    fn red_suites_reads_every_red_form_once_in_order() {
        let out = "test-a.sh ok\ntest-b.sh RED\n  test-c.sh TIMEOUT\ntest-d.sh was killed\ntest-b.sh RED\nx test-e.sh FAILED";
        assert_eq!(
            red_suites(out, &[]),
            ["test-b.sh", "test-c.sh", "test-d.sh", "test-e.sh"]
        );
    }

    #[test_id(te54m10)]
    #[test]
    fn ran_suites_includes_passes_and_skips() {
        let out = "test-a.sh ok\ntest-b.sh SKIPPED\ntest-c.sh QUARANTINED-RED\ntest-d.sh RED\nnoise.sh words";
        assert_eq!(
            ran_suites(out, &[]),
            ["test-a.sh", "test-b.sh", "test-c.sh", "test-d.sh"]
        );
    }

    #[test_id(tiektu9)]
    #[test]
    fn verdict_ran_reads_the_runners_count() {
        assert_eq!(verdict_ran("  a.sh ok\nVERDICT GREEN ran=66 skipped=2"), Some(66));
        assert_eq!(verdict_ran("VERDICT RED ran=3 red=1"), Some(3));
        assert!(suites_step_ran("VERDICT EMPTY rc=5 ran=0 skipped=2 — all 2 selected suites skipped", &[]));
        assert_eq!(verdict_ran("a.sh ok\nran=5 but no verdict"), None);
    }

    #[test_id(tv1y66k)]
    #[test]
    fn a_fault_line_counts_as_ran() {
        assert_eq!(ran_suites("  test-f.sh   FAULT   podman lost the exit status", &[]), ["test-f.sh"]);
    }

    #[test_id(tapg7o4)]
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
    #[test_id(tklr78r)]
    #[test]
    fn an_undeclared_skip_reclassified_by_testenv_still_reads_as_a_red_suite() {
        let out = "  test-f.sh                        RED     undeclared skip — skip:server testdb not available\nVERDICT RED ran=1 red=1";
        assert_eq!(red_suites(out, &[]), ["test-f.sh"]);
        assert_eq!(ran_suites(out, &[]), ["test-f.sh"]);
        let out_req = "  test-e.sh                        RED     undeclared skip — requires:claude\nVERDICT RED ran=0 red=1";
        assert_eq!(red_suites(out_req, &[]), ["test-e.sh"]);
    }

    #[test_id(tp8tr5d)]
    #[test]
    fn a_word_not_ending_in_sh_is_not_a_suite() {
        assert!(red_suites("cargo RED\nfoo.shx RED", &[]).is_empty());
    }

    #[test_id(tinkyw1)]
    #[test]
    fn a_missing_cargo_bin_exe_path_is_found_and_other_failures_are_not() {
        assert!(missing_built_binary("assertion failed: Path::new(env!(\"CARGO_BIN_EXE_spira-claim\")).is_file()"));
        assert!(!missing_built_binary("assertion failed: x.is_file()"));
        assert!(!missing_built_binary("error[E0308]: mismatched types"));
    }

    #[test_id(tt923v4)]
    #[test]
    fn tree_key_is_filesystem_safe() {
        assert_eq!(tree_key("spira/sp-a1"), "spira-sp-a1");
        assert_eq!(tree_key("concierge/x_y z"), "concierge-x-y-z");
        assert_eq!(tree_key("a.b-c"), "a.b-c");
    }

    #[test_id(t8jiglh)]
    #[test]
    fn bash_paths_takes_only_relative_paths_after_a_word_boundary() {
        let cmd = r#"bash spira/a.sh && bash /abs/b.sh && bash -c 'x' && mybash spira/c.sh && { bash spira/d.sh; } && "$X" --only y && bash "$Q" && bash spira/e.sh|cat"#;
        assert_eq!(bash_paths(cmd), ["spira/a.sh", "spira/d.sh", "spira/e.sh"]);
        assert!(bash_paths("").is_empty());
        assert!(bash_paths("cargo test").is_empty());
    }

    #[test_id(t8lt032)]
    #[test]
    fn fault_detail_keeps_the_runners_refusal_and_drops_the_passing_steps() {
        let msg = "gate: g's own gate reported a harness fault (exit 75)\ngate: command: x && y\nfence: lint checked 3 files\ngate-step ok rc=0 :: lint\nbatch: KEY is not declared; refusing\nVERDICT FAULT rc=2 ran=0 reason=settings-refused\ngate: phase 'reentry' failed (exit 75): _b=0\ngate: VERDICT=NO_VERDICT reason=harness-fault";
        let d = fault_detail(msg);
        assert!(d.contains("KEY is not declared") && d.contains("reason=settings-refused"), "{d}");
        assert!(!d.contains('\n') && !d.contains("fence:") && !d.contains("_b=0") && !d.contains("gate-step"), "{d}");
        assert_eq!(fault_detail("gate: command: x"), "");
    }

    #[test_id(t3guv0c)]
    #[test]
    fn harness_fault_detail_takes_the_last() {
        let out = "x\nbatch: harness fault — container died mid-batch (one)\nbatch: harness fault — container died mid-batch (two)  \n";
        assert_eq!(harness_fault_detail(out).as_deref(), Some("two"));
        assert_eq!(harness_fault_detail("nothing"), None);
    }



    #[test_id(tuamak1)]
    #[test]
    fn attribution_base_untestable_when_the_base_did_not_run() {
        assert_eq!(
            attribute("test-a.sh RED", false, 1, "", NONE),
            Attribution::BaseUntestable
        );
    }

    #[test_id(t3fp839)]
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

    #[test_id(tworxzs)]
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

    #[test_id(tw5ca5p)]
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
    #[test_id(t355di8)]
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
    #[test_id(tqv9jpl)]
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

    #[test_id(t95qksc)]
    #[test]
    fn attribution_a_red_suite_the_base_lacks_is_the_branchs() {
        assert_eq!(
            attribute("test-new.sh RED", true, 0, "", &["test-new.sh".to_string()]),
            Attribution::BranchRed("test-new.sh".into())
        );
    }

    #[test_id(tgmw3x3)]
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

    #[test_id(tl3dn2f)]
    #[test]
    fn tail_bytes_keeps_the_end_and_whole_characters() {
        assert_eq!(tail_bytes("abcdef", 4), "def");
        assert_eq!(tail_bytes("ab", 100), "ab");
        assert_eq!(tail_bytes("a—b", 3), "b");
    }
}

/// The steps file a gate command writes (`SPIRA_GATE_STEPS`): `<name>\t<epoch secs>` per step as
/// it starts. Each step lasts until the next line's epoch; the last until `phase_end`. Lines
/// that do not read are skipped and counted, never guessed at.
pub fn step_secs(steps: &str, phase_end: u64) -> (Vec<(String, u64)>, usize) {
    let mut starts: Vec<(String, u64)> = Vec::new();
    let mut skipped = 0;
    for line in steps.lines().filter(|l| !l.trim().is_empty()) {
        match line.split_once('\t').map(|(n, e)| (n.trim(), e.trim().parse::<u64>())) {
            Some((n, Ok(e))) if !n.is_empty() => starts.push((n.to_string(), e)),
            _ => skipped += 1,
        }
    }
    let ends: Vec<u64> = starts.iter().skip(1).map(|s| s.1).chain([phase_end]).collect();
    let secs = starts.into_iter().zip(ends).map(|((n, e), end)| (n, end.saturating_sub(e))).collect();
    (secs, skipped)
}

/// The longest step; the first of equals. None when the file names no step.
pub fn slowest_step(steps: &str, phase_end: u64) -> Option<(String, u64)> {
    longest(step_secs(steps, phase_end).0)
}

pub fn longest(steps: Vec<(String, u64)>) -> Option<(String, u64)> {
    steps.into_iter().reduce(|best, s| if s.1 > best.1 { s } else { best })
}

#[cfg(test)]
mod step_tests {
    use super::*;

    #[test_id(tz9xswa)]
    #[test]
    fn an_empty_steps_file_names_no_step() {
        assert_eq!(slowest_step("", 100), None);
    }

    #[test_id(td2ilf1)]
    #[test]
    fn a_malformed_line_is_skipped_and_counted() {
        let (steps, skipped) = step_secs("fences\t10\nno tab here\nsuites\tsoon\n\tbare\nsuites\t14\n", 20);
        assert_eq!(steps, vec![("fences".to_string(), 4), ("suites".to_string(), 6)]);
        assert_eq!(skipped, 3);
    }

    #[test_id(t214890)]
    #[test]
    fn a_single_step_lasts_until_the_phase_end() {
        assert_eq!(slowest_step("build\t10\n", 25), Some(("build".to_string(), 15)));
    }

    #[test_id(t63be1w)]
    #[test]
    fn the_longest_step_wins_and_a_tie_keeps_the_first() {
        assert_eq!(slowest_step("a\t0\nb\t5\nc\t10\n", 15), Some(("a".to_string(), 5)));
    }
}

/// Seconds in cargo's `<h>h <m>m <s>s` duration (`1.39s`, `3m 27s`).
fn cargo_duration(d: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut any = false;
    for w in d.split_whitespace() {
        let (n, unit) = w.split_at(w.len().checked_sub(1)?);
        let n: f64 = n.parse().ok()?;
        total += n * match unit {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            _ => return None,
        };
        any = true;
    }
    any.then_some(total)
}

/// The seconds of a cargo `Finished \`<profile>\` profile [...] target(s) in <dur>` line.
fn finished_secs(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix("Finished `")?;
    let (_, rest) = rest.split_once("` profile [")?;
    let (_, dur) = rest.split_once("target(s) in ")?;
    cargo_duration(dur.trim())
}

/// The names `test_split` returns, in order.
pub const TEST_PARTS: [&str; 5] = ["test-build", "test-list", "test", "test-integration", "test-other"];

/// The unit test phase's wall split into [`TEST_PARTS`] by the `gate-substep` markers the phase
/// prints. Each compile is credited to the command whose marker follows it and is capped at that
/// command's seconds; each part is capped at what the wall has left, and `test-other` takes the
/// rest, so the parts sum to `wall`. None when no marker is present.
pub fn test_split(out: &str, wall: u64) -> Option<Vec<(String, u64)>> {
    let (mut compile, mut unit, mut integ) = (0.0, None, None);
    let mut pending = 0.0;
    for line in out.lines() {
        if let Some(s) = finished_secs(line) {
            pending += s;
            continue;
        }
        let Some(rest) = line.trim().strip_prefix(crate::compose::SUBSTEP_MARK) else { continue };
        let mut w = rest.split_whitespace();
        let (Some(label), Some(secs)) = (w.next(), w.next().and_then(|s| s.strip_prefix("secs=")?.parse::<f64>().ok())) else { continue };
        let own = pending.min(secs);
        pending = 0.0;
        match label {
            "unit" => unit = Some((secs, own)),
            "integration" => integ = Some((secs, own)),
            _ => continue,
        }
        compile += own;
    }
    if unit.is_none() && integ.is_none() {
        return None;
    }
    let (u, uc) = unit.unwrap_or_default();
    let (i, ic) = integ.unwrap_or_default();
    let run = spira_config::unit_tier::timings(out).run_secs;
    let raw = [compile, (u - uc - run).max(0.0), run, (i - ic).max(0.0)];
    let mut left = wall;
    let mut parts = Vec::new();
    for (name, v) in TEST_PARTS.iter().zip(raw) {
        let v = (v.round() as u64).min(left);
        left -= v;
        parts.push((name.to_string(), v));
    }
    parts.push((TEST_PARTS[4].to_string(), left));
    Some(parts)
}

/// Seconds past which compiling inside the test phase, after the build phase ran, is a recompile.
pub const RECOMPILE_SECS: u64 = 30;

/// The `test-build` seconds of a gate whose build phase ran and whose test phase still compiled
/// for more than [`RECOMPILE_SECS`]; None otherwise.
pub fn recompiled_secs<'a>(phases: impl IntoIterator<Item = (&'a str, u64)>) -> Option<u64> {
    let (mut built, mut test_build) = (false, 0);
    for (n, s) in phases {
        built |= n == "build";
        if n == "test-build" {
            test_build = s;
        }
    }
    (built && test_build > RECOMPILE_SECS).then_some(test_build)
}

#[cfg(test)]
mod test_split_tests {
    use super::*;

    #[test_id(t39wvdf)]
    #[test]
    fn a_short_compile_or_a_gate_that_never_built_is_not_a_recompile() {
        assert_eq!(recompiled_secs([("build", 1), ("test-build", 30)]), None);
        assert_eq!(recompiled_secs([("test-build", 207)]), None);
        assert_eq!(recompiled_secs([("build", 1), ("test-build", 207)]), Some(207));
    }

    const FIN: &str = "    Finished `test` profile [unoptimized + debuginfo] target(s) in";

    fn sum(p: &[(String, u64)]) -> u64 {
        p.iter().map(|x| x.1).sum()
    }

    #[test_id(tj2kpvr)]
    #[test]
    fn a_minutes_and_seconds_duration_is_read_as_seconds() {
        let out = format!("{FIN} 3m 27s\ngate-substep unit secs=210\n     Summary [   1.000s] 1 test run: 1 passed, 0 skipped\n");
        let p = test_split(&out, 211).unwrap();
        assert_eq!(p[0], ("test-build".to_string(), 207));
        assert_eq!(p[1], ("test-list".to_string(), 2));
        assert_eq!(finished_secs(&format!("{FIN} 1.39s")), Some(1.39));
    }

    #[test_id(t7bqqm1)]
    #[test]
    fn an_output_with_no_marker_is_not_split() {
        assert_eq!(test_split(&format!("{FIN} 3.00s\n"), 7), None);
    }

    #[test_id(tmq6vy2)]
    #[test]
    fn the_parts_name_the_five_and_sum_to_the_wall_even_when_the_markers_exceed_it() {
        let out = format!("{FIN} 3.00s\ngate-substep unit secs=50\n{FIN} 3.00s\ngate-substep integration secs=40\n");
        for wall in [0, 7, 60, 200] {
            let p = test_split(&out, wall).unwrap();
            assert_eq!(p.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), TEST_PARTS, "{wall}");
            assert_eq!(sum(&p), wall, "{p:?}");
        }
    }
}
