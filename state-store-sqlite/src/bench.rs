//! state-store-bench's core: argument parsing, the CAS load at a fixed rate, the synthetic IO
//! load, and the quantiles it reports. A measurement tool, never a corpus assertion.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write as _};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use state_store::{
    Backend, Compare, Config, Contract, MonotonicClock, Object, Ports, Request, Txn,
};

use crate::{Options, SqliteBackend};

pub const USAGE: &str = "usage: state-store-bench --path <db> --rate <txn/s> --duration-secs <n> \
[--load-dir <dir> --load-mb-per-s <n>] [--mix reconciler|audited] [--limit-ms <ms> [--min-secs <n>]] [--json]\n       state-store-bench conformance --path <db>";

/// Keys the CAS load cycles through, so the store holds a working set rather than one row.
const KEYS: u64 = 1_000;
const LOAD_CHUNK: usize = 1 << 20;
/// The load generator rewrites one file of at most this size.
const LOAD_FILE_MAX: u64 = 256 << 20;
/// Section keys the reconciler mix cycles through: one per publishing component.
const SECTIONS: u64 = 16;
/// Padding on a section write, so it carries a section document's bytes, not a counter's. An
/// estimate of a section with its actions and distance terms, sized up rather than down.
const SECTION_BYTES: usize = 4 << 10;
/// The TTLs the real writers put on a section and on a pass record.
const SECTION_TTL_SECS: u64 = 7 * 24 * 3600;
const PASS_TTL_SECS: u64 = 24 * 3600;

/// A sample slower than this is a stall.
pub const STALL_MS: f64 = 100.0;

/// Bytes of the WAL index the bench reads: the two `WalIndexHdr` copies, then `nBackfill`.
const SHM_PREFIX: usize = 100;
const BACKFILL_OFFSET: usize = 96;

/// One stall: when it ended, how long the commit itself took, how late it was against its
/// schedule, and whether the WAL's backfill mark moved across the commit.
struct Stall {
    ended_unix_us: u128,
    service_ms: f64,
    total_ms: f64,
    checkpointed: bool,
}

/// A sync of the load generator that took over [`STALL_MS`]: the device was slow then.
#[derive(Clone, Copy)]
struct SlowSync {
    ended_unix_us: u128,
    took_us: u64,
}

type SlowSyncs = Arc<std::sync::Mutex<Vec<SlowSync>>>;

fn unix_us() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros())
}

/// `nBackfill` of a WAL index (`<db>-shm`), or `None` for a buffer too short to hold it.
fn backfill(shm: &[u8]) -> Option<u32> {
    let b = shm.get(BACKFILL_OFFSET..SHM_PREFIX)?;
    Some(u32::from_ne_bytes(b.try_into().ok()?))
}

pub(crate) fn read_backfill(shm: &File, path: &str) -> std::result::Result<u32, String> {
    let mut buf = [0u8; SHM_PREFIX];
    shm.read_exact_at(&mut buf, 0)
        .map_err(|e| format!("cannot read the WAL index {path}: {e}"))?;
    backfill(&buf).ok_or_else(|| format!("the WAL index {path} is too short"))
}

/// The checkpointer's completion signal: blocks until more than `after` checkpoints have
/// completed and returns the count, or `None` when `cap` runs out. `cap` is a hang guard, never a verdict.
pub fn await_checkpoint(b: &SqliteBackend, after: u64, cap: Duration) -> Option<u64> {
    let c = &b.0.checkpointer;
    let (s, _) = c
        .done
        .wait_timeout_while(crate::lock(&c.state), cap, |s| s.completed <= after)
        .unwrap_or_else(|p| p.into_inner());
    (s.completed > after).then_some(s.completed)
}

