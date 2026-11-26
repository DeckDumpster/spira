//! The watchers that used to poll as scripts, now handlers that subscribe to events. A handler
//! here is pure: what the world looks like arrives as the outcome of an observe effect, and
//! what it has to say leaves as an `inbox.append` effect.

pub mod asks;
pub mod cr_cadence;
pub mod design_asks;
pub mod halt;
pub mod brain_dirty;
pub mod claim_count;
pub mod dependency;
pub mod bead_sweep;
pub mod inbox_keeper;
pub mod margin_comments;
pub mod overrides;
pub mod pr_notify;
pub mod publish_backlog;
pub mod readiness_dependents;
pub mod reconciler_flow;
pub mod reconciler;
pub mod round_duty;
pub mod supersede_duty;
pub mod view;
pub mod wake_audit;
pub mod writer_grants;

use runtime::event::*;
use serde::de::DeserializeOwned;

pub const INBOX_APPEND: &str = "inbox.append";
pub const CONCIERGE_WAKE: &str = "concierge.wake";

/// The body of the latest `tag` event on `stream`, or the default before the first.
pub fn carried<T: DeserializeOwned + Default>(view: &Projection, stream: &StreamKey, tag: EventKindTag) -> T {
    view.events(stream).iter().rev().find(|e| e.kind.tag == tag).and_then(|e| serde_json::from_str(&e.kind.body).ok()).unwrap_or_default()
}

/// The newest `Activated` entry among the release machine records under `run_dir`, epoch seconds.
pub fn last_activation(run_dir: &std::path::Path) -> Option<u64> {
    let releases = release::machine::all(&run_dir.join("release")).ok()?;
    releases.iter().flat_map(|r| r.events.iter()).filter(|e| e.to == release::machine::ReleaseState::Activated).map(|e| e.at).max()
}

/// A release activation whose store restart may still be under way.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Activation {
    pub sha: String,
    pub since: u64,
}

/// The newest release record whose newest entry is `Published` or `Activated`, when that entry is at most `bound_s` before `now`.
pub fn activation_window(run_dir: &std::path::Path, now: u64, bound_s: u64) -> Option<Activation> {
    use release::machine::ReleaseState::{Activated, Published};
    let releases = release::machine::all(&run_dir.join("release")).ok()?;
    let (sha, last) = releases.iter().filter_map(|r| r.events.last().map(|e| (&r.sha, e))).filter(|(_, e)| matches!(e.to, Published | Activated)).max_by_key(|(_, e)| e.at)?;
    (now.saturating_sub(last.at) <= bound_s).then(|| Activation { sha: sha.clone(), since: last.at })
}

/// `activation_window` at the present, under the configured bound; `None` when the bound does not resolve.
pub fn current_activation(run_dir: &std::path::Path) -> Option<Activation> {
    let bound: u64 = spira_config::process::cfg_parse("SPIRA_ACTIVATION_STORE_BOUND").ok()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    activation_window(run_dir, now, bound)
}

/// The non-paging line a watcher writes for an unreadable store inside an activation's window.
pub fn restart_wait(a: &Activation, bound_s: u64, why: &str) -> String {
    format!("WAIT BLIND: store restarting for activation of {} since {} (bound {bound_s}s): {why}", a.sha.chars().take(12).collect::<String>(), iso8601(a.since))
}

/// The non-paging line a watcher writes for an unreadable store held stopped by a live maintenance.
pub fn maintenance_wait(m: &spira_world::freeze::Maintenance) -> String {
    format!("WAIT BLIND: store stopped for maintenance by {} since {}: {}", m.owner, iso8601(m.since), m.why)
}

/// The one page for a maintenance whose owning process is gone with the hold still on.
pub fn maintenance_orphaned(m: &spira_world::freeze::Maintenance) -> String {
    format!("P0 BLIND: store maintenance by {} since {} is orphaned (pid {} gone); run world thaw-store", m.owner, iso8601(m.since), m.pid)
}

/// The name of the effect that `ev` is the outcome of, when its stream requested one.
pub fn answered_effect(view: &Projection, ev: &Event) -> Option<String> {
    let cause = ev.cause.as_ref()?;
    let requested = view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)?;
    serde_json::from_str::<serde_json::Value>(&requested.kind.body).ok()?["spec"]["name"].as_str().map(str::to_string)
}

fn answered_request<'a>(view: &'a Projection, ev: &Event) -> Option<&'a Event> {
    let cause = ev.cause.as_ref()?;
    view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)
}

/// The `at` (ms) of the request that `ev` is the outcome of, when its stream requested one.
pub fn answered_request_at(view: &Projection, ev: &Event) -> Option<u64> {
    answered_request(view, ev).map(|e| e.at)
}

/// The spec of the effect that `ev` is the outcome of, when its stream requested one.
pub fn answered_spec(view: &Projection, ev: &Event) -> Option<EffectSpec> {
    let requested = answered_request(view, ev)?;
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
    use super::{activation_window, iso8601, last_activation};
    use release::machine::{enter, ReleaseState};

    #[test_id(tnxw39b)]
    #[test]
    fn the_last_activation_is_the_activated_record_under_run_dir_and_nothing_else() {
        let t = testkit::TempDir::new("last-activation");
        let state = t.path().join("release");
        assert_eq!(last_activation(t.path()), None, "no records: no activation");
        enter(&state, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", ReleaseState::Published, "published").unwrap();
        assert_eq!(last_activation(t.path()), None, "a published release is not an activation");
        enter(&state, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", ReleaseState::Activated, "current switched").unwrap();
        assert!(last_activation(t.path()).is_some(), "the activated record is read");
    }

    fn window_with(last: ReleaseState) -> (testkit::TempDir, u64) {
        let t = testkit::TempDir::new("activation-window");
        let state = t.path().join("release");
        enter(&state, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", last, "r").unwrap();
        let at = release::machine::all(&state).unwrap()[0].events.last().unwrap().at;
        (t, at)
    }

    #[test_id(t939xhq)]
    #[test]
    fn a_published_record_is_a_window_while_within_the_bound() {
        let (t, at) = window_with(ReleaseState::Published);
        let w = activation_window(t.path(), at + 10, 90).expect("inside the bound");
        assert_eq!((w.sha.as_str(), w.since), ("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", at));
        assert_eq!(activation_window(t.path(), at + 100, 90), None, "past the bound");
    }

    #[test_id(t64rl03)]
    #[test]
    fn an_activated_record_is_a_window_and_a_rolled_back_one_is_not() {
        let (t, at) = window_with(ReleaseState::Activated);
        assert!(activation_window(t.path(), at + 10, 90).is_some());
        let (t, at) = window_with(ReleaseState::RolledBack);
        assert_eq!(activation_window(t.path(), at + 10, 90), None);
    }

    #[test_id(ti53on6)]
    #[test]
    fn epoch_seconds_render_as_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_793_166_343), "2026-10-28T05:45:43Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }
}
