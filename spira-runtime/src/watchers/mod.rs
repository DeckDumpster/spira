//! The watchers that used to poll as scripts, now handlers that subscribe to events. A handler
//! here is pure: what the world looks like arrives as the outcome of an observe effect, and
//! what it has to say leaves as an `inbox.append` effect.

pub mod asks;
pub mod design_asks;
pub mod halt;
pub mod brain_dirty;
pub mod bead_sweep;
pub mod inbox_keeper;
pub mod overrides;
pub mod pr_notify;
pub mod publish_backlog;
pub mod readiness_dependents;
pub mod reconciler_flow;
pub mod reconciler;
pub mod round_duty;
pub mod supersede_duty;
pub mod view;

use runtime::event::*;
use serde::de::DeserializeOwned;

pub const INBOX_APPEND: &str = "inbox.append";
pub const CONCIERGE_WAKE: &str = "concierge.wake";

/// The body of the latest `tag` event on `stream`, or the default before the first.
pub fn carried<T: DeserializeOwned + Default>(view: &Projection, stream: &StreamKey, tag: EventKindTag) -> T {
    view.events(stream).iter().rev().find(|e| e.kind.tag == tag).and_then(|e| serde_json::from_str(&e.kind.body).ok()).unwrap_or_default()
}

/// How long after an activation restarts the store server a watcher's unreadable store is
/// read again before it is called BLIND: the server's first reads pay for its cold build.
pub const ACTIVATION_RETRY_S: u64 = 60;

/// Whether an activation at `activated_at` (epoch seconds) is no more than `ACTIVATION_RETRY_S` before `now_s`.
pub fn just_activated(activated_at: Option<u64>, now_s: u64) -> bool {
    activated_at.is_some_and(|at| at <= now_s && now_s - at <= ACTIVATION_RETRY_S)
}

/// The newest `Activated` entry among the release machine records under `run_dir`, epoch seconds.
pub fn last_activation(run_dir: &std::path::Path) -> Option<u64> {
    let releases = release::machine::all(&run_dir.join("release")).ok()?;
    releases.iter().flat_map(|r| r.events.iter()).filter(|e| e.to == release::machine::ReleaseState::Activated).map(|e| e.at).max()
}

/// The name of the effect that `ev` is the outcome of, when its stream requested one.
pub fn answered_effect(view: &Projection, ev: &Event) -> Option<String> {
    let cause = ev.cause.as_ref()?;
    let requested = view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)?;
    serde_json::from_str::<serde_json::Value>(&requested.kind.body).ok()?["spec"]["name"].as_str().map(str::to_string)
}

/// The spec of the effect that `ev` is the outcome of, when its stream requested one.
pub fn answered_spec(view: &Projection, ev: &Event) -> Option<EffectSpec> {
    let cause = ev.cause.as_ref()?;
    let requested = view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)?;
    serde_json::from_value(serde_json::from_str::<serde_json::Value>(&requested.kind.body).ok()?["spec"].clone()).ok()
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
    reconciler_engine::clock::iso_of(secs)
}

#[cfg(test)]
mod tests {
    use super::{iso8601, just_activated, last_activation};
    use release::machine::{enter, ReleaseState};

    #[test]
    fn the_last_activation_is_the_activated_record_under_run_dir_and_nothing_else() {
        let t = testkit::TempDir::new("last-activation");
        let state = t.path().join("release");
        assert_eq!(last_activation(t.path()), None, "no records: no activation");
        enter(&state, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", ReleaseState::Published, "published").unwrap();
        assert_eq!(last_activation(t.path()), None, "a published release is not an activation");
        enter(&state, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", ReleaseState::Activated, "current switched").unwrap();
        let at = last_activation(t.path()).expect("the activated record is read");
        assert!(just_activated(Some(at), at + 60) && !just_activated(Some(at), at + 61) && !just_activated(Some(at), at.saturating_sub(1)));
    }

    #[test]
    fn epoch_seconds_render_as_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_793_166_343), "2026-10-28T05:45:43Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }
}
