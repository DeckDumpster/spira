//! The control store's off-box copy as spira-lc runs it: lc-serve's shipper thread and its snapshot thread, and the
//! operator's `admin-restore` and `admin-drill`, which read local files only; and lc-backup's
//! put-only upload of a dolt store, `admin-dolt-ship`, with `admin-dolt-assemble` for its restore.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use runtime::ports::WallClock;
use spira_otel::metrics::{MetricsSink, Point};
use state_store_sqlite::durability::dolt;
use state_store_sqlite::durability::drill::{self, Exclusions};
use state_store_sqlite::durability::put::{Dest, PutNew, S3Creds, Uploader, MAX_PUT_BYTES};
use state_store_sqlite::durability::restore;
use state_store_sqlite::durability::ship::{self, ShipConfig, Shipper, Snapshotter};
use state_store_sqlite::SqliteBackend;

use crate::{CANNOT_TELL, REFUSED};

pub const GAUGE: &str = "spira_lc_ship_oldest_unshipped_seconds";
const MAX_MAINS: usize = 10_000;

pub struct Shipping {
    pub shipper: Shipper,
    pub snapshotter: Snapshotter,
    pub every: Duration,
}

/// `SPIRA_LC_SHIP_DIR`, or `SPIRA_RUN/lc-ship` while it is unset.
fn ship_dir(run: &Path) -> PathBuf {
    ship_dir_at(spira_config::process::cfg("SPIRA_LC_SHIP_DIR").ok(), run)
}

fn ship_dir_at(configured: Option<String>, run: &Path) -> PathBuf {
    ship::dir_at(configured, run)
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
        let snapshot_secs = cfg_u64("SPIRA_LC_SHIP_SNAPSHOT_SECS")?;
        let max_age_secs = cfg_u64("SPIRA_LC_SHIP_MAX_AGE_SECS")?;
        staging(backend, ship_dir(run), &url, (every, snapshot_secs, max_age_secs))
    })())
}

/// The shipper for a destination: it stages in `dir` and uploads nothing, because lc-serve
/// starts no process; `ship::drain`, run by the scheduler's `lc.ship` task, uploads.
fn staging(backend: SqliteBackend, dir: PathBuf, url: &str, (every, snapshot_secs, max_age_secs): (u64, u64, u64)) -> Result<Shipping, String> {
    if !(1..=5).contains(&every) {
        return Err(format!("SPIRA_LC_SHIP_SECS is {every}; it ships every 1 to 5 seconds"));
    }
    Dest::parse(url)?;
    let cfg = ShipConfig {
        dir,
        max_mains: MAX_MAINS,
        snapshot_every_ms: snapshot_secs.saturating_mul(1_000),
        stage_every_ms: ship::stage_every_ms(every, max_age_secs),
    };
    let shipper = Shipper::open_staging(backend, cfg, Arc::new(WallClock))?;
    Ok(Shipping {
        snapshotter: shipper.snapshotter(),
        shipper,
        every: Duration::from_secs(every),
    })
}

