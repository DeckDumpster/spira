//! `intake.gh_closeout`: settles the GitHub issue of every bead the lifecycle store recorded as
//! landed, superseded or dropped. Pattern P1, stateless; replaces the landing pass's per-pass
//! `gh-intake unlanded-scan`.
//!
//! # Tick
//!
//! Every `SPIRA_RUNTIME_GH_CLOSEOUT_SECS`. `intake.gh_closeout.observe` counts the pending
//! closeouts (`spira-lc gh-closeout pending`); none, or a halted world, ends the tick. Otherwise
//! `intake.gh_closeout.drain` runs `gh_intake::closeout::drain`, which reads the pending rows
//! and points by id, comments and closes the issue, and removes the row; a failed GitHub call
//! leaves the row for the next tick.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key |
//! |---|---|---|---|
//! | `intake.gh_closeout.observe` (probe) | store | 30 s | tick id |
//! | `intake.gh_closeout.drain` | forge | 600 s | `gh-closeout/<period index>` |

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use gh_intake::closeout::{self, Deps};
use gh_intake::ports::Closeouts;
use gh_intake::real::{RealBd, RealCloseouts, RealGh, RealGit, RealLifecycle, RealMail, RealRepo};
use spira_store::lc_call::LcClient;

use super::trigger_groom::{config_input, config_of, halted};
use super::Cycle;

pub const NAME: &str = "intake.gh_closeout";
pub const STREAM: &str = "task.intake.gh_closeout";
pub const TICK: EventKindTag = "intake.gh_closeout.tick";

pub const OBSERVE: &str = "intake.gh_closeout.observe";
pub const DRAIN: &str = "intake.gh_closeout.drain";
pub const EFFECTS: &[&str] = &[OBSERVE, DRAIN];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_GH_CLOSEOUT_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_MAIL_BIN"];

const OBSERVE_DEADLINE_MS: u64 = 30_000;
const DRAIN_DEADLINE_MS: u64 = 600_000;
const CYCLE: Cycle = Cycle {
    stream: STREAM,
    tick: TICK,
    observe: OBSERVE,
    deadline_ms: OBSERVE_DEADLINE_MS,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub pending: Result<u64, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Skip(String),
    Drain,
}

/// The pure decision over one snapshot: drain only when something is pending.
pub fn decide(snap: &Snapshot) -> Decision {
    if snap.halted {
        return Decision::Skip("halted".into());
    }
    match snap.pending {
        Err(_) => Decision::Skip("unreadable".into()),
        Ok(0) => Decision::Skip("none".into()),
        Ok(_) => Decision::Drain,
    }
}

/// `intake.gh_closeout.observe`: whether the world is halted and how many closeouts are pending.
pub fn run_observe(e: &crate::Effects, _key: &str, _s: &EffectSpec) -> Result<String, String> {
    let lc = LcClient::socket(&e.world.lc_socket)
        .within(std::time::Duration::from_millis(OBSERVE_DEADLINE_MS));
    let snap = Snapshot {
        halted: halted(e),
        pending: RealCloseouts::over(lc)
            .pending()
            .map(|rows| rows.len() as u64),
    };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `intake.gh_closeout.drain`: one drain, its log as the answer.
pub fn run_drain(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let config = config_of(s)?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let (bd, gh) = (
        RealBd {
            lc: Some(LcClient::socket(&e.world.lc_socket)),
        },
        RealGh,
    );
    let repo = RealRepo {
        spira_home: home.to_string_lossy().into_owned(),
    };
    let mail = RealMail::new(
        config
            .get("SPIRA_MAIL_BIN")
            .cloned()
            .filter(|m| !m.is_empty()),
    );
    let lc = RealLifecycle::over(LcClient::socket(&e.world.lc_socket));
    let co = RealCloseouts::over(LcClient::socket(&e.world.lc_socket));
    let d = Deps {
        bd: &bd,
        gh: &gh,
        git: &RealGit,
        repo: &repo,
        mail: &mail,
        lc: &lc,
        co: &co,
    };
    Ok(closeout::drain(
        &d,
        &closeout::Ctx {
            run: e.world.run_dir.clone(),
        },
    )
    .join("\n"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeGhCloseout {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl IntakeGhCloseout {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(IntakeGhCloseout {
            interval_ms: super::cadence_ms(settings, CADENCE_KEY)?,
            config: super::pinned(settings, KEYS)?,
        })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("intake.gh_closeout/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for IntakeGhCloseout {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, String::new, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else {
                return vec![];
            };
            match decide(&snap) {
                Decision::Skip(why) => {
                    ctx.event().set("gh_closeout.skip", why.as_str());
                    vec![]
                }
                Decision::Drain => vec![Command::Effect(EffectSpec::keyed(
                    DRAIN,
                    format!("gh-closeout/{}", ev.at / self.interval_ms.max(1)),
                    config_input(&self.config),
                    DRAIN_DEADLINE_MS,
                ))],
            }
        })
    }
}
