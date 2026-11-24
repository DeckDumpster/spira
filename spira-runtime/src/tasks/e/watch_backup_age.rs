//! # watch.backup_age — contract
//!
//! watchd row `lc-backup` (`spira/watchers:91`), target `@SPIRA_HOME@/lc-backup.sh watch --interval 900`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]). Only the age alarm moves; the backup itself (`lc-backup.timer` → `lc-backup.sh run`) and the weekly drill (`lc-backup-drill.timer`) are separate installed units and are not this task.
//!
//! Catalogue differs: the watch loop forks no `aws`. It reads local stamp files and mails the operator (`spira/lc-backup.sh:328-337`, `mail send operator --kind alert`); it never writes the Concierge inbox. Children: `lib.sh`, `mail`, `date`, `tr`, `grep`.
//!
//! ## Tick
//!
//! - Cadence 900 s (`spira/watchers:91`; loop `:367-385`). Natural extra wake: `lc-backup.service` finishing (a `unit.ran` from S6/S9) (unverified wiring). Single-flight.
//!
//! ## Observe
//!
//! `watch.backup_age.observe` (host class) returns:
//!
//! ```text
//! Snapshot { dbs: Vec<(name, Ok{age} | Stale{age} | Never | Unreadable)> } | NoDestination | NoDatabase
//! ```
//!
//! - `NoDestination` when `SPIRA_LC_BACKUP_URL` is empty (`:174`); `NoDatabase` when `SPIRA_LC_BACKUP_DB` names none (`:175`).
//! - Per db, the stamp `$SPIRA_RUN/lc-backup.<db>.ok` (`:52`), whose first field is the epoch of the last good upload. It is written only by the backup run after a successful `spira-lc admin-dolt-ship` (`:117-120`), so it is the backup component's own record of success, not a re-derivation. Absent → `Never` (`:162`); non-numeric → `Unreadable` (`:164`); age > `SPIRA_LC_BACKUP_MAX_AGE` → `Stale` (`:167`).
//!
//! Each store also gets a `drill:<db>` entry from `$SPIRA_RUN/lc-backup/<db>/drill.ok`, the epoch the weekly drill last passed (a failed drill truncates `drill.verdict`, so the stamp is kept apart). Past `DRILL_STALE_S` (eight days: one weekly slot plus a day) it is `Stale`; ages under that are `Ok`; no stamp is `Never`, so a store whose drill has never passed is red. With `SPIRA_LC_STORE_PATH` set, the control store's drill is watched the same way as `drill:control` from `lc-backup/control/drill.ok`.
//!
//! ## Decide
//!
//! - Aggregate: `ok` only when every db is `Ok` (`_lcb_all_ok`, `:184`); `NoDestination`, `NoDatabase`, `Never`, `Stale`, `Unreadable` are all red (`:340-341`).
//! - Carry `watch.backup_age.state` = `Ok | Red` — today `run/watchd/lc-backup.state` (`:47`, rewritten each tick `:350-351`).
//! - `Red` and prev ≠ `Red` → "RED: a store has no good backup within <MAX>s (<db: state;…>) — losing its disk now loses its data and history" (`:346`).
//! - `Ok` and prev = `Red` → "CLEAR: every store has a fresh backup again (<db: state;…>)" (`:348`).
//! - Fixtures: `lc-backup.sh age --now EPOCH` (`:186-198`) is the seam. `spira-lc/tests/lc_backup_drill.rs` covers the run, drill deferral and registry copy, not the watch tick. New: all ok; one stale; never; unreadable stamp; no destination; crossing and clear.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.backup_age.observe` | host | 5 s | (probe) | — |
//! | `watch.backup_age.mail` | store (mail through the bead store) | 10 s | `backup_age/<state event id>` | empty text |
//!
//! Mail today (`lc-backup.sh:330-336`): mailbox `operator`, From `LC Backup <lc-backup@spira>`, Subject `lc-backup: <line>`, kind `alert`, body `## Alert\n<line>`. The runtime's `operator.note` runner (`spira-runtime/src/lib.rs:86`) fixes kind `note`, From `PR Notify` and `digest: true`, so it cannot carry an alert; this is its own runner on `mail::cmds::send`, or `operator.note` generalised to take kind and From (an S2 shared change; decision).
//!
//! This row has no inbox line today and gains none: it alerts the operator, not the Concierge. The stdout copy (`:329`) lands in `run/watchd/lc-backup.log`. Readers of that log: none by name in the release or `run/concierge-notes` (hits for `lc-backup.log` are the backup run's own `$SPIRA_RUN/lc-backup.log`, a different file, `:138`); watchd's generic walkers only; no cursor exists. Dropped.
//!
//! Health (`lc-backup.sh health`, `:354-365`) → S13 `scheduler status`.
//!
//! ## Silence
//!
//! State unchanged → probe only; no `Emit` (today the state file is rewritten every tick), no mail.
//!
//! ## Replaces
//!
//! - `spira-watch-lc-backup-prod.service`; row `spira/watchers:89-91`.
//! - Children: `bash lib.sh` (`:45`), `mail send` (`:330`), `date`, `tr`, `grep`.
//! - Files retired: `run/watchd/lc-backup.{state,health,log,restarts}`. Kept: `run/lc-backup.<db>.ok` (written by the run).
//! - Config pinned: `SPIRA_LC_BACKUP_URL`, `SPIRA_LC_BACKUP_DB`, `SPIRA_LC_BACKUP_MAX_AGE` (default 50400), `SPIRA_LC_STORE_PATH`, `SPIRA_RUN`.
//!
//! ## Reverse
//!
//! Release rollback restores unit and row; `Observe` mode shadows the mail.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;
use crate::tasks::e::alert::Alert;

