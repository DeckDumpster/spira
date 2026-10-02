//! Host-wide admission per heavy phase — pipeline shaping (sp-f4ig1; design:
//! `gate/DESIGN-admission.md`). Three pools — compile, test, gate — each sized from the
//! measured cost of its phase, so N aeons are spread across the phases instead of peaking in
//! each one together. A slot is admission, never a limit: an admitted job runs at full
//! width (law-reduce-the-count-never-throttle-the-job).
//!
//! Compile and test slots are **leases**: a `slot.<n>` file naming a pid and its starttime,
//! written under a short flock on the pool's `lock` file and dead when the pid is. The gate's
//! pool stays the gate's flock slots (`gate/src/real.rs`); this module only sizes it and reads
//! its holders for `status`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Set on a job that already runs on an admitted slot (`<pool>:<slot>`, or any non-empty
/// value): nothing under it takes a slot of its own. The gate sets it on every command it
/// runs; testenv on its build.
pub const INHERIT_ENV: &str = "SPIRA_ADMISSION";
/// The label a lease carries (`who=`): the aeon's bead.
pub const WHO_ENV: &str = "SPIRA_ADMIT_WHO";
/// The compiler the `spira-admit` RUSTC_WRAPPER execs after admission (sccache's absolute
/// path; empty: rustc itself).
pub const INNER_ENV: &str = "SPIRA_ADMIT_INNER";
/// The binary: RUSTC_WRAPPER mode, `status`, `run`.
pub const BIN: &str = "spira-admit";
/// The summon jitter's key (seconds; 0 disables).
pub const JITTER_ENV: &str = "SPIRA_SUMMON_JITTER";
/// The jitter when the key is unset.
pub const JITTER_DEFAULT: u64 = 20;
/// The telemetry family: `<run>/tsd/admission.jsonl`.
pub const FAMILY: &str = "admission";
/// How often a waiter repeats its waiting line.
pub const SAY_EVERY: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pool {
    Compile,
    Test,
    Gate,
}

impl Pool {
    pub const ALL: [Pool; 3] = [Pool::Compile, Pool::Test, Pool::Gate];

    pub fn name(self) -> &'static str {
        match self {
            Pool::Compile => "compile",
            Pool::Test => "test",
            Pool::Gate => "gate",
        }
    }

    pub fn parse(s: &str) -> Option<Pool> {
        Pool::ALL.into_iter().find(|p| p.name() == s)
    }

    /// `<run>/<name>-admission` — the gate's is the directory it has always used.
    pub fn dir(self, run: &Path) -> PathBuf {
        run.join(format!("{}-admission", self.name()))
    }

    /// The size key, env spelling (spira.toml `compile_par` / `test_par` / `certify_par`).
    pub fn size_env(self) -> &'static str {
        match self {
            Pool::Compile => "SPIRA_COMPILE_PAR",
            Pool::Test => "SPIRA_TEST_PAR",
            Pool::Gate => "SPIRA_CERTIFY_PAR",
        }
    }
}

/// What the box has, for the derived sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host {
    pub cores: u64,
    pub mem_avail_mib: u64,
}

impl Host {
    pub fn read() -> Host {
        let cores = std::thread::available_parallelism().map(|n| n.get() as u64).unwrap_or(4);
        let mem_avail_mib = fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|m| {
                m.lines()
                    .find(|l| l.starts_with("MemAvailable"))
                    .and_then(|l| l.split_whitespace().nth(1).and_then(|k| k.parse::<u64>().ok()))
            })
            .map(|kib| kib / 1024)
            .unwrap_or(1600);
        Host { cores, mem_avail_mib }
    }
}

/// The derived size of a pool (DESIGN-admission.md §5): from the measured cost of one job of
/// that phase. Never below 1.
pub fn derive(pool: Pool, h: Host) -> u64 {
    let (per_core, per_mib) = match pool {
        // One aeon-profile workspace build: ~9 cores average (14 peak), 2.7 GiB anon; a
        // release (LTO) build heavier. 10 cores and 4 GiB a slot.
        Pool::Compile => (10, 4096),
        // One testenv trial: eight private dolt sql-servers, fsync IO and a tmpfs slot; few
        // cores. 8 cores and 8 GiB a slot.
        Pool::Test => (8, 8192),
        // The gate's own formula (gate/DESIGN.md), unchanged.
        Pool::Gate => (4, 400),
    };
    (h.cores / per_core).min(h.mem_avail_mib / per_mib).max(1)
}

