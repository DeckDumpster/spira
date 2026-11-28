//! # watch.ami_stale — contract
//!
//! watchd row `round-ami-stale` (`spira/watchers:158`), target `@SPIRA_HOME@/round-ami-stale.sh watch --interval 300`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! ## Tick
//!
//! - One observe at boot. Every release activation is a boot, and only a new release changes the recipe hash. A tick only probes; the answer decides the next one.
//! - EC2 spill answering `Stale` or `Unreadable`, or an observe that failed, timed out or answered no snapshot, arms the next tick 300 s after the answer. Only an image build ends staleness, and nothing local signals it.
//! - `Fresh`, or spill not on EC2, arms nothing until the next boot. An image deregistered outside Spira is refused by the EC2 round at launch.
//!
//! ## Observe
//!
//! `watch.ami_stale.observe` returns:
//!
//! ```text
//! Snapshot { spill: String, verdict: Fresh | Stale { line } | Unreadable { rc, out } }
//! ```
//!
//! - `spill` from `SPIRA_ROUND_VM_SPILL`; anything but `ec2`, `ec2-always`, `ec2-only` → idle (`round-ami-stale.sh:290`).
//! - `verdict` from round-vm's own decision, not re-derived: today the child `timeout 60 round-vm ami-check` (`:292`) → `round-vm/src/cli.rs:269` → `Ec2::ami_check` (`round-vm/src/ec2.rs:328`, `ami_staleness(images, recipe_sha)`). In-process this becomes a library call into `round-vm`'s `ami_check`; the AWS image listing stays the external edge.
//! - Exit 0 → `Fresh` (`:294`); exit 1 with a last line → `Stale{line}`; any other exit or empty output → `Unreadable` with the line composed at `:296` ("round AMI check could not read the images: 'round-vm ami-check' exited <rc>: <out[..400]>").
//!
//! ## Decide
//!
//! - Carry `watch.ami_stale.state` = `{ said: Option<String>, published }`; `said` is today `run/watchd/round-ami-stale.said` (`:280`), which holds the last line said (`:300`), removed on fresh (`:294`).
//! - `Fresh` → `said = None`, nothing. `Stale|Unreadable` with `line == said` → nothing. Otherwise one inbox line, `said = Some(line)`.
//! - So a *different* stale line re-announces, and an unreadable listing is never read as fresh (law-absence-needs-a-positive-control).
//! - Fixtures: none in the release (unverified: no `test-round-ami-stale*.sh`). New: spill off; fresh; stale once; same stale twice; stale→different stale; unreadable; fresh clears then stale again.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.ami_stale.observe` | host (aws edge) | 60 s (today's `timeout 60`, `:292`) | (probe) | spill not ec2 → returns `spill` only, no AWS call |
//! | `inbox.append` (shared) | host | 10 s | `ami_stale/<fnv1a(line)>/<state event id>` | — |
//! | `watch.publish` (shared) | host | 10 s | `watch/round-ami-stale/<pass ms>/<fnv1a(input)>` | — |
//!
//! `watch/round-ami-stale/all` holds the line while the verdict is stale or unreadable, and is cleared on fresh or spill off. The observe is asked with what the carry last published, which a restarted process publishes again.
//!
//! Inbox line, exactly as today (`spira/round-ami-stale.sh:285`, text `:299`):
//!
//! ```text
//! <YYYY-MM-DDTHH:MM:SSZ> [watch:round-ami-stale] <line>; EC2 rounds refuse the image until the Round AMI workflow builds one
//! ```
//!
//! Today the line goes both to stdout → `run/watchd/round-ami-stale.log` (`:283`) and directly to `SPIRA_CONCIERGE_INBOX` (`:284-285`). Readers of `run/watchd/round-ami-stale.log`: none by name in the release or `run/concierge-notes`; only watchd's generic walkers (`notify`, `drain`, `peek`, `tail`); no cursor file exists. The log copy is dropped.
//!
//! Health (`round-ami-stale.sh health`, `:303-311`) → S13 `scheduler status` for `watch.ami_stale` (last fired, lag, last outcome — the outcome now distinguishes `Unreadable`, which the health file never did).
//!
//! ## Silence
//!
//! Spill off, or `Fresh` with `said=None`, or the same line as `said`: probe only, no `Emit`, no effect. After spill off or `Fresh` there is no further probe, so no AWS call, until the next boot.
//!
//! ## Replaces
//!
//! - `spira-watch-round-ami-stale-prod.service`; row `spira/watchers:158`.
//! - Children: `bash lib.sh` (`:277`), `timeout` + `round-vm ami-check` (`:292`), `grep`, `tail`, `tr`, `cut`.
//! - Files retired: `run/watchd/round-ami-stale.{said,health,log,restarts}`.
//! - Config pinned: `SPIRA_ROUND_VM_SPILL` (`spira/conf.d.index:3539`), plus whatever `round-vm`'s `Config::load` needs for `ami_check` (region, recipe sha) (unverified list).
//!
//! ## Reverse
//!
//! Release rollback restores unit and row; `Observe` mode shadows `inbox.append`.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use crate::tasks::d::pattern;
use crate::watch_topics::{self, Published};
use crate::watchers::{answers_observe, carried, INBOX_APPEND, WATCH_PUBLISH};

