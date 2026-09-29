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
        ],
        true,
    )
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

/// Whose fault a failed branch trial is (`gate_attribute`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribution {
    BranchRed(String),
    BaseRed(String),
    BaseTimeout(String),
    BaseUntestable,
}

/// `base_rc` is only read when `base_ran`.
pub fn attribute(branch_out: &str, base_ran: bool, base_rc: i32, base_out: &str) -> Attribution {
    if !base_ran {
        return Attribution::BaseUntestable;
    }
    if base_rc == 0 {
        let s = red_suites(branch_out)
            .into_iter()
            .next()
            .unwrap_or_else(|| "-".into());
        return Attribution::BranchRed(s);
    }
    let base_reds = red_suites(base_out);
    let branch_only: Vec<String> = red_suites(branch_out)
        .into_iter()
        .filter(|s| !base_reds.contains(s))
        .collect();
    if !base_reds.is_empty() && !branch_only.is_empty() {
        return Attribution::BranchRed(branch_only[0].clone());
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
    use super::*;

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
    fn timed_out_is_only_the_watchdogs() {
        let out = "test-a.sh TIMEOUT\ntest-b.sh RED\ntest-c.sh was killed";
        assert_eq!(timed_out_suites(out), ["test-a.sh", "test-c.sh"]);
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
            attribute("test-a.sh RED", false, 1, ""),
            Attribution::BaseUntestable
        );
    }

    #[test]
    fn attribution_branch_red_when_the_base_passes() {
        assert_eq!(
            attribute("test-a.sh RED", true, 0, ""),
            Attribution::BranchRed("test-a.sh".into())
        );
        assert_eq!(
            attribute("silent", true, 0, ""),
            Attribution::BranchRed("-".into())
        );
    }

    #[test]
    fn attribution_branch_red_on_a_suite_red_only_on_the_branch() {
        let a = attribute("test-a.sh RED\ntest-b.sh RED", true, 1, "test-a.sh RED");
        assert_eq!(a, Attribution::BranchRed("test-b.sh".into()));
    }

    #[test]
    fn attribution_base_red_when_the_base_has_the_same_reds() {
        let a = attribute("test-a.sh RED", true, 1, "test-a.sh RED\ntest-z.sh RED");
        assert_eq!(a, Attribution::BaseRed("test-a.sh".into()));
        assert_eq!(
            attribute("x", true, 1, "no suite named"),
            Attribution::BaseRed("-".into())
        );
    }

    #[test]
    fn attribution_base_timeout_when_every_base_red_was_killed() {
        let a = attribute(
            "test-a.sh TIMEOUT",
            true,
            1,
            "test-a.sh TIMEOUT\ntest-b.sh was killed",
        );
        assert_eq!(a, Attribution::BaseTimeout("test-a.sh".into()));
        let mixed = attribute(
            "test-a.sh TIMEOUT",
            true,
            1,
            "test-a.sh TIMEOUT\ntest-b.sh RED",
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