/// The pool's size: the key when it is a positive integer, else derived from the box.
pub fn size(pool: Pool, var: Option<&str>, h: Host) -> u64 {
    var.map(str::trim)
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| derive(pool, h))
}

/// [`size`] from this process's environment and box.
pub fn size_from_env(pool: Pool) -> u64 {
    size(pool, std::env::var(pool.size_env()).ok().as_deref(), Host::read())
}

/// The size the config document sets for `pool`, when it sets one.
pub fn doc_size(pool: Pool, doc: &crate::SpiraToml) -> Option<u64> {
    let s = doc.spira.as_ref()?;
    match pool {
        Pool::Compile => s.compile_par,
        Pool::Test => s.test_par,
        Pool::Gate => s.certify_par,
    }
    .map(u64::from)
    .filter(|n| *n > 0)
}

/// The pool's size as the pools read it: the config document first (the gate re-reads it live),
/// then this process's environment, then derived from the box.
pub fn size_configured(pool: Pool) -> u64 {
    let doc = crate::discover(None).and_then(|p| crate::load(&p).ok());
    doc.as_ref().and_then(|d| doc_size(pool, d)).unwrap_or_else(|| size_from_env(pool))
}

/// Who holds (or waits for) a slot: a process, identified across pid reuse by its starttime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub start: u64,
    pub who: String,
    /// Units of the pool this job takes (§5: a release/LTO build is [`WEIGHT_RELEASE`]).
    pub weight: u64,
}

/// One lease line: `pid=<p> start=<s> who=<w> since=<epoch> waited=<secs> last=<epoch>
/// weight=<units>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub slot: u64,
    pub pid: u32,
    pub start: u64,
    pub who: String,
    pub since: u64,
    pub waited: u64,
    pub last: u64,
    pub weight: u64,
}

/// A compile job's weight in units of the compile pool (DESIGN-admission.md §5). A unit is
/// what one aeon-profile workspace build costs (≈2.7 GiB anon, ≈9 cores); a release build
/// (LTO, codegen-units=1) peaked at 15.3 GiB anon with 22 rustc alive, 145 s — ⌈15.3 ÷ 4⌉ = 4
/// units. A job heavier than the whole pool runs alone. Measured on the fat-LTO profile that
/// sp-zqo8s replaced (DESIGN-admission.md §5): a safe starting value, re-derived after rollout.
pub const WEIGHT_RELEASE: u64 = 4;

/// The weight of a rustc invocation's build: an optimised (`-C opt-level=` other than 0) or
/// LTO compile is a release-like build.
pub fn build_weight(rustc_args: &[String]) -> u64 {
    let mut prev_c = false;
    for a in rustc_args {
        let flag = if prev_c { Some(a.as_str()) } else { a.strip_prefix("-C") };
        prev_c = a == "-C";
        if let Some(f) = flag {
            let heavy = f.strip_prefix("opt-level=").is_some_and(|v| v != "0")
                || f == "lto"
                || f.strip_prefix("lto=").is_some_and(|v| v != "off" && v != "no" && v != "false");
            if heavy {
                return WEIGHT_RELEASE;
            }
        }
    }
    1
}

/// The weight of a cargo build of `profile` (testenv's in-place build).
pub fn profile_weight(profile: &str) -> u64 {
    if profile == "release" {
        WEIGHT_RELEASE
    } else {
        1
    }
}

fn clean_who(w: &str) -> String {
    let w: String = w.chars().map(|c| if c.is_whitespace() { '_' } else { c }).collect();
    if w.is_empty() {
        "-".into()
    } else {
        w
    }
}

impl Lease {
    pub fn render(&self) -> String {
        format!(
            "pid={} start={} who={} since={} waited={} last={} weight={}\n",
            self.pid,
            self.start,
            clean_who(&self.who),
            self.since,
            self.waited,
            self.last,
            self.weight.max(1)
        )
    }