pub const NAME: &str = "watch.backup_age";
pub const STREAM: &str = "watch.backup_age";
pub const TICK: EventKindTag = "watch.backup_age.tick";
pub const STATE: EventKindTag = "watch.backup_age.state";
pub const OBSERVE: &str = "watch.backup_age.observe";
pub const MAIL: &str = "watch.backup_age.mail";
pub const EFFECTS: &[&str] = &[OBSERVE, MAIL];
pub const KEYS: &[&str] = &["SPIRA_LC_BACKUP_URL", "SPIRA_LC_BACKUP_DB", "SPIRA_LC_BACKUP_MAX_AGE", "SPIRA_LC_STORE_PATH", "SPIRA_RUN"];
/// `--interval 900`.
pub const CADENCE_MS: u64 = 900_000;
const OBSERVE_DEADLINE_MS: u64 = 5_000;
const MAIL_DEADLINE_MS: u64 = 10_000;
/// The mail as `lc-backup.sh:330-336` sent it.
pub const MAILBOX: &str = "operator";
pub const FROM: &str = "LC Backup <lc-backup@spira>";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub url: String,
    pub db: String,
    pub max_age_s: u64,
    /// The control store's path; non-empty when this box runs one, so its drill is watched too.
    pub lc_store_path: String,
    pub run: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { url: cfg("SPIRA_LC_BACKUP_URL")?, db: cfg("SPIRA_LC_BACKUP_DB")?, max_age_s: cfg_parse("SPIRA_LC_BACKUP_MAX_AGE")?, lc_store_path: cfg("SPIRA_LC_STORE_PATH")?, run: cfg("SPIRA_RUN")? })
    }
}

/// One store's last good upload, from its `lc-backup.<db>.ok` stamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Db {
    Ok { age: u64 },
    Stale { age: u64 },
    Never,
    Unreadable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Snapshot {
    Dbs(Vec<(String, Db)>),
    NoDestination,
    NoDatabase,
}

/// Today's `run/watchd/lc-backup.state`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Carry {
    #[default]
    Ok,
    Red,
}

