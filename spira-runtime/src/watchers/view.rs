//! The cockpit view follower: keeps the cockpit showing `hunk` while a review that has not been
//! dismissed is live, and `brain` otherwise. Each tick asks the world which hunk sessions are
//! live and what has been dismissed; the decision over that is `decide`, which does no IO. The
//! view flips only on a transition, and the first reading after a start only seeds.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answers_observe, carried};

pub const STREAM: &str = "watch.view";
pub const TICK: EventKindTag = "view.tick";
pub const STATE: EventKindTag = "view.state";
pub const OBSERVE: &str = "view.observe";
pub const FLIP: &str = "view.flip";
pub const FORGET: &str = "view.forget";
pub const NAME: &str = "view";

pub const SEED: &str = "seed";
const OBSERVE_DEADLINE_MS: u64 = 5_000;
const ACT_DEADLINE_MS: u64 = 10_000;
pub const HUNK: &str = "hunk";
pub const BRAIN: &str = "brain";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub interval_ms: u64,
    pub forget_secs: u64,
    pub enabled: bool,
}

/// What the world said: hunk's live sessions (`None` when no daemon answered, with why) and the
/// dismissal files. `seed` marks the first reading after the service started.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub seed: bool,
    pub live: Option<Vec<String>>,
    pub error: Option<String>,
    pub dismissed: Dismissed,
}

/// Session ids dismissed from the view, and since when each dismissed id has been absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dismissed {
    pub ids: Vec<String>,
    pub absent: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub want: Option<String>,
    pub absent: BTreeMap<String, u64>,
}

/// The dismissal files as they should now read; `rewrite` is false when only the absence marks
/// change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forget {
    pub rewrite: bool,
    pub dismissed: Dismissed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub carry: Carry,
    pub flip: Option<String>,
    pub forget: Option<Forget>,
}

impl View {
    /// A dismissal is forgotten only once its session has been absent for `forget_secs` straight:
    /// a hunk daemon restart looks like an ended session until the TUI registers again.
    pub fn decide(&self, carry: &Carry, live: Option<&[String]>, dismissed: &Dismissed, now: u64) -> Decision {
        let Some(live) = live else { return Decision { carry: carry.clone(), ..Decision::default() } };
        let (kept, marks) = if dismissed.ids.is_empty() {
            (vec![], BTreeMap::new())
        } else {
            let mut marks = BTreeMap::new();
            let kept = dismissed
                .ids
                .iter()
                .filter(|id| {
                    if live.contains(*id) {
                        return true;
                    }
                    let since = carry.absent.get(*id).or_else(|| dismissed.absent.get(*id)).copied().unwrap_or(now);
                    let keep = now.saturating_sub(since) < self.forget_secs;
                    if keep {
                        marks.insert((*id).clone(), since);
                    }
                    keep
                })
                .cloned()
                .collect::<Vec<_>>();
            (kept, marks)
        };
        let rewrite = !dismissed.ids.is_empty() && kept != dismissed.ids;
        let forget = (rewrite || marks != dismissed.absent).then(|| Forget { rewrite, dismissed: Dismissed { ids: kept.clone(), absent: marks.clone() } });
        let want = if live.iter().any(|id| !kept.contains(id)) { HUNK } else { BRAIN };
        let flip = carry.want.as_deref().filter(|last| *last != want).map(|_| want.to_string());
        Decision { carry: Carry { want: Some(want.to_string()), absent: marks }, flip, forget }
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("view/boot", STREAM, EventKind::new(TICK, SEED))
    }
}

