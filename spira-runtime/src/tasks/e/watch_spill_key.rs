//! # watch.spill_key — contract
//!
//! watchd row `round-spill-key` (`spira/watchers:157`), target `@SPIRA_HOME@/round-spill-key.sh watch --interval 3600`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! Catalogue differs: the children are not just `lib.sh` — each pass may fork `round-vm status --json` (`spira/round-spill-key.sh:64`), `round-vm ec2-probe --dry-run` (`:85`), `round-vm template-check` (`:103`) and `python3 -I` (`:66`). The task watches three conditions, not one: the tailscale key's expiry, whether an EC2 launch can succeed, and whether the local round template exists.
//!
//! ## Tick
//!
//! - Cadence 3600 s (`spira/watchers:157`; loop `:152-168`). No waking events. Single-flight; the probe leg can run minutes (see deadline), so a tick while one is outstanding is dropped.
//!
//! ## Observe
//!
//! `watch.spill_key.observe` returns:
//!
//! ```text
//! Snapshot {
//!   spill: String,
//!   template: Option<Ok | Missing { tail2 }>,                     // only when spill ∉ {ec2-always, ec2-only}
//!   launch: Option<RecentOk | RecentFail { reason } | Probed { rc, out_tail3, took_s }>,  // only when spill ∈ ec2*
//!   key_file: Option<String>, expires: Option<u64>, now: u64
//! }
//! ```
//!
//! - `template` from round-vm's own check: `round-vm template-check` (`:103`), rc 0|3 = ok (`:105`). Library call in-process.
//! - `launch`: first the round-vm pool record, the component that recorded the launches — `SPIRA_ROUND_VM_STATE_DIR/pool.json` (`:60-62`) or `round-vm status --json` when unset (`:64`) — reduced by the embedded Python (`:39-57`) to `ok | fail <reason> | none` within `SPIRA_ROUND_VM_EC2_LAUNCH_WINDOW`. Only on `none` does it run the dry-run probe `round-vm ec2-probe --dry-run` (`:85`, `round-vm/src/ec2.rs:333` `probe_dry`). The Python reduction moves into Rust over round-vm's pool type.
//! - `expires` from `<SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE>.expires` (`:123`), written by the operator who pastes the key. `took_s` feeds the adaptive deadline (`:83`).
//!
//! ## Decide
//!
//! - Carry `watch.spill_key.state` = `{ said: BTreeSet<String>, probe_latency_s: Option<u64> }` — today `run/watchd/round-spill-key.said` (one key per line, `:33-36`) and `run/watchd/round-spill-key.latency` (`:76`, `:88`).
//! - Keys and lines (all said once per key, `_rsk_once` `:32-37`):
//!   - `template-<cksum>` → "ROUND TEMPLATE: the local round pool cannot lease: …" (`:107`); template ok clears `template-*` (`:111-115`).
//!   - `probe-<cksum>` → "ROUND SPILL LAUNCH: a real EC2 launch failed; EC2 rounds cannot launch: …" (`:80`) or "…round-vm ec2-probe --dry-run failed…" (`:97`); `probe-timeout` → "ROUND SPILL LAUNCH: probe could not decide: …" (`:93`). Recent ok or probe ok clears `probe-*` (`:69-73`).
//!   - `nofile` (`:122`), `unrecorded` (`:125`), `expired-<exp>` (`:131`), `warn-<exp>` (`:133`, `SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS`, default 14).
//! - The probe deadline is `max(SPIRA_ROUND_VM_EC2_PROBE_DEADLINE, 4 × last latency)` (`:83`) — decide computes it from carry and passes it in the next observe's input.
//! - Fixtures to port: `spira/test-round-spill-key.sh` (169 lines; tier T1): warn once, expiry, unrecorded, spill off, recent-launch stand-in, probe timeout, template missing.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.spill_key.observe` | host (aws edge) | adaptive probe deadline + 10 s | (probe) | spill off → template leg only |
//! | `inbox.append` (shared) | host | 10 s | `spill_key/<said-key>` | — |
//!
//! Inbox lines, exactly as today (`spira/round-spill-key.sh:28`): `<YYYY-MM-DDTHH:MM:SSZ> [watch:round-spill-key] <message>` with the message texts above. The script writes each line to stdout → `run/watchd/round-spill-key.log` (`:26`) and straight to `SPIRA_CONCIERGE_INBOX` (`:27-28`). Readers of `run/watchd/round-spill-key.log`: none by name in the release or `run/concierge-notes`; watchd's generic walkers only. `run/watchd/round-spill-key.pending` (2026-10-14) shows `watchd notify` once saw an undrained actionable line; no cursor exists. The log copy is dropped.
//!
//! Health (`round-spill-key.sh health`, `:142-150`) → S13 `scheduler status`.
//!
//! ## Silence
//!
//! No key added or removed and latency unchanged: probe only, no `Emit`. A successful dry-run that only moves `probe_latency_s` by under 10 % should not emit either (new rule; today the latency file is rewritten every probe, `:88`).
//!
//! ## Replaces
//!
//! - `spira-watch-round-spill-key-prod.service`; row `spira/watchers:154-157`.
//! - Children: `bash lib.sh` (`:21`), `round-vm status|ec2-probe|template-check` (`:64`, `:85`, `:103`), `python3` (`:66`), `timeout`, `cksum`, `tr`, `grep`.
//! - Files retired: `run/watchd/round-spill-key.{said,latency,health,log,pending,restarts}`.
//! - Config pinned: `SPIRA_ROUND_VM_SPILL`, `SPIRA_ROUND_VM_STATE_DIR`, `SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE` (`spira/conf.d.index:3445`), `SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS` (`:3359`), `SPIRA_ROUND_VM_EC2_LAUNCH_WINDOW` (`:3367`), `SPIRA_ROUND_VM_EC2_PROBE_DEADLINE` (`:3391`).
//!
//! ## Reverse
//!
//! Release rollback restores unit and row; `Observe` mode shadows `inbox.append`.

