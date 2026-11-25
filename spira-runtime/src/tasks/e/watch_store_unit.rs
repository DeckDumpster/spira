//! # watch.store_unit
//!
//! Reads the bead store unit's state over the scheduler's `Units` backend, starts no program, and
//! alarms the operator and the Concierge inbox once when the unit is not `active` or `activating`;
//! one clear when it is back. First tick `BOOT_DELAY_MS` after boot, then every `CADENCE_MS`.
//! With `SPIRA_DOLT_DATA` empty no store unit is installed and nothing is ever sent.

use runtime::event::*;
use runtime::units::{UnitFacts, UnitHandle, UnitState, Units};
use runtime::RuntimeError;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use crate::tasks::d::pattern;
use crate::tasks::e::alert::Alert;
use crate::watchers::INBOX_APPEND;

pub const NAME: &str = "watch.store_unit";
pub const STREAM: &str = "watch.store_unit";
pub const TICK: EventKindTag = "watch.store_unit.tick";
pub const STATE: EventKindTag = "watch.store_unit.state";
pub const OBSERVE: &str = "watch.store_unit.observe";
pub const MAIL: &str = "watch.store_unit.mail";
pub const EFFECTS: &[&str] = &[OBSERVE, MAIL, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_DOLT_UNIT", "SPIRA_DOLT_DATA"];
pub const BOOT_DELAY_MS: u64 = 120_000;
pub const CADENCE_MS: u64 = 900_000;
const OBSERVE_DEADLINE_MS: u64 = 5_000;
const SEND_DEADLINE_MS: u64 = 10_000;
pub const MAILBOX: &str = "operator";
pub const FROM: &str = "Store Watch <store-watch@spira>";
pub const INBOX_TAG: &str = "store-unit";
const BOOT_BODY: &str = "boot";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub unit: String,
    pub installed: bool,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { unit: cfg("SPIRA_DOLT_UNIT")?, installed: !cfg("SPIRA_DOLT_DATA")?.is_empty() })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Snapshot {
    NotInstalled,
    Unit { unit: String, up: bool, state: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Carry {
    #[default]
    Unseen,
    Ok,
    Down,
}

/// `Down` from anything but `Down` alarms; `Ok` from `Down` clears; the rest is silent.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64) -> (Carry, Vec<EffectSpec>) {
    let Snapshot::Unit { unit, up, state } = snap else { return (*carry, vec![]) };
    let (next, prefix, line, urgent) = match (carry, up) {
        (Carry::Down, false) => return (Carry::Down, vec![]),
        (_, true) if *carry != Carry::Down => return (Carry::Ok, vec![]),
        (_, true) => (Carry::Ok, "bead store running", format!("{unit} is '{state}': the bead store is back"), false),
        (_, false) => (Carry::Down, "bead store not running", format!("{unit} is '{state}': the bead store is down. Run: systemctl --user start {unit}"), true),
    };
    let alert = Alert::of(MAILBOX, FROM, prefix, "note", &line);
    let alert = if urgent { alert.urgent() } else { alert };
    let key = |what: &str| format!("{NAME}:{what}:{now_ms}");
    (next, vec![EffectSpec::keyed(INBOX_APPEND, key("inbox"), pattern::inbox_line(now_ms, INBOX_TAG, &line), SEND_DEADLINE_MS), alert.effect(MAIL, key("mail"), SEND_DEADLINE_MS)])
}

/// `active`, `activating` and `reloading` (which the backend reports as active) are up.
fn snapshot_of(unit: &str, facts: Result<UnitFacts, RuntimeError>) -> Snapshot {
    let (up, state) = match facts {
        Ok(f) => (matches!(f.state, UnitState::Active | UnitState::Activating), format!("{:?}", f.state).to_lowercase()),
        Err(e) => (false, e.0),
    };
    Snapshot::Unit { unit: unit.into(), up, state }
}

pub fn observe_with(config: &Config, units: Option<&dyn Units>) -> Snapshot {
    if !config.installed {
        return Snapshot::NotInstalled;
    }
    match units {
        Some(units) => snapshot_of(&config.unit, units.facts(&UnitHandle::named(&config.unit))),
        None => Snapshot::Unit { unit: config.unit.clone(), up: false, state: "no unit backend".into() },
    }
}

pub fn observe(units: Option<&dyn Units>) -> Result<String, String> {
    serde_json::to_string(&observe_with(&Config::pinned()?, units)).map_err(|e| e.to_string())
}

pub struct WatchStoreUnit;

impl WatchStoreUnit {
    pub fn pinned() -> Self {
        WatchStoreUnit
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.store_unit/boot", STREAM, EventKind::new(TICK, BOOT_BODY))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(RowCodec { stream: STREAM, also: &[], tag: STATE, split: runtime::rows::split_whole, join: runtime::rows::join_whole })
    }
}

impl Handler for WatchStoreUnit {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK && ev.kind.body == BOOT_BODY {
            return vec![Command::Timer { at: ev.at + BOOT_DELAY_MS, kind: EventKind::new(TICK, "") }];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &Ok::<(), String>(()), |c: &Carry, s: &Snapshot, now, _: &()| decide(c, s, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(state: &str, up: bool) -> Snapshot {
        Snapshot::Unit { unit: "dolt-beads.service".into(), up, state: state.into() }
    }

    fn names(fx: &[EffectSpec]) -> Vec<&str> {
        fx.iter().map(|e| e.name.as_str()).collect()
    }

    #[test_id(t3zzbo1)]
    #[test]
    fn an_up_unit_sends_nothing_at_the_first_tick_and_after() {
        for state in [UnitState::Active, UnitState::Activating] {
            let snap = snapshot_of("dolt-beads.service", Ok(UnitFacts { state, ..UnitFacts::absent() }));
            for carry in [Carry::Unseen, Carry::Ok] {
                assert_eq!(decide(&carry, &snap, 1), (Carry::Ok, vec![]), "{snap:?}");
            }
        }
        let reloading = snapshot_of("dolt-beads.service", Ok(UnitFacts { state: runtime::units::state_of_word("reloading"), ..UnitFacts::absent() }));
        assert_eq!(decide(&Carry::Ok, &reloading, 1), (Carry::Ok, vec![]), "reloading is up");
    }

    #[test_id(t0jvyou)]
    #[test]
    fn a_down_unit_alarms_once_then_clears_once_when_it_is_back() {
        let (c, fx) = decide(&Carry::Unseen, &unit("inactive", false), 1);
        assert_eq!((c, names(&fx)), (Carry::Down, vec![INBOX_APPEND, MAIL]));
        assert!(fx[0].input.ends_with("dolt-beads.service is 'inactive': the bead store is down. Run: systemctl --user start dolt-beads.service"), "{}", fx[0].input);
        let alert: Alert = serde_json::from_str(&fx[1].input).unwrap();
        assert!(alert.urgent && alert.mailbox == MAILBOX && alert.kind == "note");
        let (c, fx) = decide(&c, &unit("failed", false), 2);
        assert_eq!((c, fx.len()), (Carry::Down, 0));
        let (c, fx) = decide(&c, &unit("active", true), 3);
        assert_eq!((c, names(&fx)), (Carry::Ok, vec![INBOX_APPEND, MAIL]));
        assert!(!serde_json::from_str::<Alert>(&fx[1].input).unwrap().urgent);
        assert_eq!(decide(&c, &unit("active", true), 4).1.len(), 0);
    }

    #[test_id(t96ftuu)]
    #[test]
    fn a_unit_that_goes_down_after_being_ok_alarms_again() {
        let (c, fx) = decide(&Carry::Ok, &unit("inactive", false), 1);
        assert_eq!((c, fx.len()), (Carry::Down, 2));
    }

    #[test_id(tlg9m8r)]
    #[test]
    fn a_failed_facts_read_alarms_with_its_error_as_the_state() {
        let snap = snapshot_of("dolt-beads.service", Err(RuntimeError::new("bus gone")));
        let (c, fx) = decide(&Carry::Unseen, &snap, 1);
        assert_eq!((c, names(&fx)), (Carry::Down, vec![INBOX_APPEND, MAIL]));
        assert!(fx[0].input.contains("is 'bus gone': the bead store is down"), "{}", fx[0].input);
    }

    #[test_id(t3pzus1)]
    #[test]
    fn a_box_without_a_store_never_sends() {
        for carry in [Carry::Unseen, Carry::Ok, Carry::Down] {
            assert_eq!(decide(&carry, &Snapshot::NotInstalled, 1), (carry, vec![]));
        }
        let off = Config { unit: "dolt-beads.service".into(), installed: false };
        assert_eq!(observe_with(&off, None), Snapshot::NotInstalled);
    }
}
