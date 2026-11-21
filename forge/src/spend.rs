//! What this process's `gh` invocations spend of the hourly API budget, counted by the verb
//! that made them, under the metric the runtime's native client already pushes.

use spira_otel::metrics::{MetricsSink, Point};
use std::cell::RefCell;
use std::collections::BTreeMap;

pub const CALLS: &str = "spira_gh_api_calls";
pub const FRESH: &str = "fresh";
pub const FAILED: &str = "failed";

#[derive(Default)]
struct Tally {
    caller: String,
    calls: BTreeMap<(String, &'static str), u64>,
}

thread_local! {
    static TALLY: RefCell<Tally> = RefCell::new(Tally::default());
}

pub fn caller_of(operation: &str) -> String {
    format!("forge:{operation}")
}

pub fn set_caller(operation: &str) {
    TALLY.with(|t| t.borrow_mut().caller = caller_of(operation));
}

/// One `gh` run that exited `code`, under this thread's current verb. `gh api rate_limit` is not
/// charged, so it is not counted.
pub fn note(args: &[&str], code: i32) {
    let caller = TALLY.with(|t| {
        let t = t.borrow();
        if t.caller.is_empty() {
            caller_of("other")
        } else {
            t.caller.clone()
        }
    });
    note_as(&caller, args, code);
}

/// One `gh` run that exited `code`, counted under `caller` (`<component>:<verb>`).
pub fn note_as(caller: &str, args: &[&str], code: i32) {
    if args.starts_with(&["api", "rate_limit"]) || args.starts_with(&["auth", "token"]) {
        return;
    }
    let outcome = if code == 0 { FRESH } else { FAILED };
    TALLY.with(|t| {
        *t.borrow_mut()
            .calls
            .entry((caller.to_string(), outcome))
            .or_default() += 1;
    });
}

/// Runs one `gh` process through `spawn`, counts it under `caller` once it has exited, and pushes
/// the count to `sink`. A process that never started spent nothing and is not counted.
pub fn counted_output(
    sink: &MetricsSink,
    caller: &str,
    args: &[&str],
    spawn: impl FnOnce() -> std::io::Result<std::process::Output>,
) -> std::io::Result<std::process::Output> {
    let out = spawn()?;
    note_as(caller, args, out.status.code().unwrap_or(-1));
    flush(sink);
    Ok(out)
}

/// As [`counted_output`], for a caller that only has the exit code (a `status()` run).
pub fn counted_status(
    sink: &MetricsSink,
    caller: &str,
    args: &[&str],
    spawn: impl FnOnce() -> std::io::Result<std::process::ExitStatus>,
) -> std::io::Result<std::process::ExitStatus> {
    let st = spawn()?;
    note_as(caller, args, st.code().unwrap_or(-1));
    flush(sink);
    Ok(st)
}

/// Pushes and clears what this thread counted.
pub fn flush(sink: &MetricsSink) {
    let calls = TALLY.with(|t| std::mem::take(&mut t.borrow_mut().calls));
    for ((caller, outcome), n) in calls {
        sink.push(
            &[Point::Count {
                name: CALLS,
                value: n,
            }],
            &[("caller", &caller), ("outcome", outcome)],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    fn series_of(got: &[fixture::Sample], caller: &str, outcome: &str) -> Option<u64> {
        got.iter()
            .find(|s| {
                s.name == CALLS
                    && s.label("caller") == Some(caller)
                    && s.label("outcome") == Some(outcome)
            })
            .map(|s| s.value as u64)
    }

    #[test_id(tspb8eb)]
    #[test]
    fn a_named_caller_is_pushed_under_its_own_label_whatever_verb_the_thread_is_in() {
        set_caller("pr-state");
        note_as("gate-check:run-list", &["run", "list"], 0);
        note_as("gate-check:run-list", &["run", "list"], 0);
        note_as("gate-check:run-list", &["run", "list"], 1);
        note_as("gate-check:run-list", &["auth", "token"], 0);
        let (_, got) = fixture::capture(flush);
        assert_eq!(
            (
                series_of(&got, "gate-check:run-list", FRESH),
                series_of(&got, "gate-check:run-list", FAILED)
            ),
            (Some(2), Some(1)),
            "{got:?}"
        );
        assert_eq!(got.iter().filter(|s| s.name == CALLS).count(), 2, "{got:?}");
    }

    #[test_id(tyojy2t)]
    #[test]
    fn a_verbs_gh_calls_are_pushed_under_its_name_and_a_rate_limit_read_is_not_counted() {
        set_caller("pr-state");
        note(&["pr", "view"], 0);
        note(&["pr", "view"], 0);
        note(&["pr", "view"], 1);
        note(&["api", "rate_limit"], 0);
        let (_, got) = fixture::capture(flush);
        let of = |outcome: &str| {
            got.iter()
                .find(|s| {
                    s.name == CALLS
                        && s.label("caller") == Some("forge:pr-state")
                        && s.label("outcome") == Some(outcome)
                })
                .map(|s| s.value as u64)
        };
        assert_eq!((of(FRESH), of(FAILED)), (Some(2), Some(1)), "{got:?}");
        assert_eq!(
            got.iter().filter(|s| s.name == CALLS).count(),
            2,
            "rate_limit counted: {got:?}"
        );
    }
}
