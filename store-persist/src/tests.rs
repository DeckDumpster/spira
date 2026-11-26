use super::ports::{Clock, Conn, Dolt};
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use testkit::TempDir;

struct FakeClock(Cell<u64>);

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    fn sleep(&self, secs: u64) {
        self.0.set(self.0.get() + secs * 1000);
    }
}

/// A sync copies the database's `payload` file into the copy beside a manifest; `bloat` adds
/// bytes the way chunks the live store has since collected stay in a real backup.
#[derive(Default)]
struct FakeDolt {
    live: RefCell<Vec<u32>>,
    syncs: RefCell<Vec<(Conn, String, PathBuf)>>,
    /// Fail this many upcoming syncs.
    fail_sync: Cell<usize>,
    fail_verify: Cell<bool>,
    bloat: Cell<usize>,
    gcs: RefCell<Vec<String>>,
    full_gcs: RefCell<Vec<String>>,
    /// Commits in every database's log; a squash rewrites it to `keep + 1`.
    log: Cell<u64>,
    fail_graft: Cell<bool>,
    keep_after: Cell<u64>,
    abandoned: Cell<usize>,
    events: RefCell<Vec<String>>,
    fail_gc: RefCell<Option<String>>,
    gc_message: RefCell<String>,
}

impl Dolt for FakeDolt {
    fn sync(&self, conn: &Conn, data: &Path, db: &str, dest: &Path) -> Result<(), String> {
        if self.fail_sync.get() > 0 {
            self.fail_sync.set(self.fail_sync.get() - 1);
            return Err("planted sync failure".into());
        }
        self.syncs.borrow_mut().push((conn.clone(), db.to_string(), dest.to_path_buf()));
        fs::create_dir_all(dest).unwrap();
        fs::copy(data.join(db).join(".dolt/noms/payload"), dest.join("payload")).unwrap();
        fs::write(dest.join("manifest"), db).unwrap();
        fs::write(dest.join("garbage"), vec![b'g'; self.bloat.get()]).unwrap();
        Ok(())
    }
    fn history_len(&self, _conn: &Conn, _data: &Path, _db: &str) -> Result<u64, String> {
        Ok(self.log.get())
    }
    fn squash_history(&self, _conn: &Conn, _data: &Path, db: &str, keep: u64) -> Result<String, String> {
        self.events.borrow_mut().push(format!("squash {db} {keep}"));
        Ok("tip".into())
    }
    fn graft_history(&self, _conn: &Conn, data: &Path, db: &str, tip: &str) -> Result<(), String> {
        let held = fs::OpenOptions::new()
            .write(true)
            .open(data.parent().unwrap().join("durable/LOCK"))
            .map(|f| unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&f), libc::LOCK_EX | libc::LOCK_NB) } != 0);
        self.events.borrow_mut().push(format!("graft {db} {tip} lock_held={}", held.unwrap_or(false)));
        if self.fail_graft.get() {
            return Err("planted graft failure".into());
        }
        self.log.set(self.keep_after.get());
        Ok(())
    }
    fn abandon_history(&self, _conn: &Conn, _data: &Path, _db: &str) -> Result<(), String> {
        self.abandoned.set(self.abandoned.get() + 1);
        Ok(())
    }
    fn gc(&self, _conn: &Conn, _data: &Path, db: &str, full: bool) -> Result<(), String> {
        self.events.borrow_mut().push(format!("gc {db} full={full}"));
        if full {
            self.full_gcs.borrow_mut().push(db.to_string());
        }
        self.gcs.borrow_mut().push(db.to_string());
        match &*self.fail_gc.borrow() {
            Some(bad) if bad == db => Err(self.gc_message.borrow().clone()),
            _ => Ok(()),
        }
    }
    fn verify(&self, db_dir: &Path) -> Result<String, String> {
        if self.fail_verify.get() {
            return Err("planted corrupt copy".into());
        }
        fs::read_to_string(db_dir.join(".dolt/noms/payload")).map_err(|e| e.to_string())
    }
    fn alive(&self, pid: u32) -> bool {
        self.live.borrow().contains(&pid)
    }
}

struct World {
    _tmp: TempDir,
    cfg: Config,
    old: PathBuf,
    dolt: FakeDolt,
    clock: FakeClock,
}