impl Handler for View {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM || !self.enabled {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return vec![
                Command::Probe(EffectSpec { name: OBSERVE.into(), input: ev.kind.body.clone(), deadline_ms: OBSERVE_DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
        if let Some(why) = &snap.error {
            ctx.event().set("view.hunk_error", why.as_str());
        }
        let stored = carried::<Carry>(view, &ev.stream, STATE);
        let seen = Carry { want: stored.want.clone().filter(|_| !snap.seed), ..stored.clone() };
        let decision = self.decide(&seen, snap.live.as_deref(), &snap.dismissed, ev.at / 1000);
        let mut cmds = vec![];
        if decision.carry != stored {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&decision.carry).unwrap_or_default())));
        }
        if let Some(forget) = &decision.forget {
            cmds.push(Command::Effect(EffectSpec { name: FORGET.into(), input: serde_json::to_string(forget).unwrap_or_default(), deadline_ms: ACT_DEADLINE_MS }));
        }
        if let Some(to) = decision.flip {
            ctx.event().set("view.flip", format!("{} -> {to}; live sessions: {}", seen.want.as_deref().unwrap_or(""), snap.live.as_deref().unwrap_or_default().join(" ")));
            cmds.push(Command::Effect(EffectSpec { name: FLIP.into(), input: to, deadline_ms: ACT_DEADLINE_MS }));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const T0: u64 = 1_800_000_000;

    fn cfg() -> View {
        View { interval_ms: 3000, forget_secs: DAY, enabled: true }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn dismissed(v: &[&str]) -> Dismissed {
        Dismissed { ids: ids(v), absent: BTreeMap::new() }
    }

    fn after(want: &str) -> Carry {
        Carry { want: Some(want.into()), absent: BTreeMap::new() }
    }

    #[test]
    fn a_live_session_nobody_dismissed_wants_hunk_and_any_other_state_wants_brain() {
        let want = |live: &[&str], d: &[&str]| cfg().decide(&Carry::default(), Some(&ids(live)), &dismissed(d), T0).carry.want.unwrap();
        assert_eq!(want(&["a"], &[]), HUNK);
        assert_eq!(want(&["a", "b"], &["a"]), HUNK, "one undismissed session is a review");
        assert_eq!(want(&["a"], &["a"]), BRAIN);
        assert_eq!(want(&[], &["a"]), BRAIN);
    }

    #[test]
    fn the_first_reading_seeds_and_a_change_after_it_flips_once() {
        let first = cfg().decide(&Carry::default(), Some(&ids(&["a"])), &dismissed(&[]), T0);
        assert_eq!((first.flip, first.carry.want.as_deref()), (None, Some(HUNK)));
        let again = cfg().decide(&first.carry, Some(&ids(&["a"])), &dismissed(&[]), T0 + 3);
        assert_eq!(again.flip, None);
        let ended = cfg().decide(&again.carry, Some(&ids(&[])), &dismissed(&[]), T0 + 6);
        assert_eq!(ended.flip.as_deref(), Some(BRAIN));
    }

    #[test]
    fn a_hunk_that_did_not_answer_changes_nothing_and_forgets_nothing() {
        let carry = Carry { want: Some(BRAIN.into()), absent: BTreeMap::from([("a".to_string(), T0 - DAY - 1)]) };
        let d = cfg().decide(&carry, None, &dismissed(&["a"]), T0);
        assert_eq!((d.carry, d.flip, d.forget), (carry, None, None));
    }

    #[test]
    fn a_dismissal_is_forgotten_only_after_its_session_stays_absent_for_the_whole_window() {
        let d0 = Dismissed { ids: ids(&["a"]), absent: BTreeMap::new() };
        let first = cfg().decide(&after(BRAIN), Some(&[]), &d0, T0);
        assert_eq!(first.forget, Some(Forget { rewrite: false, dismissed: Dismissed { ids: ids(&["a"]), absent: BTreeMap::from([("a".to_string(), T0)]) } }));
        let marked = first.forget.unwrap().dismissed;
        let near = cfg().decide(&first.carry, Some(&[]), &marked, T0 + DAY - 1);
        assert_eq!(near.forget, None, "still inside the window, and nothing changed since the marks were written");
        let past = cfg().decide(&near.carry, Some(&[]), &marked, T0 + DAY);
        assert_eq!(past.forget, Some(Forget { rewrite: true, dismissed: Dismissed::default() }));
    }

    #[test]
    fn a_session_that_comes_back_loses_its_absence_clock() {
        let marked = Dismissed { ids: ids(&["a"]), absent: BTreeMap::from([("a".to_string(), T0)]) };
        let back = cfg().decide(&after(BRAIN), Some(&ids(&["a"])), &marked, T0 + 100);
        assert_eq!(back.forget, Some(Forget { rewrite: false, dismissed: dismissed(&["a"]) }));
        assert!(back.carry.absent.is_empty());
    }

    #[test]
    fn with_nothing_dismissed_only_a_stale_absence_file_is_cleared() {
        let stale = Dismissed { ids: vec![], absent: BTreeMap::from([("a".to_string(), T0)]) };
        let d = cfg().decide(&after(BRAIN), Some(&[]), &stale, T0 + 1);
        assert_eq!(d.forget, Some(Forget { rewrite: false, dismissed: Dismissed::default() }));
        assert_eq!(cfg().decide(&after(BRAIN), Some(&[]), &Dismissed::default(), T0).forget, None);
    }
}
