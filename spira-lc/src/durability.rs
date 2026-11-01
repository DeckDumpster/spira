//! The control store's off-box copy as spira-lc runs it: lc-serve's shipper thread, and the
//! operator's `admin-restore` and `admin-drill`, which read local files only.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use runtime::ports::{Clock, SysProc, WallClock};
use spira_otel::metrics::{MetricsSink, Point};
use state_store_sqlite::durability::drill::{self, Exclusions};
use state_store_sqlite::durability::put::{Dest, S3Creds, Uploader};
use state_store_sqlite::durability::restore;
use state_store_sqlite::durability::ship::{self, ShipConfig, Shipper};
use state_store_sqlite::SqliteBackend;

use crate::{CANNOT_TELL, REFUSED};

pub const GAUGE: &str = "spira_lc_ship_oldest_unshipped_seconds";
/// The watchd `log` row's file, under `SPIRA_RUN`.
pub const ALARM_LOG: &str = "lc-ship.log";
const SNAPSHOT_PAGES: i32 = 256;
const MAX_MAINS: usize = 10_000;
// batch-job: one upload, bounded by the object's size, not a round trip
const PUT_DEADLINE: Duration = Duration::from_secs(300);

pub struct Shipping {
    pub shipper: Shipper,
    pub every: Duration,
    pub max_age_ms: u64,
    pub alarm_log: PathBuf,
}

fn cfg_u64(key: &str) -> Result<u64, String> {
    spira_config::process::cfg_parse::<u64>(key)
}

/// The shipper the config declares: `None` while `SPIRA_LC_BACKUP_URL` is empty, so a store
/// with no destination ships nothing (and `lc-backup.sh age` stays red for it).
pub fn configured(backend: SqliteBackend, run: &Path) -> Option<Result<Shipping, String>> {
    let url = spira_config::process::cfg("SPIRA_LC_BACKUP_URL").ok().filter(|u| !u.is_empty())?;
    Some((|| {
        let every = cfg_u64("SPIRA_LC_SHIP_SECS")?;
        if !(1..=5).contains(&every) {
            return Err(format!("SPIRA_LC_SHIP_SECS is {every}; it ships every 1 to 5 seconds"));
        }
        let dest = Dest::parse(&url)?;
        let creds = match dest {
            Dest::S3 { .. } => Some(S3Creds::from_env()?),
            Dest::File(_) => None,
        };
        let put = Uploader { dest, creds, proc: Arc::new(SysProc), deadline: PUT_DEADLINE };
        let cfg = ShipConfig {
            dir: run.join("lc-ship"),
            max_mains: MAX_MAINS,
            snapshot_every_ms: cfg_u64("SPIRA_LC_SHIP_SNAPSHOT_SECS")?.saturating_mul(1_000),
            snapshot_pages: SNAPSHOT_PAGES,
        };
        Ok(Shipping {
            shipper: Shipper::open(backend, cfg, Arc::new(put), Arc::new(WallClock))?,
            every: Duration::from_secs(every),
            max_age_ms: cfg_u64("SPIRA_LC_SHIP_MAX_AGE_SECS")?.saturating_mul(1_000),
            alarm_log: run.join(ALARM_LOG),
        })
    })())
}

/// One shipper tick as lc-serve runs it: the age gauge every tick, and a line in the alarm
/// log on each crossing of `SPIRA_LC_SHIP_MAX_AGE_SECS`. Returns whether the alarm is red.
pub fn tick(s: &mut Shipping, was_red: bool, metrics: &MetricsSink, clock: &dyn Clock) -> bool {
    let oldest = match s.shipper.tick() {
        Ok(t) => {
            if let Some(f) = &t.failed {
                eprintln!("spira-lc serve: ship: {f}");
            }
            t.oldest_unshipped_ms
        }
        Err(e) => {
            eprintln!("spira-lc serve: ship: {e}");
            return was_red;
        }
    };
    let age = oldest.unwrap_or(0) as f64 / 1_000.0;
    metrics.push(&[Point::Gauge { name: GAUGE, value: age }], &[]);
    let (red, line) = ship::alarm(was_red, oldest, s.max_age_ms);
    if let Some(line) = line {
        let stamped = format!("{} lc-ship: {line}\n", clock.now_ms() / 1_000);
        let wrote = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&s.alarm_log)
            .and_then(|mut f| f.write_all(stamped.as_bytes()));
        if let Err(e) = wrote {
            eprintln!("spira-lc serve: {}: {e}", s.alarm_log.display());
        }
    }
    red
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