fn put(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn make_db(data: &Path, db: &str, payload: &str) {
    put(&data.join(db).join(".dolt/noms/manifest"), "m");
    put(&data.join(db).join(".dolt/noms/payload"), payload);
    put(&data.join(db).join(".dolt/repo_state.json"), &format!("{{\"head\":\"refs/heads/main\",\"db\":\"{db}\"}}"));
}

fn yaml(data: &Path) -> String {
    format!("listener:\n  port: 3999\ndata_dir: \"{}\"\n", data.display())
}

/// An old on-disk store with two databases, a RAM data directory with install's yaml in it,
/// and an empty durable directory; values pinned off the shipped defaults.
fn world() -> World {
    let tmp = TempDir::new("store-persist");
    let (old, data, durable) = (tmp.join("old"), tmp.join("ram"), tmp.join("durable"));
    make_db(&old, "spira", "beads-v1");
    make_db(&old, "spira_aux", "aux-v1");
    put(&old.join(".doltcfg/privileges.db"), "grants");
    put(&old.join("dolt-server.yaml"), &yaml(&old));
    put(&data.join("dolt-server.yaml"), &yaml(&data));
    let cfg = Config { data, durable: Some(durable), interval_secs: 7, max_lag_secs: 40, compact_pct: 150, gc_interval_secs: 100, run: tmp.join("run"), history_keep: 3, tmp_pressure_pct: 77, tmp_ttl_secs: 3600, proc_dir: tmp.join("proc") };
    World { _tmp: tmp, cfg, old, dolt: FakeDolt::default(), clock: FakeClock(Cell::new(1_000)) }
}

fn seeded() -> World {
    let w = world();
    seed(&w.cfg, &w.old, &w.dolt, &w.clock).unwrap();
    w
}

fn layout(w: &World) -> Layout {
    w.cfg.layout().unwrap()
}

fn live_server(w: &World, pid: u32) {
    put(&w.cfg.data.join(".dolt/sql-server.info"), &format!("{pid}:3999:s3cret"));
    w.dolt.live.borrow_mut().push(pid);
}

fn values(root: &Path, drop: Option<&str>) -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = [
        (KEY_DATA, root.join("ram/dolt").display().to_string()),
        (KEY_DURABLE, root.join("disk/dolt-durable").display().to_string()),
        (KEY_INTERVAL, "7".to_string()),
        (KEY_MAX_LAG, "40".to_string()),
        (KEY_COMPACT_PCT, "150".to_string()),
        (KEY_GC_INTERVAL, "100".to_string()),
        (KEY_RUN, root.join("run").display().to_string()),
        (KEY_HISTORY_KEEP, "3".to_string()),
        (KEY_TMP_PRESSURE_PCT, "77".to_string()),
        (KEY_TMP_TTL, "3600".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    if let Some(d) = drop {
        m.remove(d);
    }
    m
}

#[test_id(tmau3c6)]
#[test]
fn config_reads_every_key_and_refuses_a_missing_one_by_name() {
    let root = TempDir::new("store-persist-config");
    let c = Config::from_values(|k| values(&root, None).get(k).cloned()).unwrap();
    assert_eq!(c.durable, Some(root.join("disk/dolt-durable")));
    assert_eq!((c.interval_secs, c.max_lag_secs, c.compact_pct, c.gc_interval_secs, c.history_keep), (7, 40, 150, 100, 3));
    for k in KEYS {
        let e = Config::from_values(|q| values(&root, Some(k)).get(q).cloned()).unwrap_err();
        assert!(e.contains(k), "{k}: {e}");
    }
}

#[test_id(td3op5g)]
#[test]
fn config_declared_empty_durable_means_no_copy_and_bad_values_are_refused() {
    let root = TempDir::new("store-persist-config-bad");
    let mut v = values(&root, None);
    v.insert(KEY_DURABLE.into(), String::new());
    assert_eq!(Config::from_values(|k| v.get(k).cloned()).unwrap().durable, None);
    let nested = root.join("ram/dolt/copy").display().to_string();
    for (k, bad) in
        [(KEY_MAX_LAG, "7"), (KEY_COMPACT_PCT, "90"), (KEY_DURABLE, nested.as_str()), (KEY_DURABLE, "rel"), (KEY_INTERVAL, "x"), (KEY_HISTORY_KEEP, "1")]
    {
        let mut v = values(&root, None);
        v.insert(k.into(), bad.into());
        assert!(Config::from_values(|q| v.get(q).cloned()).is_err(), "{k}={bad} accepted");
    }
}

#[test_id(t7k5hf1)]
#[test]
fn seed_copies_every_database_and_its_meta_offline_without_the_old_server_yaml() {
    let w = seeded();
    let l = layout(&w);
    assert_eq!(durable_dbs(&l).unwrap().into_iter().collect::<Vec<_>>(), ["spira", "spira_aux"]);
    assert!(w.dolt.syncs.borrow().iter().all(|(c, _, _)| *c == Conn::Offline));
    assert!(l.meta_db("spira").join("repo_state.json").is_file());
    assert_eq!(fs::read_to_string(l.meta_root().join("doltcfg/privileges.db")).unwrap(), "grants");
    assert!(!l.meta_root().join("dolt-server.yaml").exists(), "the old yaml names the old directory");
    assert!(l.marker().is_file());
    assert_eq!(read_state(&l).unwrap().dbs, ["spira", "spira_aux"]);
    let again = seed(&w.cfg, &w.old, &w.dolt, &w.clock).unwrap_err();
    assert!(again.contains("already holds"), "{again}");
}

#[test_id(tpg6qgi)]
#[test]
fn seed_refuses_a_directory_a_live_server_holds() {
    let w = world();
    put(&w.old.join(".dolt/sql-server.info"), "77:3999:x");
    w.dolt.live.borrow_mut().push(77);
    let e = seed(&w.cfg, &w.old, &w.dolt, &w.clock).unwrap_err();
    assert!(e.contains("pid 77"), "{e}");
    assert!(!layout(&w).marker().exists());
}

#[test_id(tity8sa)]
#[test]
fn restore_fills_an_empty_ram_directory_from_the_copy() {
    let w = seeded();
    fs::remove_file(w.cfg.data.join("dolt-server.yaml")).unwrap();
    put(&layout(&w).meta_root().join("dolt-server.yaml"), &yaml(&w.cfg.data));
    let r = restore(&w.cfg, &w.dolt).unwrap();
    assert_eq!(r.restored, ["dolt-server.yaml", ".doltcfg/privileges.db", "spira", "spira_aux"]);
    assert_eq!(fs::read_to_string(w.cfg.data.join("spira/.dolt/noms/payload")).unwrap(), "beads-v1");
    assert!(w.cfg.data.join("spira/.dolt/repo_state.json").is_file());
    let link = fs::read_link(w.cfg.data.join("spira/.dolt/git-remote-cache")).unwrap();
    assert_eq!(link, layout(&w).git_cache("spira"));
    assert!(!w.cfg.data.join(".dolt/store-persist").exists(), "staging is cleared");
    assert_eq!(data_dbs(&w.cfg.data).unwrap().len(), 2);
}

#[test_id(tgelq96)]
#[test]
fn restore_keeps_a_database_already_in_ram_because_it_is_newer() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    put(&w.cfg.data.join("spira/.dolt/noms/payload"), "beads-v2");
    let r = restore(&w.cfg, &w.dolt).unwrap();
    assert_eq!(r.kept, ["spira", "spira_aux"]);
    assert!(r.restored.is_empty());
    assert_eq!(fs::read_to_string(w.cfg.data.join("spira/.dolt/noms/payload")).unwrap(), "beads-v2");
}

#[test_id(t0jag5i)]
#[test]
fn restore_refuses_an_unseeded_copy_a_live_server_and_a_yaml_naming_another_directory() {
    let w = world();
    assert!(restore(&w.cfg, &w.dolt).unwrap_err().contains("never been seeded"));
    let w = seeded();
    live_server(&w, 4242);
    assert!(restore(&w.cfg, &w.dolt).unwrap_err().contains("pid 4242"));
    let w = seeded();
    put(&w.cfg.data.join("dolt-server.yaml"), &yaml(&w.old));
    let e = restore(&w.cfg, &w.dolt).unwrap_err();
    assert!(e.contains("would serve another directory"), "{e}");
    assert!(data_dbs(&w.cfg.data).unwrap().is_empty());
}

#[test_id(tyhf4bb)]
#[test]
fn without_a_durable_copy_a_ram_backed_directory_is_refused() {
    let mut w = world();
    w.cfg.durable = None;
    let tmpfs = is_tmpfs(&w.cfg.data).unwrap();
    match restore(&w.cfg, &w.dolt) {
        Err(e) => assert!(tmpfs && e.contains("RAM-backed"), "{e}"),
        Ok(r) => assert!(!tmpfs && r.notes[0].contains("served as it is"), "{r:?}"),
    }
}

#[test_id(t4zw5f5)]
#[test]
fn is_tmpfs_tells_a_ram_filesystem_from_a_disk_one() {
    let proc_is_tmpfs = is_tmpfs(Path::new(system_paths::PROC)).unwrap();
    assert!(!proc_is_tmpfs, "procfs is not tmpfs — a matcher that says yes to everything is caught here");
    let shm = format!("{}/shm", system_paths::DEV);
    if Path::new(&shm).is_dir() {
        assert!(is_tmpfs(&Path::new(&shm).join("not-yet-created")).unwrap(), "{shm} is tmpfs on every Linux this runs on");
    }
}

#[test_id(tnozbc2)]
#[test]
fn an_online_pass_goes_through_the_server_and_records_its_start() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    w.clock.0.set(5_000);
    put(&w.cfg.data.join("spira/.dolt/noms/payload"), "beads-v3");
    let st = pass(&w.cfg, Mode::Online, &w.dolt, &w.clock).unwrap();
    assert_eq!(st.start_ms, 5_000);
    assert_eq!(read_state(&layout(&w)).unwrap(), st);
    let last = w.dolt.syncs.borrow().last().cloned().unwrap();
    assert_eq!(last.0, Conn::Server { port: 3999, secret: "s3cret".into() });
    assert_eq!(fs::read_to_string(layout(&w).db("spira").join("payload")).unwrap(), "beads-v3");
    assert!(layout(&w).meta_root().join("dolt-server.yaml").is_file(), "the live yaml is kept for the next reboot");
}

