//! The `Fs` port: every file call a server makes outside SQLite. `RealFs` is the disk; `SimFs`
//! keeps files in memory and lets a seed decide what each call costs, whether it fails and
//! whether it blocks, and what a crash leaves behind.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::{Clock, Rng, SeededRng, VirtualClock};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub len: u64,
    pub modified_ms: Option<u64>,
    pub is_dir: bool,
}

/// `append` creates the file and takes an exclusive `flock` for the write, so two processes
/// appending lines to one log do not interleave. `write` replaces a file's bytes. `create_dir`
/// makes every missing ancestor. A path that does not exist is `ErrorKind::NotFound`.
pub trait Fs: Send + Sync {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// The file's bytes in order, in pieces, for a file too large to hold whole.
    fn read_chunks(&self, path: &Path, f: &mut dyn FnMut(&[u8])) -> io::Result<()> {
        f(&self.read(path)?);
        Ok(())
    }
    fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// `write`, refused with `ErrorKind::AlreadyExists` where the file is there.
    fn create_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// The names directly under `dir`, sorted.
    fn list(&self, dir: &Path) -> io::Result<Vec<String>>;
    fn create_dir(&self, dir: &Path) -> io::Result<()>;
    fn metadata(&self, path: &Path) -> io::Result<Meta>;
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
}

extern "C" {
    fn flock(fd: i32, op: i32) -> i32;
}
const LOCK_EX: i32 = 2;

pub struct RealFs;

impl Fs for RealFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn read_chunks(&self, path: &Path, f: &mut dyn FnMut(&[u8])) -> io::Result<()> {
        let mut file = std::fs::File::open(path)?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match file.read(&mut buf)? {
                0 => return Ok(()),
                n => f(&buf[..n]),
            }
        }
    }

    fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        // SAFETY: the descriptor is open for the lifetime of `f`; the lock drops with the file.
        if unsafe { flock(f.as_raw_fd(), LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        f.write_all(bytes)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        std::fs::write(path, bytes)
    }

    fn create_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        std::fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(bytes)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir)? {
            out.push(e?.file_name().to_string_lossy().into_owned());
        }
        out.sort();
        Ok(out)
    }

    fn create_dir(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)
    }

    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        let m = std::fs::metadata(path)?;
        let modified_ms = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
        Ok(Meta { len: m.len(), modified_ms, is_dir: m.is_dir() })
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        std::fs::File::open(path)?.sync_all()
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        std::fs::File::open(dir)?.sync_all()
    }
}

/// What the seed may do to a call. Each call draws its delay, its failure and its block in the
/// same order whether or not they apply, so a seed names one history.
#[derive(Clone, Debug, Default)]
pub struct SimFaults {
    /// A call costs a whole number of virtual milliseconds in this inclusive range.
    pub delay_ms: (u64, u64),
    pub fail_per_mille: u32,
    pub block_per_mille: u32,
}

#[derive(Clone)]
enum Entry {
    File(usize),
    Dir,
}

struct Inode {
    data: Vec<u8>,
    synced: usize,
    modified_ms: u64,
}

struct State {
    rng: SeededRng,
    faults: SimFaults,
    live: BTreeMap<PathBuf, Entry>,
    durable: BTreeMap<PathBuf, Entry>,
    inodes: Vec<Inode>,
    calls: u64,
    crash_after: Option<u64>,
    down: bool,
    hold_next: u64,
    blocked: usize,
    epoch: u64,
}

/// An in-memory disk. A file's bytes survive `crash` only up to its last `sync_file`, and a
/// create, rename or remove only once `sync_dir` has covered its directory. A directory made
/// by `create_dir` is durable at once.
pub struct SimFs {
    state: Mutex<State>,
    gate: Condvar,
    clock: VirtualClock,
    accrued_ms: AtomicU64,
}

fn parent_of(p: &Path) -> &Path {
    p.parent().unwrap_or(Path::new("/"))
}

fn not_found(p: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{}: no such file or directory (sim)", p.display()))
}

fn dead(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, format!("sim: {why}"))
}