const RESTORE_USAGE: &str = "usage: spira-lc admin-restore --snapshot <file> --revlog <dir> --out <path>";

/// `admin-restore --snapshot <file> --revlog <dir> --out <path>`: exit 0 restored, 3 refused
/// (a gap, a damaged run, an existing `--out`), 2 usage.
pub fn admin_restore(args: &[String]) -> (i32, String) {
    let (Some(snap), Some(dir), Some(out)) = (flag(args, "--snapshot"), flag(args, "--revlog"), flag(args, "--out")) else {
        return (CANNOT_TELL, RESTORE_USAGE.into());
    };
    match restore::restore(Path::new(snap), Path::new(dir), Path::new(out)) {
        Ok(r) => (
            0,
            format!("admin-restore: {out} holds the snapshot at {} and {} runs replayed through {}", r.snapshot_rev, r.runs.len(), r.through),
        ),
        Err(e) => (REFUSED, format!("admin-restore: refused: {e}")),
    }
}

const DRILL_USAGE: &str = "usage: spira-lc admin-drill --scratch <dir> [--ship-dir <dir>]";

/// `admin-drill --scratch <dir> [--ship-dir <dir>]`: restores the local copies of the uploaded
/// objects into `--scratch` and folds and compares them. Exit 0 passed, 1 failed, 2 could not
/// run. `--ship-dir` defaults to `SPIRA_RUN/lc-ship`.
pub fn admin_drill(args: &[String], run: impl FnOnce() -> Result<PathBuf, String>) -> (i32, String) {
    let Some(scratch) = flag(args, "--scratch") else {
        return (CANNOT_TELL, DRILL_USAGE.into());
    };
    let dir = match flag(args, "--ship-dir") {
        Some(d) => PathBuf::from(d),
        None => match run() {
            Ok(r) => r.join("lc-ship"),
            Err(e) => return (CANNOT_TELL, format!("admin-drill: cannot tell: {e}")),
        },
    };
    match drill::drill(&dir, Path::new(scratch), Exclusions::CONTRACT) {
        Ok((_, rep)) => (if rep.passed() { 0 } else { 1 }, drill::render(&rep)),
        Err(e) => (CANNOT_TELL, format!("admin-drill: cannot tell: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use serde_json::json;
    use state_store::conformance as suite;
    use state_store::{Object, Request, Txn};
    use state_store_sqlite::durability::put::Put;
    use state_store_sqlite::durability::revlog::REVLOG;
    use state_store_sqlite::Options;
    use testkit::TempDir;

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    fn shipped(dir: &Path, clock: &VirtualClock) -> Shipper {
        let b = SqliteBackend::open(&dir.join("live.db"), Options::default()).unwrap();
        let c = suite::open(b.clone(), clock);
        let put: Arc<dyn Put> = Arc::new(Uploader { dest: Dest::File(dir.join("off")), creds: None, proc: Arc::new(SysProc), deadline: PUT_DEADLINE });
        let cfg = ShipConfig { dir: dir.join("lc-ship"), max_mains: 2, snapshot_every_ms: u64::MAX, snapshot_pages: 64 };
        let mut sh = Shipper::open(b, cfg, put, Arc::new(clock.clone())).unwrap();
        sh.tick().unwrap();
        for i in 0..6 {
            let t = Txn::new().put(&format!("lifecycle/sp-{i}"), Object::new(json!(i)));
            assert!(c.txn(&t, &Request::new(&format!("r{i}"), "t", "t.write")).unwrap().succeeded);
            sh.tick().unwrap();
        }
        sh
    }

    #[test]
    fn admin_restore_replays_every_run_and_refuses_a_gap_naming_it() {
        let dir = TempDir::new("lc-admin-restore");
        let clock = VirtualClock::new(1_000);
        let sh = shipped(dir.path(), &clock);
        let (_, snap) = drill::newest_snapshot(sh.dir()).unwrap();
        let revlog = sh.sent(REVLOG);
        let (snap, revlog) = (snap.display().to_string(), revlog.display().to_string());
        let out = dir.path().join("whole.db").display().to_string();
        let (rc, msg) = admin_restore(&s(&["--snapshot", &snap, "--revlog", &revlog, "--out", &out]));
        assert_eq!(rc, 0, "{msg}");
        assert!(msg.ends_with("6 runs replayed through 6"), "{msg}");

        std::fs::remove_file(sh.sent(REVLOG).join(state_store_sqlite::durability::revlog::run_name(3, 3))).unwrap();
        let out = dir.path().join("gap.db").display().to_string();
        let (rc, msg) = admin_restore(&s(&["--snapshot", &snap, "--revlog", &revlog, "--out", &out]));
        assert_eq!(rc, REFUSED);
        assert!(msg.starts_with("admin-restore: refused: gap: revisions 3..=3 are in no run"), "{msg}");
        assert_eq!(admin_restore(&s(&["--snapshot", &snap])).0, CANNOT_TELL);
    }

    #[test]
    fn admin_drill_passes_on_the_shipped_objects_and_prints_the_skew_procedure() {
        let dir = TempDir::new("lc-admin-drill");
        let clock = VirtualClock::new(1_000);
        let sh = shipped(dir.path(), &clock);
        let scratch = dir.path().join("scratch").display().to_string();
        let run = dir.path().to_path_buf();
        let (rc, out) = admin_drill(&s(&["--scratch", &scratch]), || Ok(run));
        assert_eq!(rc, 0, "{out}");
        assert!(out.starts_with("drill ok: snapshot at 0, restored through 6, 6 keys compared"), "{out}");
        assert!(out.contains("skew guard: after a restore: (1) restore bd"), "{out}");
        drop(sh);
        let (rc, out) = admin_drill(&s(&["--scratch", &scratch]), || Err("no run dir".into()));
        assert_eq!(rc, CANNOT_TELL, "{out}");
    }

    #[test]
    fn the_ship_tick_writes_one_alarm_line_per_crossing() {
        let dir = TempDir::new("lc-ship-alarm");
        let clock = VirtualClock::new(1_000);
        let b = SqliteBackend::open(&dir.path().join("live.db"), Options::default()).unwrap();
        let c = suite::open(b.clone(), &clock);
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        let put: Arc<dyn Put> = Arc::new(Uploader { dest: Dest::File(blocked), creds: None, proc: Arc::new(SysProc), deadline: PUT_DEADLINE });
        let cfg = ShipConfig { dir: dir.path().join("lc-ship"), max_mains: 100, snapshot_every_ms: u64::MAX, snapshot_pages: 64 };
        let mut s = Shipping {
            shipper: Shipper::open(b, cfg, put, Arc::new(clock.clone())).unwrap(),
            every: Duration::from_secs(1),
            max_age_ms: 10_000,
            alarm_log: dir.path().join(ALARM_LOG),
        };
        let t = Txn::new().put("k", Object::new(json!(1)));
        assert!(c.txn(&t, &Request::new("1", "t", "t.write")).unwrap().succeeded);
        let metrics = MetricsSink::new(None, "spira-lc", Duration::from_millis(10));
        let mut red = tick(&mut s, false, &metrics, &clock);
        assert!(!red);
        clock.advance(11_000);
        red = tick(&mut s, red, &metrics, &clock);
        red = tick(&mut s, red, &metrics, &clock);
        assert!(red);
        let log = std::fs::read_to_string(dir.path().join(ALARM_LOG)).unwrap();
        assert_eq!(log.lines().count(), 1, "{log}");
        assert!(log.contains("lc-ship: RED: the oldest unshipped control-store object has waited 11s, past 10s"), "{log}");
    }
}
