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

/// One `gh` run that exited `code`. `gh api rate_limit` is not charged, so it is not counted.
pub fn note(args: &[&str], code: i32) {
    if args.starts_with(&["api", "rate_limit"]) {
        return;
    }
    let outcome = if code == 0 { FRESH } else { FAILED };
    TALLY.with(|t| {
        let mut t = t.borrow_mut();
        let caller = if t.caller.is_empty() {
            caller_of("other")
        } else {
            t.caller.clone()
        };
        *t.calls.entry((caller, outcome)).or_default() += 1;
    });
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