pub const NAME: &str = "watch.ami_stale";
pub const STREAM: &str = "watch.ami_stale";
pub const TICK: EventKindTag = "watch.ami_stale.tick";
pub const STATE: EventKindTag = "watch.ami_stale.state";
pub const OBSERVE: &str = "watch.ami_stale.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND, WATCH_PUBLISH];
/// `round-vm`'s own `Config::load` keys for `ami_check` are an unverified list in the contract.
pub const KEYS: &[&str] = &["SPIRA_ROUND_VM_SPILL", "SPIRA_CONCIERGE_INBOX"];
/// The row's tag (`spira/round-ami-stale.sh:285`).
pub const INBOX_TAG: &str = "round-ami-stale";
/// Appended to every stale line (`round-ami-stale.sh:299`).
pub const SUFFIX: &str = "; EC2 rounds refuse the image until the Round AMI workflow builds one";
/// The one subject: the image EC2 rounds launch from.
pub const SUBJECT: &str = "all";
/// `--interval 300`.
pub const CADENCE_MS: u64 = 300_000;
const OBSERVE_DEADLINE_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub spill: String,
    pub inbox: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { spill: cfg("SPIRA_ROUND_VM_SPILL")?, inbox: cfg("SPIRA_CONCIERGE_INBOX")? })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    #[default]
    Fresh,
    Stale { line: String },
    Unreadable { rc: i32, out: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub spill: String,
    pub verdict: Verdict,
}

/// Today's `round-ami-stale.said`: the last line said, cleared on fresh; and what is held on
/// the broker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub said: Option<String>,
    pub published: Published,
}

const INBOX_DEADLINE_MS: u64 = 10_000;
/// The spill modes that send rounds to EC2 (`spira/round-ami-stale.sh:29`); any other is idle.
pub const EC2_SPILLS: &[&str] = &["ec2", "ec2-always", "ec2-only"];

/// The line a verdict says, before [`SUFFIX`]; `None` for fresh. An unreadable listing is
/// composed as the script did (`round-ami-stale.sh:34-36`): never read as fresh.
pub fn line(v: &Verdict) -> Option<String> {
    match v {
        Verdict::Fresh => None,
        Verdict::Stale { line } => Some(line.clone()),
        Verdict::Unreadable { rc, out } => Some(format!("round AMI check could not read the images: 'round-vm ami-check' exited {rc}: {out}")),
    }
}

/// `Fresh` clears; a stale or unreadable line other than `said` is said once. The topic holds
/// the line for as long as the verdict does, and nothing while spill is not on EC2.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let ec2 = EC2_SPILLS.contains(&snap.spill.as_str());
    let current = line(&snap.verdict).filter(|_| ec2);
    let held = current.iter().map(|l| (SUBJECT.to_string(), format!("{l}{SUFFIX}"))).collect();
    let (published, change) = watch_topics::settle(INBOX_TAG, &carry.published, held, now_ms);
    let mut acts: Vec<EffectSpec> = current.iter().filter(|l| carry.said.as_ref() != Some(*l)).map(|l| EffectSpec::keyed(INBOX_APPEND, format!("ami_stale/{:016x}/{now_ms}", fnv1a(l)), pattern::inbox_line(now_ms, INBOX_TAG, &format!("{l}{SUFFIX}")), INBOX_DEADLINE_MS)).collect();
    acts.extend(change.map(|c| watch_topics::effect(&c, now_ms)));
    let said = if ec2 { current } else { carry.said.clone() };
    (Carry { said, published }, acts)
}