impl SimFs {
    pub fn new(seed: u64, faults: SimFaults, clock: VirtualClock) -> Arc<SimFs> {
        let state = State {
            rng: SeededRng::new(seed),
            faults,
            live: BTreeMap::new(),
            durable: BTreeMap::new(),
            inodes: Vec::new(),
            calls: 0,
            crash_after: None,
            down: false,
            hold_next: 0,
            blocked: 0,
            epoch: 0,
        };
        Arc::new(SimFs { state: Mutex::new(state), gate: Condvar::new(), clock, accrued_ms: AtomicU64::new(0) })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Virtual milliseconds every call so far has cost.
    pub fn accrued_ms(&self) -> u64 {
        self.accrued_ms.load(Ordering::SeqCst)
    }

    pub fn calls(&self) -> u64 {
        self.lock().calls
    }

    /// The next `n` calls block, whatever the seed says, until `release`.
    pub fn hold_next(&self, n: u64) {
        self.lock().hold_next = n;
    }

    pub fn blocked(&self) -> usize {
        self.lock().blocked
    }

    /// Returns once `n` calls are blocked.
    pub fn wait_blocked(&self, n: usize) {
        let s = self.lock();
        drop(self.gate.wait_while(s, |s| s.blocked < n).unwrap_or_else(|e| e.into_inner()));
    }

    /// Completes every call now blocked.
    pub fn release(&self) {
        self.lock().epoch += 1;
        self.gate.notify_all();
    }

    /// The `n`th call from now crashes the disk and fails, as does every call after it until
    /// `revive`: the process that made them is gone.
    pub fn crash_after(&self, n: u64) {
        let mut s = self.lock();
        s.crash_after = Some(s.calls + n);
    }

    pub fn revive(&self) {
        let mut s = self.lock();
        s.down = false;
        s.crash_after = None;
    }

    /// Power loss: what was never synced is gone.
    pub fn crash(&self) {
        Self::crash_state(&mut self.lock());
    }

    fn crash_state(s: &mut State) {
        s.live = s.durable.clone();
        for ino in &mut s.inodes {
            ino.data.truncate(ino.synced);
        }
    }

    /// A file's bytes as they are now, outside the seed's reach and uncounted.
    pub fn peek(&self, path: &Path) -> Option<Vec<u8>> {
        let s = self.lock();
        match s.live.get(path) {
            Some(Entry::File(i)) => Some(s.inodes[*i].data.clone()),
            _ => None,
        }
    }

    fn enter(&self) -> io::Result<MutexGuard<'_, State>> {
        let mut s = self.lock();
        s.calls += 1;
        if s.down {
            return Err(dead("the process is down"));
        }
        if s.crash_after.is_some_and(|at| s.calls >= at) {
            Self::crash_state(&mut s);
            s.down = true;
            return Err(dead("crashed"));
        }
        let (d, f, b) = (s.rng.next_u64(), s.rng.next_u64(), s.rng.next_u64());
        let (lo, hi) = s.faults.delay_ms;
        let cost = lo + if hi > lo { d % (hi - lo + 1) } else { 0 };
        self.accrued_ms.fetch_add(cost, Ordering::SeqCst);
        if f % 1000 < u64::from(s.faults.fail_per_mille) {
            return Err(io::Error::other("sim: injected failure"));
        }
        if s.hold_next > 0 || b % 1000 < u64::from(s.faults.block_per_mille) {
            s.hold_next = s.hold_next.saturating_sub(1);
            s.blocked += 1;
            let epoch = s.epoch;
            self.gate.notify_all();
            s = self.gate.wait_while(s, |s| s.epoch == epoch).unwrap_or_else(|e| e.into_inner());
            s.blocked -= 1;
        }
        Ok(s)
    }

    fn dir_exists(s: &State, dir: &Path) -> bool {
        dir.as_os_str().is_empty() || dir == Path::new("/") || matches!(s.live.get(dir), Some(Entry::Dir))
    }

    fn file_of(s: &State, path: &Path) -> io::Result<usize> {
        match s.live.get(path) {
            Some(Entry::File(i)) => Ok(*i),
            Some(Entry::Dir) => Err(io::Error::other(format!("{}: is a directory (sim)", path.display()))),
            None => Err(not_found(path)),
        }
    }

    fn create(&self, s: &mut State, path: &Path) -> io::Result<usize> {
        if !Self::dir_exists(s, parent_of(path)) {
            return Err(not_found(path));
        }
        if let Some(Entry::File(i)) = s.live.get(path) {
            return Ok(*i);
        }
        s.inodes.push(Inode { data: Vec::new(), synced: 0, modified_ms: self.clock.now_ms() });
        let i = s.inodes.len() - 1;
        s.live.insert(path.to_path_buf(), Entry::File(i));
        Ok(i)
    }
}

