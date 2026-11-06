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
//! 6. any set or renewal → `unit_restart`, which reloads the scheduler's `submission` source.
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
//! | `submission-provision.unit_restart` | host | 30 s | `restart/<day>` | nothing set or renewed; any source but `submission` |
//! | `submission-provision.note` → `inbox.append` | store | 10 s | line text | — |
//!
//! `config_set` today forks the release's `spira-config set spira.<key> <value> <layer>`
//! (`provision.rs:82`, `provision.rs:259`) — a Spira→Spira child; it becomes a library call into
//! `spira-config`. `unit_restart` once restarted the retired `spira-submission` unit; it is now
//! the `submission` source's reload (`tasks::g::submission::reload`): the source reads its config
//! anew, in this process. The certificate, key and password need no reload at all — the listener
//! reads them per connection.
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
    /// The scheduler source a restart reloads (`tasks::g::submission::NAME`); the
    /// `spira-submission` unit it once named is retired.
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

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    TABLE.iter().find(|(s, e, _)| *s == state && *e == event).map(|(_, _, to)| *to).ok_or_else(|| format!("{NAME}: {event:?} is not legal in state {}", serde_json::to_string(&state).unwrap_or_default().trim_matches('"')))
}

/// The submission directory as the scheduler's `$HOME` places it; `~` is expanded by the
/// `config_set` and `password_write` runners, so `decide` stays pure.
const DIR: &str = "~/.config/spira/submission";

/// `config_set`'s input: one `spira.<key> <value>` for the spec's last layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSet {
    pub key: String,
    pub value: String,
}

/// `password_write`'s input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordWrite {
    pub users_file: String,
    pub user: String,
    pub password_file: String,
}

/// `firewall_open`'s input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallOpen {
    pub iface: String,
    pub port: String,
}

fn note(carry: &mut Carry, acts: &mut Vec<Act>, line: String) {
    if carry.last_note.as_deref() != Some(line.as_str()) {
        acts.push(Act::new(INBOX_APPEND, line.clone()).with_input(format!("{NAME}: {line}")));
        carry.last_note = Some(line);
    }
}

/// The address submission listens on: the configured one, else the tailnet's.
fn want_addr(snap: &Snapshot) -> String {
    if snap.plan.addr.is_empty() { snap.tailnet_ip.clone().unwrap_or_default() } else { snap.plan.addr.clone() }
}

/// Branch 3: a `config_set` for each empty value, by `spira_submission::provision::defaults`.
fn sets(snap: &Snapshot) -> Vec<Act> {
    let p = &snap.plan;
    let defaults = spira_submission::provision::defaults(std::path::Path::new(DIR), &p.cert, &p.key, &p.user, &p.rcpt_domain, &want_addr(snap));
    [&p.cert, &p.key, &p.password_file, &p.from, &p.addr]
        .into_iter()
        .zip(defaults)
        .filter(|(cur, _)| cur.is_empty())
        .map(|(_, (k, v))| Act::new(CONFIG_SET, format!("spira.{k}={v}")).with_input(serde_json::to_string(&ConfigSet { key: format!("spira.{k}"), value: v }).unwrap_or_default()))
        .collect()
}

/// Branches 4 and 5: the password file and the firewall rule.
fn password_and_firewall(carry: &mut Carry, snap: &Snapshot, acts: &mut Vec<Act>) {
    let p = &snap.plan;
    let addr = want_addr(snap);
    let password_file = if p.password_file.is_empty() { format!("{DIR}/password") } else { p.password_file.clone() };
    if !p.users_file.is_empty() && !addr.is_empty() {
        match snap.password {
            Password::Ok => {}
            Password::NoSource => note(carry, acts, format!("password source unusable: {} has no entry for {}", p.users_file, p.user)),
            Password::Drifted | Password::Missing | Password::Mode => {
                let w = PasswordWrite { users_file: p.users_file.clone(), user: p.user.clone(), password_file: password_file.clone() };
                acts.push(Act::new(PASSWORD_WRITE, format!("password/{password_file}")).with_input(serde_json::to_string(&w).unwrap_or_default()));
            }
        }
    }
    if let (Firewall::Closed, Some(iface), false) = (snap.firewall, &snap.iface, addr.is_empty()) {
        let f = FirewallOpen { iface: iface.clone(), port: p.port.clone() };
        acts.push(Act::new(FIREWALL_OPEN, format!("{iface}/{}", p.port)).with_input(serde_json::to_string(&f).unwrap_or_default()));
    }
}

/// The submission listener is the scheduler's `submission` source, so the restart is that
/// source's reload, never a unit; the effect keeps its contract name.
fn restart(snap: &Snapshot) -> Act {
    Act::new(UNIT_RESTART, format!("restart/{}", snap.day)).with_input(crate::tasks::g::submission::NAME.to_string())
}

