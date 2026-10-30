//! BRAIN DIRTY for the Concierge inbox: a path under the wiki checkout left uncommitted past the
//! age limit. Each tick asks the world for the wiki's dirty paths with their mtimes; the decision
//! over that and the keys already said is `decide`, which does no IO.

use std::collections::BTreeSet;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.brain-dirty";
pub const TICK: EventKindTag = "brain-dirty.tick";
pub const STATE: EventKindTag = "brain-dirty.state";
pub const OBSERVE: &str = "brain-dirty.observe";
pub const NAME: &str = "brain-dirty";

const DEADLINE_MS: u64 = 10_000;
const NOT_REPO: &str = "notrepo";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrainDirty {
    pub interval_ms: u64,
    pub age_secs: u64,
}

/// `not_repo` is the wiki setting naming a directory git does not own; `dirty` is (path, mtime
/// seconds) of every changed path that still exists. An unset wiki is an empty snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub wiki: String,
    pub not_repo: bool,
    pub dirty: Vec<(String, u64)>,
}

/// Carried between ticks: every key already said, which is never forgotten.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub said: BTreeSet<String>,
}

/// The paths of `git status --porcelain -z`: `XY path\0`, with a rename or copy followed by its
/// origin record, which is not a path of its own.
pub fn porcelain_paths(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut recs = raw.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut paths = vec![];
    while let Some(rec) = recs.next() {
        if rec.len() < 4 {
            continue;
        }
        if matches!(rec[0], b'R' | b'C') {
            recs.next();
        }
        paths.push(rec[3..].to_vec());
    }
    paths
}

pub fn decide(cfg: &BrainDirty, carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, Vec<String>) {
    let mut c = carry.clone();
    let mut say = vec![];
    let mut once = |key: String, msg: String| {
        if c.said.insert(key) {
            say.push(msg);
        }
    };
    if snap.not_repo {
        once(NOT_REPO.into(), format!("BRAIN DIRTY: SPIRA_WIKI ({}) is not a git repository; nothing it holds can be committed", snap.wiki));
    }
    for (path, mtime) in &snap.dirty {
        let age = now.saturating_sub(*mtime);
        if age > cfg.age_secs {
            once(format!("{path}@{mtime}"), format!("BRAIN DIRTY: {path} has been uncommitted for {} min; commit it by explicit path (git -C {} add -- {path})", age / 60, snap.wiki));
        }
    }
    (c, say)
}

impl BrainDirty {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("brain-dirty/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for BrainDirty {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
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
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body).map_err(drop).and_then(|s| if ev.kind.tag == EFFECT_SUCCEEDED { Ok(s) } else { Err(()) }) else {
            ctx.event().set("brain_dirty.skipped", ev.kind.tag.as_str());
            return vec![];
        };
        let now = ev.at / 1000;
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, say) = decide(self, &carry, &snap, now);
        ctx.event().set("brain_dirty.said", say.len() as u64);
        let mut cmds: Vec<Command> = say.into_iter().map(|m| Command::Effect(EffectSpec { name: INBOX_APPEND.into(), input: format!("{} [watch:{NAME}] {m}", iso8601(now)), deadline_ms: DEADLINE_MS })).collect();
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BrainDirty {
        BrainDirty { interval_ms: 1000, age_secs: 1800 }
    }

    fn snap(dirty: &[(&str, u64)]) -> Snapshot {
        Snapshot { wiki: "/w".into(), not_repo: false, dirty: dirty.iter().map(|(p, m)| (p.to_string(), *m)).collect() }
    }

    #[test]
    fn porcelain_records_yield_paths_and_a_rename_origin_is_not_one() {
        let raw = b" M a b.md\0?? new.md\0R  to.md\0from.md\0C  cp.md\0orig.md\0A  last.md\0";
        let got: Vec<String> = porcelain_paths(raw).into_iter().map(|p| String::from_utf8(p).unwrap()).collect();
        assert_eq!(got, ["a b.md", "new.md", "to.md", "cp.md", "last.md"]);
        assert!(porcelain_paths(b"").is_empty());
    }

    #[test]
    fn a_path_older_than_the_limit_is_said_once_per_mtime() {
        let (c, say) = decide(&cfg(), &Carry::default(), &snap(&[("old.md", 1000), ("fresh.md", 9000)]), 10_000);
        assert_eq!(say, ["BRAIN DIRTY: old.md has been uncommitted for 150 min; commit it by explicit path (git -C /w add -- old.md)"]);
        assert!(decide(&cfg(), &c, &snap(&[("old.md", 1000)]), 10_100).1.is_empty());
        assert_eq!(decide(&cfg(), &c, &snap(&[("old.md", 2000)]), 10_100).1.len(), 1, "an edit is a new offence");
    }

    #[test]
    fn a_path_exactly_at_the_limit_is_not_yet_stale() {
        assert!(decide(&cfg(), &Carry::default(), &snap(&[("p.md", 8200)]), 10_000).1.is_empty());
        assert_eq!(decide(&cfg(), &Carry::default(), &snap(&[("p.md", 8199)]), 10_000).1.len(), 1);
    }

    #[test]
    fn a_failed_or_timed_out_observe_says_nothing_and_marks_nothing() {
        use runtime::ports::VirtualClock;
        use runtime::{Engine, Executor, MemJournal, Outcome};
        for out in [Outcome::Failed("git status exited Some(128)".into()), Outcome::TimedOut] {
            let mut e = Engine::open(MemJournal::new(), std::sync::Arc::new(VirtualClock::new(1_800_000_000_000)), vec![cfg().registration()], 0).unwrap();
            let (id, stream, kind) = BrainDirty::boot();
            e.submit(id, stream, kind).unwrap();
            e.run_until_idle().unwrap();
            let observe = e.pending_effects().remove(0);
            e.complete(&observe.key, out).unwrap();
            e.run_until_idle().unwrap();
            assert!(e.pending_effects().iter().all(|p| p.spec.name != INBOX_APPEND), "an observe that read nothing must not speak");
        }
    }

    #[test]
    fn a_wiki_that_is_not_a_repository_is_said_once() {
        let s = Snapshot { not_repo: true, ..snap(&[]) };
        let (c, say) = decide(&cfg(), &Carry::default(), &s, 10_000);
        assert_eq!(say, ["BRAIN DIRTY: SPIRA_WIKI (/w) is not a git repository; nothing it holds can be committed"]);
        assert!(decide(&cfg(), &c, &s, 10_600).1.is_empty());
    }
}
