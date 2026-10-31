//! The unpublished-backlog alarm for queue.local repos. Each tick asks the world, per repo, how
//! many commits the forge has not seen and when the first of them arrived; the decision
//! over that and the last state each repo was in is `decide`, which does no IO. Only a crossing
//! is announced, in either direction, and a tick that cannot measure says so once.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.publish-backlog";
pub const TICK: EventKindTag = "publish-backlog.tick";
pub const STATE: EventKindTag = "publish-backlog.state";
pub const OBSERVE: &str = "publish-backlog.observe";
pub const NAME: &str = "publish-backlog";

const DEADLINE_MS: u64 = 60_000;
pub const REPO_MAP_KEY: &str = "_repo_map";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishBacklog {
    pub interval_ms: u64,
    pub max_count: u64,
    pub max_age_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Read {
    Blind(String),
    /// `arrival` is when the oldest unpublished commit reached the landing ref; absent when nothing is unpublished.
    /// `forge_tip_age_secs` is set only when the forge did not answer and the count was taken against an older tip.
    Measured {
        count: u64,
        arrival: Option<u64>,
        #[serde(default)]
        forge_tip_age_secs: Option<u64>,
    },
}

/// One entry per queue.local repo, or per reason the map itself could not be read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: Vec<(String, Read)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Standing {
    Ok,
    Over,
    Blind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub standing: BTreeMap<String, Standing>,
    #[serde(default)]
    pub blind_since: BTreeMap<String, u64>,
}