impl Fs for SimFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let s = self.enter()?;
        Ok(s.inodes[Self::file_of(&s, path)?].data.clone())
    }

    fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut s = self.enter()?;
        let i = self.create(&mut s, path)?;
        s.inodes[i].data.extend_from_slice(bytes);
        s.inodes[i].modified_ms = self.clock.now_ms();
        Ok(())
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut s = self.enter()?;
        let i = self.create(&mut s, path)?;
        let ino = &mut s.inodes[i];
        ino.data = bytes.to_vec();
        ino.synced = 0;
        ino.modified_ms = self.clock.now_ms();
        Ok(())
    }

    fn create_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut s = self.enter()?;
        if s.live.contains_key(path) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: exists (sim)", path.display())));
        }
        let i = self.create(&mut s, path)?;
        s.inodes[i].data = bytes.to_vec();
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut s = self.enter()?;
        if !Self::dir_exists(&s, parent_of(to)) {
            return Err(not_found(to));
        }
        let e = s.live.remove(from).ok_or_else(|| not_found(from))?;
        s.live.insert(to.to_path_buf(), e);
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        let mut s = self.enter()?;
        Self::file_of(&s, path)?;
        s.live.remove(path);
        Ok(())
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        let s = self.enter()?;
        if !Self::dir_exists(&s, dir) {
            return Err(not_found(dir));
        }
        let names: BTreeSet<String> = s.live.keys().filter(|p| parent_of(p) == dir).filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        Ok(names.into_iter().collect())
    }

    fn create_dir(&self, dir: &Path) -> io::Result<()> {
        let mut s = self.enter()?;
        for d in dir.ancestors().filter(|d| !d.as_os_str().is_empty() && *d != Path::new("/")) {
            if matches!(s.live.get(d), Some(Entry::File(_))) {
                return Err(io::Error::other(format!("{}: not a directory (sim)", d.display())));
            }
            s.live.insert(d.to_path_buf(), Entry::Dir);
            s.durable.insert(d.to_path_buf(), Entry::Dir);
        }
        Ok(())
    }

    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        let s = self.enter()?;
        match s.live.get(path) {
            Some(Entry::File(i)) => Ok(Meta { len: s.inodes[*i].data.len() as u64, modified_ms: Some(s.inodes[*i].modified_ms), is_dir: false }),
            Some(Entry::Dir) => Ok(Meta { len: 0, modified_ms: None, is_dir: true }),
            None => Err(not_found(path)),
        }
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        let mut s = self.enter()?;
        let i = Self::file_of(&s, path)?;
        s.inodes[i].synced = s.inodes[i].data.len();
        Ok(())
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        let mut s = self.enter()?;
        if !Self::dir_exists(&s, dir) {
            return Err(not_found(dir));
        }
        let live: Vec<(PathBuf, Entry)> = s.live.iter().filter(|(p, _)| parent_of(p) == dir).map(|(p, e)| (p.clone(), e.clone())).collect();
        let st = &mut *s;
        st.durable.retain(|p, _| parent_of(p) != dir || st.live.contains_key(p));
        st.durable.extend(live);
        Ok(())
    }
}

#[cfg(test)]
mod real_tests {
    use super::*;

    fn dir(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(name)
    }

    #[test_id(tvovajy)]
    #[test]
    fn real_read_returns_the_bytes_and_names_a_missing_file() {
        let t = dir("fs-read");
        let p = t.path().join("a");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(RealFs.read(&p).unwrap(), b"abc");
        assert_eq!(RealFs.read(&t.path().join("none")).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test_id(timcch9)]
    #[test]
    fn real_read_chunks_delivers_a_file_larger_than_one_chunk_in_order() {
        let t = dir("fs-chunks");
        let p = t.path().join("big");
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &bytes).unwrap();
        let (mut got, mut pieces) = (Vec::new(), 0);
        RealFs.read_chunks(&p, &mut |c| {
            got.extend_from_slice(c);
            pieces += 1;
        })
        .unwrap();
        assert_eq!(got, bytes);
        assert!(pieces > 1);
    }

    #[test_id(t392mcp)]
    #[test]
    fn real_append_creates_then_extends_and_holds_the_exclusive_lock_for_its_write() {
        let t = dir("fs-append");
        let p = t.path().join("log");
        RealFs.append(&p, b"one\n").unwrap();
        RealFs.append(&p, b"two\n").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one\ntwo\n");
        let held = std::fs::File::open(&p).unwrap();
        // SAFETY: `held` is open for the whole call.
        assert_eq!(unsafe { flock(held.as_raw_fd(), LOCK_EX | 4) }, 0, "the lock is taken");
        let other = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        // SAFETY: as above; LOCK_NB makes the refusal immediate.
        assert_ne!(unsafe { flock(other.as_raw_fd(), LOCK_EX | 4) }, 0, "a second holder is refused while it is held");
    }

