//! # submission-provision — contract
//!
//! Pattern P4 (calendar tick) from [[scheduler-consolidation-2026-11-03]] §3, family A row
//! `submission-provision`. Today: `spira-submission provision` (`spira-submission/src/main.rs:16-18`,
//! `main.rs:76-97`). Paths are relative to `spira-releases/current`.
//!
//! ## Tick
//!
//! `OnCalendar=daily` plus `OnBootSec=2min` (`spira-submission-provision-prod.timer`). Becomes
//! `Calendar("daily").next_after(now)` (S8) with one boot tick 2 min after scheduler start.
//! Single-flight. No event wakes it.
//!
//! ## Observe
//!
//! The probe `submission.provision.observe` returns a `Snapshot`:
//!
//! | Field | Source today |
//! |---|---|
//! | `plan` (configured `submission_*` values, users file, dir, unit) | `Plan::from_spira` (`provision.rs:64-84`) — pinned settings, not a fork |
//! | `tailnet_ip` | `tailscale ip -4` (`provision.rs:252`, parsed `provision.rs:87`) |
//! | `dns_name` | `tailscale status --json` (`provision.rs:253`, `provision.rs:92`) |
//! | `cert_days` | parse of the cert file (`provision.rs:233-237`) |
//! | `password` state | `password_state` (`provision.rs:203`) over the users file |
//! | `iface` | `ip -o -4 addr show` (`provision.rs:324`, `provision.rs:99`) |
//! | `firewall` | `/etc/ufw/ufw.conf` + `sudo -n ufw status` (`provision.rs:152-163`) |
//!
//! No store verb exists for any of this: it is host state, read by the `host` class. Exempt
//! from law-a-watcher-asks-the-component-that-decides because the deciding component is the
//! host itself (tailscale, ufw).
//!
//! ## Decide
//!
//! `provision(host, plan)` (`provision.rs:245-362`) interleaves reads and writes through the
//! `Host` trait. The port splits it: `decide(snapshot) -> Vec<Act>`, pure, with the same
//! branches —
//!
//! 1. no tailnet ip → note only (`provision.rs:254-256`);
//! 2. cert missing or `< RENEW_DAYS=30` days → `cert.renew` (`provision.rs:10`, `provision.rs:282`);
//! 3. cert obtained → `config.set` for each empty `submission_cert|key|password_file|from|addr`
//!    (`provision.rs:307-318`);
//! 4. password drifted/missing/mode → `password.write` (`provision.rs:323-336`);
//! 5. firewall `Closed` → `firewall.open` (`provision.rs:339-345`);
//! 6. any set or renewal → `unit.restart` (`provision.rs:355-360`).
//!
//! Step 3 depends on the outcome of step 2 (files present after `tailscale cert`), so decide
//! runs twice: once on the snapshot, once on `cert.renew`'s outcome. Carried state: none.
//! Fixtures: the existing `provision.rs` tests over a fake `Host`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `submission-provision.cert_renew` | host | 120 s (unit `TimeoutStartSec`) | `cert/<dns_name>/<day>` | `needs_operator` → note, not error (`provision.rs:15-17`, `provision.rs:285`) |
//! | `submission-provision.config_set` | host | 10 s | `spira.<key>=<value>` | non-empty current value (only empties are set, `provision.rs:315`) |
//! | `submission-provision.password_write` | host | 5 s | digest of the derived line | no users file (`NoSource`, `provision.rs:326`) |
//! | `submission-provision.firewall_open` | host | 10 s | `<iface>/<port>` | ufw absent or already open (`provision.rs:345`) |
//! | `submission-provision.unit_restart` | unit | 30 s | `restart/<day>` | nothing set or renewed |
//! | `submission-provision.note` → `inbox.append` | store | 10 s | line text | — |
//!
//! `config_set` today forks the release's `spira-config set spira.<key> <value> <layer>`
//! (`provision.rs:82`, `provision.rs:259`) — a Spira→Spira child; it becomes a library call into
//! `spira-config`. `unit_restart` restarts `spira-submission` (`provision.rs:81`); once
//! submission is a source of this scheduler, that becomes "restart the `submission` source",
//! not a systemd call.
//!
//! ## Silence
//!
//! A daily tick whose decide yields only "certificate has N days left" (`provision.rs:302`)
//! emits nothing: notes go to the inbox only on change or error.
//!
//! ## Replaces
//!
//! - Timer + service `spira-submission-provision-prod.{timer,service}`; no watchd row.
//! - Children: `tailscale` ×2–3, `ip`, `sudo -n ufw` ×1–2, `systemctl --user restart`,
//!   `spira-config set` ×≤5 (all `RealHost::run`, `provision.rs:27-37`). Catalogue says
//!   "(param)"; the code forks `spira-config` (Spira→Spira) and `systemctl` (S6) too.
//! - Pins: `SPIRA_SUBMISSION_PORT|ADDR|CERT|KEY|PASSWORD_FILE|FROM|USER|RCPT_DOMAIN|USERS_FILE`,
//!   `SPIRA_INSTANCE`, the toml spec layer (`provision.rs:66-82`).
//!
//! ## Reverse
//!
//! Release rollback restores the timer. `Observe` mode shadows every act (the effects are all
//! idempotent host writes), so a misbehaving port is safe to leave in Observe.
//!

use std::collections::BTreeMap;