/// The verdict from round-vm's own `ami-check` in-process (`round_vm::cli::ami_check_answer`):
/// exit 0 fresh; exit 1 with a last non-empty line stale; anything else unreadable, its output
/// squeezed to single spaces and cut at 400 characters (`round-ami-stale.sh:31-36`). With spill
/// not on EC2, no AWS call.
pub fn observe() -> Result<String, String> {
    let spill = cfg("SPIRA_ROUND_VM_SPILL")?;
    let verdict = if EC2_SPILLS.contains(&spill.as_str()) { verdict(round_vm::cli::ami_check_answer()) } else { Verdict::Fresh };
    serde_json::to_string(&Snapshot { spill, verdict }).map_err(|e| e.to_string())
}

/// `(rc, output)` of `ami-check` as the script read it.
pub fn verdict((rc, out): (i32, String)) -> Verdict {
    let last = out.lines().rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
    match rc {
        0 => Verdict::Fresh,
        1 if !last.is_empty() => Verdict::Stale { line: last },
        _ => Verdict::Unreadable { rc, out: out.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(400).collect() },
    }
}

pub struct WatchAmiStale {
    pub config: Result<Config, String>,
}

impl WatchAmiStale {
    pub fn new(config: Config) -> Self {
        WatchAmiStale { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchAmiStale { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.ami_stale/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("said"), runtime::rows::Part::Map { name: "published", drop: &[] }];

impl Handler for WatchAmiStale {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let held = watch_topics::carried(INBOX_TAG, &carried::<Carry>(view, &ev.stream, STATE).published);
            return vec![Command::Probe(EffectSpec::probe(OBSERVE, held, OBSERVE_DEADLINE_MS))];
        }
        if let Some(again) = watch_topics::again(view, ev, &carried::<Carry>(view, &ev.stream, STATE).published) {
            return vec![Command::Effect(again)];
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let mut out = pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide);
        // Only a snapshot that was read settles it: any other outcome asks again.
        let settled = pattern::answer::<Snapshot>(view, ev, OBSERVE).is_some_and(|s| !EC2_SPILLS.contains(&s.spill.as_str()) || s.verdict == Verdict::Fresh);
        if !settled {
            out.push(Command::Timer { at: ev.at + CADENCE_MS, kind: EventKind::new(TICK, "") });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch_topics::Change;

    fn pass(carry: &Carry, spill: &str, verdict: Verdict, now_ms: u64) -> (Carry, Option<Change>) {
        let config = Config { spill: spill.into(), inbox: "inbox".into() };
        let (next, acts) = decide(carry, &Snapshot { spill: spill.into(), verdict }, now_ms, &config);
        let changes: Vec<Change> = acts.iter().filter(|a| a.name == WATCH_PUBLISH).map(|a| serde_json::from_str(&a.input).unwrap()).collect();
        assert!(changes.len() <= 1, "{changes:?}");
        (next, changes.into_iter().next())
    }

    fn stale(line: &str) -> Verdict {
        Verdict::Stale { line: line.into() }
    }

    #[test_id(ter9rdi)]
    #[test]
    fn the_topic_is_set_when_the_image_goes_stale_left_alone_while_unchanged_and_cleared_on_fresh() {
        let (c, change) = pass(&Carry::default(), "ec2", stale("old"), 10_000);
        let change = change.expect("appear");
        assert_eq!((change.row.as_str(), change.set.keys().collect::<Vec<_>>(), change.clear.len()), (INBOX_TAG, vec![&SUBJECT.to_string()], 0));
        let value = &change.set[SUBJECT];
        assert_eq!((value.text.clone(), value.level.as_str(), value.since, value.at), (format!("old{SUFFIX}"), "P2", 10, 10));
        let (held, change) = pass(&c, "ec2", stale("old"), 310_000);
        assert_eq!((change, &held), (None, &c), "hold");
        let (c, change) = pass(&held, "ec2", stale("newer"), 610_000);
        let value = &change.expect("change").set[SUBJECT];
        assert_eq!((value.text.clone(), value.since, value.at), (format!("newer{SUFFIX}"), 10, 610), "since survives a change of text");
        let (c, change) = pass(&c, "ec2", Verdict::Fresh, 910_000);
        let change = change.expect("clear");
        assert_eq!((change.set.len(), change.clear), (0, vec![SUBJECT.to_string()]));
        assert_eq!(c, Carry::default());
    }

    #[test_id(tgnx9ag)]
    #[test]
    fn spill_leaving_ec2_clears_the_topic_and_keeps_what_was_said() {
        let (c, _) = pass(&Carry::default(), "ec2", stale("old"), 10_000);
        let (c, change) = pass(&c, "local", Verdict::Fresh, 20_000);
        assert_eq!(change.expect("clear").clear, vec![SUBJECT.to_string()]);
        assert_eq!((c.said.as_deref(), c.published.len()), (Some("old"), 0));
    }
}