impl PublishBacklog {
    pub fn decide(&self, carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, Vec<String>) {
        let mut c = carry.clone();
        let mut say = vec![];
        for (name, read) in &snap.repos {
            let prev = c.standing.get(name).copied().unwrap_or(Standing::Ok);
            let (count, arrival, stale) = match read {
                Read::Blind(why) => {
                    if prev != Standing::Blind {
                        say.push(format!("BLIND {name}: the backlog watcher cannot measure — {why}"));
                        c.standing.insert(name.clone(), Standing::Blind);
                        c.blind_since.insert(name.clone(), now);
                    }
                    continue;
                }
                Read::Measured { count, arrival, forge_tip_age_secs } => (*count, *arrival, *forge_tip_age_secs),
            };
            if prev == Standing::Blind {
                c.standing.remove(name);
                let spell = c.blind_since.remove(name).map_or(String::new(), |t| format!(" after {}s blind", now.saturating_sub(t)));
                say.push(format!("SIGHTED {name}: the backlog watcher can measure again{spell}"));
            }
            let tip = stale.map_or(String::new(), |a| format!("; forge unreachable, measured against a tip {a}s old"));
            let age = arrival.map_or(0, |t| now.saturating_sub(t));
            let over = count > self.max_count || age > self.max_age_secs;
            let was_over = prev == Standing::Over;
            if over && !was_over {
                say.push(format!("OVER {name}: {count} unpublished commit(s), oldest {age}s old (threshold {} commits / {}s) — the publish queue is falling behind{tip}", self.max_count, self.max_age_secs));
            } else if !over && was_over {
                say.push(format!("CLEAR {name}: back under threshold ({count} unpublished, oldest {age}s{tip})"));
            }
            c.standing.insert(name.clone(), if over { Standing::Over } else { Standing::Ok });
        }
        (c, say)
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("publish-backlog/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn carry(view: &Projection, stream: &StreamKey) -> Carry {
        super::carried(view, stream, STATE)
    }
}

impl Handler for PublishBacklog {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return vec![
                Command::Probe(EffectSpec { name: OBSERVE.into(), input: String::new(), deadline_ms: DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !super::answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
        let now = ev.at / 1000;
        let (next, say) = self.decide(&Self::carry(view, &ev.stream), &snap, now);
        ctx.event().set("publish_backlog.said", say.len() as u64);
        let mut cmds = vec![Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default()))];
        for m in say {
            cmds.push(Command::Effect(EffectSpec { name: INBOX_APPEND.into(), input: format!("{} [watch:{NAME}] {m}", iso8601(now)), deadline_ms: DEADLINE_MS }));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn watcher() -> PublishBacklog {
        PublishBacklog { interval_ms: 1, max_count: 50, max_age_secs: 10_800 }
    }

    fn snap(r: Read) -> Snapshot {
        Snapshot { repos: vec![("r".into(), r)] }
    }

    fn measured(count: u64, age: u64) -> Read {
        Read::Measured { count, arrival: (count > 0).then_some(NOW - age), forge_tip_age_secs: None }
    }

    #[test]
    fn under_both_thresholds_is_silent_and_a_crossing_is_announced_once_each_way() {
        let w = watcher();
        let (c, say) = w.decide(&Carry::default(), &snap(measured(50, 10_800)), NOW);
        assert!(say.is_empty(), "the thresholds are exclusive: {say:?}");
        let (c, say) = w.decide(&c, &snap(measured(51, 5)), NOW);
        assert_eq!(say, ["OVER r: 51 unpublished commit(s), oldest 5s old (threshold 50 commits / 10800s) — the publish queue is falling behind"]);
        let (c, say) = w.decide(&c, &snap(measured(60, 9_000)), NOW);
        assert!(say.is_empty(), "a backlog that stays over is not a siren: {say:?}");
        let (c, say) = w.decide(&c, &snap(measured(3, 20)), NOW);
        assert_eq!(say, ["CLEAR r: back under threshold (3 unpublished, oldest 20s)"]);
        assert!(w.decide(&c, &snap(measured(3, 20)), NOW).1.is_empty());
    }

    #[test]
    fn age_alone_crosses() {
        let (_, say) = watcher().decide(&Carry::default(), &snap(measured(1, 10_801)), NOW);
        assert_eq!(say.len(), 1);
        assert!(say[0].starts_with("OVER r: 1 unpublished commit(s), oldest 10801s old"), "{say:?}");
    }

    #[test]
    fn a_repo_that_cannot_be_measured_is_blind_once_and_sighted_once_and_forgets_it_was_over() {
        let w = watcher();
        let (c, _) = w.decide(&Carry::default(), &snap(measured(99, 1)), NOW);
        let (c, say) = w.decide(&c, &snap(Read::Blind("no reflog".into())), NOW);
        assert_eq!(say, ["BLIND r: the backlog watcher cannot measure — no reflog"]);
        assert!(w.decide(&c, &snap(Read::Blind("no reflog".into())), NOW).1.is_empty());
        let (_, say) = w.decide(&c, &snap(measured(99, 1)), NOW);
        assert_eq!(say.len(), 2, "{say:?}");
        assert_eq!(say[0], "SIGHTED r: the backlog watcher can measure again after 0s blind");
        assert!(say[1].starts_with("OVER r:"), "blindness resets the standing: {say:?}");
    }

    #[test]
    fn a_stale_forge_tip_is_named_with_its_age_in_the_line_it_qualifies() {
        let w = watcher();
        let stale = Read::Measured { count: 51, arrival: Some(NOW - 5), forge_tip_age_secs: Some(777) };
        let (c, say) = w.decide(&Carry::default(), &snap(stale), NOW);
        assert_eq!(say.len(), 1);
        assert!(say[0].ends_with("falling behind; forge unreachable, measured against a tip 777s old"), "{say:?}");
        let clear = Read::Measured { count: 1, arrival: Some(NOW - 5), forge_tip_age_secs: Some(900) };
        let (_, say) = w.decide(&c, &snap(clear), NOW);
        assert_eq!(say, ["CLEAR r: back under threshold (1 unpublished, oldest 5s; forge unreachable, measured against a tip 900s old)"]);
    }

    #[test]
    fn a_sighted_line_says_how_long_the_watcher_was_blind() {
        let w = watcher();
        let (c, _) = w.decide(&Carry::default(), &snap(Read::Blind("fetch timed out".into())), NOW);
        let (_, say) = w.decide(&c, &snap(measured(0, 0)), NOW + 125);
        assert_eq!(say, ["SIGHTED r: the backlog watcher can measure again after 125s blind"]);
    }
}