#[test_id(t7qhm6r)]
#[test]
fn a_pass_never_leaves_a_durable_database_behind() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    fs::remove_dir_all(w.cfg.data.join("spira_aux")).unwrap();
    let before = read_state(&layout(&w)).unwrap();
    let e = pass(&w.cfg, Mode::Offline, &w.dolt, &w.clock).unwrap_err();
    assert!(e.contains("spira_aux"), "{e}");
    assert_eq!(read_state(&layout(&w)).unwrap(), before, "a refused pass records nothing");
}

#[test_id(ts7wv9q)]
#[test]
fn the_daemon_reports_a_recovery_once() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    w.dolt.fail_sync.set(2);
    let mut log = Vec::new();
    run(&w.cfg, &w.dolt, &w.clock, Some(5), &mut |l| log.push(l.to_string()));
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(log[0].contains("failed") && log[1].contains("failed") && log[2].contains("recovered"), "{log:?}");
    assert_eq!(w.clock.now_ms(), 1_000 + 4 * 7 * 1000, "one interval between passes");
}

fn keep_upkeep(w: &World, gc_ago_s: u64, compact_ago_s: u64) {
    let now = w.clock.now_ms();
    let u = Upkeep { last_gc_ms: now - gc_ago_s * 1000, last_compact_ms: now - compact_ago_s * 1000, ..Default::default() };
    put_durable(&layout(w).upkeep(), u.render().as_bytes()).unwrap();
}