/// `Ok` only when every store is; a crossing to `Red` mails RED, the way back mails CLEAR.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let state = if all_ok(snap) { Carry::Ok } else { Carry::Red };
    let out = measured(snap);
    let drill_late = matches!(snap, Snapshot::Dbs(dbs) if dbs.iter().any(|(n, d)| n.starts_with("drill:") && !matches!(d, Db::Ok { .. })));
    let line = match (state, carry) {
        (Carry::Red, Carry::Ok) if drill_late => format!("RED: a store's restore drill has not passed within {DRILL_STALE_S}s ({out}) — its backups are unproven to restore"),
        (Carry::Red, Carry::Ok) => format!("RED: a store has no good backup within {}s ({out}) — losing its disk now loses its data and history", cfg.max_age_s),
        (Carry::Ok, Carry::Red) => format!("CLEAR: every store has a fresh backup again ({out})"),
        _ => return (state, vec![]),
    };
    (state, vec![Alert::of(MAILBOX, FROM, "lc-backup", "alert", &line).effect(MAIL, format!("{NAME}:{now_ms}"), MAIL_DEADLINE_MS)])
}

/// `_lcb_all_ok`: every store measured, and every one `ok`.
fn all_ok(snap: &Snapshot) -> bool {
    matches!(snap, Snapshot::Dbs(dbs) if dbs.iter().all(|(_, d)| matches!(d, Db::Ok { .. })))
}

/// `_lcb_measure`'s lines joined by `;`, as `_lcb_tick` folded them; one unreadable stamp makes
/// the whole measure `unreadable`, as its non-zero exit did.
pub fn measured(snap: &Snapshot) -> String {
    match snap {
        Snapshot::NoDestination => "no-destination".into(),
        Snapshot::NoDatabase => "no-database".into(),
        Snapshot::Dbs(dbs) if dbs.iter().any(|(_, d)| *d == Db::Unreadable) => "unreadable".into(),
        Snapshot::Dbs(dbs) => dbs
            .iter()
            .map(|(name, d)| match d {
                Db::Ok { age } => format!("{name}: ok {age}"),
                Db::Stale { age } => format!("{name}: stale {age}"),
                Db::Never => format!("{name}: never"),
                Db::Unreadable => format!("{name}: unreadable"),
            })
            .collect::<Vec<_>>()
            .join(";"),
    }
}

/// `_lcb_measure_db`: the stamp's first field against `now_s`.
pub fn measure_db(stamp: Option<&str>, now_s: u64, max_age_s: u64) -> Db {
    let Some(text) = stamp else { return Db::Never };
    match text.split_whitespace().next().filter(|t| t.bytes().all(|b| b.is_ascii_digit())).and_then(|t| t.parse::<u64>().ok()) {
        None => Db::Unreadable,
        Some(t) if now_s.saturating_sub(t) > max_age_s => Db::Stale { age: now_s - t },
        Some(t) => Db::Ok { age: now_s.saturating_sub(t) },
    }
}

/// The weekly drill is stale past eight days: one weekly slot plus a day.
pub const DRILL_STALE_S: u64 = 8 * 24 * 3600;

/// `lc-backup/<db>/drill.ok`, the epoch a drill last passed (kept across a failed drill, which
/// truncates `drill.verdict`), against `now_s`.
pub fn measure_drill(stamp: Option<&str>, now_s: u64) -> Db {
    measure_db(stamp, now_s, DRILL_STALE_S)
}

/// The observe runner: each configured store's `lc-backup.<db>.ok` stamp under the run dir.
pub fn observe() -> Result<String, String> {
    let now_s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    serde_json::to_string(&observe_at(&Config::pinned()?, now_s)).map_err(|e| e.to_string())
}

/// What the stamps under `cfg.run` say at `now_s`.
pub fn observe_at(cfg: &Config, now_s: u64) -> Snapshot {
    let dbs: Vec<&str> = cfg.db.split_whitespace().collect();
    if cfg.url.is_empty() {
        return Snapshot::NoDestination;
    }
    if dbs.is_empty() {
        return Snapshot::NoDatabase;
    }
    let read = |rel: String| std::fs::read_to_string(std::path::Path::new(&cfg.run).join(rel)).ok();
    let backups = dbs.iter().map(|db| (db.to_string(), measure_db(read(format!("lc-backup.{db}.ok")).as_deref(), now_s, cfg.max_age_s)));
    let drills = dbs.iter().map(|db| (format!("drill:{db}"), measure_drill(read(format!("lc-backup/{db}/drill.ok")).as_deref(), now_s)));
    let control = (!cfg.lc_store_path.is_empty()).then(|| ("drill:control".to_string(), measure_drill(read("lc-backup/control/drill.ok".into()).as_deref(), now_s)));
    Snapshot::Dbs(backups.chain(drills).chain(control).collect())
}

