//! The fences' positive controls (sp-ufbkh; DESIGN.md "Every fence proves it checked").
//!
//! A fence that exits 0 having checked nothing (skipped, a precondition unmet, empty input)
//! looks exactly like a fence that checked everything and found it clean. So every fence the
//! gate string runs prints one line on success,
//!
//! ```text
//! fence: <name> checked <n> <unit>[ anything]
//! ```
//!
//! with `n > 0`, and the gate reads the trial's output for it. A trial that exits 0 while
//! a fence it ran is missing its line, or reports `checked 0`, is NO_VERDICT
//! `reason=fence-silent`, never PASS.
//!
//! The string is the tree's own definition (`gate.steps`, def.rs, sp-quu2w) composed, so
//! the expectation moves with the branch that ports or deletes a fence.
//!
//! Which fences a gate string runs is read from the string itself, the same way the
//! preflight reads its `bash <path>` words ([`parse::bash_paths`]): each `bash <dir>/<x>.sh`
//! is the fence `<x>`, except the suite selector ([`SELECTOR`]), which runs none; the
//! `"$SPIRA_LINT_BIN"` word is spira-lint and the rules it prints a line for.

use crate::parse;
use std::collections::BTreeMap;

/// The suite selector's script stem. It is not a fence and runs none (sp-ufbkh): the gate
/// string captures it as `_s="$(…)";`, so a fence failing inside it emptied the selection
/// and the string exited 0. Its fences are spira-lint rules now, in the `&&` chain;
/// `the_selector_runs_no_fence_…` fails if one comes back.
pub const SELECTOR: &str = "gate-touched";


/// The spira-lint word in a gate string (quoted or not, braced or not).
const LINT_WORDS: &[&str] = &["$SPIRA_LINT_BIN", "${SPIRA_LINT_BIN}"];

/// spira-lint's own line, always.
pub const LINT: &str = "spira-lint";

/// The spira-lint rules that print their own line, beside spira-lint's (a rule that runs
/// only with `--only <rule>` or with no `--only`). Each was a bash fence the gate could not
/// see skip (sp-ufbkh).
pub const LINT_RULE_FENCES: &[&str] = &[
    "plan-matrix",
    "lockfile-lint",
    "tier-budget-allowlist",
    "tier-budget-area-allowlist",
    "tier-budget-areas",
];

