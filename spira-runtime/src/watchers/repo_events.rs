//! Finished workflow runs worth the Concierge's attention, for every repository registered:
//! acceptance runs whatever their result, and runs on `main`/`master` that did not succeed. Each
//! tick asks the world for the latest runs of every forge; the decision over snapshot and carried
//! keys is `decide`, which does no IO. PR transitions are `pr-notify`'s, not said here.

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.repo-events";
pub const TICK: EventKindTag = "repo-events.tick";
pub const STATE: EventKindTag = "repo-events.state";
pub const OBSERVE: &str = "repo-events.observe";
pub const NAME: &str = "repo-events";

/// The newest keys kept: older runs have left the window the forge lists (30 per repository).
/// The carry rides one journal event, so the whole list must fit `kind_body`.
pub const KEEP: usize = 600;
const TITLE_CHARS: usize = 80;
const OBSERVE_DEADLINE_MS: u64 = 60_000;
const DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoEvents {
    pub interval_ms: u64,
}

/// One completed run as the forge listed it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: u64,
    pub workflow: String,
    pub conclusion: String,
    pub branch: String,
    pub event: String,
    pub title: String,
}

/// `seed` is the dedup keys a previous owner of this duty left behind; it is only asked for while
/// the carry has not yet seen a forge answer. A repository the forge could not answer for carries its reason.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub seed: Vec<String>,
    pub repos: Vec<(String, Result<Vec<Run>, String>)>,
}

/// Carried between ticks as the body of the latest `repo-events.state` event, and the question
/// each observe asks: `runs` are the `run|<slug>|<id>|<conclusion>` keys already settled, oldest
/// first; `seeded` that a forge has answered once; `failed` the forge failures already said.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub seeded: bool,
    pub runs: Vec<String>,
    pub failed: Vec<String>,
}

pub fn key(slug: &str, r: &Run) -> String {
    format!("run|{slug}|{}|{}", r.id, r.conclusion)
}

/// A title as the old script printed it: cut to 80 characters, with tab, newline and the
/// backslash written out as `jq @tsv` wrote them.
pub fn title_of(raw: &str) -> String {
    raw.chars().take(TITLE_CHARS).collect::<String>().replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn line(slug: &str, r: &Run) -> Option<String> {
    let concl = r.conclusion.to_uppercase();
    if r.workflow.starts_with("Acceptance") {
        return Some(format!("{slug} acceptance run {} {concl} ({}) — {}", r.id, r.event, r.title));
    }
    let on_trunk = r.branch == "main" || r.branch == "master";
    (on_trunk && !matches!(r.conclusion.as_str(), "success" | "skipped")).then(|| format!("{slug} {} on {} {concl} (run {}) — {}", r.workflow, r.branch, r.id, r.title))
}

/// The lines a snapshot calls for, and the state to carry into the next tick. With nothing carried
/// and nothing seeded the runs on the forge are adopted silently: announcing a repository's whole
/// history would wake the pane for nothing.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<String>) {
    let silent = !carry.seeded && snap.seed.is_empty();
    let mut next = carry.clone();
    if !carry.seeded {
        next.runs = snap.seed.clone();
    }
    let mut say = vec![];
    let mut failed = vec![];
    for (slug, found) in &snap.repos {
        match found {
            Err(why) => {
                let m = format!("REPO-EVENTS FAILED: {slug}: {why}; its finished runs cannot be seen");
                if !carry.failed.contains(&m) {
                    say.push(m.clone());
                }
                failed.push(m);
            }
            Ok(runs) => {
                next.seeded = true;
                let fresh: Vec<&Run> = runs.iter().filter(|r| !next.runs.contains(&key(slug, r))).collect();
                for r in &fresh {
                    if !silent {
                        say.extend(line(slug, r));
                    }
                }
                next.runs.extend(fresh.iter().rev().map(|r| key(slug, r)));
            }
        }
    }
    next.runs.drain(..next.runs.len().saturating_sub(KEEP));
    next.failed = failed;
    (next, say)
}