    pub fn parse(slot: u64, text: &str) -> Option<Lease> {
        let mut l = Lease { slot, pid: 0, start: 0, who: String::new(), since: 0, waited: 0, last: 0, weight: 1 };
        for f in text.split_whitespace() {
            let (k, v) = f.split_once('=')?;
            match k {
                "pid" => l.pid = v.parse().ok()?,
                "start" => l.start = v.parse().ok()?,
                "who" => l.who = v.to_string(),
                "since" => l.since = v.parse().ok()?,
                "waited" => l.waited = v.parse().unwrap_or(0),
                "last" => l.last = v.parse().unwrap_or(0),
                "weight" => l.weight = v.parse::<u64>().unwrap_or(1).max(1),
                _ => {}
            }
        }
        (l.pid > 0).then_some(l)
    }

    fn is(&self, h: &Holder) -> bool {
        self.pid == h.pid && self.start == h.start
    }
}

/// The process table, behind a seam so the lease logic is testable without real pids.
pub trait Procs {
    /// Field 22 of `/proc/<pid>/stat`; None when the process does not exist.
    fn start_of(&self, pid: u32) -> Option<u64>;
    fn ppid_of(&self, pid: u32) -> Option<u32>;
}

pub struct RealProcs;

fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm may contain spaces and parens: the fields after it start past the LAST ')'.
    let rest = &s[s.rfind(')')? + 1..];
    Some(rest.split_whitespace().map(str::to_string).collect())
}

impl Procs for RealProcs {
    fn start_of(&self, pid: u32) -> Option<u64> {
        // rest[0] is field 3 (state); starttime is field 22 → rest[19].
        stat_fields(pid)?.get(19)?.parse().ok()
    }
    fn ppid_of(&self, pid: u32) -> Option<u32> {
        stat_fields(pid)?.get(1)?.parse().ok()
    }
}

/// A holder for a live process: its pid and starttime.
pub fn holder_for(pid: u32, who: &str, weight: u64, procs: &dyn Procs) -> Option<Holder> {
    Some(Holder { pid, start: procs.start_of(pid)?, who: who.to_string(), weight: weight.max(1) })
}

fn alive(pid: u32, start: u64, procs: &dyn Procs) -> bool {
    procs.start_of(pid) == Some(start)
}

/// The outcome of one scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Take {
    /// This holder has slot `slot` (fresh: taken by this scan; else it already held it).
    Admitted { slot: u64, fresh: bool },
    /// An ancestor of the holder holds `slot`: this job runs on it.
    Inherited { slot: u64, pid: u32 },
    /// Every slot up to the size is held by a live holder.
    Busy { holders: Vec<Lease> },
}

/// A lease that ended: what its telemetry row says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub pool: Pool,
    pub lease: Lease,
    pub size: u64,
    pub held: u64,
    pub end: &'static str,
}

/// The pool directory's mutex: an exclusive flock on `<dir>/lock`, held for one scan.
struct Mutex(File);

impl Mutex {
    fn lock(dir: &Path) -> std::io::Result<Mutex> {
        fs::create_dir_all(dir)?;
        let f = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("lock"))?;
        loop {
            // SAFETY: flock on an fd we own for the life of `f`.
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            if rc == 0 {
                return Ok(Mutex(f));
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

impl Drop for Mutex {
    fn drop(&mut self) {
        // SAFETY: unlocking our own fd; closing would release it too.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    fs::rename(&tmp, path)
}

/// Every lease file in `dir` (`slot.<n>`), parsed; unparsable files are dead leases.
fn read_leases(dir: &Path) -> Vec<(u64, Option<Lease>)> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(n) = name.strip_prefix("slot.").and_then(|n| n.parse::<u64>().ok()) else {
                continue;
            };
            let text = fs::read_to_string(e.path()).unwrap_or_default();
            out.push((n, Lease::parse(n, &text)));
        }
    }
    out.sort_by_key(|(n, _)| *n);
    out
}

/// A live waiter: `wait.<pid>` holding `start= who= since= weight=`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiter {
    pub pid: u32,
    pub since: u64,
    pub who: String,
    pub weight: u64,
}