/// One shipper tick as lc-serve runs it: the age gauge, which the `watch.off_box` task does not
/// read; it measures what was uploaded.
pub fn tick(s: &mut Shipping, metrics: &MetricsSink) {
    let oldest = match s.shipper.tick() {
        Ok(t) => {
            if let Some(f) = &t.failed {
                eprintln!("spira-lc serve: ship: {f}");
            }
            t.oldest_unshipped_ms
        }
        Err(e) => {
            eprintln!("spira-lc serve: ship: {e}");
            s.shipper.lag_ms().unwrap_or_else(|e| {
                eprintln!("spira-lc serve: ship: {e}");
                None
            })
        }
    };
    let age = oldest.unwrap_or(0) as f64 / 1_000.0;
    metrics.push(&[Point::Gauge { name: GAUGE, value: age }], &[]);
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
/// run. `--ship-dir` defaults to `SPIRA_LC_SHIP_DIR`.
pub fn admin_drill(args: &[String], run: impl FnOnce() -> Result<PathBuf, String>) -> (i32, String) {
    let Some(scratch) = flag(args, "--scratch") else {
        return (CANNOT_TELL, DRILL_USAGE.into());
    };
    let dir = match flag(args, "--ship-dir") {
        Some(d) => PathBuf::from(d),
        None => match run() {
            Ok(r) => ship_dir(&r),
            Err(e) => return (CANNOT_TELL, format!("admin-drill: cannot tell: {e}")),
        },
    };
    match drill::drill(&dir, Path::new(scratch), Exclusions::CONTRACT) {
        Ok((_, rep)) => (if rep.passed() { 0 } else { 1 }, drill::render(&rep)),
        Err(e) => (CANNOT_TELL, format!("admin-drill: cannot tell: {e}")),
    }
}

const DOLT_SHIP_USAGE: &str =
    "usage: spira-lc admin-dolt-ship --url <s3://bucket/prefix/|file://dir> --db <name> --sync <dir> --state <dir> --stamp <UTC stamp> --head <commit>";
// batch-job: one table file's upload, bounded by its size, not a round trip
const TABLE_PUT_DEADLINE: Duration = Duration::from_secs(1_500);

/// `admin-dolt-ship`: uploads what `--sync` (a `DOLT_BACKUP('sync-url')` directory) holds that
/// the ledger in `--state` lacks, then its manifest. Exit 0 uploaded, 1 failed (the previous
/// manifest is still the newest off-box), 2 usage.
pub fn admin_dolt_ship(args: &[String]) -> (i32, String) {
    let get = |n| flag(args, n);
    let (Some(url), Some(db), Some(sync), Some(state), Some(stamp), Some(head)) =
        (get("--url"), get("--db"), get("--sync"), get("--state"), get("--stamp"), get("--head"))
    else {
        return (CANNOT_TELL, DOLT_SHIP_USAGE.into());
    };
    let run = || -> Result<dolt::Shipped, String> {
        let dest = Dest::parse(url)?;
        let creds = match dest {
            Dest::S3 { .. } => Some(S3Creds::from_env()?),
            Dest::File(_) => None,
        };
        let put = Uploader { dest, creds, deadline: TABLE_PUT_DEADLINE };
        dolt::ship(&put, Path::new(sync), Path::new(state), db, stamp, head)
    };
    match run() {
        Ok(s) => (0, format!("admin-dolt-ship: {db}: {} table file(s) and manifest {}, {} bytes", s.uploaded.len(), s.key, s.bytes)),
        Err(e) => (1, format!("admin-dolt-ship: {db}: {e}")),
    }
}

const ARCHIVE_PUT_USAGE: &str = "usage: spira-lc admin-archive-put --file <path> --key <name>";
// batch-job: one archive's upload, bounded by its size (up to one PUT's limit), not a round trip
const ARCHIVE_PUT_DEADLINE: Duration = Duration::from_secs(3_600);

/// A key is one plain file name: the archive is flat, and a name cannot reach outside it.
fn archive_key_ok(key: &str) -> bool {
    !key.is_empty() && !key.starts_with('.') && key.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// `admin-archive-put --file <path> --key <name>`: uploads the file once, put-only, to
/// `<backup_url>/archive/<name>`. Exit 0 uploaded, 1 failed, 3 refused (the key is already
/// stored, the key is not a plain name, or the file is over one PUT's limit), 2 usage or no
/// destination.
pub fn admin_archive_put(args: &[String], backup_url: &str) -> (i32, String) {
    let (Some(file), Some(key)) = (flag(args, "--file"), flag(args, "--key")) else {
        return (CANNOT_TELL, ARCHIVE_PUT_USAGE.into());
    };
    if backup_url.is_empty() {
        return (CANNOT_TELL, "admin-archive-put: SPIRA_LC_BACKUP_URL is empty: there is nowhere to archive to".into());
    }
    if !archive_key_ok(key) {
        return (REFUSED, format!("admin-archive-put: refused: --key {key:?} is not a plain file name ([A-Za-z0-9._-], not starting with a dot)"));
    }
    let len = match std::fs::metadata(file) {
        Ok(m) => m.len(),
        Err(e) => return (1, format!("admin-archive-put: {file}: {e}")),
    };
    if len > MAX_PUT_BYTES {
        return (REFUSED, format!("admin-archive-put: refused: {file} is {len} bytes, over the single-PUT limit of {MAX_PUT_BYTES}; this verb has no multipart upload"));
    }
    let name = format!("archive/{key}");
    let up = match Dest::parse(backup_url).and_then(|dest| {
        let creds = match dest {
            Dest::S3 { .. } => Some(S3Creds::from_env()?),
            Dest::File(_) => None,
        };
        Ok(Uploader { dest, creds, deadline: ARCHIVE_PUT_DEADLINE })
    }) {
        Ok(u) => u,
        Err(e) => return (CANNOT_TELL, format!("admin-archive-put: {e}")),
    };
    match up.put_new(Path::new(file), &name) {
        Ok(()) => (0, format!("admin-archive-put: {name}: {len} bytes")),
        Err(PutNew::Exists) => (REFUSED, format!("admin-archive-put: refused: {name} exists; an archive is put once and never overwritten")),
        Err(PutNew::Failed(e)) => (1, format!("admin-archive-put: {name}: {e}")),
    }
}

const DOLT_ASSEMBLE_USAGE: &str =
    "usage: spira-lc admin-dolt-assemble (--state <dir> | --manifest <file>) --files <dir> --out <dir>";

/// `admin-dolt-assemble`: lays the newest manifest in `--state` (or `--manifest`) and the files
/// it names in `--files` into a fresh `--out`, the source `dolt backup restore` reads. Prints
/// `<head> <files> <bytes>`. Exit 0, 1 refused, 2 usage.
pub fn admin_dolt_assemble(args: &[String]) -> (i32, String) {
    let (Some(files), Some(out)) = (flag(args, "--files"), flag(args, "--out")) else {
        return (CANNOT_TELL, DOLT_ASSEMBLE_USAGE.into());
    };
    let manifest = match (flag(args, "--manifest"), flag(args, "--state")) {
        (Some(m), None) => Ok(PathBuf::from(m)),
        (None, Some(s)) => dolt::newest_manifest(Path::new(s)),
        _ => return (CANNOT_TELL, DOLT_ASSEMBLE_USAGE.into()),
    };
    let assembled = manifest.and_then(|m| dolt::assemble(&m, Path::new(files), Path::new(out)).map(|a| (a, m)));
    match assembled {
        Ok((a, m)) => (0, format!("{} {} {} {}", a.head, a.files, a.bytes, m.display())),
        Err(e) => (1, format!("admin-dolt-assemble: {e}")),
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

    const PUT_DEADLINE: Duration = Duration::from_secs(300);

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    fn shipped(dir: &Path, clock: &VirtualClock) -> Shipper {
        let b = SqliteBackend::open(&dir.join("live.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let c = suite::open(b.clone(), clock);
        let put: Arc<dyn Put> = Arc::new(Uploader { dest: Dest::File(dir.join("off")), creds: None, deadline: PUT_DEADLINE });
        let cfg = ShipConfig { dir: dir.join("lc-ship"), max_mains: 2, snapshot_every_ms: u64::MAX, stage_every_ms: 0 };
        let mut sh = Shipper::open(b, cfg, put, Arc::new(clock.clone())).unwrap();
        sh.snapshotter().step().unwrap();
        sh.tick().unwrap();
        for i in 0..6 {
            let t = Txn::new().put(&format!("lifecycle/sp-{i}"), Object::new(json!(i)));
            assert!(c.txn(&t, &Request::new(&format!("r{i}"), "t", "t.write")).unwrap().succeeded);
            sh.tick().unwrap();
        }
        sh
    }

    #[test_id(txbjuqn)]
    #[test]
    fn a_staging_shipper_uploads_nothing_and_the_drain_ships_what_it_queued() {
        let dir = TempDir::new("lc-ship-staging");
        let clock = VirtualClock::new(1_000);
        let b = SqliteBackend::open(&dir.path().join("live.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let c = suite::open(b.clone(), &clock);
        let cfg = ShipConfig { dir: dir.path().join("lc-ship"), max_mains: 100, snapshot_every_ms: u64::MAX, stage_every_ms: 0 };
        let mut sh = Shipper::open_staging(b, cfg, Arc::new(clock.clone())).unwrap();
        sh.snapshotter().step().unwrap();
        sh.tick().unwrap();
        let t = Txn::new().put("lifecycle/sp-1", Object::new(json!(1)));
        assert!(c.txn(&t, &Request::new("1", "t", "t.write")).unwrap().succeeded);
        let staged = sh.tick().unwrap();
        assert_eq!((staged.uploaded.len(), staged.failed, staged.shipped_through), (0, None, 0));
        assert!(!ship::names(&runtime::ports::RealFs, &sh.dir().join("queue").join(REVLOG)).unwrap().is_empty(), "the run waits in queue/");

        let off = dir.path().join("off");
        let drained = ship::drain(&runtime::ports::RealFs, sh.dir(), &Uploader { dest: Dest::File(off.clone()), creds: None, deadline: PUT_DEADLINE }).unwrap();
        assert!(drained.failed.is_none() && drained.uploaded.len() == 2, "{drained:?}");
        assert!(off.join("revlog").is_dir());
        assert_eq!(sh.tick().unwrap().shipped_through, 1, "the staging shipper reads what the drain recorded");
    }

    #[test_id(tbd0vxr)]
    #[test]
    fn the_served_shippers_tick_leaves_a_queued_segment_for_the_scheduler() {
        let dir = TempDir::new("lc-ship-served");
        let clock = VirtualClock::new(1_000);
        let b = SqliteBackend::open(&dir.path().join("live.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let c = suite::open(b.clone(), &clock);
        let off = dir.path().join("off");
        let url = format!("file://{}", off.display());
        let mut s = staging(b, dir.path().join("lc-ship"), &url, (1, 3_600, 60)).unwrap();
        let metrics = MetricsSink::new(None, "spira-lc", Duration::from_millis(10));
        tick(&mut s, &metrics);
        let t = Txn::new().put("lifecycle/sp-1", Object::new(json!(1)));
        assert!(c.txn(&t, &Request::new("1", "t", "t.write")).unwrap().succeeded);
        tick(&mut s, &metrics);
        assert!(!ship::names(&runtime::ports::RealFs, &s.shipper.dir().join("queue").join(REVLOG)).unwrap().is_empty());
        assert!(!off.exists(), "lc-serve uploads nothing");
        assert!(matches!(staging(SqliteBackend::open(&dir.path().join("x.db"), Options::default()).unwrap(), dir.path().join("s2"), &url, (9, 1, 60)), Err(e) if e.contains("1 to 5")));
    }

    #[test_id(tsse27u)]
    #[test]
    fn admin_dolt_assemble_prints_the_manifest_it_used_after_head_files_and_bytes() {
        let dir = TempDir::new("lc-assemble-manifest");
        let state = dir.path().join("state");
        std::fs::create_dir_all(state.join("manifests")).unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        let m = state.join("manifests/20261102T010000000Z-h1");
        std::fs::write(&m, "5:__DOLT__:l:r:g").unwrap();
        let (files, st, mp) = (dir.path().join("files").display().to_string(), state.display().to_string(), m.display().to_string());
        let (rc, via_state) = admin_dolt_assemble(&s(&["--state", &st, "--files", &files, "--out", &dir.path().join("a").display().to_string()]));
        assert_eq!((rc, via_state), (0, format!("h1 0 0 {mp}")));
        let (rc, via_file) = admin_dolt_assemble(&s(&["--manifest", &mp, "--files", &files, "--out", &dir.path().join("b").display().to_string()]));
        assert_eq!((rc, via_file), (0, format!("h1 0 0 {mp}")));
    }

    #[test_id(tzt12u3)]
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

    #[test_id(tg8ps54)]
    #[test]
    fn the_shipper_stages_in_the_configured_directory_and_falls_back_to_the_run_dir() {
        let run = Path::new("run-here");
        assert_eq!(ship_dir_at(Some("ssd/ship".into()), run), Path::new("ssd/ship"));
        assert_eq!(ship_dir_at(Some(String::new()), run), Path::new("run-here/lc-ship"));
        assert_eq!(ship_dir_at(None, run), Path::new("run-here/lc-ship"));
    }

    #[test_id(tu4wjdt)]
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
        assert!(out.contains("skew guard: after a restore: (0) move the shipper directory (SPIRA_LC_SHIP_DIR, else SPIRA_RUN/lc-ship) aside"), "{out}");
        drop(sh);
        let (rc, out) = admin_drill(&s(&["--scratch", &scratch]), || Err("no run dir".into()));
        assert_eq!(rc, CANNOT_TELL, "{out}");
    }

    #[test_id(tq1w6ze)]
    #[test]
    fn admin_drill_whose_restore_fails_names_the_step_and_the_seconds_each_took() {
        let dir = TempDir::new("lc-admin-drill-step");
        let clock = VirtualClock::new(1_000);
        let _sh = shipped(dir.path(), &clock);
        let revlog = dir.path().join("lc-ship").join("sent").join(REVLOG);
        let mut runs: Vec<_> = std::fs::read_dir(&revlog).unwrap().map(|e| e.unwrap().path()).collect();
        runs.sort();
        std::fs::remove_file(&runs[0]).unwrap();
        let scratch = dir.path().join("scratch").display().to_string();
        let run = dir.path().to_path_buf();
        let (rc, out) = admin_drill(&s(&["--scratch", &scratch]), || Ok(run));
        assert_eq!(rc, CANNOT_TELL, "{out}");
        assert!(out.starts_with("admin-drill: cannot tell: restore: "), "{out}");
        assert!(out.contains("; steps: checksum "), "{out}");
        assert!(!out.contains("drill ok:"), "{out}");
    }

    #[test_id(t6ifb45)]
    #[test]
    fn admin_archive_put_lands_the_file_byte_identical_and_refuses_a_second_put_of_the_key() {
        let dir = TempDir::new("lc-archive-put");
        let url = format!("file://{}", dir.path().join("off").display());
        let export = dir.path().join("export.jsonl");
        let body: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
        std::fs::write(&export, &body).unwrap();
        let file = export.display().to_string();

        let (rc, out) = admin_archive_put(&s(&["--file", &file, "--key", "lifecycle-export.jsonl"]), &url);
        assert_eq!(rc, 0, "{out}");
        assert_eq!(std::fs::read(dir.path().join("off/archive/lifecycle-export.jsonl")).unwrap(), body);

        std::fs::write(&export, b"something else").unwrap();
        let (rc, out) = admin_archive_put(&s(&["--file", &file, "--key", "lifecycle-export.jsonl"]), &url);
        assert_eq!(rc, REFUSED, "{out}");
        assert!(out.contains("archive/lifecycle-export.jsonl exists"), "{out}");
        assert_eq!(std::fs::read(dir.path().join("off/archive/lifecycle-export.jsonl")).unwrap(), body, "never overwritten");
    }

    #[test_id(tgwigmb)]
    #[test]
    fn admin_archive_put_refuses_by_name_what_one_put_cannot_carry() {
        let dir = TempDir::new("lc-archive-refuse");
        let url = format!("file://{}", dir.path().join("off").display());
        let big = dir.path().join("big");
        std::fs::File::create(&big).unwrap().set_len(MAX_PUT_BYTES + 1).unwrap();
        let (rc, out) = admin_archive_put(&s(&["--file", &big.display().to_string(), "--key", "big"]), &url);
        assert_eq!(rc, REFUSED, "{out}");
        assert!(out.contains("over the single-PUT limit"), "{out}");
        assert!(!dir.path().join("off").exists());

        let small = dir.path().join("small");
        std::fs::write(&small, b"x").unwrap();
        let small = small.display().to_string();
        for key in ["../escape", "a/b", ".hidden", ""] {
            let (rc, out) = admin_archive_put(&s(&["--file", &small, "--key", key]), &url);
            assert_eq!(rc, REFUSED, "{key}: {out}");
        }
        let (rc, out) = admin_archive_put(&s(&["--file", &small, "--key", "k"]), "");
        assert_eq!(rc, CANNOT_TELL, "{out}");
        assert!(out.contains("SPIRA_LC_BACKUP_URL is empty"), "{out}");
        assert_eq!(admin_archive_put(&s(&["--file", &small]), &url).0, CANNOT_TELL);
    }
}
