//! A Dolt data directory served from RAM, kept durable by a copy on disk. Contract: DESIGN.md.
//!
//! The copy is one `dolt backup` per database (`<durable>/db/<db>`, which Dolt itself writes
//! table-files-then-manifest, each fsynced), plus the files a backup does not carry
//! (`<durable>/meta`). Every write committed before a pass's recorded `start` is in the copy
//! once that pass has finished.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod history;
pub mod ports;

#[cfg(test)]
mod tests;

use ports::{Clock, Conn, Dolt};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub const KEY_DATA: &str = "SPIRA_DOLT_DATA";
pub const KEY_DURABLE: &str = "SPIRA_DOLT_DURABLE";
pub const KEY_INTERVAL: &str = "SPIRA_DOLT_PERSIST_INTERVAL_SECS";
pub const KEY_MAX_LAG: &str = "SPIRA_DOLT_PERSIST_MAX_LAG_SECS";
pub const KEY_COMPACT_PCT: &str = "SPIRA_DOLT_PERSIST_COMPACT_PCT";
pub const KEY_GC_INTERVAL: &str = "SPIRA_DOLT_PERSIST_GC_INTERVAL_SECS";
pub const KEY_HISTORY_KEEP: &str = "SPIRA_DOLT_PERSIST_HISTORY_KEEP";
pub const KEY_RUN: &str = "SPIRA_RUN";
pub const KEY_TMP_PRESSURE_PCT: &str = "SPIRA_DOLT_TMP_PRESSURE_PCT";
pub const KEY_TMP_TTL: &str = "SPIRA_DOLT_TMP_TTL";
pub const KEYS: [&str; 10] =
    [KEY_DATA, KEY_DURABLE, KEY_INTERVAL, KEY_MAX_LAG, KEY_COMPACT_PCT, KEY_GC_INTERVAL, KEY_RUN, KEY_HISTORY_KEEP, KEY_TMP_PRESSURE_PCT, KEY_TMP_TTL];

/// Seconds between prunes of the server's temp spool by `run`.
pub const TMP_PRUNE_EVERY_SECS: u64 = 60;

/// Seconds between the rebuilds `run` considers for a durable copy grown past its ratio.
pub const COMPACT_EVERY_SECS: u64 = 86_400;
/// Where `run` records a failed daily collection or rebuild; `spira/watchers` reads it.
pub const COLLECT_LOG: &str = "dolt-collect.log";

const SERVER_YAML: &str = "dolt-server.yaml";
const DB_META: [&str; 2] = ["repo_state.json", "config.json"];
const TMPFS_MAGIC: i64 = 0x0102_1994;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub data: PathBuf,
    /// `None` when the store is not RAM-backed: the key is declared empty.
    pub durable: Option<PathBuf>,
    pub interval_secs: u64,
    pub max_lag_secs: u64,
    pub compact_pct: u64,
    /// Seconds between online collections of the live store by `run`; 0 leaves it to the daily rebuild.
    pub gc_interval_secs: u64,
    pub run: PathBuf,
    /// Commits `compact` leaves in a database's log, the oldest squashed into one base; 0 keeps all.
    pub history_keep: u64,
    /// Filesystem use, in percent, at or above which `prune_tmp` cuts the largest spill.
    pub tmp_pressure_pct: u64,
    /// Seconds past which an unheld spill is stale.
    pub tmp_ttl_secs: u64,
    pub proc_dir: PathBuf,
}

impl Config {
    /// From resolved config values; a key the resolution does not carry is refused by name.
    pub fn from_values(get: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let need = |k: &str| get(k).ok_or_else(|| format!("{k} is not declared in the config — refusing"));
        let num = |k: &str| -> Result<u64, String> {
            let v = need(k)?;
            v.trim().parse::<u64>().map_err(|_| format!("{k}={v:?} is not a whole number"))
        };
        let data = need(KEY_DATA)?;
        if data.is_empty() {
            return Err(format!("{KEY_DATA} is empty — there is no store to persist"));
        }
        let durable = need(KEY_DURABLE)?;
        let cfg = Config {
            data: PathBuf::from(data),
            durable: (!durable.is_empty()).then(|| PathBuf::from(durable)),
            interval_secs: num(KEY_INTERVAL)?,
            max_lag_secs: num(KEY_MAX_LAG)?,
            compact_pct: num(KEY_COMPACT_PCT)?,
            gc_interval_secs: num(KEY_GC_INTERVAL)?,
            run: PathBuf::from(need(KEY_RUN)?),
            history_keep: num(KEY_HISTORY_KEEP)?,
            tmp_pressure_pct: num(KEY_TMP_PRESSURE_PCT)?,
            tmp_ttl_secs: num(KEY_TMP_TTL)?,
            proc_dir: PathBuf::from(system_paths::PROC),
        };
        if cfg.history_keep == 1 {
            return Err(format!("{KEY_HISTORY_KEEP} of 1 cannot hold the root and a base; use 0 to keep all or at least 2"));
        }
        if cfg.max_lag_secs <= cfg.interval_secs {
            return Err(format!("{KEY_MAX_LAG} ({}) must exceed {KEY_INTERVAL} ({})", cfg.max_lag_secs, cfg.interval_secs));
        }
        if cfg.compact_pct < 100 {
            return Err(format!("{KEY_COMPACT_PCT} ({}) below 100 would rebuild the copy on every run", cfg.compact_pct));
        }
        if let Some(d) = &cfg.durable {
            if !d.is_absolute() || !cfg.data.is_absolute() {
                return Err(format!("{KEY_DATA} and {KEY_DURABLE} must be absolute paths"));
            }
            if d.starts_with(&cfg.data) || cfg.data.starts_with(d) {
                return Err(format!("{KEY_DURABLE} {} and {KEY_DATA} {} must not nest", d.display(), cfg.data.display()));
            }
        }
        Ok(cfg)
    }