/// Branches 1, 2, 4, 5 and 6 of `provision` over the snapshot. A due certificate is renewed
/// first and the rest waits on its outcome (`after_renewal`); otherwise the empty values are
/// set, the password and firewall mended, and a set restarts the unit once every set is done.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let mut next = Carry { state: State::Idle, last_note: carry.last_note.clone() };
    let mut acts = vec![];
    if snap.tailnet_ip.is_none() {
        note(&mut next, &mut acts, "tailscale is not up: no tailnet address, certificate or firewall rule to provision".into());
        return (next, acts);
    }
    if next.last_note.as_deref().is_some_and(|n| n.starts_with("tailscale is not up")) {
        next.last_note = None;
    }
    if let Some(name) = &snap.dns_name {
        if spira_submission::provision::renew_due(snap.cert_days.is_some(), snap.cert_days.map(i64::from)) {
            next.state = State::Renewing;
            acts.push(Act::new(CERT_RENEW, format!("cert/{name}/{}", snap.day)).with_input(serde_json::to_string(snap).unwrap_or_default()));
            return (next, acts);
        }
    }
    finish(next, snap, false, acts)
}

/// Sets (only once a certificate exists), the password and firewall, and the restart: now when
/// nothing is set and `renewed`, after the sets otherwise.
fn finish(mut next: Carry, snap: &Snapshot, renewed: bool, mut acts: Vec<Act>) -> (Carry, Vec<Act>) {
    let sets = if snap.cert_days.is_some() { sets(snap) } else { vec![] };
    password_and_firewall(&mut next, snap, &mut acts);
    next.state = if !sets.is_empty() {
        State::Setting
    } else if renewed {
        acts.push(restart(snap));
        State::Restarting
    } else {
        State::Idle
    };
    acts.extend(sets);
    (next, acts)
}

/// Branch 3, on `cert_renew`'s outcome: a `config_set` for each empty cert, key, password
/// file, from and addr, now that the files exist.
pub fn after_renewal(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    after_renewal_with(carry, snap, true)
}

/// [`after_renewal`] when the renewal may have left the certificate unchanged: then, with
/// nothing to set, there is nothing to restart either.
pub fn after_renewal_with(carry: &Carry, snap: &Snapshot, renewed: bool) -> (Carry, Vec<Act>) {
    let renewed_snap = Snapshot { cert_days: snap.cert_days.or(Some(0)), ..snap.clone() };
    finish(Carry { state: State::Setting, last_note: carry.last_note.clone() }, &renewed_snap, renewed, vec![])
}

/// What `cert_renew` answered: renewed, unchanged, or the operator note it needs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renewal {
    #[serde(default)]
    pub renewed: bool,
    #[serde(default)]
    pub note: Option<String>,
}

fn renewal_of(body: &str) -> Renewal {
    if body.trim() == "renewed" {
        return Renewal { renewed: true, note: None };
    }
    serde_json::from_str(body).unwrap_or(Renewal { renewed: false, note: Some(body.to_string()) })
}

/// Whether every `config_set` asked since the carry was last recorded has an outcome.
fn sets_done(view: &Projection, ev: &Event) -> bool {
    let events = view.events(&ev.stream);
    let from = events.iter().rposition(|e| e.kind.tag == STATE).map_or(0, |i| i + 1);
    let asked: Vec<&EventId> = events[from..].iter().filter(|e| e.kind.tag == EFFECT_REQUESTED && serde_json::from_str::<serde_json::Value>(&e.kind.body).ok().is_some_and(|v| v["spec"]["name"] == CONFIG_SET)).map(|e| &e.id).collect();
    let answered: Vec<&EventId> = events[from..].iter().filter(|e| [EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT].contains(&e.kind.tag.as_str())).filter_map(|e| e.cause.as_ref()).chain(ev.cause.as_ref()).collect();
    asked.iter().all(|a| answered.contains(a))
}

// ---------------------------------------------------------------------------------------------
// Runners. Host reads (`tailscale`, `ip`, `ufw`) go through the process port; the config write
// is `spira_config` in this process and the restart is the Units port's.

struct ProcHost;

impl spira_submission::provision::Host for ProcHost {
    fn run(&self, argv: &[&str]) -> Result<String, String> {
        use runtime::ports::Proc;
        let (prog, args) = argv.split_first().ok_or("empty command")?;
        let spec = runtime::ports::ProcSpec { prog: prog.to_string(), args: args.iter().map(|a| a.to_string()).collect(), stdin: vec![], deadline: std::time::Duration::from_secs(110) };
        let out = runtime::ports::SysProc.run(&spec)?;
        match out.code {
            Some(0) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
            code => Err(format!("{} exited {}: {}", argv.join(" "), code.map_or("signal".into(), |c| c.to_string()), String::from_utf8_lossy(&out.stderr).trim())),
        }
    }