use std::collections::BTreeSet;

use round_vm::machine::{PoolEvent, VmState};
use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;
use crate::watchers::{carried, INBOX_APPEND};

pub const NAME: &str = "watch.spill_key";
pub const STREAM: &str = "watch.spill_key";
pub const TICK: EventKindTag = "watch.spill_key.tick";
pub const STATE: EventKindTag = "watch.spill_key.state";
pub const OBSERVE: &str = "watch.spill_key.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_ROUND_VM_SPILL", "SPIRA_ROUND_VM_STATE_DIR", "SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE", "SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS", "SPIRA_ROUND_VM_EC2_LAUNCH_WINDOW", "SPIRA_ROUND_VM_EC2_PROBE_DEADLINE", "SPIRA_CONCIERGE_INBOX"];
/// The row's tag (`spira/round-spill-key.sh:28`).
pub const INBOX_TAG: &str = "round-spill-key";
/// `--interval 3600`.
pub const CADENCE_MS: u64 = 3_600_000;
/// Added to the adaptive probe deadline for the observe as a whole.
const OBSERVE_SLACK_MS: u64 = 10_000;
const INBOX_DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub spill: String,
    pub state_dir: String,
    pub key_file: String,
    pub warn_days: u64,
    pub launch_window_s: u64,
    pub probe_deadline_s: u64,
    pub inbox: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config {
            spill: cfg("SPIRA_ROUND_VM_SPILL")?,
            state_dir: cfg("SPIRA_ROUND_VM_STATE_DIR")?,
            key_file: cfg("SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE")?,
            warn_days: cfg_parse("SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS")?,
            launch_window_s: cfg_parse("SPIRA_ROUND_VM_EC2_LAUNCH_WINDOW")?,
            probe_deadline_s: cfg_parse("SPIRA_ROUND_VM_EC2_PROBE_DEADLINE")?,
            inbox: cfg("SPIRA_CONCIERGE_INBOX")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Template {
    Ok,
    Missing { tail2: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Launch {
    RecentOk,
    RecentFail { reason: String },
    Probed { rc: i32, out_tail3: String, took_s: u64 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub spill: String,
    /// Only when spill is not `ec2-always` / `ec2-only`.
    pub template: Option<Template>,
    /// Only when spill is an `ec2*` mode.
    pub launch: Option<Launch>,
    pub key_file: Option<String>,
    pub expires: Option<u64>,
    pub now: u64,
}

/// `{ said, probe_latency_s }`: today's `.said` keys and `.latency`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub said: BTreeSet<String>,
    pub probe_latency_s: Option<u64>,
}

/// The probe deadline the next observe gets: `max(PROBE_DEADLINE, 4 × last latency)`, in seconds.
pub fn probe_deadline_s(carry: &Carry, cfg: &Config) -> u64 {
    cfg.probe_deadline_s.max(4 * carry.probe_latency_s.unwrap_or(0))
}

/// Each key family said once; `template-*` and `probe-*` cleared when the condition clears.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let (mut next, mut acts) = (carry.clone(), vec![]);
    let mut once = |next: &mut Carry, key: String, message: String| {
        if next.said.insert(key.clone()) {
            acts.push(EffectSpec::keyed(INBOX_APPEND, format!("spill_key/{key}"), pattern::inbox_line(now_ms, INBOX_TAG, &message), INBOX_DEADLINE_MS));
        }
    };
    let clear = |next: &mut Carry, prefix: &str| next.said.retain(|k| !k.starts_with(prefix));
    match &snap.template {
        Some(Template::Ok) => clear(&mut next, "template-"),
        Some(Template::Missing { tail2 }) => once(&mut next, format!("template-{}", cksum(tail2.as_bytes())), format!("ROUND TEMPLATE: the local round pool cannot lease: {tail2}")),
        None => {}
    }
    if !matches!(snap.spill.as_str(), "ec2" | "ec2-always" | "ec2-only") {
        return (next, acts);
    }
    match &snap.launch {
        Some(Launch::RecentOk) => clear(&mut next, "probe-"),
        Some(Launch::RecentFail { reason }) => once(&mut next, format!("probe-{}", cksum(format!("fail {reason}").as_bytes())), format!("ROUND SPILL LAUNCH: a real EC2 launch failed; EC2 rounds cannot launch: {reason}")),
        Some(Launch::Probed { rc: 0, took_s, .. }) => {
            // A latency that moved under 10 % is not news: the carry keeps the old one.
            if next.probe_latency_s.is_none_or(|old| took_s.abs_diff(old) * 10 >= old.max(1)) {
                next.probe_latency_s = Some(*took_s);
            }
            clear(&mut next, "probe-");
        }
        Some(Launch::Probed { rc: 124, .. }) => once(&mut next, "probe-timeout".into(), format!("ROUND SPILL LAUNCH: probe could not decide: round-vm ec2-probe --dry-run gave no answer in {}s and no real EC2 launch is recorded in the last {}s", probe_deadline_s(carry, cfg), cfg.launch_window_s)),
        Some(Launch::Probed { out_tail3, .. }) => once(&mut next, format!("probe-{}", cksum(out_tail3.as_bytes())), format!("ROUND SPILL LAUNCH: round-vm ec2-probe --dry-run failed; EC2 rounds cannot launch: {out_tail3}")),
        None => {}
    }
    let Some(file) = snap.key_file.as_deref().filter(|f| !f.is_empty()) else {
        once(&mut next, "nofile".into(), "ROUND SPILL KEY: SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE is not set".into());
        return (next, acts);
    };
    let Some(exp) = snap.expires else {
        once(&mut next, "unrecorded".into(), format!("ROUND SPILL KEY: no expiry recorded in {file}.expires; record the key's expiry (epoch seconds) beside it"));
        return (next, acts);
    };
    let left = exp as i64 - snap.now as i64;
    if left <= 0 {
        once(&mut next, format!("expired-{exp}"), format!("ROUND SPILL KEY: the tailscale auth key in {file} EXPIRED; spilled passes cannot join the tailnet until it is replaced"));
    } else if left <= cfg.warn_days as i64 * 86_400 {
        once(&mut next, format!("warn-{exp}"), format!("ROUND SPILL KEY: the tailscale auth key in {file} expires in {} days; replace it and record the new expiry in {file}.expires", left / 86_400));
    }
    (next, acts)
}

/// POSIX `cksum`'s CRC over `data` (its length folded in), the number the script keyed on.
pub fn cksum(data: &[u8]) -> u32 {
    const fn table() -> [u32; 256] {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = (i as u32) << 24;
            let mut k = 0;
            while k < 8 {
                c = if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04C1_1DB7 } else { c << 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    }
    const T: [u32; 256] = table();
    let step = |crc: u32, b: u8| (crc << 8) ^ T[((crc >> 24) ^ u32::from(b)) as usize & 0xff];
    let mut crc = data.iter().fold(0, |c, b| step(c, *b));
    let mut len = data.len();
    while len > 0 {
        crc = step(crc, (len & 0xff) as u8);
        len >>= 8;
    }
    !crc
}

/// `_RSK_LAUNCHES` over round-vm's pool record: the newest good EC2 lease within the window, or
/// the newest EC2 VM that never leased and was destroyed or doomed, whichever is later; `None`
/// when neither is in the window.
pub fn recent_launch(events: &[PoolEvent], since_s: u64) -> Option<Launch> {
    let ec2: Vec<&PoolEvent> = events.iter().filter(|e| e.vm.starts_with("ec2-")).collect();
    let ok = ec2.iter().filter(|e| e.to == VmState::Leased && e.reason.contains("(provider: ec2)") && e.at >= since_s).map(|e| e.at).max();
    let vms: BTreeSet<&str> = ec2.iter().map(|e| e.vm.as_str()).collect();
    let mut bad: Option<&PoolEvent> = None;
    for vm in vms {
        let mine: Vec<&&PoolEvent> = ec2.iter().filter(|e| e.vm == vm).collect();
        if mine.iter().any(|e| e.to == VmState::Leased) || !mine.iter().any(|e| matches!(e.to, VmState::Destroyed | VmState::Doomed)) {
            continue;
        }
        let last = mine[mine.len() - 1];
        if last.at >= since_s && bad.is_none_or(|b| last.at > b.at) {
            bad = Some(last);
        }
    }
    match (bad, ok) {
        (Some(b), ok) if ok.is_none_or(|ok| b.at > ok) => Some(Launch::RecentFail { reason: b.reason.clone() }),
        (_, Some(_)) => Some(Launch::RecentOk),
        _ => None,
    }
}

/// The last `n` lines of `out` joined by spaces, as `tail -n N | tr '\n' ' '` left them.
fn tail_joined(out: &str, n: usize) -> String {
    let lines: Vec<&str> = out.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| format!("{l} ")).collect()
}

/// `round-vm ec2-probe --dry-run` under `deadline_s`, on its own thread: 124 when it gave no
/// answer in time, as `timeout` reported it.
fn probe(deadline_s: u64) -> Launch {
    let t0 = std::time::Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(round_vm::cli::ec2_probe_answer(true));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(deadline_s)) {
        Ok((rc, out)) => Launch::Probed { rc, out_tail3: tail_joined(&out, 3), took_s: t0.elapsed().as_secs() },
        Err(_) => Launch::Probed { rc: 124, out_tail3: String::new(), took_s: deadline_s },
    }
}

/// The observe runner: the template leg while the local pool is in use, the launch record (and
/// the dry-run probe only when it holds nothing in the window) and the key's recorded expiry
/// while spill is an EC2 mode. `input` is the probe deadline in seconds.
pub fn observe(input: &str) -> Result<String, String> {
    let cfg = Config::pinned()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let deadline_s = input.trim().parse().unwrap_or(cfg.probe_deadline_s);
    let template = (!matches!(cfg.spill.as_str(), "ec2-always" | "ec2-only")).then(|| match round_vm::cli::template_check_answer() {
        (0 | 3, _) => Template::Ok,
        (_, out) => Template::Missing { tail2: tail_joined(&out, 2) },
    });
    let ec2 = matches!(cfg.spill.as_str(), "ec2" | "ec2-always" | "ec2-only");
    let launch = ec2.then(|| {
        let dir = if cfg.state_dir.is_empty() { round_vm::config::Config::load().map(|c| c.state_dir).ok() } else { Some(cfg.state_dir.clone().into()) };
        let events = dir.and_then(|d| round_vm::pool::read_state(&d.join("pool.json")).ok()).map(|s| s.events).unwrap_or_default();
        recent_launch(&events, now.saturating_sub(cfg.launch_window_s)).unwrap_or_else(|| probe(deadline_s))
    });
    let key_file = (ec2 && !cfg.key_file.is_empty()).then(|| cfg.key_file.clone());
    let expires = key_file.as_ref().and_then(|f| std::fs::read_to_string(format!("{f}.expires")).ok()).map(|t| t.split_whitespace().collect::<String>()).filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())).and_then(|t| t.parse().ok());
    serde_json::to_string(&Snapshot { spill: cfg.spill, template, launch, key_file, expires, now }).map_err(|e| e.to_string())
}