/// Remove dead leases and dead waiters; return the live leases, the ended ones (for
/// telemetry) and the live waiters, oldest first.
fn reap(dir: &Path, pool: Pool, size: u64, procs: &dyn Procs) -> (Vec<Lease>, Vec<Ended>, Vec<Waiter>) {
    let mut live = Vec::new();
    let mut ended = Vec::new();
    for (n, l) in read_leases(dir) {
        match l {
            Some(l) if alive(l.pid, l.start, procs) => live.push(l),
            other => {
                let _ = fs::remove_file(dir.join(format!("slot.{n}")));
                if let Some(l) = other {
                    let held = l.last.max(l.since).saturating_sub(l.since);
                    ended.push(Ended { pool, lease: l, size, held, end: "reclaimed" });
                }
            }
        }
    }
    let mut waiters = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(pid) = name.strip_prefix("wait.").and_then(|p| p.parse::<u32>().ok()) {
                let text = fs::read_to_string(e.path()).unwrap_or_default();
                match Lease::parse(0, &format!("pid={pid} {text}")) {
                    Some(w) if alive(pid, w.start, procs) => waiters.push(Waiter { pid, since: w.since, who: w.who, weight: w.weight.max(1) }),
                    _ => {
                        let _ = fs::remove_file(e.path());
                    }
                }
            }
        }
    }
    waiters.sort_by_key(|w| (w.since, w.pid));
    (live, ended, waiters)
}

/// The ancestors of `pid`, nearest first (at most 64, stopping at pid 1).
fn ancestors(pid: u32, procs: &dyn Procs) -> Vec<u32> {
    let mut out = Vec::new();
    let mut p = pid;
    while out.len() < 64 {
        match procs.ppid_of(p) {
            Some(pp) if pp > 1 && pp != p => {
                out.push(pp);
                p = pp;
            }
            _ => break,
        }
    }
    out
}

/// ONE SCAN of a lease pool (compile or test) under its mutex: already ours, inherited from
/// an ancestor's lease, admitted, or busy. Admission is by units and first come, first served:
/// `me` is admitted when no older live waiter is queued ahead of it and its weight fits what
/// the live leases leave of `size` — or the pool is empty (a job heavier than the pool runs
/// alone). `waited` is recorded in a fresh lease.
pub fn try_take(
    run: &Path,
    pool: Pool,
    size: u64,
    me: &Holder,
    waited: u64,
    now: u64,
    procs: &dyn Procs,
) -> std::io::Result<(Take, Vec<Ended>)> {
    let dir = pool.dir(run);
    let _m = Mutex::lock(&dir)?;
    let (live, ended, waiters) = reap(&dir, pool, size, procs);
    if let Some(l) = live.iter().find(|l| l.is(me)) {
        if l.last < now {
            let mut l2 = l.clone();
            l2.last = now;
            let _ = write_atomic(&dir.join(format!("slot.{}", l.slot)), &l2.render());
        }
        return Ok((Take::Admitted { slot: l.slot, fresh: false }, ended));
    }
    let anc = ancestors(me.pid, procs);
    if let Some(l) = live.iter().find(|l| anc.contains(&l.pid)) {
        return Ok((Take::Inherited { slot: l.slot, pid: l.pid }, ended));
    }
    let used: u64 = live.iter().map(|l| l.weight.max(1)).sum();
    let first = waiters.first().map_or(true, |w| w.pid == me.pid);
    let fits = used == 0 || used + me.weight.max(1) <= size.max(1);
    if first && fits {
        let n = (1..).find(|n| !live.iter().any(|l| l.slot == *n)).unwrap_or(1);
        let l = Lease { slot: n, pid: me.pid, start: me.start, who: me.who.clone(), since: now, waited, last: now, weight: me.weight.max(1) };
        write_atomic(&dir.join(format!("slot.{n}")), &l.render())?;
        let _ = fs::remove_file(dir.join(format!("wait.{}", me.pid)));
        return Ok((Take::Admitted { slot: n, fresh: true }, ended));
    }
    let wf = dir.join(format!("wait.{}", me.pid));
    let queued = fs::read_to_string(&wf)
        .ok()
        .and_then(|t| Lease::parse(0, &format!("pid={} {t}", me.pid)))
        .is_some_and(|w| w.start == me.start);
    if !queued {
        // Written once: `since` is the queue position (first come, first served).
        let _ = write_atomic(&wf, &format!("start={} who={} since={} weight={}\n", me.start, clean_who(&me.who), now, me.weight.max(1)));
    }
    Ok((Take::Busy { holders: live }, ended))
}