/// One checkpoint pass, the same step the checkpointer thread takes: PASSIVE, then the WAL
/// restart. `true` when the restart was not busy. Each call waits at most `max_read_ms`.
pub fn checkpoint_pass(b: &SqliteBackend) -> Result<bool, String> {
    let conn = crate::connect(&b.0.path).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::ZERO).map_err(|e| e.to_string())?;
    let max_read_ms = b.0.max_read_ms.load(Ordering::SeqCst);
    let pass = crate::checkpoint_step(&conn, &b.0.writer, &b.0.read_gate, &*b.0.clock, max_read_ms, b.0.wal_line_bytes, true, |_| {})?;
    Ok(!pass.busy)
}

/// `nBackfill` of the open store at `db`: the WAL frames a checkpoint has copied home.
pub fn wal_backfill(db: &std::path::Path) -> std::result::Result<u32, String> {
    let shm_path = format!("{}-shm", db.display());
    let shm = File::open(&shm_path).map_err(|e| format!("cannot open the WAL index {shm_path}: {e}"))?;
    read_backfill(&shm, &shm_path)
}

impl Stall {
    /// Why this commit was slow: it was only queued behind an earlier stall, it ran a
    /// checkpoint or WAL restart, the load generator's own sync was slow in the same window
    /// (the device), or nothing says.
    fn cause(&self, slow: &[SlowSync]) -> &'static str {
        if self.service_ms <= STALL_MS {
            return "queued";
        }
        if self.checkpointed {
            return "checkpoint";
        }
        let began = self
            .ended_unix_us
            .saturating_sub((self.service_ms * 1_000.0) as u128);
        let overlaps = |ended: u128, took_us: u64| {
            ended >= began && ended.saturating_sub(u128::from(took_us)) <= self.ended_unix_us
        };
        if slow.iter().any(|s| overlaps(s.ended_unix_us, s.took_us)) {
            "device"
        } else {
            "unattributed"
        }
    }
}

fn report_stalls(stalls: &[Stall], slow: &[SlowSync]) {
    let mut by_cause = std::collections::BTreeMap::<&str, u64>::new();
    let mut roots = 0;
    for s in stalls {
        let cause = s.cause(slow);
        *by_cause.entry(cause).or_default() += 1;
        if s.service_ms > STALL_MS {
            roots += 1;
        }
        eprintln!(
            "state-store-bench: stall end_unix_us={} service_ms={:.1} total_ms={:.1} cause={cause}",
            s.ended_unix_us, s.service_ms, s.total_ms
        );
    }
    eprintln!(
        "state-store-bench: {} samples over {STALL_MS}ms by cause: {by_cause:?}; {roots} commits took over {STALL_MS}ms themselves",
        stalls.len()
    );
}

/// What each commit of the run writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mix {
    /// Every commit is an audited CAS, the store's load before the reconciler.
    Audited,
    /// The goodput reconciler's daily traffic in proportion: of every twelve commits, five
    /// audited CAS writes (today's load), six section publishes and one pass record. Those
    /// seven touch only `telemetry/`, so they write no `events/` key, carry a TTL, and a
    /// section is several KiB rather than a counter.
    Reconciler,
}

/// One commit's kind under a [`Mix`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Commit {
    Audited,
    Section,
    Pass,
}