/// The fences `cmd` runs, in the order they appear, each once.
pub fn expected(cmd: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |n: &str| {
        if !out.iter().any(|x| x == n) {
            out.push(n.to_string());
        }
    };
    // Positions, so the lint word and the bash words keep their order in the string.
    let mut words: Vec<(usize, Vec<String>)> = Vec::new();
    let mut from = 0;
    for w in parse::bash_paths(cmd) {
        let at = cmd[from..].find(&w).map(|i| from + i).unwrap_or(from);
        from = at + w.len();
        let Some(stem) = w
            .rsplit('/')
            .next()
            .and_then(|b| b.strip_suffix(".sh"))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if stem != SELECTOR {
            words.push((at, vec![stem.to_string()]));
        }
    }
    for lw in LINT_WORDS {
        let mut i = 0;
        while let Some(off) = cmd[i..].find(lw) {
            let at = i + off;
            let end = at + lw.len();
            // `$SPIRA_LINT_BIN` inside `${SPIRA_LINT_BIN}` is the braced word, counted once.
            let braced_dup = *lw == "$SPIRA_LINT_BIN" && cmd[..at].ends_with("${");
            i = end;
            if braced_dup || cmd[end..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let rest = cmd[end..].trim_start_matches('"').trim_start();
            let only = rest
                .strip_prefix("--only")
                .map(|r| r.trim_start().split(|c: char| c.is_whitespace() || ";&|)}".contains(c)).next().unwrap_or(""));
            let mut names = vec![LINT.to_string()];
            for r in LINT_RULE_FENCES {
                if only.is_none_or(|o| o == *r) {
                    names.push(r.to_string());
                }
            }
            words.push((at, names));
        }
    }
    words.sort_by_key(|(at, _)| *at);
    for (_, names) in words {
        for n in names {
            push(&n);
        }
    }
    out
}

/// Every `fence: <name> checked <n> <unit>` line in `out`: the largest `n` each name reported,
/// with its unit.
pub fn lines(out: &str) -> BTreeMap<String, (u64, String)> {
    let mut m: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for l in out.lines() {
        let Some(rest) = l.trim_start().strip_prefix("fence: ") else {
            continue;
        };
        let mut it = rest.split_whitespace();
        let (Some(name), Some("checked"), Some(n), Some(unit)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        let Ok(n) = n.parse::<u64>() else { continue };
        let e = m.entry(name.to_string()).or_insert((0, unit.to_string()));
        if n >= e.0 {
            *e = (n, unit.to_string());
        }
    }
    m
}

/// The fences of `expected` whose line is missing from `out` or reports `checked 0`.
pub fn silent(expected: &[String], out: &str) -> Vec<String> {
    let seen = lines(out);
    expected
        .iter()
        .filter(|f| seen.get(f.as_str()).is_none_or(|(n, _)| *n == 0))
        .cloned()
        .collect()
}

/// `name=n unit, …` over `expected`, for the PASS message.
pub fn summary(expected: &[String], out: &str) -> String {
    let seen = lines(out);
    expected
        .iter()
        .filter_map(|f| seen.get(f).map(|(n, u)| format!("{f}={n} {u}")))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parser fixture: the spira repository's gate string as config held it on 2026-09-29.
    /// The live definition is the tree's `gate.steps` (def.rs, sp-quu2w); a fence port edits
    /// that file, not this fixture.
    const PROD: &str = r#"bash spira/inventory.sh && bash spira/literal-lint.sh && bash spira/scratch-fence.sh && "$SPIRA_LINT_BIN" && bash spira/testdb-mode-lint.sh && bash spira/bd-stdin-lint.sh && bash spira/gh-intake-lint.sh && bash spira/incident-cause-lint.sh && bash spira/tmux-scope-fence.sh && bash spira/wiki-add-fence.sh && bash spira/build-fence.sh && { _s="$(bash spira/gate-touched.sh "$SPIRA_GATE_BASE" "${SPIRA_GATE_SELECT_HEAD:-$SPIRA_GATE_BRANCH}")"; [ -n "$_s" ] || exit 0; _b=0; "$SPIRA_TESTENV_BIN" --deadline "${SPIRA_GATE_BUDGET:-300}" --suites "${_s//$'\n'/,}" "$SPIRA_GATE_BRANCH" || _b=$?; case "$_b" in 2|3) exit 75;; *) exit "$_b";; esac; }"#;

    #[test]
    fn the_production_gate_string_runs_these_fences() {
        assert_eq!(
            expected(PROD),
            [
                "inventory",
                "literal-lint",
                "scratch-fence",
                "spira-lint",
                "plan-matrix",
                "lockfile-lint",
                "tier-budget-allowlist",
                "tier-budget-area-allowlist",
                "tier-budget-areas",
                "testdb-mode-lint",
                "bd-stdin-lint",
                "gh-intake-lint",
                "incident-cause-lint",
                "tmux-scope-fence",
                "wiki-add-fence",
                "build-fence",
            ]
        );
    }

    #[test]
    fn spira_lint_with_only_expects_that_rule_alone() {
        assert_eq!(
            expected(r#""$SPIRA_LINT_BIN" --only payload-argv-lint && x"#),
            ["spira-lint"]
        );
        assert_eq!(
            expected(r#""${SPIRA_LINT_BIN}" --only plan-matrix"#),
            ["spira-lint", "plan-matrix"]
        );
        assert_eq!(
            expected("bash spira/gate-touched.sh b h"),
            Vec::<String>::new(),
            "the selector runs no fence"
        );
        assert_eq!(expected("$SPIRA_LINT_BIN_OTHER"), Vec::<String>::new());
    }

    #[test]
    fn a_string_with_no_fence_expects_none() {
        assert!(expected("").is_empty());
        assert!(expected("cargo test && run-suites").is_empty());
        assert_eq!(expected("bash spira/fence.sh && bash spira/fence.sh"), ["fence"]);
    }

    #[test]
    fn lines_read_the_positive_control_and_its_count() {
        let out = "noise\nfence: inventory checked 5321 files\n  fence: plan-matrix checked 212 use-cases (40 at base)\nfence: bad checked many things\nfence: z checked 0 files\n";
        let m = lines(out);
        assert_eq!(m["inventory"], (5321, "files".into()));
        assert_eq!(m["plan-matrix"], (212, "use-cases".into()));
        assert_eq!(m["z"], (0, "files".into()));
        assert!(!m.contains_key("bad"));
    }

    #[test]
    fn a_fence_with_no_line_or_a_zero_count_is_silent() {
        let want: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let out = "fence: a checked 3 files\nfence: c checked 0 files\n";
        assert_eq!(silent(&want, out), ["b", "c"]);
        assert!(silent(&want, "fence: a checked 1 x\nfence: b checked 1 x\nfence: c checked 1 x").is_empty());
        assert_eq!(summary(&want, out), "a=3 files, c=0 files");
    }

    /// The selector's preamble (everything before it reads HEAD) calls no script at all, and
    /// every rule the gate expects a line from is one spira-lint runs.
    #[test]
    fn the_selector_runs_no_fence_and_spira_lint_has_every_rule_fence() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let src = std::fs::read_to_string(dir.join("gate-touched.sh")).unwrap();
        let section = src.split("\nHEAD=").next().unwrap();
        let calls: Vec<&str> = section
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && l.contains("bash "))
            .collect();
        assert!(calls.is_empty(), "{calls:?}");
        let rules: Vec<&str> = spira_lint::all_rules().iter().map(|r| r.name()).collect();
        for f in LINT_RULE_FENCES {
            assert!(rules.contains(f), "spira-lint has no rule {f}: {rules:?}");
        }
    }
}