/// What [`take_now`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Now {
    /// A lease was written at `slot`; `held_before` units were already held by others
    /// (> size: this lease oversubscribes the pool).
    Took { slot: u64, held_before: u64, fresh: bool },
    /// An ancestor of the holder already holds a lease in this pool.
    Inherited { slot: u64 },
}

/// A GATE's build takes a compile lease WITHOUT WAITING (sp-f4ig1-fix, DESIGN-admission.md
/// D11): written at once whatever the pool holds — oversubscribing it when full — so the
/// gate never waits, while agent leases (which must fit, [`try_take`]) queue behind it until
/// it is released. Already ours and inherited-from-an-ancestor behave as in [`try_take`].
pub fn take_now(run: &Path, pool: Pool, size: u64, me: &Holder, now: u64, procs: &dyn Procs) -> std::io::Result<(Now, Vec<Ended>)> {
    let dir = pool.dir(run);
    let _m = Mutex::lock(&dir)?;
    let (live, ended, _) = reap(&dir, pool, size, procs);
    let held_before: u64 = live.iter().filter(|l| !l.is(me)).map(|l| l.weight.max(1)).sum();
    if let Some(l) = live.iter().find(|l| l.is(me)) {
        return Ok((Now::Took { slot: l.slot, held_before, fresh: false }, ended));
    }
    let anc = ancestors(me.pid, procs);
    if let Some(l) = live.iter().find(|l| anc.contains(&l.pid)) {
        return Ok((Now::Inherited { slot: l.slot }, ended));
    }
    let n = (1..).find(|n| !live.iter().any(|l| l.slot == *n)).unwrap_or(1);
    let l = Lease { slot: n, pid: me.pid, start: me.start, who: me.who.clone(), since: now, waited: 0, last: now, weight: me.weight.max(1) };
    write_atomic(&dir.join(format!("slot.{n}")), &l.render())?;
    Ok((Now::Took { slot: n, held_before, fresh: true }, ended))
}

/// The log line of a fresh [`take_now`].
pub fn take_now_line(pool: Pool, size: u64, slot: u64, weight: u64, held_before: u64) -> String {
    let over = if held_before + weight > size { format!(" — oversubscribed: {} of {size} held", held_before + weight) } else { String::new() };
    format!("gate build took {} slot {slot} (weight {weight}) without waiting{over}; new agent builds queue behind it", pool.name())
}

/// [`take_now`] for process `holder_pid`, as a [`Guard`] that releases on drop. Never fails
/// the job: an unusable pool directory is said and the build runs unleased.
/// `q.inherit` is not consulted: the caller already knows this is a gate's build.
pub fn take_now_guard(q: &Request, size: u64, procs: &dyn Procs, say: &mut dyn FnMut(&str)) -> Guard {
    let (run, pool) = (q.run, q.pool);
    let Some(me) = holder_for(q.holder_pid, q.who, q.weight, procs) else {
        return Guard::inherited(pool, "none");
    };
    match take_now(run, pool, size, &me, now_epoch(), procs) {
        Ok((Now::Took { slot, held_before, fresh }, ended)) => {
            record(run, &ended);
            if fresh {
                say(&take_now_line(pool, size, slot, me.weight, held_before));
            }
            Guard { run: run.to_path_buf(), pool, size, slot, me: Some(me), waited: 0, token: format!("{}:{slot}", pool.name()) }
        }
        Ok((Now::Inherited { slot }, ended)) => {
            record(run, &ended);
            Guard::inherited(pool, &format!("{}:{slot}", pool.name()))
        }
        Err(e) => {
            say(&format!("admission: {} pool at {} unusable ({e}) — the gate build runs unleased", pool.name(), pool.dir(run).display()));
            Guard::inherited(pool, "none")
        }
    }
}

/// Release `slot` if it still names `me`; the ended lease for telemetry.
pub fn release(run: &Path, pool: Pool, size: u64, slot: u64, me: &Holder, now: u64) -> Option<Ended> {
    let dir = pool.dir(run);
    let _m = Mutex::lock(&dir).ok()?;
    let path = dir.join(format!("slot.{slot}"));
    let l = Lease::parse(slot, &fs::read_to_string(&path).ok()?)?;
    if !l.is(me) {
        return None;
    }
    let _ = fs::remove_file(&path);
    let held = now.saturating_sub(l.since);
    Some(Ended { pool, lease: l, size, held, end: "released" })
}

