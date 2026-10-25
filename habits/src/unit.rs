use std::path::Path;
use std::time::{Duration, Instant};

use spira_otel::metrics::{MetricsSink, Point};

use crate::config::{self, Stop};
use crate::exec::{self, Ran};
use crate::state;

const MAX_TESTS: usize = 5;
const MAX_DETAIL: usize = 3;
const MAX_LINE: usize = 160;

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Green,
    Red { failed: Vec<(String, Vec<String>)> },
    Fault(String),
}

fn clip(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > MAX_LINE {
        format!("{}…", s.chars().take(MAX_LINE).collect::<String>())
    } else {
        s.to_string()
    }
}

/// testenv's `VERDICT` line plus cargo's failure blocks: each failing test's name and the first
/// lines of its `---- name stdout ----` block.
pub fn parse(out: &str) -> Verdict {
    let verdict = out.lines().rev().find_map(|l| l.strip_prefix("VERDICT "));
    match verdict {
        Some(v) if v.starts_with("GREEN") => return Verdict::Green,
        Some(v) if v.starts_with("FAULT") => return Verdict::Fault(v.to_string()),
        Some(v) if v.starts_with("RED") => {}
        _ => return Verdict::Fault("no testenv verdict".into()),
    }
    let mut names: Vec<String> = out.lines().filter_map(|l| l.trim().strip_prefix("test ")?.strip_suffix(" ... FAILED").map(str::to_string)).collect();
    names.dedup();
    let lines: Vec<&str> = out.lines().collect();
    let failed = names
        .into_iter()
        .map(|n| {
            let head = format!("---- {n} stdout ----");
            let detail = lines
                .iter()
                .position(|l| l.trim() == head)
                .map(|i| lines[i + 1..].iter().take_while(|l| !l.starts_with("---- ") && !l.starts_with("failures:")).filter(|l| !l.trim().is_empty()).take(MAX_DETAIL).map(|l| clip(l)).collect())
                .unwrap_or_default();
            (n, detail)
        })
        .collect();
    Verdict::Red { failed }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("timeout").args(["5", "git", "-C"]).arg(root).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn count(sink: &MetricsSink, name: &'static str, repo: &str, bead: &str) {
    sink.push(&[Point::Count { name, value: 1 }], &[("repo", repo), ("bead", bead)]);
}

/// Runs the repo's unit-test runner on the branch's committed tip. Says nothing when green;
/// the Stop it answers is never refused.
pub fn stop(s: &Stop, root: &Path, repo: &str, tmp: &Path, sink: &MetricsSink) -> Option<String> {
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let bead = std::env::var("SPIRA_WORK_BEAD_ID").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| branch.rsplit('/').next().unwrap_or(&branch).to_string());
    let t = Instant::now();
    let argv = config::argv(&s.run, &[("branch", &branch)]);
    let ran = exec::run(&argv, root, Duration::from_millis(s.budget_ms));
    let ms = t.elapsed().as_millis() as u64;
    let marker = state::red_marker(tmp, &bead);
    let out = match ran {
        Ran::Missing => return Some(format!("habits: {} skipped (tool not found)", s.name)),
        Ran::Timeout => return Some(format!("habits: {} skipped (over its {} ms budget)", s.name, s.budget_ms)),
        Ran::Out { stdout, stderr } => format!("{stdout}\n{stderr}"),
    };
    match parse(&out) {
        Verdict::Fault(why) => Some(format!("habits: {} skipped ({why})", s.name)),
        Verdict::Green => {
            crate::push(sink, repo, &s.name, 0, 0, ms);
            if marker.exists() {
                let _ = std::fs::remove_file(&marker);
                count(sink, "habit_unit_tests_fixed_before_submit", repo, &bead);
            }
            None
        }
        Verdict::Red { failed } => {
            crate::push(sink, repo, &s.name, failed.len().max(1) as u64, 0, ms);
            count(sink, "habit_unit_tests_red_at_stop", repo, &bead);
            let _ = std::fs::create_dir_all(tmp);
            let _ = std::fs::write(&marker, &branch);
            let mut lines = vec![format!("habits: {} is RED on the committed tip of {branch}: {} failing test(s)", s.name, failed.len())];
            for (name, detail) in failed.iter().take(MAX_TESTS) {
                lines.push(format!("  {name}"));
                lines.extend(detail.iter().map(|d| format!("    {d}")));
            }
            if failed.len() > MAX_TESTS {
                lines.push(format!("  (+{} more)", failed.len() - MAX_TESTS));
            }
            Some(lines.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: &str = "running 2 tests\ntest a::ok ... ok\ntest a::bad ... FAILED\n\nfailures:\n\n---- a::bad stdout ----\nthread 'a::bad' panicked at src/a.rs:9:5:\nassertion `left == right` failed\n  left: 1\n right: 2\n\nfailures:\n    a::bad\n\nVERDICT RED ran=1 red=unit-tests.sh\n";

    #[test]
    fn red_names_the_test_and_the_first_assertion_lines() {
        let Verdict::Red { failed } = parse(RED) else { panic!("not red") };
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "a::bad");
        assert_eq!(failed[0].1.len(), 3);
        assert!(failed[0].1[1].contains("assertion"));
    }

    #[test]
    fn green_fault_and_a_missing_verdict_are_told_apart() {
        assert_eq!(parse("VERDICT GREEN ran=1"), Verdict::Green);
        assert!(matches!(parse("VERDICT FAULT rc=2 ran=0 reason=usage"), Verdict::Fault(_)));
        assert!(matches!(parse("test x ... FAILED\n"), Verdict::Fault(_)));
    }
}