pub struct WatchBackupAge {
    pub config: Result<Config, String>,
}

impl WatchBackupAge {
    pub fn new(config: Config) -> Self {
        WatchBackupAge { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchBackupAge { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.backup_age/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(RowCodec { stream: STREAM, also: &[], tag: STATE, split: runtime::rows::split_whole, join: runtime::rows::join_whole })
    }
}

impl Handler for WatchBackupAge {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;

    #[test_id(tbi6lu7)]
    #[test]
    fn a_drill_is_fresh_to_eight_days_stale_past_it_and_never_without_a_stamp() {
        let now = 10_000_000;
        let at = |age: u64| format!("{}\n", now - age);
        assert_eq!(measure_drill(Some(&at(7 * 24 * H + H)), now), Db::Ok { age: 7 * 24 * H + H });
        assert_eq!(measure_drill(Some(&at(8 * 24 * H)), now), Db::Ok { age: 8 * 24 * H });
        assert_eq!(measure_drill(Some(&at(8 * 24 * H + 1)), now), Db::Stale { age: 8 * 24 * H + 1 });
        assert_eq!(measure_drill(None, now), Db::Never);
        assert_eq!(measure_drill(Some("junk"), now), Db::Unreadable);
    }

    #[test_id(tdi1wev)]
    #[test]
    fn a_stale_drill_turns_the_aggregate_red() {
        let snap = Snapshot::Dbs(vec![("a".into(), Db::Ok { age: 1 }), ("drill:a".into(), Db::Stale { age: 50 * H })]);
        let cfg = Config { url: "u".into(), db: "a".into(), max_age_s: 1, lc_store_path: String::new(), run: "r".into() };
        let (carry, fx) = decide(&Carry::Ok, &snap, 0, &cfg);
        assert_eq!((carry, fx.len()), (Carry::Red, 1));
    }

    fn stamped(files: &[(&str, u64)], store: &str) -> Vec<(String, Db)> {
        let dir = testkit::TempDir::new("backup-age-observe");
        for (rel, at) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, format!("{at}\n")).unwrap();
        }
        let cfg = Config { url: "s3://b/lc".into(), db: "spira".into(), max_age_s: 50_400, lc_store_path: store.into(), run: dir.path().display().to_string() };
        match observe_at(&cfg, 10_000_000) {
            Snapshot::Dbs(v) => v,
            other => panic!("{other:?}"),
        }
    }

    #[test_id(tfdweq7)]
    #[test]
    fn observe_at_reports_the_control_drill_when_the_store_is_configured() {
        let found = |v: &[(String, Db)]| v.iter().find(|(n, _)| n == "drill:control").map(|(_, d)| d.clone());
        let fresh = 10_000_000 - 3600;
        let present = stamped(&[("lc-backup/control/drill.ok", fresh)], "store");
        assert_eq!(found(&present), Some(Db::Ok { age: 3600 }));
        assert_eq!(found(&stamped(&[], "store")), Some(Db::Never));
        assert_eq!(found(&stamped(&[("lc-backup/control/drill.ok", fresh)], "")), None);
        let stale = stamped(&[("lc-backup/control/drill.ok", 10_000_000 - 9 * 24 * H)], "store");
        assert!(matches!(found(&stale), Some(Db::Stale { .. })));
        let cfg = Config { url: "u".into(), db: "spira".into(), max_age_s: 1, lc_store_path: "store".into(), run: "r".into() };
        let (carry, _) = decide(&Carry::Ok, &Snapshot::Dbs(stale), 0, &cfg);
        assert_eq!(carry, Carry::Red);
    }
}