/// Drop a waiter's `wait.<pid>` file (a waiter that gives up or is inherited).
pub fn unwait(run: &Path, pool: Pool, pid: u32) {
    let _ = fs::remove_file(pool.dir(run).join(format!("wait.{pid}")));
}

/// The waiting line (DESIGN-admission.md §3.1). Held is in units: a release build shows its
/// weight (`×4`).
pub fn wait_line(pool: Pool, size: u64, holders: &[Lease], now: u64) -> String {
    let named: Vec<String> = holders
        .iter()
        .map(|l| {
            let w = if l.weight > 1 { format!(" ×{}", l.weight) } else { String::new() };
            format!("{}{w} (pid {}, {}s)", l.who, l.pid, now.saturating_sub(l.since))
        })
        .collect();
    format!(
        "waiting for a {} slot: {} of {} held by {}",
        pool.name(),
        holders.iter().map(|l| l.weight.max(1)).sum::<u64>(),
        size,
        if named.is_empty() { "-".to_string() } else { named.join(", ") }
    )
}

pub fn now_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn host_name() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".into())
}

/// The telemetry row of an ended lease (no trailing newline).
pub fn row(e: &Ended, ts: &str, host: &str) -> Result<String, String> {
    use serde_json::Value;
    let f = |k: &str, v: Value| (k.to_string(), v);
    tsd::build_row(
        ts,
        host,
        FAMILY,
        &[
            f("pool", Value::from(e.pool.name())),
            f("who", Value::from(e.lease.who.clone())),
            f("slot", Value::from(e.lease.slot)),
            f("size", Value::from(e.size)),
            f("weight", Value::from(e.lease.weight.max(1))),
            f("waited_secs", Value::from(e.lease.waited)),
            f("held_secs", Value::from(e.held)),
            f("end", Value::from(e.end)),
        ],
    )
}

/// Append ended leases to `<run>/tsd/admission.jsonl`. Best-effort: a meter never blocks a job.
pub fn record(run: &Path, ended: &[Ended]) {
    if ended.is_empty() {
        return;
    }
    let path = tsd::family_path(run, FAMILY);
    if let Some(d) = path.parent() {
        let _ = fs::create_dir_all(d);
    }
    let ts = tsd::iso_utc(now_epoch());
    let host = host_name();
    let mut text = String::new();
    for e in ended {
        if let Ok(l) = row(e, &ts, &host) {
            text.push_str(&l);
            text.push('\n');
        }
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// A held (or inherited) slot. Dropping it releases a slot this process took.
#[derive(Debug)]
pub struct Guard {
    run: PathBuf,
    pool: Pool,
    size: u64,
    slot: u64,
    me: Option<Holder>,
    /// Seconds spent waiting before admission.
    pub waited: u64,
    /// `<pool>:<slot>` for [`INHERIT_ENV`] on what this job runs.
    pub token: String,
}

impl Guard {
    /// A job that runs on someone else's slot (or with no admission): releases nothing.
    pub fn inherited(pool: Pool, token: &str) -> Guard {
        Guard { run: PathBuf::new(), pool, size: 0, slot: 0, me: None, waited: 0, token: token.to_string() }
    }
    pub fn slot(&self) -> u64 {
        self.slot
    }
    pub fn held(&self) -> bool {
        self.me.is_some()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(me) = self.me.take() {
            if let Some(e) = release(&self.run, self.pool, self.size, self.slot, &me, now_epoch()) {
                record(&self.run, &[e]);
            }
        }
    }
}

/// What [`acquire`] is asked for.
pub struct Request<'a> {
    pub run: &'a Path,
    pub pool: Pool,
    /// The process the slot is for (the cargo, for the RUSTC_WRAPPER; else ourselves).
    pub holder_pid: u32,
    pub who: &'a str,
    /// The caller's [`INHERIT_ENV`] value.
    pub inherit: Option<&'a str>,
    /// Units of the pool the job takes: 1, or [`WEIGHT_RELEASE`] for a release build.
    pub weight: u64,
}

/// What [`acquire`] reads and waits through: the pool's size (re-read every pass), the
/// process table and the sleep.
pub struct Seams<'a> {
    pub size_of: &'a dyn Fn() -> u64,
    pub procs: &'a dyn Procs,
    pub sleep: &'a dyn Fn(Duration),
}