fn serving() -> World {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    w.clock.0.set(10 * COMPACT_EVERY_SECS * 1000);
    w
}

#[test_id(tudo6r1)]
#[test]
fn the_daemon_collects_the_live_store_once_per_gc_interval_between_passes() {
    let w = serving();
    keep_upkeep(&w, 0, 0);
    let mut log = Vec::new();
    run(&w.cfg, &w.dolt, &w.clock, Some(32), &mut |l| log.push(l.to_string()));
    assert_eq!(w.dolt.gcs.borrow().len(), 2 * 2, "interval 7s, gc every 100s: two collections of both databases in 32 passes");
    assert!(log.iter().any(|l| l.contains("collected, live")), "{log:?}");
}

#[test_id(tg6glyp)]
#[test]
fn a_gc_interval_of_zero_leaves_collection_to_the_daily_rebuild() {
    let mut w = serving();
    w.cfg.gc_interval_secs = 0;
    keep_upkeep(&w, 0, 0);
    run(&w.cfg, &w.dolt, &w.clock, Some(30), &mut |_| {});
    assert!(w.dolt.gcs.borrow().is_empty());
}

#[test_id(t920l6q)]
#[test]
fn the_daemon_collects_whatever_the_free_space_and_never_says_not_collected() {
    let w = serving();
    keep_upkeep(&w, 0, 0);
    let mut log = Vec::new();
    run(&w.cfg, &w.dolt, &w.clock, Some(30), &mut |l| log.push(l.to_string()));
    assert!(!w.dolt.gcs.borrow().is_empty());
    assert!(!log.iter().any(|l| l.contains("not collected")), "{log:?}");
}

#[test_id(th6nqjn)]
#[test]
fn a_restarted_daemon_with_overdue_times_collects_and_rebuilds_on_its_first_loop() {
    let w = serving();
    keep_upkeep(&w, 7200, 2 * COMPACT_EVERY_SECS);
    run(&w.cfg, &w.dolt, &w.clock, Some(2), &mut |_| {});
    assert_eq!(*w.dolt.gcs.borrow(), ["spira", "spira_aux"], "the daily check collects each database once");
    let kept = read_upkeep(&layout(&w));
    assert_eq!(kept.last_compact_ms, 10 * COMPACT_EVERY_SECS * 1000);
    assert_eq!(kept.last_gc_ms, kept.last_compact_ms);
}

#[test_id(t42hf89)]
#[test]
fn a_restarted_daemon_within_its_intervals_neither_collects_nor_checks() {
    let w = serving();
    keep_upkeep(&w, 60, 3600);
    run(&w.cfg, &w.dolt, &w.clock, Some(2), &mut |_| {});
    assert!(w.dolt.gcs.borrow().is_empty());
    assert_eq!(read_upkeep(&layout(&w)).last_compact_ms, w.clock.now_ms() - 3600 * 1000 - 7000, "untouched");
}

#[test_id(t4s0u9y)]
#[test]
fn the_daemon_rebuilds_a_copy_past_its_ratio_once_a_day_without_a_compact_verb() {
    let w = serving();
    keep_upkeep(&w, 0, 2 * COMPACT_EVERY_SECS);
    put(&layout(&w).db("spira").join("shed-chunks"), &"x".repeat(4096));
    let bloated = tree_bytes(&layout(&w).db("spira"));
    let mut log = Vec::new();
    run(&w.cfg, &w.dolt, &w.clock, Some(3), &mut |l| log.push(l.to_string()));
    assert!(tree_bytes(&layout(&w).db("spira")) < bloated);
    assert_eq!(log.iter().filter(|l| l.contains("rebuilt, copy")).count(), 1, "only the bloated copy, once: {log:?}");
}

#[test_id(tm4ownd)]
#[test]
fn a_failed_daily_collection_writes_one_line_naming_the_database_and_dolts_error() {
    let w = serving();
    keep_upkeep(&w, 7200, 2 * COMPACT_EVERY_SECS);
    *w.dolt.fail_gc.borrow_mut() = Some("spira".into());
    *w.dolt.gc_message.borrow_mut() = "disk quota exceeded".into();
    run(&w.cfg, &w.dolt, &w.clock, Some(40), &mut |_| {});
    let text = fs::read_to_string(w.cfg.run.join(COLLECT_LOG)).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("spira: collection failed: disk quota exceeded"), "{text}");
    assert!(text.starts_with("1970-01-11T00:00:"), "an iso8601 stamp of the fake clock: {text}");
    run(&w.cfg, &w.dolt, &w.clock, Some(40), &mut |_| {});
    assert_eq!(fs::read_to_string(w.cfg.run.join(COLLECT_LOG)).unwrap(), text, "a later hourly failure on the same day adds nothing");
}