    fn layout(&self) -> Result<Layout, String> {
        self.durable
            .as_ref()
            .map(|d| Layout { root: d.clone() })
            .ok_or_else(|| format!("{KEY_DURABLE} is empty — this store has no durable copy"))
    }
}

/// The durable copy's directory layout.
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn dbs_dir(&self) -> PathBuf {
        self.root.join("db")
    }
    pub fn db(&self, name: &str) -> PathBuf {
        self.dbs_dir().join(name)
    }
    pub fn meta_root(&self) -> PathBuf {
        self.root.join("meta/root")
    }
    pub fn meta_db(&self, name: &str) -> PathBuf {
        self.root.join("meta/db").join(name)
    }
    pub fn git_cache(&self, name: &str) -> PathBuf {
        self.root.join("git-remote-cache").join(name)
    }
    pub fn compact_dir(&self) -> PathBuf {
        self.root.join("compact")
    }
    pub fn state(&self) -> PathBuf {
        self.root.join("persist.state")
    }
    pub fn upkeep(&self) -> PathBuf {
        self.root.join("upkeep.state")
    }
    pub fn marker(&self) -> PathBuf {
        self.root.join("store-persist.id")
    }
    fn lock(&self) -> PathBuf {
        self.root.join("LOCK")
    }
}

/// Databases a data directory serves: every child holding `.dolt/noms/manifest`. Dolt serves
/// dot-named children too, so they are not skipped.
pub fn data_dbs(data: &Path) -> Result<BTreeSet<String>, String> {
    child_dirs_with(data, |p| p.join(".dolt/noms/manifest").is_file())
}

/// Databases the durable copy holds: every `db/<name>` holding a backup manifest.
pub fn durable_dbs(l: &Layout) -> Result<BTreeSet<String>, String> {
    if !l.dbs_dir().is_dir() {
        return Ok(BTreeSet::new());
    }
    child_dirs_with(&l.dbs_dir(), |p| p.join("manifest").is_file())
}

fn child_dirs_with(dir: &Path, keep: impl Fn(&Path) -> bool) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for e in fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))? {
        let p = e.map_err(|e| e.to_string())?.path();
        if p.is_dir() && keep(&p) {
            if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                out.insert(n.to_string());
            }
        }
    }
    Ok(out)
}

/// `<data>/.dolt/sql-server.info` is `pid:port:secret` while a server holds the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub pid: u32,
    pub port: u16,
    pub secret: String,
}

pub fn server_info(data: &Path) -> Option<ServerInfo> {
    let raw = fs::read_to_string(data.join(".dolt/sql-server.info")).ok()?;
    let mut it = raw.trim().splitn(3, ':');
    Some(ServerInfo {
        pid: it.next()?.parse().ok()?,
        port: it.next()?.parse().ok()?,
        secret: it.next()?.to_string(),
    })
}

fn live_server(data: &Path, dolt: &dyn Dolt) -> Option<ServerInfo> {
    server_info(data).filter(|i| dolt.alive(i.pid))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Through the running server; refused when none holds the directory.
    Online,
    /// With no server: the stop-time pass, and seeding. Refused while one is live.
    Offline,
}

fn conn_for(data: &Path, mode: Mode, dolt: &dyn Dolt) -> Result<Conn, String> {
    match (mode, live_server(data, dolt)) {
        (Mode::Online, Some(i)) => Ok(Conn::Server { port: i.port, secret: i.secret }),
        (Mode::Online, None) => Err(format!("no live dolt server holds {} — an online pass syncs only through it", data.display())),
        (Mode::Offline, None) => Ok(Conn::Offline),
        (Mode::Offline, Some(i)) => Err(format!("dolt server pid {} holds {} — an offline pass would contend for its lock", i.pid, data.display())),
    }
}