fn real_sleep(d: Duration) {
    std::thread::sleep(d)
}

/// [`acquire`] against the real process table, sized from this process's environment.
pub fn acquire_real(q: &Request, say: &mut dyn FnMut(&str)) -> Guard {
    let pool = q.pool;
    let size_of = move || size_from_env(pool);
    acquire(q, &Seams { size_of: &size_of, procs: &RealProcs, sleep: &real_sleep }, say)
}

/// BLOCK until `holder_pid` holds a slot of `pool` (or inherits one), saying so on `say`.
/// Never fails for waiting: an IO error on the pool directory is said and the job proceeds
/// unadmitted (a scheduling tool never stops the world). `inherit` is the caller's
/// [`INHERIT_ENV`] value.
pub fn acquire(q: &Request, s: &Seams, say: &mut dyn FnMut(&str)) -> Guard {
    let (run, pool, holder_pid, who, inherit) = (q.run, q.pool, q.holder_pid, q.who, q.inherit);
    let (size_of, procs, sleep) = (s.size_of, s.procs, s.sleep);
    if let Some(t) = inherit.map(str::trim).filter(|t| !t.is_empty()) {
        return Guard::inherited(pool, t);
    }
    let Some(me) = holder_for(holder_pid, who, q.weight, procs) else {
        say(&format!("admission: cannot read process {holder_pid} — running without a {} slot", pool.name()));
        return Guard::inherited(pool, "none");
    };
    let t0 = now_epoch();
    let mut said_at: Option<u64> = None;
    loop {
        let size = size_of();
        let now = now_epoch();
        let waited = now.saturating_sub(t0);
        match try_take(run, pool, size, &me, waited, now, procs) {
            Ok((take, ended)) => {
                record(run, &ended);
                match take {
                    Take::Admitted { slot, .. } => {
                        if said_at.is_some() {
                            say(&format!("admitted to {} slot {slot} after {waited}s", pool.name()));
                        }
                        return Guard {
                            run: run.to_path_buf(),
                            pool,
                            size,
                            slot,
                            me: Some(me),
                            waited,
                            token: format!("{}:{slot}", pool.name()),
                        };
                    }
                    Take::Inherited { slot, .. } => {
                        unwait(run, pool, me.pid);
                        return Guard::inherited(pool, &format!("{}:{slot}", pool.name()));
                    }
                    Take::Busy { holders } => {
                        if due(said_at, now) {
                            say(&wait_line(pool, size, &holders, now));
                            said_at = Some(now);
                        }
                    }
                }
            }
            Err(e) => {
                say(&format!("admission: {} pool at {} unusable ({e}) — running without a slot", pool.name(), pool.dir(run).display()));
                return Guard::inherited(pool, "none");
            }
        }
        sleep(Duration::from_millis(1000));
    }
}

/// Say the waiting line now: the first time, then every [`SAY_EVERY`] seconds.
fn due(said_at: Option<u64>, now: u64) -> bool {
    said_at.map_or(true, |t| now.saturating_sub(t) >= SAY_EVERY)
}

/// One pool's occupancy, for `spira-admit status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occupancy {
    pub pool: Pool,
    pub size: u64,
    pub holders: Vec<Lease>,
    pub waiting: usize,
    /// Live waiters, head of the queue first.
    pub waiters: Vec<Waiter>,
}

impl Occupancy {
    /// Units held (a release build counts its weight).
    pub fn used(&self) -> u64 {
        self.holders.iter().map(|l| l.weight.max(1)).sum()
    }

    /// The head waiter's line when it cannot be admitted yet though a slot is free: FIFO
    /// holds everyone behind a head that needs more than the pool leaves.
    pub fn head_blocked(&self) -> Option<String> {
        let head = self.waiters.first()?;
        let used = self.used();
        let size = self.size.max(1);
        let fits = used == 0 || used + head.weight <= size;
        if fits || used >= size {
            return None;
        }
        Some(if head.weight >= size {
            format!("head needs {} of {}: waits for an empty pool", head.weight, size)
        } else {
            format!("head needs {} of {} with {} held: waits until {} free", head.weight, size, used, head.weight)
        })
    }
}