impl Mix {
    fn parse(v: &str) -> std::result::Result<Mix, String> {
        match v {
            "reconciler" => Ok(Mix::Reconciler),
            "audited" => Ok(Mix::Audited),
            _ => Err(format!("--mix is reconciler or audited, got {v}")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mix::Audited => "audited",
            Mix::Reconciler => "reconciler",
        }
    }

    pub fn commit(self, i: u64) -> Commit {
        match (self, i % 12) {
            (Mix::Audited, _) | (Mix::Reconciler, 0..=4) => Commit::Audited,
            (Mix::Reconciler, 11) => Commit::Pass,
            (Mix::Reconciler, _) => Commit::Section,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CasArgs {
    pub path: PathBuf,
    pub rate: f64,
    pub duration_secs: u64,
    pub load_dir: Option<PathBuf>,
    pub load_mb_per_s: u64,
    pub mix: Mix,
    pub json: bool,
    /// `--limit-ms`: the p99.9 the gate holds the store to. Given, the run stops as soon as
    /// the verdict is settled (see [`decide`]); `--duration-secs` is then the most it runs.
    pub limit_ms: Option<f64>,
    /// `--min-secs`: the least a settled run lasts, so slow periodic writers on the disk (the
    /// journal commit, a persist pass) are inside the window. Default 300.
    pub min_secs: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Cas(CasArgs),
    Conformance { path: PathBuf },
}

pub fn parse(args: &[String]) -> std::result::Result<Command, String> {
    let (conformance, rest) = match args.first().map(String::as_str) {
        Some("conformance") => (true, &args[1..]),
        _ => (false, args),
    };
    let mut path = None;
    let mut rate = None;
    let mut duration = None;
    let mut load_dir = None;
    let mut load_mb = None;
    let mut mix = Mix::Reconciler;
    let mut json = false;
    let mut limit = None;
    let mut min_secs = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
        };
        match a.as_str() {
            "--path" => path = Some(PathBuf::from(val("--path")?)),
            "--rate" if !conformance => rate = Some(val("--rate")?),
            "--duration-secs" if !conformance => duration = Some(val("--duration-secs")?),
            "--load-dir" if !conformance => load_dir = Some(PathBuf::from(val("--load-dir")?)),
            "--load-mb-per-s" if !conformance => load_mb = Some(val("--load-mb-per-s")?),
            "--mix" if !conformance => mix = Mix::parse(&val("--mix")?)?,
            "--json" if !conformance => json = true,
            "--limit-ms" if !conformance => limit = Some(val("--limit-ms")?),
            "--min-secs" if !conformance => min_secs = Some(val("--min-secs")?),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    let path = path.ok_or_else(|| format!("--path is required\n{USAGE}"))?;
    if conformance {
        return Ok(Command::Conformance { path });
    }
    let num = |name: &str, v: Option<String>| -> std::result::Result<f64, String> {
        let v = v.ok_or_else(|| format!("{name} is required\n{USAGE}"))?;
        v.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| format!("{name} must be a positive number, got {v}"))
    };
    let rate = num("--rate", rate)?;
    let duration_secs = match duration {
        None => return Err(format!("--duration-secs is required\n{USAGE}")),
        Some(v) => v.parse::<u64>().ok().filter(|n| *n >= 1).ok_or_else(|| {
            format!("--duration-secs must be a whole number of seconds >= 1, got {v}")
        })?,
    };
    let load_mb_per_s = match (&load_dir, load_mb) {
        (Some(_), Some(v)) => num("--load-mb-per-s", Some(v))? as u64,
        (Some(_), None) => return Err(format!("--load-dir needs --load-mb-per-s\n{USAGE}")),
        (None, Some(_)) => return Err(format!("--load-mb-per-s needs --load-dir\n{USAGE}")),
        (None, None) => 0,
    };
    let limit_ms = match limit {
        None => None,
        Some(v) => Some(num("--limit-ms", Some(v))?),
    };
    let min_secs = match min_secs {
        None => 300,
        Some(_) if limit_ms.is_none() => return Err(format!("--min-secs needs --limit-ms\n{USAGE}")),
        Some(v) => v.parse::<u64>().map_err(|_| format!("--min-secs must be a whole number of seconds, got {v}"))?,
    };
    Ok(Command::Cas(CasArgs {
        path,
        rate,
        duration_secs,
        load_dir,
        load_mb_per_s,
        mix,
        json,
        limit_ms,
        min_secs,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quantiles {
    pub samples: usize,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub max_ms: f64,
    /// p99 of each commit's own time, from when it began: the lateness quantiles above also
    /// carry the queue a stall leaves behind it.
    pub service_p99_ms: f64,
    /// Commits whose own time passed [`STALL_MS`].
    pub slow_commits: u64,
    /// Samples slower than `--limit-ms` (0 without one).
    pub exceed: u64,
    pub ended: Ended,
}

/// How a run ended: the whole `--duration-secs`, or early once [`decide`] settled it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Full,
    EarlyPass,
    EarlyFail,
}

impl Ended {
    pub fn name(self) -> &'static str {
        match self {
            Ended::Full => "full",
            Ended::EarlyPass => "early-pass",
            Ended::EarlyFail => "early-fail",
        }
    }
}

/// The share of samples a p99.9 at the limit lets exceed it.
const TAIL: f64 = 0.001;
/// How unlikely a settled verdict must be under "p99.9 is exactly at the limit".
const ALPHA: f64 = 0.01;

/// P(X <= k) for X ~ Poisson(lambda), summed in log space.
fn poisson_cdf(k: u64, lambda: f64) -> f64 {
    let (mut term, mut sum) = ((-lambda).exp(), 0.0);
    for i in 0..=k {
        if i > 0 {
            term *= lambda / i as f64;
        }
        sum += term;
    }
    sum.min(1.0)
}

/// Whether `exceed` slow samples out of `n` already settle "p99.9 is under the limit"
/// (EarlyPass) or "over it" (EarlyFail): a sequential test against a p99.9 exactly at the
/// limit, where the count of slow samples is Poisson(n x 0.001). Too few to be chance is a
/// pass, too many a fail; in between, keep sampling. None: not settled.
pub fn decide(n: u64, exceed: u64) -> Option<Ended> {
    let lambda = n as f64 * TAIL;
    if lambda < 1.0 {
        return None;
    }
    if exceed > 0 && 1.0 - poisson_cdf(exceed - 1, lambda) < ALPHA {
        return Some(Ended::EarlyFail);
    }
    if poisson_cdf(exceed, lambda) < ALPHA {
        return Some(Ended::EarlyPass);
    }
    None
}

/// Nearest-rank quantiles of `ms`; all zero for no samples.
pub fn quantiles(ms: &mut [f64]) -> Quantiles {
    ms.sort_by(f64::total_cmp);
    let rank = |q: f64| {
        if ms.is_empty() {
            return 0.0;
        }
        let i = ((q * ms.len() as f64).ceil() as usize).clamp(1, ms.len());
        ms[i - 1]
    };
    Quantiles {
        samples: ms.len(),
        p50_ms: rank(0.5),
        p99_ms: rank(0.99),
        p999_ms: rank(0.999),
        max_ms: ms.last().copied().unwrap_or(0.0),
        service_p99_ms: 0.0,
        slow_commits: 0,
        exceed: 0,
        ended: Ended::Full,
    }
}

pub fn report(q: &Quantiles, a: &CasArgs) -> serde_json::Value {
    json!({
        "samples": q.samples,
        "p50_ms": q.p50_ms,
        "p99_ms": q.p99_ms,
        "p999_ms": q.p999_ms,
        "max_ms": q.max_ms,
        "exceed": q.exceed,
        "limit_ms": a.limit_ms,
        "ended": q.ended.name(),
        "rate": a.rate,
        "duration_secs": a.duration_secs,
        "load_mb_per_s": a.load_mb_per_s,
        "mix": a.mix.name(),
        "service_p99_ms": q.service_p99_ms,
        "slow_commits": q.slow_commits,
    })
}

/// Writes and fsyncs `mb_per_s` MiB a second into one file under `dir` until `stop`.
fn io_load(
    dir: &Path,
    mb_per_s: u64,
    stop: Arc<AtomicBool>,
    slow: SlowSyncs,
) -> std::io::Result<()> {
    let path = dir.join(format!("state-store-bench-load-{}", std::process::id()));
    let chunk = vec![0x5a_u8; LOAD_CHUNK];
    let every = Duration::from_secs_f64(1.0 / mb_per_s as f64);
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;
    let mut written = 0;
    let mut next = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if written >= LOAD_FILE_MAX {
            f.set_len(0)?;
            f.seek(SeekFrom::Start(0))?;
            written = 0;
        }
        f.write_all(&chunk)?;
        let began = Instant::now();
        f.sync_data()?;
        let took = began.elapsed();
        if took.as_secs_f64() * 1_000.0 > STALL_MS {
            slow.lock().unwrap_or_else(|e| e.into_inner()).push(SlowSync {
                ended_unix_us: unix_us(),
                took_us: took.as_micros() as u64,
            });
        }
        written += LOAD_CHUNK as u64;
        next += every;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    drop(f);
    std::fs::remove_file(&path)
}

/// One CAS txn per tick at `rate`, of the kinds `a.mix` names, for `duration_secs`. A sample
/// is measured from the txn's scheduled start, so a stall delays the samples behind it too.
pub fn run_cas(a: &CasArgs) -> std::result::Result<Quantiles, String> {
    let backend = SqliteBackend::open(&a.path, Options::default()).map_err(|e| e.to_string())?;
    let mono = Arc::new(MonotonicClock::new());
    let ports = Ports {
        mono: mono.clone(),
        wall: Arc::new(runtime::ports::WallClock),
    };
    let c = Contract::open(
        backend,
        ports,
        Config {
            min_lock_ttl_ms: 1_000,
        },
    )
    .map_err(|e| e.to_string())?;
    let shm_path = format!("{}-shm", a.path.display());
    let shm = File::open(&shm_path)
        .map_err(|e| format!("cannot open the WAL index {shm_path} to attribute stalls: {e}"))?;
    let mut stalls = Vec::new();
    let mut services = Vec::new();

    let stop = Arc::new(AtomicBool::new(false));
    let slow = SlowSyncs::default();
    let load = a.load_dir.clone().map(|dir| {
        let (stop, mb, slow) = (stop.clone(), a.load_mb_per_s, slow.clone());
        std::thread::spawn(move || io_load(&dir, mb, stop, slow))
    });

    let every = Duration::from_secs_f64(1.0 / a.rate);
    let total = (a.rate * a.duration_secs as f64).ceil() as u64;
    let run = format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
        std::process::id()
    );
    let mut samples = Vec::with_capacity(total as usize);
    let start = Instant::now();
    let mut failed = None;
    let (mut exceed, mut ended) = (0u64, Ended::Full);
    let mut next_report = Duration::from_secs(60);
    for i in 0..total {
        let due = start + every.mul_f64(i as f64);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let before = match read_backfill(&shm, &shm_path) {
            Ok(b) => b,
            Err(e) => {
                failed = Some(e);
                break;
            }
        };
        let began = Instant::now();
        let committed = commit(&c, a.mix, i, &run);
        let service_ms = began.elapsed().as_secs_f64() * 1_000.0;
        let after = match read_backfill(&shm, &shm_path) {
            Ok(b) => b,
            Err(e) => {
                failed = Some(e);
                break;
            }
        };
        match committed {
            Ok(()) => {
                let ms = due.elapsed().as_secs_f64() * 1_000.0;
                services.push(service_ms);
                if ms > STALL_MS {
                    stalls.push(Stall {
                        ended_unix_us: unix_us(),
                        service_ms,
                        total_ms: ms,
                        checkpointed: before != after,
                    });
                }
                if a.limit_ms.is_some_and(|l| ms > l) {
                    exceed += 1;
                }
                samples.push(ms);
            }
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
        let elapsed = start.elapsed();
        if elapsed >= next_report {
            next_report += Duration::from_secs(60);
            let q = quantiles(&mut samples.clone());
            eprintln!(
                "state-store-bench: {}s samples={} p50={:.3}ms p99={:.3}ms p99.9={:.3}ms max={:.3}ms exceed={exceed}",
                elapsed.as_secs(),
                q.samples,
                q.p50_ms,
                q.p99_ms,
                q.p999_ms,
                q.max_ms
            );
        }
        if a.limit_ms.is_some() && elapsed.as_secs() >= a.min_secs {
            if let Some(e) = decide(samples.len() as u64, exceed) {
                ended = e;
                break;
            }
        }
    }
    stop.store(true, Ordering::SeqCst);
    report_stalls(&stalls, &slow.lock().unwrap_or_else(|e| e.into_inner()));
    if let Some(h) = load {
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("load generator: {e}")),
            Err(_) => return Err("load generator panicked".into()),
        }
    }
    match failed {
        Some(e) => Err(e),
        None => {
            let mut q = quantiles(&mut samples);
            q.exceed = exceed;
            q.ended = ended;
            q.slow_commits = services.iter().filter(|ms| **ms > STALL_MS).count() as u64;
            q.service_p99_ms = quantiles(&mut services).p99_ms;
            Ok(q)
        }
    }
}

/// Commit `i` of a run under `mix`: a CAS on the key it rewrites, compared on the revision
/// read just before, as every real writer does. A lost CAS is an error: the bench is the only
/// writer, so one means the store did not keep what it acknowledged.
pub fn commit<B: Backend>(
    c: &Contract<B>,
    mix: Mix,
    i: u64,
    run: &str,
) -> std::result::Result<(), String> {
    let kind = mix.commit(i);
    let (key, value) = match kind {
        Commit::Audited => (
            format!("bench/{}", i % KEYS),
            Object::new(json!({ "i": i })),
        ),
        Commit::Section => (
            format!("telemetry/bench/sections/{}", i % SECTIONS),
            Object::new(json!({ "i": i, "status": "s".repeat(SECTION_BYTES) }))
                .with_ttl(SECTION_TTL_SECS),
        ),
        Commit::Pass => (
            "telemetry/bench/pass".to_string(),
            Object::new(json!({ "n": i / 12 })).with_ttl(PASS_TTL_SECS),
        ),
    };
    let seen = c.get(&key).map_err(|e| e.to_string())?.map(|v| v.mod_rev);
    let mut t = Txn::new()
        .when(Compare::ModRev(key.clone(), seen))
        .put(&key, value.clone());
    if kind == Commit::Pass {
        t = t.put(&format!("telemetry/bench/passes/{}", i / 12), value);
    }
    let r = c
        .txn(
            &t,
            &Request::new(
                &format!("bench-{run}-{i}"),
                "state-store-bench",
                "bench.cas",
            ),
        )
        .map_err(|e| e.to_string())?;
    if r.succeeded {
        Ok(())
    } else {
        Err(format!("CAS on {key} lost: {r:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_stall_is_blamed_on_a_checkpoint_before_the_device() {
        let stall = |service_ms, checkpointed| Stall {
            ended_unix_us: 1_000_000,
            service_ms,
            total_ms: service_ms,
            checkpointed,
        };
        let slow = [SlowSync { ended_unix_us: 1_000_000, took_us: 150_000 }];
        assert_eq!(stall(300.0, true).cause(&slow), "checkpoint");
        assert_eq!(stall(300.0, false).cause(&slow), "device");
        assert_eq!(stall(300.0, false).cause(&[]), "unattributed");
        let far = [SlowSync { ended_unix_us: 9_000_000, took_us: 150_000 }];
        assert_eq!(stall(300.0, false).cause(&far), "unattributed");
        let queued = Stall { ended_unix_us: 1_000_000, service_ms: 0.5, total_ms: 250.0, checkpointed: false };
        assert_eq!(queued.cause(&[]), "queued");
    }

    #[test]
    fn backfill_reads_the_ckpt_info_field_and_refuses_a_short_index() {
        let mut shm = vec![0u8; 136];
        shm[96..100].copy_from_slice(&7u32.to_ne_bytes());
        assert_eq!(backfill(&shm), Some(7));
        assert_eq!(backfill(&shm[..99]), None);
    }

    #[test]
    fn the_report_carries_the_commits_own_p99_beside_the_lateness_quantiles() {
        let a = CasArgs {
            path: "p".into(),
            rate: 50.0,
            duration_secs: 1,
            load_dir: None,
            load_mb_per_s: 0,
            mix: Mix::Reconciler,
            json: true,
            limit_ms: None,
            min_secs: 300,
        };
        let mut q = quantiles(&mut [1.0, 2.0]);
        q.service_p99_ms = 7.5;
        q.slow_commits = 3;
        let r = report(&q, &a);
        assert_eq!((r["service_p99_ms"].as_f64(), r["slow_commits"].as_u64()), (Some(7.5), Some(3)));
    }

    fn cas_args(path: std::path::PathBuf) -> CasArgs {
        CasArgs {
            path,
            rate: 20.0,
            duration_secs: 1,
            load_dir: None,
            load_mb_per_s: 0,
            mix: Mix::Reconciler,
            json: false,
            limit_ms: None,
            min_secs: 300,
        }
    }

    #[test]
    fn a_run_whose_store_cannot_be_opened_is_refused_naming_the_path() {
        let path = std::path::PathBuf::from("nonexistent-bench-dir/b.db");
        let e = run_cas(&cas_args(path)).unwrap_err();
        assert!(e.contains("nonexistent-bench-dir"), "{e}");
    }

    #[test]
    fn inline_checkpoint_is_an_unknown_argument() {
        let got = parse(&args(&["--path", "p", "--rate", "5", "--duration-secs", "1", "--inline-checkpoint"]));
        assert!(got.is_err());
    }

    #[test]
    fn too_few_samples_settle_nothing_however_slow() {
        assert_eq!(decide(500, 500), None);
        assert_eq!(decide(0, 0), None);
    }

    #[test]
    fn the_settled_thresholds_match_a_poisson_tail_at_one_in_a_thousand() {
        // lambda = 15: P(X <= 6) ~ 0.0076 (pass), P(X <= 7) ~ 0.018 (not yet);
        // P(X >= 26) ~ 0.0062 (fail), P(X >= 25) ~ 0.0112 (not yet).
        assert_eq!(decide(15_000, 6), Some(Ended::EarlyPass));
        assert_eq!(decide(15_000, 7), None);
        assert_eq!(decide(15_000, 26), Some(Ended::EarlyFail));
        assert_eq!(decide(15_000, 25), None);
    }

    #[test]
    fn a_limit_is_parsed_with_its_minimum_window_and_a_window_alone_is_refused() {
        let got = parse(&args(&["--path", "p", "--rate", "50", "--duration-secs", "1800", "--limit-ms", "20", "--min-secs", "120"])).unwrap();
        assert!(matches!(got, Command::Cas(CasArgs { limit_ms: Some(l), min_secs: 120, .. }) if l == 20.0));
        assert!(parse(&args(&["--path", "p", "--rate", "50", "--duration-secs", "9", "--min-secs", "5"])).is_err());
    }

    #[test]
    fn the_cas_mode_takes_named_arguments_and_the_load_needs_both_halves() {
        let got = parse(&args(&[
            "--path",
            "p",
            "--rate",
            "50",
            "--duration-secs",
            "2",
            "--json",
        ]))
        .unwrap();
        assert_eq!(
            got,
            Command::Cas(CasArgs {
                path: "p".into(),
                rate: 50.0,
                duration_secs: 2,
                load_dir: None,
                load_mb_per_s: 0,
                mix: Mix::Reconciler,
                json: true,
                limit_ms: None,
                min_secs: 300,
            })
        );
        let load = parse(&args(&[
            "--path",
            "p",
            "--rate",
            "5",
            "--duration-secs",
            "1",
            "--load-dir",
            "d",
            "--load-mb-per-s",
            "8",
        ]))
        .unwrap();
        assert!(matches!(
            load,
            Command::Cas(CasArgs {
                load_mb_per_s: 8,
                ..
            })
        ));
        assert!(parse(&args(&[
            "--path",
            "p",
            "--rate",
            "5",
            "--duration-secs",
            "1",
            "--load-dir",
            "d"
        ]))
        .is_err());
        assert!(parse(&args(&[
            "--path",
            "p",
            "--rate",
            "0",
            "--duration-secs",
            "1"
        ]))
        .is_err());
        let half = ["--path", "p", "--rate", "5", "--duration-secs", "0.5"];
        assert!(parse(&args(&half)).is_err());
        assert!(parse(&args(&["--rate", "5", "--duration-secs", "1"])).is_err());
        assert!(parse(&args(&["p"])).is_err());
    }

    #[test]
    fn the_mix_defaults_to_the_reconcilers_and_refuses_an_unknown_name() {
        let with = |mix: &str| {
            parse(&args(&[
                "--path",
                "p",
                "--rate",
                "5",
                "--duration-secs",
                "1",
                "--mix",
                mix,
            ]))
        };
        assert!(matches!(
            with("audited"),
            Ok(Command::Cas(CasArgs {
                mix: Mix::Audited,
                ..
            }))
        ));
        assert!(matches!(
            with("reconciler"),
            Ok(Command::Cas(CasArgs {
                mix: Mix::Reconciler,
                ..
            }))
        ));
        assert!(matches!(
            parse(&args(&[
                "--path",
                "p",
                "--rate",
                "5",
                "--duration-secs",
                "1"
            ])),
            Ok(Command::Cas(CasArgs {
                mix: Mix::Reconciler,
                ..
            }))
        ));
        assert!(with("telemetry").is_err());
        assert!(parse(&args(&["conformance", "--path", "p", "--mix", "audited"])).is_err());
    }

    #[test]
    fn the_reconciler_mix_writes_the_designs_daily_proportions() {
        let count = |mix: Mix, kind: Commit| (0..120).filter(|i| mix.commit(*i) == kind).count();
        assert_eq!(
            [Commit::Audited, Commit::Section, Commit::Pass].map(|k| count(Mix::Reconciler, k)),
            [50, 60, 10]
        );
        assert_eq!(count(Mix::Audited, Commit::Audited), 120);
    }

    #[test]
    fn a_reconciler_mix_run_audits_only_its_audited_writes() {
        let clock = runtime::ports::VirtualClock::new(1_000);
        let c = Contract::open(
            state_store::MemBackend::new(),
            Ports::virtual_clock(&clock),
            Config {
                min_lock_ttl_ms: 1_000,
            },
        )
        .unwrap();
        for i in 0..24 {
            commit(&c, Mix::Reconciler, i, "t").unwrap();
        }
        let keys = |prefix: &str| {
            c.list(&[prefix], &state_store::Selector::Everything)
                .unwrap()
                .0
        };
        assert_eq!(
            keys("events/").len(),
            10,
            "only the audited ten write events/"
        );
        let telemetry = keys("telemetry/");
        // Sections 5..=10, then 1..=6: the second cycle's CAS rewrites 5 and 6 at their revision.
        assert_eq!(
            telemetry.len(),
            10 + 1 + 2,
            "ten sections, the pass, two histories"
        );
        assert!(telemetry
            .iter()
            .all(|k| k.value.metadata.ttl_secs.is_some()));
        let section = c.get("telemetry/bench/sections/5").unwrap().unwrap();
        assert!(section.value.encode().len() > SECTION_BYTES);
        assert_eq!(keys("bench/").len(), 10);
    }

    #[test]
    fn the_conformance_mode_takes_only_a_path() {
        assert_eq!(
            parse(&args(&["conformance", "--path", "live.db"])).unwrap(),
            Command::Conformance {
                path: "live.db".into()
            }
        );
        assert!(parse(&args(&["conformance", "--path", "p", "--rate", "5"])).is_err());
    }

    #[test]
    fn quantiles_are_nearest_rank_and_empty_is_zero() {
        let mut ms: Vec<f64> = (1..=1_000).map(f64::from).collect();
        let q = quantiles(&mut ms);
        assert_eq!(
            (q.samples, q.p50_ms, q.p99_ms, q.p999_ms, q.max_ms),
            (1_000, 500.0, 990.0, 999.0, 1_000.0)
        );
        let mut ten: Vec<f64> = (1..=10).map(f64::from).collect();
        let q = quantiles(&mut ten);
        assert_eq!((q.p50_ms, q.p99_ms), (5.0, 10.0), "a rank rounds up");
        let q = quantiles(&mut []);
        assert_eq!((q.samples, q.max_ms), (0, 0.0));
        let q = quantiles(&mut [3.0]);
        assert_eq!((q.p50_ms, q.p999_ms), (3.0, 3.0));
    }
}