/// An exclusive `flock` on the durable copy, held for one pass, one compaction swap or one seed.
pub struct Lock(#[allow(dead_code)] fs::File);

pub fn lock(l: &Layout) -> Result<Lock, String> {
    fs::create_dir_all(&l.root).map_err(|e| format!("cannot create {}: {e}", l.root.display()))?;
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(l.lock())
        .map_err(|e| format!("cannot open {}: {e}", l.lock().display()))?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!("cannot lock {}: {}", l.lock().display(), std::io::Error::last_os_error()));
    }
    Ok(Lock(f))
}

#[cfg(test)]
thread_local! {
    pub(crate) static FLUSHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The one place a pass forces the disk; tests count its calls.
fn flush(f: &fs::File) -> std::io::Result<()> {
    #[cfg(test)]
    FLUSHES.with(|c| c.set(c.get() + 1));
    f.sync_all()
}

pub fn fsync_dir(dir: &Path) -> Result<(), String> {
    fs::File::open(dir).and_then(|f| flush(&f)).map_err(|e| format!("cannot fsync {}: {e}", dir.display()))
}

/// Writes `bytes` to `dst` durably: temp file, fsync, rename, fsync the directory. A `dst`
/// already holding exactly `bytes` is left alone.
pub fn put_durable(dst: &Path, bytes: &[u8]) -> Result<bool, String> {
    if fs::read(dst).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    put_file(dst, bytes, true)?;
    Ok(true)
}

/// Temp file and rename; with `durable` false nothing is forced, and a crash may leave the
/// older `dst`.
fn put_file(dst: &Path, bytes: &[u8], durable: bool) -> Result<(), String> {
    let dir = dst.parent().ok_or_else(|| format!("{} has no parent", dst.display()))?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{}.tmp", dst.file_name().and_then(|n| n.to_str()).unwrap_or("f")));
    let mut f = fs::File::create(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    f.write_all(bytes).and_then(|_| if durable { flush(&f) } else { Ok(()) }).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, dst).map_err(|e| format!("cannot rename onto {}: {e}", dst.display()))?;
    if durable {
        fsync_dir(dir)?;
    }
    Ok(())
}

fn read(p: &Path) -> Result<Vec<u8>, String> {
    fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))
}

/// The pass record: every write committed before `start_ms` is in the durable copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub start_ms: u64,
    pub finish_ms: u64,
    pub dbs: Vec<String>,
}

impl State {
    pub fn render(&self) -> String {
        format!("start_ms={}\nfinish_ms={}\ndbs={}\n", self.start_ms, self.finish_ms, self.dbs.join(","))
    }
    pub fn parse(text: &str) -> Result<State, String> {
        let field = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
                .ok_or_else(|| format!("persist state has no {k}="))
        };
        let n = |k: &str| field(k).and_then(|v| v.parse::<u64>().map_err(|_| format!("persist state {k}={v:?} is not a number")));
        Ok(State {
            start_ms: n("start_ms")?,
            finish_ms: n("finish_ms")?,
            dbs: field("dbs")?.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect(),
        })
    }
}

pub fn read_state(l: &Layout) -> Result<State, String> {
    State::parse(&String::from_utf8_lossy(&read(&l.state())?))
}

/// A stop-time pass copies only a database with a file written since the last finished pass
/// began; the daemon keeps the copy that close to the live store, so a full sync is redundant.
const MTIME_MARGIN_MS: u64 = 1000;