    #[test_id(tbe27en)]
    #[test]
    fn real_write_replaces_the_bytes() {
        let t = dir("fs-write");
        let p = t.path().join("w");
        RealFs.write(&p, b"long content").unwrap();
        RealFs.write(&p, b"short").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"short");
    }

    #[test_id(t4fvp0r)]
    #[test]
    fn real_create_new_writes_a_new_file_and_refuses_one_that_exists() {
        let t = dir("fs-create-new");
        let p = t.path().join("n");
        RealFs.create_new(&p, b"first").unwrap();
        assert_eq!(RealFs.create_new(&p, b"second").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&p).unwrap(), b"first");
    }

    #[test_id(ti9s09a)]
    #[test]
    fn real_rename_moves_the_file() {
        let t = dir("fs-rename");
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        std::fs::write(&a, b"x").unwrap();
        RealFs.rename(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"x");
    }

    #[test_id(tnb0heg)]
    #[test]
    fn real_remove_deletes_the_file_and_names_a_missing_one() {
        let t = dir("fs-remove");
        let p = t.path().join("r");
        std::fs::write(&p, b"x").unwrap();
        RealFs.remove(&p).unwrap();
        assert!(!p.exists());
        assert_eq!(RealFs.remove(&p).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test_id(t90w7n5)]
    #[test]
    fn real_list_returns_sorted_names_and_names_a_missing_directory() {
        let t = dir("fs-list");
        for n in ["b", "a", "c"] {
            std::fs::write(t.path().join(n), b"").unwrap();
        }
        assert_eq!(RealFs.list(t.path()).unwrap(), ["a", "b", "c"]);
        assert_eq!(RealFs.list(&t.path().join("none")).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test_id(tu3espw)]
    #[test]
    fn real_create_dir_makes_every_missing_ancestor_and_is_repeatable() {
        let t = dir("fs-mkdir");
        let d = t.path().join("a").join("b");
        RealFs.create_dir(&d).unwrap();
        RealFs.create_dir(&d).unwrap();
        assert!(d.is_dir());
    }

    #[test_id(td7rf8k)]
    #[test]
    fn real_metadata_reports_length_kind_and_modified_time() {
        let t = dir("fs-meta");
        let p = t.path().join("m");
        std::fs::write(&p, b"12345").unwrap();
        let m = RealFs.metadata(&p).unwrap();
        assert_eq!((m.len, m.is_dir), (5, false));
        assert!(m.modified_ms.is_some());
        assert!(RealFs.metadata(t.path()).unwrap().is_dir);
    }

    #[test_id(tfn940h)]
    #[test]
    fn real_sync_file_succeeds_on_a_file_and_fails_on_a_missing_one() {
        let t = dir("fs-sync-file");
        let p = t.path().join("s");
        std::fs::write(&p, b"x").unwrap();
        RealFs.sync_file(&p).unwrap();
        assert!(RealFs.sync_file(&t.path().join("none")).is_err());
    }

    #[test_id(trdgep2)]
    #[test]
    fn real_sync_dir_succeeds_on_a_directory_and_fails_on_a_missing_one() {
        let t = dir("fs-sync-dir");
        RealFs.sync_dir(t.path()).unwrap();
        assert!(RealFs.sync_dir(&t.path().join("none")).is_err());
    }
}

#[cfg(test)]
mod sim_tests {
    use super::*;

    fn fs(seed: u64, faults: SimFaults) -> Arc<SimFs> {
        SimFs::new(seed, faults, VirtualClock::new(0))
    }

    #[test_id(tneorag)]
    #[test]
    fn sim_files_live_in_memory_and_follow_the_same_contract_as_the_disk() {
        let f = fs(1, SimFaults::default());
        let d = Path::new("run/x");
        assert_eq!(f.append(&d.join("a"), b"1").unwrap_err().kind(), io::ErrorKind::NotFound);
        f.create_dir(d).unwrap();
        f.append(&d.join("a"), b"1").unwrap();
        f.append(&d.join("a"), b"2").unwrap();
        f.write(&d.join("b"), b"w").unwrap();
        f.rename(&d.join("b"), &d.join("c")).unwrap();
        assert_eq!(f.list(d).unwrap(), ["a", "c"]);
        assert_eq!(f.read(&d.join("a")).unwrap(), b"12");
        assert_eq!(f.metadata(&d.join("c")).unwrap().len, 1);
        f.remove(&d.join("c")).unwrap();
        assert_eq!(f.read(&d.join("c")).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test_id(trywqb7)]
    #[test]
    fn sim_create_new_refuses_a_file_that_exists() {
        let f = fs(1, SimFaults::default());
        f.create_dir(Path::new("d")).unwrap();
        f.create_new(Path::new("d/n"), b"first").unwrap();
        assert_eq!(f.create_new(Path::new("d/n"), b"second").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(f.peek(Path::new("d/n")).unwrap(), b"first");
    }

    #[test_id(tjsdwdz)]
    #[test]
    fn sim_crash_keeps_synced_bytes_only() {
        let f = fs(1, SimFaults::default());
        let d = Path::new("d");
        f.create_dir(d).unwrap();
        let (kept, torn, unlisted) = (d.join("kept"), d.join("torn"), d.join("unlisted"));
        f.append(&kept, b"synced").unwrap();
        f.sync_file(&kept).unwrap();
        f.append(&kept, b"-lost").unwrap();
        f.append(&torn, b"never synced").unwrap();
        f.sync_file(&torn).unwrap();
        f.append(&unlisted, b"x").unwrap();
        f.sync_file(&unlisted).unwrap();
        f.sync_dir(d).unwrap();
        f.write(&d.join("late"), b"after the sync").unwrap();
        f.sync_file(&d.join("late")).unwrap();
        f.remove(&unlisted).unwrap();
        f.crash();
        assert_eq!(f.peek(&kept).unwrap(), b"synced");
        assert_eq!(f.peek(&torn).unwrap(), b"never synced");
        assert_eq!(f.peek(&unlisted).unwrap(), b"x", "a remove not covered by a directory sync is undone");
        assert!(f.peek(&d.join("late")).is_none(), "a file whose directory entry was never synced is gone");
    }

    #[test_id(tsktzp3)]
    #[test]
    fn sim_a_file_without_a_directory_sync_is_lost_to_a_crash_though_its_bytes_were_synced() {
        let f = fs(1, SimFaults::default());
        let d = Path::new("d");
        f.create_dir(d).unwrap();
        f.write(&d.join("a.tmp"), b"v2").unwrap();
        f.sync_file(&d.join("a.tmp")).unwrap();
        f.rename(&d.join("a.tmp"), &d.join("a")).unwrap();
        f.crash();
        assert!(f.peek(&d.join("a")).is_none());
        assert!(f.peek(&d.join("a.tmp")).is_none());
    }

    #[test_id(tu0shnr)]
    #[test]
    fn sim_release_completes_a_blocked_call() {
        let f = fs(1, SimFaults::default());
        f.create_dir(Path::new("d")).unwrap();
        f.hold_next(1);
        let writer = {
            let f = f.clone();
            std::thread::spawn(move || f.append(Path::new("d/log"), b"line"))
        };
        f.wait_blocked(1);
        assert!(f.peek(Path::new("d/log")).is_none(), "the call has not completed");
        f.release();
        writer.join().unwrap().unwrap();
        assert_eq!(f.peek(Path::new("d/log")).unwrap(), b"line");
        assert_eq!(f.blocked(), 0);
    }

    #[test_id(tq36hei)]
    #[test]
    fn sim_crash_after_fails_that_call_and_every_later_one_until_revived() {
        let f = fs(1, SimFaults::default());
        f.create_dir(Path::new("d")).unwrap();
        f.crash_after(2);
        f.write(Path::new("d/a"), b"1").unwrap();
        assert!(f.write(Path::new("d/b"), b"2").is_err());
        assert!(f.read(Path::new("d/a")).is_err());
        f.revive();
        assert!(f.peek(Path::new("d/a")).is_none(), "nothing was synced");
        f.write(Path::new("d/b"), b"2").unwrap();
    }

    #[test_id(t0o0aev)]
    #[test]
    fn sim_the_same_seed_costs_and_fails_the_same_calls() {
        let faults = SimFaults { delay_ms: (1, 40), fail_per_mille: 300, block_per_mille: 0 };
        let run = |seed| {
            let f = fs(seed, faults.clone());
            f.create_dir(Path::new("d")).ok();
            let ok: Vec<bool> = (0..30).map(|_| f.append(Path::new("d/l"), b"x").is_ok()).collect();
            (ok, f.accrued_ms())
        };
        assert_eq!(run(5), run(5));
        assert_ne!(run(5), run(6));
    }
}