/// Read a lease pool (reaping the dead as any scan does); `None` when its mutex cannot be taken.
pub fn read_occupancy(run: &Path, pool: Pool, size: u64, procs: &dyn Procs) -> Option<Occupancy> {
    let dir = pool.dir(run);
    let _m = Mutex::lock(&dir).ok()?;
    let (holders, ended, waiters) = reap(&dir, pool, size, procs);
    record(run, &ended);
    Some(Occupancy { pool, size, holders, waiting: waiters.len(), waiters })
}

/// [`read_occupancy`], an empty pool when unreadable.
pub fn occupancy(run: &Path, pool: Pool, size: u64, procs: &dyn Procs) -> Occupancy {
    read_occupancy(run, pool, size, procs).unwrap_or(Occupancy { pool, size, holders: Vec::new(), waiting: 0, waiters: Vec::new() })
}

/// The gate's flock pool: a slot is held when its lock cannot be taken; its holder is the
/// sidecar `slot.<n>.holder` the gate writes (advisory).
pub fn gate_occupancy(run: &Path, size: u64) -> Occupancy {
    let dir = Pool::Gate.dir(run);
    let mut holders = Vec::new();
    let top = size.max(highest_gate_slot(&dir));
    for n in 1..=top {
        let lock = dir.join(format!("slot.{n}.lock"));
        let Ok(f) = OpenOptions::new().write(true).open(&lock) else { continue };
        // SAFETY: probing our own fd; released at once on success.
        let got = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if got {
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
            continue;
        }
        let text = fs::read_to_string(dir.join(format!("slot.{n}.holder"))).unwrap_or_default();
        holders.push(Lease::parse(n, &text).unwrap_or(Lease {
            slot: n,
            pid: 0,
            start: 0,
            who: "unknown".into(),
            since: 0,
            waited: 0,
            last: 0,
            weight: 1,
        }));
    }
    Occupancy { pool: Pool::Gate, size, holders, waiting: 0, waiters: Vec::new() }
}

fn highest_gate_slot(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.strip_prefix("slot.")?.strip_suffix(".lock")?.parse::<u64>().ok()
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// The gate's holder sidecar line for slot `n`.
pub fn gate_holder_line(pid: u32, start: u64, who: &str, since: u64) -> String {
    Lease { slot: 0, pid, start, who: who.to_string(), since, waited: 0, last: since, weight: 1 }.render()
}

/// A uniform jitter in `0..=max` seconds from `seed` (the aeon's pid and clock), so a batch
/// of aeons summoned together never starts in lockstep (DESIGN-admission.md §3.4).
pub fn jitter(max: u64, seed: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    // splitmix64: a well-mixed value from a weak seed.
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    z % (max + 1)
}

/// What the RUSTC_WRAPPER does with one rustc invocation (DESIGN-admission.md §3.3, D11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapperAction {
    /// Exec the compiler at once: a probe, or a job already admitted to a non-gate slot.
    Exec,
    /// Queue for a compile slot first (an agent's build).
    Wait,
    /// A gate's build: take a compile lease for the cargo without waiting, then exec.
    TakeNow,
}

/// The wrapper's one decision. `inherit` is [`INHERIT_ENV`]: `gate` (or `gate:<n>`) marks a
/// gate's build, any other non-empty value a job admitted elsewhere.
pub fn wrapper_action(inherit: Option<&str>, rustc_args: &[String]) -> WrapperAction {
    if !is_compile(rustc_args) {
        return WrapperAction::Exec;
    }
    match inherit.map(str::trim).filter(|t| !t.is_empty()) {
        None => WrapperAction::Wait,
        Some(t) if is_gate_token(t) => WrapperAction::TakeNow,
        Some(_) => WrapperAction::Exec,
    }
}

/// Is this [`INHERIT_ENV`] value a gate's?
pub fn is_gate_token(t: &str) -> bool {
    let t = t.trim();
    t == "gate" || t.starts_with("gate:")
}

/// Does this rustc argument list compile a crate? The probes (`-vV`, `--print …`) that
/// `cargo metadata` makes too are not builds and never queue (DESIGN-admission.md D6).
pub fn is_compile(args: &[String]) -> bool {
    args.iter().any(|a| a == "--crate-name")
}

#[cfg(test)]
mod tests;
