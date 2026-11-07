//! The Lifecycle section over the 10-31 replay: goodput computed by `spira_lc::goodput` from the
//! typed events of the replayed world, published through `register_publisher` and read back
//! through `section get`.

use serde_json::Value;

use super::ranking::stack;
use super::{open, round_controller, Slice, STACKED_REPO};
use crate::replay::lifecycle;

/// One pass's read of the Lifecycle section: the stacked repo's 1 h and 24 h rate.
struct Rates {
    at: u64,
    hour: Option<f64>,
    day: Option<f64>,
}

fn value(v: &Value) -> Option<f64> {
    v["value"].as_f64()
}

pub fn the_lifecycle_sections_one_hour_rate_falls_below_its_day_rate_while_the_stacked_pointer_stands() {
    let mut r = open(Slice::Oct31);
    round_controller::register(&mut r);
    lifecycle::register(&mut r);
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let out = seen.clone();
    r.register_pass(move |store, now| {
        let (code, v) = store.call(&["section", "get", "Lifecycle/all"], None);
        if code != 0 {
            return;
        }
        let repo = &v["section"]["status"]["repos"][STACKED_REPO];
        out.borrow_mut().push(Rates { at: now / 1000, hour: value(&repo["goodput_rate_1h"]), day: value(&repo["goodput_rate_24h"]) });
    });
    r.run();
    let seen = seen.borrow();
    let s = stack();
    let hour_before = seen.iter().filter(|p| p.at >= s.from - 7200 && p.at < s.from).filter_map(|p| p.hour).fold(0.0, f64::max);
    let day_before = seen.iter().rfind(|p| p.at < s.from).and_then(|p| p.day).expect("a 24h rate before the stack");
    assert!(hour_before > day_before, "the stacked repo was landing ahead of its day's rate: {hour_before} against {day_before}");

    let settled: Vec<&Rates> = seen.iter().filter(|p| p.at >= s.from + 3600 && p.at < s.until).collect();
    assert!(settled.len() >= 120, "the stack stands for hours: {} passes", settled.len());
    for p in &settled {
        let (hour, day) = (p.hour.expect("a 1h rate"), p.day.expect("a 24h rate"));
        assert!(hour < hour_before && hour < day, "at {}: 1h rate {hour} against 24h rate {day}, {hour_before}/h before the stack", p.at);
    }
}