#[test_id(tfe30s4)]
#[test]
fn a_rebuild_that_fails_is_a_failure_line_naming_the_database() {
    let w = serving();
    keep_upkeep(&w, 7200, 2 * COMPACT_EVERY_SECS);
    put(&layout(&w).db("spira").join("shed-chunks"), &"x".repeat(4096));
    w.dolt.fail_sync.set(100);
    run(&w.cfg, &w.dolt, &w.clock, Some(2), &mut |_| {});
    let text = fs::read_to_string(w.cfg.run.join(COLLECT_LOG)).unwrap();
    assert!(text.contains("store-persist: spira: rebuild failed: planted sync failure"), "{text}");
}

#[test_id(trtxvmo)]
#[test]
fn a_quiet_daily_check_writes_no_failure_log() {
    let w = serving();
    keep_upkeep(&w, 7200, 2 * COMPACT_EVERY_SECS);
    run(&w.cfg, &w.dolt, &w.clock, Some(3), &mut |_| {});
    assert!(!w.cfg.run.join(COLLECT_LOG).exists());
}

#[test_id(tnko430)]
#[test]
fn iso8601_renders_a_known_instant() {
    assert_eq!(iso8601(1_795_000_000_000), "2026-11-18T11:06:40Z");
    assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
}

#[test_id(ttu132n)]
#[test]
fn the_watchers_file_reads_the_log_run_writes() {
    let rows = include_str!("../../spira/watchers");
    let row = rows.lines().find(|l| l.starts_with("dolt-collect|")).expect("a dolt-collect row");
    let mut f = row.split('|');
    f.next();
    assert_eq!(f.next(), Some("log"));
    let target = f.next().unwrap().replace("@SPIRA_RUN@", "run");
    assert_eq!(target, format!("run/{COLLECT_LOG}"));
}

#[test_id(ts9hpwn)]
#[test]
fn state_round_trips() {
    let st = State { start_ms: 10, finish_ms: 12, dbs: vec!["a".into(), "b".into()] };
    assert_eq!(State::parse(&st.render()).unwrap(), st);
    assert!(State::parse("finish_ms=1\ndbs=\n").unwrap_err().contains("start_ms"));
}

#[test_id(trui3x0)]
#[test]
fn put_durable_leaves_identical_bytes_alone() {
    let t = TempDir::new("store-persist-put");
    let p = t.join("a/b");
    assert!(put_durable(&p, b"x").unwrap());
    assert!(!put_durable(&p, b"x").unwrap());
    assert!(put_durable(&p, b"y").unwrap());
    assert_eq!(fs::read(&p).unwrap(), b"y");
}

#[test_id(tbvzv68)]
#[test]
fn compaction_rebuilds_only_a_copy_past_its_ratio_and_swaps_it_whole() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    let l = layout(&w);
    let out = compact(&w.cfg, &w.dolt).unwrap().lines;
    assert!(out.iter().filter(|o| o.contains("copy")).all(|o| o.contains("within")), "{out:?}");
    w.dolt.bloat.set(4096);
    pass(&w.cfg, Mode::Online, &w.dolt, &w.clock).unwrap();
    let bloated = tree_bytes(&l.db("spira"));
    w.dolt.bloat.set(0);
    let out = compact(&w.cfg, &w.dolt).unwrap().lines;
    assert!(out.iter().filter(|o| o.contains("copy")).all(|o| o.contains("rebuilt")), "{out:?}");
    assert!(tree_bytes(&l.db("spira")) < bloated);
    assert_eq!(fs::read_to_string(l.db("spira").join("payload")).unwrap(), "beads-v1");
    assert!(!l.compact_dir().join("spira").exists(), "the superseded copy is removed");
}

#[test_id(tj8ma88)]
#[test]
fn compaction_collects_the_live_store_whatever_the_free_space() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    let out = compact(&w.cfg, &w.dolt).unwrap().lines;
    assert_eq!(*w.dolt.gcs.borrow(), ["spira", "spira_aux"]);
    assert!(out.iter().any(|o| o.contains("collected, live")), "{out:?}");
    assert!(!out.iter().any(|o| o.contains("not collected")), "{out:?}");
}

#[test_id(tuar6ix)]
#[test]
fn gc_collects_each_live_database_without_touching_the_durable_copy() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    let l = layout(&w);
    let copy = tree_bytes(&l.db("spira"));
    w.dolt.syncs.borrow_mut().clear();
    let out = gc(&w.cfg, &w.dolt).unwrap();
    assert_eq!(*w.dolt.gcs.borrow(), ["spira", "spira_aux"]);
    assert!(w.dolt.syncs.borrow().is_empty(), "{out:?}");
    assert_eq!(tree_bytes(&l.db("spira")), copy);
}

#[test_id(t1eyctf)]
#[test]
fn gc_still_collects_the_other_database_when_one_fails_and_reports_the_failure() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    *w.dolt.fail_gc.borrow_mut() = Some("spira".into());
    *w.dolt.gc_message.borrow_mut() = "planted gc failure".into();
    let err = gc(&w.cfg, &w.dolt).unwrap_err();
    assert!(err.contains("spira: collection failed: planted"), "{err}");
    assert_eq!(*w.dolt.gcs.borrow(), ["spira", "spira_aux"]);
}