fn newest_mtime_ms(p: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(p) else { return 0 };
    rd.filter_map(Result::ok)
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => {
                if e.file_name() == "git-remote-cache" || e.file_name() == "store-persist" { 0 } else { newest_mtime_ms(&e.path()) }
            }
            Ok(_) => e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(u64::MAX),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

/// One pass: every database `source` serves is synced into the durable copy, with the files a
/// backup does not carry. `server_yaml` is false only when seeding from another directory,
/// whose server config names that directory, not `cfg.data`.
fn pass_from(l: &Layout, source: &Path, mode: Mode, server_yaml: bool, dolt: &dyn Dolt, clock: &dyn Clock) -> Result<State, String> {
    let incremental = server_yaml;
    let conn = conn_for(source, mode, dolt)?;
    let _lock = lock(l)?;
    let live = data_dbs(source)?;
    let held = durable_dbs(l)?;
    let gone: Vec<&String> = held.difference(&live).collect();
    if !gone.is_empty() {
        return Err(format!(
            "the durable copy holds {gone:?}, which {} does not serve — refusing a pass that would leave them behind (restore them, or move them out of {} by hand)",
            source.display(),
            l.dbs_dir().display()
        ));
    }
    let covered = if incremental { read_state(l).ok().map(|s| s.start_ms.saturating_sub(MTIME_MARGIN_MS)) } else { None };
    let start_ms = clock.now_ms();
    fs::create_dir_all(l.dbs_dir()).map_err(|e| format!("cannot create {}: {e}", l.dbs_dir().display()))?;
    let mut changed = mode == Mode::Offline;
    for db in &live {
        let untouched = covered.is_some_and(|c| held.contains(db) && newest_mtime_ms(&source.join(db).join(".dolt")) < c);
        if !untouched {
            dolt.sync(&conn, source, db, &l.db(db))?;
            changed = true;
        }
        for f in DB_META {
            let p = source.join(db).join(".dolt").join(f);
            if p.is_file() {
                changed |= put_durable(&l.meta_db(db).join(f), &read(&p)?)?;
            }
        }
    }
    if server_yaml {
        let y = source.join(SERVER_YAML);
        if y.is_file() {
            changed |= put_durable(&l.meta_root().join(SERVER_YAML), &read(&y)?)?;
        }
    }
    let cfg_dir = source.join(".doltcfg");
    if cfg_dir.is_dir() {
        for e in fs::read_dir(&cfg_dir).map_err(|e| format!("cannot list {}: {e}", cfg_dir.display()))? {
            let p = e.map_err(|e| e.to_string())?.path();
            if p.is_file() {
                let name = p.file_name().unwrap_or_default();
                changed |= put_durable(&l.meta_root().join("doltcfg").join(name), &read(&p)?)?;
            }
        }
    }
    let st = State { start_ms, finish_ms: clock.now_ms(), dbs: live.into_iter().collect() };
    if changed {
        fsync_dir(&l.dbs_dir())?;
        put_durable(&l.state(), st.render().as_bytes())?;
    } else {
        put_file(&l.state(), st.render().as_bytes(), false)?;
    }
    Ok(st)
}

/// With `durable` declared empty there is nothing to restore or persist — unless the data
/// directory is RAM-backed, which a reboot would erase.
pub fn unconfigured(cfg: &Config) -> Result<String, String> {
    if is_tmpfs(&cfg.data)? {
        return Err(format!("{} is RAM-backed and {KEY_DURABLE} is empty — refusing to serve a store that a reboot erases", cfg.data.display()));
    }
    Ok(format!("{KEY_DURABLE} is empty — {} is served as it is", cfg.data.display()))
}

/// One pass over `cfg.data`, once the copy has been seeded.
pub fn pass(cfg: &Config, mode: Mode, dolt: &dyn Dolt, clock: &dyn Clock) -> Result<State, String> {
    let l = cfg.layout()?;
    if !l.marker().is_file() {
        return Err(format!("{} has never been seeded (no {}) — run `store-persist seed --from <dir>` first", l.root.display(), l.marker().display()));
    }
    pass_from(&l, &cfg.data, mode, true, dolt, clock)
}

/// Builds the durable copy from a stopped data directory: the cutover, and a fresh install
/// (seeded from an empty directory). Refused over a copy that was already seeded.
pub fn seed(cfg: &Config, from: &Path, dolt: &dyn Dolt, clock: &dyn Clock) -> Result<State, String> {
    let l = cfg.layout()?;
    if l.marker().exists() || durable_dbs(&l)?.iter().next().is_some() {
        return Err(format!("{} already holds a durable copy — seeding would overwrite it", l.root.display()));
    }
    let st = pass_from(&l, from, Mode::Offline, false, dolt, clock)?;
    put_durable(&l.marker(), format!("seeded={} from={}\n", st.finish_ms, from.display()).as_bytes())?;
    Ok(st)
}

pub fn is_tmpfs(p: &Path) -> Result<bool, String> {
    let mut probe = p.to_path_buf();
    while !probe.exists() {
        probe = probe.parent().ok_or_else(|| format!("no existing ancestor of {}", p.display()))?.to_path_buf();
    }
    let c = std::ffi::CString::new(probe.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return Err(format!("cannot statfs {}: {}", probe.display(), std::io::Error::last_os_error()));
    }
    Ok(s.f_type as i64 == TMPFS_MAGIC)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
    for e in fs::read_dir(src).map_err(|e| format!("cannot list {}: {e}", src.display()))? {
        let p = e.map_err(|e| e.to_string())?.path();
        let to = dst.join(p.file_name().unwrap_or_default());
        if p.is_dir() {
            copy_tree(&p, &to)?;
        } else {
            fs::copy(&p, &to).map_err(|e| format!("cannot copy {} to {}: {e}", p.display(), to.display()))?;
        }
    }
    Ok(())
}

pub fn tree_bytes(p: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(p) else { return 0 };
    rd.filter_map(Result::ok)
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => tree_bytes(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn yaml_data_dir(yaml: &str) -> Option<String> {
    yaml.lines()
        .find_map(|l| l.trim_start().strip_prefix("data_dir:"))
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
}

/// `.dolt/git-remote-cache` lives on disk: the server never reads it, and it is larger than the store.
fn link_git_cache(l: &Layout, db_dir: &Path, db: &str) -> Result<Option<String>, String> {
    let link = db_dir.join(".dolt/git-remote-cache");
    let target = l.git_cache(db);
    fs::create_dir_all(&target).map_err(|e| format!("cannot create {}: {e}", target.display()))?;
    match fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => Ok(None),
        Ok(_) => Ok(Some(format!("{db}: {} is a real directory, held in RAM — move it to {}", link.display(), target.display()))),
        Err(_) => std::os::unix::fs::symlink(&target, &link)
            .map(|_| None)
            .map_err(|e| format!("cannot link {}: {e}", link.display())),
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Restored {
    pub restored: Vec<String>,
    pub kept: Vec<String>,
    pub notes: Vec<String>,
}

/// Before the server starts: every database the durable copy holds and `cfg.data` lacks is
/// copied in (staged under `.dolt`, which the server does not scan, verified, then renamed
/// into place). A database already in RAM is newer than its copy and is kept.
pub fn restore(cfg: &Config, dolt: &dyn Dolt) -> Result<Restored, String> {
    if cfg.durable.is_none() {
        return Ok(Restored { notes: vec![unconfigured(cfg)?], ..Default::default() });
    }
    let l = cfg.layout()?;
    if !l.marker().is_file() {
        return Err(format!("{} has never been seeded (no {}) — refusing to start a store with no durable copy", l.root.display(), l.marker().display()));
    }
    if let Some(i) = live_server(&cfg.data, dolt) {
        return Err(format!("dolt server pid {} already holds {} — restore runs before the server, never under it", i.pid, cfg.data.display()));
    }
    fs::create_dir_all(&cfg.data).map_err(|e| format!("cannot create {}: {e}", cfg.data.display()))?;
    let mut out = Restored::default();

    let yaml = cfg.data.join(SERVER_YAML);
    if !yaml.is_file() {
        let saved = l.meta_root().join(SERVER_YAML);
        if !saved.is_file() {
            return Err(format!("no {SERVER_YAML} in {} or in the durable copy — run install to place it", cfg.data.display()));
        }
        fs::copy(&saved, &yaml).map_err(|e| format!("cannot restore {}: {e}", yaml.display()))?;
        out.restored.push(SERVER_YAML.to_string());
    }
    let named = yaml_data_dir(&String::from_utf8_lossy(&read(&yaml)?));
    if named.as_deref() != cfg.data.to_str() {
        return Err(format!("{} names data_dir {named:?}, not {} — the server would serve another directory", yaml.display(), cfg.data.display()));
    }
    let saved_cfg = l.meta_root().join("doltcfg");
    if saved_cfg.is_dir() {
        let live_cfg = cfg.data.join(".doltcfg");
        fs::create_dir_all(&live_cfg).map_err(|e| format!("cannot create {}: {e}", live_cfg.display()))?;
        for e in fs::read_dir(&saved_cfg).map_err(|e| format!("cannot list {}: {e}", saved_cfg.display()))? {
            let p = e.map_err(|e| e.to_string())?.path();
            let to = live_cfg.join(p.file_name().unwrap_or_default());
            if p.is_file() && !to.exists() {
                fs::copy(&p, &to).map_err(|e| format!("cannot restore {}: {e}", to.display()))?;
                out.restored.push(format!(".doltcfg/{}", to.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    }

    let stage_root = cfg.data.join(".dolt/store-persist");
    let _ = fs::remove_dir_all(&stage_root);
    let present = data_dbs(&cfg.data)?;
    for db in durable_dbs(&l)? {
        let live = cfg.data.join(&db);
        if present.contains(&db) {
            out.notes.extend(link_git_cache(&l, &live, &db)?);
            out.kept.push(db);
            continue;
        }
        if live.exists() {
            return Err(format!("{} exists but is not a dolt database — refusing to restore {db} over it", live.display()));
        }
        let stage = stage_root.join(&db);
        copy_tree(&l.db(&db), &stage.join(".dolt/noms"))?;
        for f in DB_META {
            let saved = l.meta_db(&db).join(f);
            if saved.is_file() {
                fs::copy(&saved, stage.join(".dolt").join(f)).map_err(|e| format!("cannot restore {db}/{f}: {e}"))?;
            } else if f == "repo_state.json" {
                return Err(format!("the durable copy of {db} has no {f} — refusing a database with no head"));
            }
        }
        out.notes.extend(link_git_cache(&l, &stage, &db)?);
        dolt.verify(&stage).map_err(|e| format!("restored {db} does not open: {e}"))?;
        fs::rename(&stage, &live).map_err(|e| format!("cannot move {db} into place: {e}"))?;
        out.restored.push(db);
    }
    let _ = fs::remove_dir_all(&stage_root);
    Ok(out)
}

/// Milliseconds since the newest finished pass began, and whether that exceeds the bound.
pub fn age(cfg: &Config, now_ms: u64) -> Result<(u64, bool), String> {
    let st = read_state(&cfg.layout()?)?;
    let lag_ms = now_ms.saturating_sub(st.start_ms);
    Ok((lag_ms, lag_ms > cfg.max_lag_secs.saturating_mul(1000)))
}

fn exchange(a: &Path, b: &Path) -> Result<(), String> {
    let ca = std::ffi::CString::new(a.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    let rc = unsafe { libc::renameat2(libc::AT_FDCWD, ca.as_ptr(), libc::AT_FDCWD, cb.as_ptr(), libc::RENAME_EXCHANGE) };
    if rc != 0 {
        return Err(format!("cannot exchange {} and {}: {}", a.display(), b.display(), std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Online `dolt_gc` of one live database. Returns the report line and whether it was collected.
fn collect(cfg: &Config, dolt: &dyn Dolt, conn: &Conn, db: &str, full: bool) -> Result<(String, bool), String> {
    let noms = cfg.data.join(db).join(".dolt/noms");
    let before = tree_bytes(&noms);
    match dolt.gc(conn, &cfg.data, db, full) {
        Ok(()) => Ok((format!("{db}: collected, live {before} B -> {} B", tree_bytes(&noms)), true)),
        Err(e) => Ok((format!("{db}: collection failed: {e}"), false)),
    }
}

/// Collects every live database in the data directory and nothing else: no sync, no copy
/// rebuild. Fails when any database was not collected, so a remedy that freed nothing says so.
pub fn gc(cfg: &Config, dolt: &dyn Dolt) -> Result<Vec<String>, String> {
    let conn = conn_for(&cfg.data, Mode::Online, dolt)?;
    let dbs = child_dirs_with(&cfg.data, |p| p.join(".dolt/noms").is_dir())?;
    let mut out = Vec::new();
    let mut all = true;
    for db in dbs.iter().filter(|d| tree_bytes(&cfg.data.join(d).join(".dolt/noms")) > 0) {
        let (line, done) = collect(cfg, dolt, &conn, db, false)?;
        all &= done;
        out.push(line);
    }
    if all { Ok(out) } else { Err(out.join("; ")) }
}

/// What a daily upkeep pass did: a report line per step, and the lines of the steps that failed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Compacted {
    pub lines: Vec<String>,
    pub failures: Vec<String>,
}

impl Compacted {
    fn report(&mut self, line: String, failed: bool) {
        if failed {
            self.failures.push(line.clone());
        }
        self.lines.push(line);
    }
}

fn rebuild(l: &Layout, cfg: &Config, dolt: &dyn Dolt, conn: &Conn, db: &str, fresh: &Path) -> Result<(), String> {
    dolt.sync(conn, &cfg.data, db, fresh)?;
    {
        let _lock = lock(l)?;
        dolt.sync(conn, &cfg.data, db, fresh)?;
        exchange(fresh, &l.db(db))?;
        fsync_dir(&l.dbs_dir())?;
        fsync_dir(&l.compact_dir())?;
    }
    fs::remove_dir_all(fresh).map_err(|e| format!("cannot remove the superseded copy {}: {e}", fresh.display()))
}

/// Bounds one database's history to `cfg.history_keep`. The side branch is built unlocked; only the move onto it holds the
/// persist lock, so no sync reads the ancestry mid-rewrite. A rewritten history shares no chunks
/// with the copy's, so the caller rebuilds the copy when this returns true.
fn squash_history(cfg: &Config, l: &Layout, dolt: &dyn Dolt, conn: &Conn, db: &str, out: &mut Compacted) -> Result<bool, String> {
    let len = match dolt.history_len(conn, &cfg.data, db) {
        Ok(n) => n,
        Err(e) => {
            out.report(format!("{db}: history not squashed: cannot read the log length: {e}"), true);
            return Ok(false);
        }
    };
    if !history::needs_squash(len, cfg.history_keep) {
        return Ok(false);
    }
    let rewrite = dolt.squash_history(conn, &cfg.data, db, cfg.history_keep).and_then(|tip| {
        let _lock = lock(l)?;
        dolt.graft_history(conn, &cfg.data, db, &tip)
    });
    match rewrite {
        Ok(()) => {
            let now = dolt.history_len(conn, &cfg.data, db).map_or_else(|e| format!("unreadable ({e})"), |n| n.to_string());
            out.report(format!("{db}: history squashed, {len} commits -> {now}"), false);
            Ok(true)
        }
        Err(e) => {
            let _ = dolt.abandon_history(conn, &cfg.data, db);
            out.report(format!("{db}: history not squashed: {e}"), true);
            Ok(false)
        }
    }
}

/// Collects each live database first; automatic GC alone left multi-GB table files standing.
///
/// A backup never sheds a chunk the live store has collected, so each copy grows until it is
/// rebuilt. One past `compact_pct` of its live size is rebuilt beside itself without the lock,
/// caught up under the lock, and exchanged in one rename.
pub fn compact(cfg: &Config, dolt: &dyn Dolt) -> Result<Compacted, String> {
    let l = cfg.layout()?;
    let conn = conn_for(&cfg.data, Mode::Online, dolt)?;
    let mut out = Compacted::default();
    for db in durable_dbs(&l)? {
        let noms = cfg.data.join(&db).join(".dolt/noms");
        let squashed = squash_history(cfg, &l, dolt, &conn, &db, &mut out)?;
        if tree_bytes(&noms) > 0 {
            let (line, done) = collect(cfg, dolt, &conn, &db, squashed)?;
            out.report(line, !done);
        }
        let (copy, live) = (tree_bytes(&l.db(&db)), tree_bytes(&noms));
        if live == 0 || (!squashed && copy.saturating_mul(100) <= live.saturating_mul(cfg.compact_pct)) {
            out.report(format!("{db}: copy {copy} B, live {live} B — within {}%", cfg.compact_pct), false);
            continue;
        }
        let fresh = l.compact_dir().join(&db);
        let _ = fs::remove_dir_all(&fresh);
        fs::create_dir_all(l.compact_dir()).map_err(|e| format!("cannot create {}: {e}", l.compact_dir().display()))?;
        match rebuild(&l, cfg, dolt, &conn, &db, &fresh) {
            Ok(()) => out.report(format!("{db}: rebuilt, copy {copy} B -> {} B (live {live} B)", tree_bytes(&l.db(&db))), false),
            Err(e) => out.report(format!("{db}: rebuild failed: {e}"), true),
        }
    }
    Ok(out)
}

/// When `run` last collected and last rebuilt, kept in the durable directory so a restart
/// does not reset either schedule. A missing file is a schedule that has never run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Upkeep {
    pub last_gc_ms: u64,
    pub last_compact_ms: u64,
    /// In memory only: a restart prunes at once.
    pub last_tmp_prune_ms: u64,
}

impl Upkeep {
    fn render(&self) -> String {
        format!("last_gc_ms={}\nlast_compact_ms={}\n", self.last_gc_ms, self.last_compact_ms)
    }

    fn parse(text: &str) -> Upkeep {
        let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')?.trim().parse().ok()).unwrap_or(0);
        Upkeep { last_gc_ms: field("last_gc_ms"), last_compact_ms: field("last_compact_ms"), last_tmp_prune_ms: 0 }
    }
}

pub fn read_upkeep(l: &Layout) -> Upkeep {
    fs::read_to_string(l.upkeep()).map(|t| Upkeep::parse(&t)).unwrap_or_default()
}

fn iso8601(ms: u64) -> String {
    let (days, rem) = ((ms / 86_400_000) as i64, ms / 1000 % 86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

fn record_failures(cfg: &Config, now_ms: u64, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    if cfg.run.as_os_str().is_empty() {
        eprintln!("store-persist: {KEY_RUN} is empty — nowhere to record a failed collection");
        return;
    }
    let body: String = lines.iter().map(|l| format!("{} store-persist: {l}\n", iso8601(now_ms))).collect();
    let appended = fs::create_dir_all(&cfg.run)
        .and_then(|_| fs::OpenOptions::new().create(true).append(true).open(cfg.run.join(COLLECT_LOG)))
        .and_then(|mut f| f.write_all(body.as_bytes()));
    if let Err(e) = appended {
        eprintln!("store-persist: cannot record to {}: {e}", cfg.run.join(COLLECT_LOG).display());
    }
}

/// The daemon: a pass every `interval_secs`, an online collection every `gc_interval_secs`, and
/// a daily rebuild of any copy past its ratio. A failed pass is reported, never retried early,
/// and the state keeps the last good start, so `age` turns red on its own. Each schedule is
/// recorded after its attempt, failed or not, so a failing collection is tried once per interval.
pub fn run(cfg: &Config, dolt: &dyn Dolt, clock: &dyn Clock, passes: Option<u64>, log: &mut dyn FnMut(&str)) {
    let mut failing = false;
    let mut n = 0u64;
    let layout = cfg.layout().ok();
    let mut upkeep = layout.as_ref().map(read_upkeep).unwrap_or_default();
    loop {
        match pass(cfg, Mode::Online, dolt, clock) {
            Ok(st) if failing => {
                log(&format!("store-persist: recovered — pass {}..{} synced {}", st.start_ms, st.finish_ms, st.dbs.join(",")));
                failing = false;
            }
            Ok(_) => {}
            Err(e) => {
                log(&format!("store-persist: pass failed: {e}"));
                failing = true;
            }
        }
        n += 1;
        if passes.is_some_and(|p| n >= p) {
            return;
        }
        let due = |last: u64, every_secs: u64| every_secs > 0 && clock.now_ms().saturating_sub(last) >= every_secs * 1000;
        if due(upkeep.last_compact_ms, COMPACT_EVERY_SECS) {
            let failed = match compact(cfg, dolt) {
                Ok(done) => {
                    done.lines.iter().for_each(|l| log(&format!("store-persist: {l}")));
                    done.failures
                }
                Err(e) => vec![format!("compact failed: {e}")],
            };
            failed.iter().for_each(|l| log(&format!("store-persist: {l}")));
            record_failures(cfg, clock.now_ms(), &failed);
            upkeep.last_compact_ms = clock.now_ms();
            upkeep.last_gc_ms = upkeep.last_compact_ms;
            save_upkeep(layout.as_ref(), &upkeep);
        } else if due(upkeep.last_gc_ms, cfg.gc_interval_secs) {
            match gc(cfg, dolt) {
                Ok(lines) => lines.iter().for_each(|l| log(&format!("store-persist: {l}"))),
                Err(e) => log(&format!("store-persist: collection: {e}")),
            }
            upkeep.last_gc_ms = clock.now_ms();
            save_upkeep(layout.as_ref(), &upkeep);
        }
        if due(upkeep.last_tmp_prune_ms, TMP_PRUNE_EVERY_SECS) {
            prune_tmp(&cfg.data, &cfg.proc_dir, cfg.tmp_pressure_pct, cfg.tmp_ttl_secs, clock.now_ms()).iter().for_each(|l| log(&format!("store-persist: {l}")));
            upkeep.last_tmp_prune_ms = clock.now_ms();
        }
        clock.sleep(cfg.interval_secs);
    }
}

fn kib_on_disk(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = fs::symlink_metadata(p) else { return 0 };
    let own = m.blocks() * 512 / 1024;
    if !m.is_dir() {
        return own;
    }
    own + fs::read_dir(p).into_iter().flatten().flatten().map(|e| kib_on_disk(&e.path())).sum::<u64>()
}

fn fs_used_pct(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut v: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut v) } != 0 {
        return None;
    }
    let (used, avail) = ((v.f_blocks - v.f_bfree) as u64, v.f_bavail as u64);
    (used + avail > 0).then(|| (used * 100).div_ceil(used + avail))
}

fn truncate_files(p: &Path) {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p).into_iter().flatten().flatten().for_each(|e| truncate_files(&e.path())),
        Ok(m) if m.is_file() => {
            let _ = fs::OpenOptions::new().write(true).truncate(true).open(p);
        }
        _ => {}
    }
}

fn remove_entry(p: &Path) -> bool {
    let gone = if fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) { fs::remove_dir_all(p) } else { fs::remove_file(p) };
    gone.is_ok()
}

/// Prunes the server's temp spool, `<data>/.dolt/tmp`, which Dolt leaves behind when it dies.
/// At or above `pressure_pct` of the filesystem it removes the largest entry first, held open
/// or not, truncating files so their blocks return at once; then it removes every entry older
/// than `ttl_secs` that the server named in `<data>/.dolt/sql-server.info` holds no fd on.
pub fn prune_tmp(data: &Path, proc_dir: &Path, pressure_pct: u64, ttl_secs: u64, now_ms: u64) -> Vec<String> {
    let dir = data.join(".dolt/tmp");
    prune_tmp_using(data, proc_dir, pressure_pct, ttl_secs, now_ms, &mut || fs_used_pct(&dir))
}

pub fn prune_tmp_using(data: &Path, proc_dir: &Path, pressure_pct: u64, ttl_secs: u64, now_ms: u64, used_pct: &mut dyn FnMut() -> Option<u64>) -> Vec<String> {
    let Ok(dir) = fs::canonicalize(data.join(".dolt/tmp")) else { return vec![] };
    let entries = || -> Vec<PathBuf> { fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect() };
    let mut out = vec![];
    while let Some(used) = used_pct().filter(|u| *u >= pressure_pct) {
        let Some((kib, entry)) = entries().into_iter().map(|e| (kib_on_disk(&e), e)).max() else { break };
        truncate_files(&entry);
        if !remove_entry(&entry) {
            break;
        }
        out.push(format!("pruned dolt tmp entry {} ({kib}KiB freed, filesystem at {used}% >= threshold {pressure_pct}%)", entry.display()));
    }
    let held: Vec<PathBuf> = server_info(data)
        .into_iter()
        .flat_map(|i| fs::read_dir(proc_dir.join(i.pid.to_string()).join("fd")).into_iter().flatten().flatten().filter_map(|e| fs::read_link(e.path()).ok()))
        .collect();
    for entry in entries() {
        let modified = fs::symlink_metadata(&entry).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        let age = modified.map_or(0, |m| now_ms.saturating_sub(m.as_millis() as u64) / 1000);
        if age < ttl_secs || held.iter().any(|h| h.starts_with(&entry)) {
            continue;
        }
        let kib = kib_on_disk(&entry);
        if remove_entry(&entry) {
            out.push(format!("pruned dolt tmp entry {} ({kib}KiB freed, age {age}s >= window {ttl_secs}s)", entry.display()));
        }
    }
    out
}

fn save_upkeep(layout: Option<&Layout>, upkeep: &Upkeep) {
    if let Some(l) = layout {
        if let Err(e) = put_durable(&l.upkeep(), upkeep.render().as_bytes()) {
            eprintln!("store-persist: cannot keep the upkeep schedule: {e}");
        }
    }
}