impl RepoEvents {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("repo-events/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for RepoEvents {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let input = serde_json::to_string(&carried::<Carry>(view, &ev.stream, STATE)).unwrap_or_default();
            return vec![
                Command::Probe(EffectSpec { name: OBSERVE.into(), input, deadline_ms: OBSERVE_DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
        let now = ev.at / 1000;
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, say) = decide(&carry, &snap);
        ctx.event().set("repo_events.said", say.len() as u64);
        let mut cmds = vec![];
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        for m in say {
            cmds.push(Command::Effect(EffectSpec { name: INBOX_APPEND.into(), input: format!("{} [watch:{NAME}] {m}", iso8601(now)), deadline_ms: DEADLINE_MS }));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `runtime_event.kind_body` is a MySQL TEXT column: 65,535 bytes.
    const KIND_BODY_MAX: usize = 65_535;

    #[test]
    fn a_full_carry_fits_the_journal_column() {
        let slug = "DeckDumpster/remarkable-pipeline-extended";
        let runs = (0..KEEP as u64).map(|i| format!("run|{slug}|{}|action_required", 90_000_000_000 + i)).collect();
        let carry = Carry { seeded: true, runs, failed: vec![] };
        let body = serde_json::to_string(&carry).unwrap();
        assert!(body.len() < KIND_BODY_MAX, "a full carry is {} bytes; kind_body holds {KIND_BODY_MAX}", body.len());
    }

    fn run(id: u64, wf: &str, concl: &str, br: &str) -> Run {
        Run { id, workflow: wf.into(), conclusion: concl.into(), branch: br.into(), event: "push".into(), title: format!("t{id}") }
    }

    fn snap(runs: Vec<Run>) -> Snapshot {
        Snapshot { seed: vec![], repos: vec![("o/r".into(), Ok(runs))] }
    }

    #[test]
    fn a_first_start_with_nothing_seeded_adopts_the_forge_silently() {
        let (c, say) = decide(&Carry::default(), &snap(vec![run(1, "CI", "failure", "main")]));
        assert_eq!((c.runs, say), (vec!["run|o/r|1|failure".to_string()], vec![]));
    }

    #[test]
    fn nothing_adopted_yet_stays_silent_until_a_forge_answers() {
        let down = Snapshot { seed: vec![], repos: vec![("o/r".into(), Err("x".into()))] };
        let (c, _) = decide(&Carry::default(), &down);
        assert!(!c.seeded);
        let (c, _) = decide(&c, &snap(vec![]));
        assert!(c.seeded);
        assert_eq!(decide(&c, &snap(vec![run(1, "CI", "failure", "main")])).1.len(), 1, "once adopted, an empty forge is not a first start");
    }

    #[test]
    fn a_seeded_start_announces_what_the_seed_lacks_and_not_what_it_has() {
        let mut s = snap(vec![run(2, "CI", "failure", "main"), run(1, "CI", "failure", "main")]);
        s.seed = vec!["run|o/r|1|failure".into(), "pr|o/r|3|OPEN|none".into()];
        let (c, say) = decide(&Carry::default(), &s);
        assert_eq!(say, ["o/r CI on main FAILURE (run 2) — t2"]);
        assert_eq!(c.runs, ["run|o/r|1|failure", "pr|o/r|3|OPEN|none", "run|o/r|2|failure"]);
    }

    #[test]
    fn the_carry_keeps_only_the_newest_keys() {
        let carry = Carry { seeded: true, runs: (0..KEEP as u64).map(|i| format!("run|o/r|{i}|success")).collect(), failed: vec![] };
        let (c, _) = decide(&carry, &snap(vec![run(9_999, "CI", "success", "main")]));
        assert_eq!((c.runs.len(), c.runs[0].as_str(), c.runs.last().unwrap().as_str()), (KEEP, "run|o/r|1|success", "run|o/r|9999|success"));
    }

    #[test]
    fn a_full_carry_drops_its_oldest_keys_not_the_newest_of_a_batch() {
        let carry = Carry { seeded: true, runs: (0..KEEP as u64).map(|i| format!("run|o/r|{i}|success")).collect(), failed: vec![] };
        let (c, _) = decide(&carry, &snap(vec![run(20_002, "CI", "success", "main"), run(20_001, "CI", "success", "main")]));
        assert_eq!(c.runs.len(), KEEP);
        assert_eq!(&c.runs[KEEP - 2..], ["run|o/r|20001|success", "run|o/r|20002|success"]);
    }

    #[test]
    fn a_failure_is_said_once_while_it_lasts_and_again_after_it_clears() {
        let down = Snapshot { seed: vec![], repos: vec![("o/r".into(), Err("http://f: status code 503".into()))] };
        let (c, say) = decide(&Carry::default(), &down);
        assert_eq!(say, ["REPO-EVENTS FAILED: o/r: http://f: status code 503; its finished runs cannot be seen"]);
        let (c, say) = decide(&c, &down);
        assert!(say.is_empty());
        let (c, _) = decide(&c, &snap(vec![]));
        assert!(c.failed.is_empty());
        assert_eq!(decide(&c, &down).1.len(), 1);
    }

    #[test]
    fn titles_are_cut_and_escaped_like_the_script_wrote_them() {
        assert_eq!(title_of(&format!("a\tb\n{}", "x".repeat(100))).chars().count(), 80 + 2);
        assert_eq!(title_of("a\\b"), "a\\\\b");
    }
}