#[test_id(t87d084)]
#[test]
fn a_stop_time_pass_syncs_only_a_database_written_since_the_last_pass() {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    w.clock.0.set(now + 60_000);
    pass(&w.cfg, Mode::Online, &w.dolt, &w.clock).unwrap();
    w.dolt.live.borrow_mut().clear();
    w.dolt.syncs.borrow_mut().clear();
    w.clock.0.set(now + 120_000);
    pass(&w.cfg, Mode::Offline, &w.dolt, &w.clock).unwrap();
    assert!(w.dolt.syncs.borrow().is_empty(), "nothing written since the daemon's pass: nothing to copy");
    assert_eq!(read_state(&layout(&w)).unwrap().start_ms, now + 120_000);

    put(&w.cfg.data.join("spira/.dolt/noms/payload"), "beads-v9");
    let later = std::time::UNIX_EPOCH + std::time::Duration::from_millis(now + 300_000);
    fs::File::options().write(true).open(w.cfg.data.join("spira/.dolt/noms/payload")).unwrap().set_modified(later).unwrap();
    w.clock.0.set(now + 400_000);
    pass(&w.cfg, Mode::Offline, &w.dolt, &w.clock).unwrap();
    let synced: Vec<String> = w.dolt.syncs.borrow().iter().map(|s| s.1.clone()).collect();
    assert_eq!(synced, ["spira"], "only the database written since the last pass is copied");
    assert_eq!(fs::read_to_string(layout(&w).db("spira").join("payload")).unwrap(), "beads-v9");
}

fn online_syncs(w: &World, at: u64) -> Vec<String> {
    w.clock.0.set(at);
    w.dolt.syncs.borrow_mut().clear();
    pass(&w.cfg, Mode::Online, &w.dolt, &w.clock).unwrap();
    let synced = w.dolt.syncs.borrow().iter().map(|s| s.1.clone()).collect();
    synced
}

fn online_world() -> (World, u64) {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    online_syncs(&w, now + 60_000);
    (w, now)
}

#[test_id(t9c2h0y)]
#[test]
fn an_online_pass_with_no_file_written_since_the_last_pass_syncs_nothing() {
    let (w, now) = online_world();
    assert!(online_syncs(&w, now + 120_000).is_empty());
    assert_eq!(read_state(&layout(&w)).unwrap().start_ms, now + 120_000);
}

#[test_id(twn3fj4)]
#[test]
fn an_online_pass_syncs_exactly_the_database_written_since_the_last_pass() {
    let (w, now) = online_world();
    let p = w.cfg.data.join("spira_aux/.dolt/noms/payload");
    put(&p, "aux-v9");
    let later = std::time::UNIX_EPOCH + std::time::Duration::from_millis(now + 300_000);
    fs::File::options().write(true).open(&p).unwrap().set_modified(later).unwrap();
    assert_eq!(online_syncs(&w, now + 400_000), ["spira_aux"]);
}

#[test_id(t0al5ue)]
#[test]
fn an_online_pass_syncs_a_database_the_durable_copy_lacks_though_it_is_old() {
    let (w, now) = online_world();
    fs::remove_dir_all(layout(&w).db("spira_aux")).unwrap();
    assert_eq!(online_syncs(&w, now + 120_000), ["spira_aux"]);
}

fn compactable() -> World {
    let w = seeded();
    restore(&w.cfg, &w.dolt).unwrap();
    live_server(&w, 4242);
    w.dolt.keep_after.set(w.cfg.history_keep + 1);
    w
}

#[test_id(tew4oaa)]
#[test]
fn compaction_squashes_a_log_past_the_keep_depth_under_the_lock_then_collects_in_full() {
    let w = compactable();
    w.dolt.log.set(500);
    let out = compact(&w.cfg, &w.dolt).unwrap();
    assert_eq!(w.dolt.log.get(), w.cfg.history_keep + 1);
    assert!(out.lines.iter().any(|o| o.contains("history squashed, 500 commits -> 4")), "{out:?}");
    let ev = w.dolt.events.borrow().clone();
    assert_eq!(ev[..3], ["squash spira 3", "graft spira tip lock_held=true", "gc spira full=true"], "{ev:?}");
    assert_eq!(*w.dolt.full_gcs.borrow(), ["spira"], "only the rewritten database is collected in full");
}

#[test_id(tc9i7se)]
#[test]
fn compaction_rebuilds_the_copy_after_a_squash_though_it_is_within_its_ratio() {
    let w = compactable();
    w.dolt.log.set(500);
    let out = compact(&w.cfg, &w.dolt).unwrap();
    assert!(out.lines.iter().any(|o| o.starts_with("spira: rebuilt")), "{out:?}");
    w.dolt.log.set(4);
    let out = compact(&w.cfg, &w.dolt).unwrap();
    assert!(out.lines.iter().any(|o| o.starts_with("spira: copy") && o.contains("within")), "{out:?}");
}

#[test_id(t4xipli)]
#[test]
fn compaction_leaves_a_log_at_the_bound_alone_and_a_zero_keep_disables_the_bound() {
    let w = compactable();
    w.dolt.log.set(w.cfg.history_keep + 1);
    compact(&w.cfg, &w.dolt).unwrap();
    assert!(w.dolt.events.borrow().iter().all(|e| !e.starts_with("squash")));
    let mut w = compactable();
    w.cfg.history_keep = 0;
    w.dolt.log.set(1_000_000);
    compact(&w.cfg, &w.dolt).unwrap();
    assert!(w.dolt.events.borrow().iter().all(|e| !e.starts_with("squash")));
    assert!(w.dolt.full_gcs.borrow().is_empty(), "a full collection is the squash's, not every compact's");
}