    fn now(&self) -> i64 {
        spira_submission::clock::now()
    }
}

fn home() -> Result<std::path::PathBuf, String> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(Into::into).ok_or_else(|| "HOME is unset: the submission directory has no root".to_string())
}

fn expand(p: &str) -> Result<String, String> {
    Ok(match p.strip_prefix("~/") {
        Some(rest) => home()?.join(rest).display().to_string(),
        None => p.to_string(),
    })
}

fn the_day(now: i64) -> String {
    let (y, m, d) = crate::watchers::civil(now.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// The configured values, the tailnet, the certificate, the password file, the interface and
/// the firewall, read on the host.
pub fn observe() -> Result<String, String> {
    use spira_submission::provision as p;
    let get = |k: &str| spira_config::process::cfg(k).map(|v| v.trim().to_string());
    let plan = Plan {
        port: get("SPIRA_SUBMISSION_PORT")?,
        addr: get("SPIRA_SUBMISSION_ADDR")?,
        cert: get("SPIRA_SUBMISSION_CERT")?,
        key: get("SPIRA_SUBMISSION_KEY")?,
        password_file: get("SPIRA_SUBMISSION_PASSWORD_FILE")?,
        from: get("SPIRA_SUBMISSION_FROM")?,
        user: get("SPIRA_SUBMISSION_USER")?,
        rcpt_domain: get("SPIRA_SUBMISSION_RCPT_DOMAIN")?,
        users_file: get("SPIRA_SUBMISSION_USERS_FILE")?,
        unit: crate::tasks::g::submission::NAME.to_string(),
    };
    let host = ProcHost;
    let now = p::Host::now(&host);
    let tailnet_ip = p::Host::run(&host, &["tailscale", "ip", "-4"]).ok().and_then(|o| p::tailnet_ip(&o));
    let dns_name = p::Host::run(&host, &["tailscale", "status", "--json"]).ok().and_then(|o| p::dns_name(&o));
    let dir = p::dir_under(&home()?);
    let cert = if plan.cert.is_empty() { dir.join("cert.pem") } else { plan.cert.clone().into() };
    let key = if plan.key.is_empty() { dir.join("key.pem") } else { plan.key.clone().into() };
    let cert_days = if cert.is_file() && key.is_file() { p::cert_days(&cert, now).map(|d| d.clamp(0, i64::from(u32::MAX)) as u32) } else { None };
    let password_file = if plan.password_file.is_empty() { dir.join("password").display().to_string() } else { plan.password_file.clone() };
    let password = match p::password_state_of(&plan.users_file, &plan.user, &password_file) {
        p::Password::Ok => Password::Ok,
        p::Password::Missing => Password::Missing,
        p::Password::Mode => Password::Mode,
        p::Password::Drifted => Password::Drifted,
        p::Password::NoSource(_) => Password::NoSource,
    };
    let iface = tailnet_ip.as_ref().and_then(|ip| p::Host::run(&host, &["ip", "-o", "-4", "addr", "show"]).ok().and_then(|o| p::iface_for(ip, &o)));
    let firewall = match &iface {
        None => Firewall::Absent,
        Some(i) => {
            let lib_plan = p::Plan { toml_spec: String::new(), addr: plan.addr.clone(), cert: String::new(), key: String::new(), password_file: String::new(), from: String::new(), user: plan.user.clone(), port: plan.port.parse().map_err(|e| format!("SPIRA_SUBMISSION_PORT does not parse: {e}"))?, rcpt_domain: String::new(), users_file: String::new(), dir: dir.clone(), ufw_conf: "/etc/ufw/ufw.conf".into(), spira_config: String::new() };
            match p::firewall_state(&host, &lib_plan, i) {
                p::Firewall::Closed => Firewall::Closed,
                p::Firewall::Open => Firewall::Open,
                p::Firewall::Absent => Firewall::Absent,
                p::Firewall::Unknown(e) => return Err(format!("cannot read the firewall: {e}")),
            }
        }
    };
    serde_json::to_string(&Snapshot { plan, tailnet_ip, dns_name, cert_days, password, iface, firewall, day: the_day(now) }).map_err(|e| e.to_string())
}

/// `tailscale cert` into the submission directory. Operator-needed is an answer, not an error.
pub fn cert_renew(input: &str) -> Result<String, String> {
    use spira_submission::provision as p;
    let snap: Snapshot = serde_json::from_str(input).map_err(|e| format!("{CERT_RENEW}: bad input: {e}"))?;
    let name = snap.dns_name.clone().ok_or_else(|| format!("{CERT_RENEW}: no tailnet name — refused"))?;
    let dir = p::dir_under(&home()?);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let cert = if snap.plan.cert.is_empty() { dir.join("cert.pem").display().to_string() } else { snap.plan.cert.clone() };
    let key = if snap.plan.key.is_empty() { dir.join("key.pem").display().to_string() } else { snap.plan.key.clone() };
    let before = (std::fs::read(&cert).ok(), std::fs::read(&key).ok());
    let out = match p::Host::run(&ProcHost, &["tailscale", "cert", "--cert-file", &cert, "--key-file", &key, &name]) {
        Err(e) if p::needs_operator(&e) => Renewal { renewed: false, note: Some(format!("{} (tailscale cert {name}: {e})", p::OPERATOR_NOTE)) },
        Err(e) => return Err(format!("tailscale cert {name}: {e}")),
        Ok(_) => {
            let _ = std::fs::set_permissions(&key, std::os::unix::fs::PermissionsExt::from_mode(0o600));
            Renewal { renewed: (std::fs::read(&cert).ok(), std::fs::read(&key).ok()) != before, note: None }
        }
    };
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

/// `spira-config set spira.<key> <value> <layer>`, as `spira_config::set_paths_in_file` on the
/// spec's last layer. Only an empty value is set.
pub fn config_set(input: &str) -> Result<String, String> {
    let c: ConfigSet = serde_json::from_str(input).map_err(|e| format!("{CONFIG_SET}: bad input: {e}"))?;
    let env_key = format!("SPIRA_{}", c.key.trim_start_matches("spira.").to_ascii_uppercase());
    if spira_config::process::cfg(&env_key).is_ok_and(|v| !v.trim().is_empty()) {
        return Err(format!("{CONFIG_SET}: {} is already set — refused", c.key));
    }
    let spec = spira_config::process::spec()?;
    let layer = spec.rsplit(':').next().unwrap_or("").to_string();
    let value = expand(&c.value)?;
    spira_config::set_paths_in_file(std::path::Path::new(&layer), &[(&c.key, &value)])?;
    Ok(format!("set {} = {value}", c.key))
}

/// The password file, mode 0600, derived from the users file.
pub fn password_write(input: &str) -> Result<String, String> {
    let w: PasswordWrite = serde_json::from_str(input).map_err(|e| format!("{PASSWORD_WRITE}: bad input: {e}"))?;
    let file = expand(&w.password_file)?;
    spira_submission::provision::write_password(&w.users_file, &w.user, &file)?;
    Ok(format!("wrote {file} from {}", w.users_file))
}

/// `sudo -n ufw allow in on <iface> to any port <port> proto tcp`.
pub fn firewall_open(input: &str) -> Result<String, String> {
    let f: FirewallOpen = serde_json::from_str(input).map_err(|e| format!("{FIREWALL_OPEN}: bad input: {e}"))?;
    let port: u16 = f.port.parse().map_err(|e| format!("{FIREWALL_OPEN}: port {}: {e}", f.port))?;
    let cmd = spira_submission::provision::allow_command(&f.iface, port);
    let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
    spira_submission::provision::Host::run(&ProcHost, &argv).map(|_| format!("opened {port}/tcp on {}", f.iface))
}

/// The listener is the scheduler's `submission` source: the restart asks that source to reload,
/// reading its config anew. No unit, and no restart of the scheduler.
pub fn unit_restart(_: &crate::Effects, input: &str) -> Result<String, String> {
    crate::tasks::g::submission::reload(input.trim())
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

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
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
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
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
        let Some(spec) = crate::watchers::answered_spec(view, ev) else { return vec![] };
        let ok = ev.kind.tag == EFFECT_SUCCEEDED;
        let (next, acts) = match (carry.state, spec.name.as_str()) {
            (_, OBSERVE) if ok => match serde_json::from_str::<Snapshot>(&ev.kind.body) {
                Ok(snap) => decide(&carry, &snap),
                Err(_) => return vec![],
            },
            (State::Renewing, CERT_RENEW) => {
                let Ok(snap) = serde_json::from_str::<Snapshot>(&spec.input) else { return vec![] };
                if !ok {
                    (Carry { state: State::Idle, ..carry.clone() }, vec![])
                } else {
                    let r = renewal_of(&ev.kind.body);
                    let (mut next, mut acts) = after_renewal_with(&carry, &snap, r.renewed);
                    if let Some(n) = r.note {
                        note(&mut next, &mut acts, n);
                    }
                    (next, acts)
                }
            }
            (State::Setting, CONFIG_SET) if sets_done(view, ev) => {
                // The restart reloads the scheduler's own submission source.
                let snap = Snapshot { day: the_day(ev.at as i64 / 1000), ..Snapshot::default() };
                (Carry { state: State::Restarting, ..carry.clone() }, vec![restart(&snap)])
            }
            (State::Restarting, UNIT_RESTART) => (Carry { state: State::Idle, ..carry.clone() }, vec![]),
            _ => return vec![],
        };
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
