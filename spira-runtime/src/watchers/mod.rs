//! The watchers that used to poll as scripts, now handlers that subscribe to events. A handler
//! here is pure: what the world looks like arrives as the outcome of an observe effect, and
//! what it has to say leaves as an `inbox.append` effect.

pub mod asks;
pub mod design_asks;
pub mod brain_dirty;
pub mod bead_sweep;
pub mod inbox_keeper;
pub mod pr_notify;
pub mod publish_backlog;
pub mod round_duty;
pub mod supersede_duty;

use runtime::event::*;
use serde::de::DeserializeOwned;

pub const INBOX_APPEND: &str = "inbox.append";
pub const CONCIERGE_WAKE: &str = "concierge.wake";

/// The body of the latest `tag` event on `stream`, or the default before the first.
pub fn carried<T: DeserializeOwned + Default>(view: &Projection, stream: &StreamKey, tag: EventKindTag) -> T {
    view.events(stream).iter().rev().find(|e| e.kind.tag == tag).and_then(|e| serde_json::from_str(&e.kind.body).ok()).unwrap_or_default()
}

/// The name of the effect that `ev` is the outcome of, when its stream requested one.
pub fn answered_effect(view: &Projection, ev: &Event) -> Option<String> {
    let cause = ev.cause.as_ref()?;
    let requested = view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)?;
    serde_json::from_str::<serde_json::Value>(&requested.kind.body).ok()?["spec"]["name"].as_str().map(str::to_string)
}

/// Whether `ev` is the outcome of the effect named `observe` that its stream requested.
pub fn answers_observe(view: &Projection, ev: &Event, observe: &str) -> bool {
    answered_effect(view, ev).is_some_and(|n| n == observe)
}

/// (year, month, day) of a count of days since 1970-01-01.
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a UTC epoch-seconds value.
pub fn iso8601(secs: u64) -> String {
    let (y, m, d) = civil((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::iso8601;

    #[test]
    fn epoch_seconds_render_as_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_793_166_343), "2026-10-28T05:45:43Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }
}