#[test_id(t4g2gq3)]
#[test]
fn a_failed_graft_abandons_the_side_branch_keeps_the_log_and_does_not_rebuild_the_copy() {
    let w = compactable();
    w.dolt.log.set(500);
    w.dolt.fail_graft.set(true);
    let out = compact(&w.cfg, &w.dolt).unwrap();
    assert_eq!(w.dolt.log.get(), 500);
    assert_eq!(w.dolt.abandoned.get(), 2, "one per database");
    assert!(out.lines.iter().any(|o| o.contains("history not squashed: planted graft failure")), "{out:?}");
    assert!(out.lines.iter().all(|o| !o.contains("rebuilt")), "{out:?}");
}


/// Epoch ms the fixtures call "now"; entries are aged against it, not the real clock.
const NOW_MS: u64 = 10_000_000;

fn spill(data: &Path, name: &str, bytes: usize, mtime_ms: u64) -> PathBuf {
    let p = data.join(".dolt/tmp").join(name);
    put(&p, &"x".repeat(bytes));
    let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(mtime_ms)).unwrap();
    p
}

fn serve_holding(w: &World, pid: u32, held: &Path) {
    put(&w.cfg.data.join(".dolt/sql-server.info"), &format!("{pid}:3999:s3cret"));
    let fd = w.cfg.proc_dir.join(pid.to_string()).join("fd");
    fs::create_dir_all(&fd).unwrap();
    std::os::unix::fs::symlink(held, fd.join("7")).unwrap();
}

fn prune(w: &World) -> Vec<String> {
    prune_tmp(&w.cfg.data, &w.cfg.proc_dir, 101, w.cfg.tmp_ttl_secs, NOW_MS)
}

#[test_id(tp4q8mz)]
#[test]
fn prune_tmp_removes_a_stale_entry_nothing_holds() {
    let w = world();
    let stale = spill(&w.cfg.data, "stale", 10, 0);
    let log = prune(&w);
    assert!(!stale.exists());
    assert!(log.len() == 1 && log[0].contains("pruned dolt tmp entry") && log[0].contains("age 10000s >= window 3600s"), "{log:?}");
}

#[test_id(tr6j2xw)]
#[test]
fn prune_tmp_keeps_a_stale_entry_the_server_holds_open() {
    let w = world();
    let held = spill(&w.cfg.data, "held", 10, 0);
    serve_holding(&w, 4242, &held);
    assert!(prune(&w).is_empty());
    assert!(held.exists());
}

#[test_id(tv9d3ns)]
#[test]
fn prune_tmp_keeps_a_fresh_entry() {
    let w = world();
    let fresh = spill(&w.cfg.data, "fresh", 10, NOW_MS - 60_000);
    assert!(prune(&w).is_empty());
    assert!(fresh.exists());
}

#[test_id(tg2h7ck)]
#[test]
fn prune_tmp_under_pressure_removes_the_largest_entry_first_held_or_not() {
    let w = world();
    let big = spill(&w.cfg.data, "big", 40_000, NOW_MS);
    let small = spill(&w.cfg.data, "small", 10, NOW_MS);
    serve_holding(&w, 4242, &big);
    let mut calls = 0;
    let log = prune_tmp_using(&w.cfg.data, &w.cfg.proc_dir, 80, 3600, NOW_MS, &mut || {
        calls += 1;
        Some(if calls == 1 { 90 } else { 10 })
    });
    assert!(!big.exists() && small.exists());
    assert!(log.len() == 1 && log[0].contains("big") && log[0].contains("filesystem at 90% >= threshold 80%"), "{log:?}");
}

struct PlantingClock {
    now: Cell<u64>,
    dir: PathBuf,
    sleeps: Cell<u32>,
    /// Whether the entry planted by the first sleep still existed when the second began.
    survived_first_gap: Cell<bool>,
}

impl Clock for PlantingClock {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }
    fn sleep(&self, secs: u64) {
        let n = self.sleeps.get() + 1;
        self.sleeps.set(n);
        if n == 2 {
            self.survived_first_gap.set(self.dir.join("planted-1").exists());
        }
        self.now.set(self.now.get() + secs * 1000);
        spill(self.dir.parent().unwrap().parent().unwrap(), &format!("planted-{n}"), 1, 0);
    }
}

#[test_id(tw5m1ab)]
#[test]
fn run_prunes_the_spool_once_a_minute_not_on_every_pass() {
    let mut w = seeded();
    w.cfg.interval_secs = 30;
    w.cfg.max_lag_secs = 90;
    w.cfg.tmp_pressure_pct = 101;
    live_server(&w, 4242);
    spill(&w.cfg.data, "planted-0", 1, 0);
    let clock = PlantingClock { now: Cell::new(NOW_MS), dir: w.cfg.data.join(".dolt/tmp"), sleeps: Cell::new(0), survived_first_gap: Cell::new(false) };
    let mut log = Vec::new();
    run(&w.cfg, &w.dolt, &clock, Some(4), &mut |l| log.push(l.to_string()));
    let pruned = |n: u32| log.iter().filter(|l| l.contains(&format!("planted-{n} "))).count();
    assert_eq!((pruned(0), pruned(1), pruned(2)), (1, 1, 1), "{log:?}");
    assert!(clock.survived_first_gap.get(), "the 30 s pass pruned");
}