use runtime::calendar::Calendar;
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "submission-provision";
pub const STREAM: &str = "task.submission-provision";
pub const TICK: EventKindTag = "submission-provision.tick";
pub const STATE: EventKindTag = "submission-provision.state";
/// The contract names the probe `submission.provision.observe` and every act
/// `submission-provision.*`; the probe takes the acts' prefix so the task has one.
pub const OBSERVE: &str = "submission-provision.observe";
pub const CERT_RENEW: &str = "submission-provision.cert_renew";
pub const CONFIG_SET: &str = "submission-provision.config_set";
pub const PASSWORD_WRITE: &str = "submission-provision.password_write";
pub const FIREWALL_OPEN: &str = "submission-provision.firewall_open";
pub const UNIT_RESTART: &str = "submission-provision.unit_restart";
/// `submission-provision.note` is an `inbox.append`.
pub const INBOX_APPEND: &str = crate::watchers::INBOX_APPEND;

pub const CALENDAR: &str = "daily";
/// `OnBootSec=2min`: the one boot tick.
pub const BOOT_DELAY_MS: u64 = 120_000;
pub const OBSERVE_DEADLINE_MS: u64 = 30_000;
/// `RENEW_DAYS` (`provision.rs:10`).
pub const RENEW_DAYS: u32 = 30;

pub const ACTS: &[(&str, u64)] = &[(CERT_RENEW, 120_000), (CONFIG_SET, 10_000), (PASSWORD_WRITE, 5_000), (FIREWALL_OPEN, 10_000), (UNIT_RESTART, 30_000), (INBOX_APPEND, 10_000)];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, CERT_RENEW, CONFIG_SET, PASSWORD_WRITE, FIREWALL_OPEN, UNIT_RESTART, INBOX_APPEND];
pub const KEYS: &[&str] = &[
    "SPIRA_SUBMISSION_PORT",
    "SPIRA_SUBMISSION_ADDR",
    "SPIRA_SUBMISSION_CERT",
    "SPIRA_SUBMISSION_KEY",
    "SPIRA_SUBMISSION_PASSWORD_FILE",
    "SPIRA_SUBMISSION_FROM",
    "SPIRA_SUBMISSION_USER",
    "SPIRA_SUBMISSION_RCPT_DOMAIN",
    "SPIRA_SUBMISSION_USERS_FILE",
    "SPIRA_INSTANCE",
];
pub const UNREGISTERED: &[&str] = &[];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// The configured `submission_*` values (`Plan::from_spira`); an empty one is one to set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub port: String,
    pub addr: String,
    pub cert: String,
    pub key: String,
    pub password_file: String,
    pub from: String,
    pub user: String,
    pub rcpt_domain: String,
    pub users_file: String,
    pub unit: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Password {
    #[default]
    Ok,
    Drifted,
    Missing,
    Mode,
    NoSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Firewall {
    #[default]
    Open,
    Closed,
    Absent,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub plan: Plan,
    pub tailnet_ip: Option<String>,
    pub dns_name: Option<String>,
    /// Days the certificate has left; `None` when there is none.
    pub cert_days: Option<u32>,
    pub password: Password,
    pub iface: Option<String>,
    pub firewall: Firewall,
    /// The day this snapshot was taken, for the `cert/<dns_name>/<day>` and `restart/<day>` keys.
    pub day: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Idle,
    Observing,
    Renewing,
    Setting,
    Restarting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Tick,
    NothingToDo,
    CertDue,
    Sets,
    Renewed,
    SetsDone,
    SetsDoneUnchanged,
    Restarted,
    /// A `config_set` asked for while the renewal's outcome is still out.
    ConfigSet,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Tick, State::Observing),
    (State::Observing, Trigger::NothingToDo, State::Idle),
    (State::Observing, Trigger::CertDue, State::Renewing),
    (State::Observing, Trigger::Sets, State::Setting),
    (State::Renewing, Trigger::Renewed, State::Setting),
    (State::Setting, Trigger::SetsDone, State::Restarting),
    (State::Setting, Trigger::SetsDoneUnchanged, State::Idle),
    (State::Restarting, Trigger::Restarted, State::Idle),
];

/// Carried: the machine, and the last note sent, so a note goes to the inbox only on change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub state: State,
    pub last_note: Option<String>,
}

pub fn step(_state: State, _event: Trigger) -> Result<State, String> {
    crate::not_ported!(NAME)
}

/// Branches 1, 2, 4, 5 and 6 of `provision` over the snapshot.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

/// Branch 3, on `cert_renew`'s outcome: a `config_set` for each empty cert, key, password
/// file, from and addr, now that the files exist.
pub fn after_renewal(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmissionProvision {
    pub cadence: Cadence,
    pub boot_delay_ms: u64,
    pub pinned: BTreeMap<String, String>,
}

impl SubmissionProvision {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        SubmissionProvision { cadence: Cadence::Calendar(Calendar::parse(CALENDAR).expect("daily parses")), boot_delay_ms: BOOT_DELAY_MS, pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    /// The boot tick only arms the one tick `BOOT_DELAY_MS` later.
    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("submission-provision/boot", STREAM, EventKind::new(TICK, "boot"))
    }
}

impl Handler for SubmissionProvision {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK && ev.kind.body == "boot" {
            return vec![Command::Timer { at: ev.at + self.boot_delay_ms, kind: EventKind::new(TICK, "") }];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