pub struct WatchSpillKey {
    pub config: Result<Config, String>,
}

impl WatchSpillKey {
    pub fn new(config: Config) -> Self {
        WatchSpillKey { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchSpillKey { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.spill_key/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("said"), runtime::rows::Part::Field("probe_latency_s")];

impl Handler for WatchSpillKey {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let next = Command::Timer { at: ev.at + CADENCE_MS, kind: EventKind::new(TICK, "") };
            let config = match &self.config {
                Ok(c) => c,
                Err(e) => {
                    ctx.event().set("config.refused", e.as_str());
                    return vec![next];
                }
            };
            let deadline_s = probe_deadline_s(&carried::<Carry>(view, &ev.stream, STATE), config);
            return pattern::tick(ev, OBSERVE, deadline_s.to_string(), deadline_s * 1000 + OBSERVE_SLACK_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cksum_is_posix_cksum() {
        assert_eq!(cksum(b""), 4_294_967_295);
        assert_eq!(cksum(b"abc"), 1_219_131_554);
        assert_eq!(cksum(b"fail InsufficientInstanceCapacity"), 3_904_663_009);
    }

    fn ev(vm: &str, to: VmState, reason: &str, at: u64) -> PoolEvent {
        PoolEvent { vm: vm.into(), to, reason: reason.into(), at }
    }

    #[test]
    fn the_launch_record_reduces_as_the_python_did() {
        let lease = ev("ec2-a", VmState::Leased, "leased (provider: ec2)", 100);
        let dead = [ev("ec2-b", VmState::Provisioning, "boot", 110), ev("ec2-b", VmState::Destroyed, "InsufficientInstanceCapacity", 120)];
        assert_eq!(recent_launch(std::slice::from_ref(&lease), 50), Some(Launch::RecentOk));
        assert_eq!(recent_launch(&[lease.clone(), dead[0].clone(), dead[1].clone()], 50), Some(Launch::RecentFail { reason: "InsufficientInstanceCapacity".into() }));
        assert_eq!(recent_launch(std::slice::from_ref(&lease), 200), None, "outside the window");
        assert_eq!(recent_launch(&[ev("pve-1", VmState::Destroyed, "x", 100)], 0), None, "local VMs do not count");
    }
}