#[test_id(tf8r3qd)]
#[test]
fn prune_tmp_reads_the_real_filesystem_and_cuts_the_largest_at_a_zero_threshold() {
    let w = world();
    let big = spill(&w.cfg.data, "big", 40_000, NOW_MS);
    let small = spill(&w.cfg.data, "small", 10, NOW_MS);
    let log = prune_tmp(&w.cfg.data, &w.cfg.proc_dir, 0, 3600, NOW_MS);
    assert!(!big.exists() && !small.exists());
    assert!(log[0].contains("big") && log[1].contains("small"), "{log:?}");
    assert!(prune_tmp(&w.cfg.data, &w.cfg.proc_dir, 101, 3600, NOW_MS).is_empty());
}

fn flushes_in(f: impl FnOnce()) -> u64 {
    let before = FLUSHES.with(|c| c.get());
    f();
    FLUSHES.with(|c| c.get()) - before
}

fn online_flushes(w: &World, at: u64) -> u64 {
    flushes_in(|| {
        online_syncs(w, at);
    })
}

#[test_id(tywrxjm)]
#[test]
fn an_idle_online_pass_forces_no_flush_and_still_records_its_start() {
    let (w, now) = online_world();
    assert_eq!(online_flushes(&w, now + 120_000), 0);
    assert_eq!(read_state(&layout(&w)).unwrap().start_ms, now + 120_000);
}

#[test_id(t0fq2sp)]
#[test]
fn an_online_pass_after_a_payload_write_forces_the_flushes() {
    let (w, now) = online_world();
    let p = w.cfg.data.join("spira/.dolt/noms/payload");
    put(&p, "v9");
    let later = std::time::UNIX_EPOCH + std::time::Duration::from_millis(now + 300_000);
    fs::File::options().write(true).open(&p).unwrap().set_modified(later).unwrap();
    assert!(online_flushes(&w, now + 400_000) >= 3);
}

#[test_id(t6adwyh)]
#[test]
fn an_online_pass_after_only_a_metadata_change_forces_the_flushes() {
    let (w, now) = online_world();
    put(&w.cfg.data.join("spira/.dolt/repo_state.json"), "{\"changed\":true}");
    assert!(online_flushes(&w, now + 120_000) >= 3);
}

#[test_id(t0wffer)]
#[test]
fn an_idle_offline_pass_still_forces_flushes() {
    let (w, now) = online_world();
    w.dolt.live.borrow_mut().clear();
    w.clock.0.set(now + 120_000);
    let n = flushes_in(|| {
        pass(&w.cfg, Mode::Offline, &w.dolt, &w.clock).unwrap();
    });
    assert!(n >= 2, "{n}");
}

#[test_id(tes0nfx)]
#[test]
fn seed_still_forces_flushes() {
    let w = world();
    let n = flushes_in(|| {
        seed(&w.cfg, &w.old, &w.dolt, &w.clock).unwrap();
    });
    assert!(n >= 3, "{n}");
}

#[test_id(tnoeyc5)]
#[test]
fn a_truncated_record_makes_the_next_online_pass_sync_every_database_and_rewrite_it() {
    let (w, now) = online_world();
    fs::write(layout(&w).state(), "start_ms=").unwrap();
    let synced = online_syncs(&w, now + 120_000);
    assert_eq!(synced, ["spira", "spira_aux"]);
    assert_eq!(read_state(&layout(&w)).unwrap().start_ms, now + 120_000);
}

fn op_records(w: &World) -> Vec<Vec<(String, serde_json::Value)>> {
    spira_otel::logs::read_all(&spira_otel::dir_of(&w.cfg.run), "store-persist")
}

fn attr(rec: &[(String, serde_json::Value)], k: &str) -> String {
    rec.iter().find(|(n, _)| n == k).map(|(_, v)| v["stringValue"].as_str().unwrap_or_default().to_string()).unwrap_or_default()
}

#[test_id(tlrrxpr)]
#[test]
fn a_squash_that_ran_sends_one_store_squash_op_with_the_database_as_subject() {
    let w = compactable();
    w.dolt.log.set(500);
    compact(&w.cfg, &w.dolt).unwrap();
    let recs = op_records(&w);
    let squashes: Vec<_> = recs.iter().filter(|r| attr(r, "operation") == "store-squash").collect();
    assert_eq!(squashes.len(), 1);
    assert!(squashes.iter().any(|r| attr(r, "subject") == "spira" && attr(r, "result") == "ok"));
}

#[test_id(tbbg3sk)]
#[test]
fn a_failed_squash_sends_a_failed_op_and_a_log_within_the_bound_sends_none() {
    let w = compactable();
    w.dolt.log.set(500);
    w.dolt.fail_graft.set(true);
    compact(&w.cfg, &w.dolt).unwrap();
    assert!(op_records(&w).iter().all(|r| attr(r, "result") == "failed"));
    assert!(!op_records(&w).is_empty());
    let w = compactable();
    w.dolt.log.set(w.cfg.history_keep + 1);
    compact(&w.cfg, &w.dolt).unwrap();
    assert!(op_records(&w).is_empty());
}
