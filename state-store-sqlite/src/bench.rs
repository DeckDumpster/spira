//! state-store-bench's core: argument parsing, the CAS load at a fixed rate, the synthetic IO
//! load, and the quantiles it reports. A measurement tool, never a corpus assertion.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use state_store::{Compare, Config, Contract, MonotonicClock, Object, Ports, Request, Txn};

use crate::{Options, SqliteBackend};

pub const USAGE: &str = "usage: state-store-bench --path <db> --rate <txn/s> --duration-secs <n> \
[--load-dir <dir> --load-mb-per-s <n>] [--json]\n       state-store-bench conformance --path <db>";

/// Keys the CAS load cycles through, so the store holds a working set rather than one row.
const KEYS: u64 = 1_000;
const LOAD_CHUNK: usize = 1 << 20;
/// The load generator rewrites one file of at most this size.
const LOAD_FILE_MAX: u64 = 256 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct CasArgs {
    pub path: PathBuf,
    pub rate: f64,
    pub duration_secs: u64,
    pub load_dir: Option<PathBuf>,
    pub load_mb_per_s: u64,
    pub json: bool,
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
    let mut json = false;
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
            "--json" if !conformance => json = true,
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
    Ok(Command::Cas(CasArgs {
        path,
        rate,
        duration_secs,
        load_dir,
        load_mb_per_s,
        json,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quantiles {
    pub samples: usize,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub max_ms: f64,
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
    }
}

pub fn report(q: &Quantiles, a: &CasArgs) -> serde_json::Value {
    json!({
        "samples": q.samples,
        "p50_ms": q.p50_ms,
        "p99_ms": q.p99_ms,
        "p999_ms": q.p999_ms,
        "max_ms": q.max_ms,
        "rate": a.rate,
        "duration_secs": a.duration_secs,
        "load_mb_per_s": a.load_mb_per_s,
    })
}

/// Writes and fsyncs `mb_per_s` MiB a second into one file under `dir` until `stop`.
fn io_load(dir: &Path, mb_per_s: u64, stop: Arc<AtomicBool>) -> std::io::Result<()> {
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
        f.sync_data()?;
        written += LOAD_CHUNK as u64;
        next += every;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    drop(f);
    std::fs::remove_file(&path)
}

/// One CAS txn per tick at `rate`, each with its `events/` key, for `duration_secs`. A sample
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

    let stop = Arc::new(AtomicBool::new(false));
    let load = a.load_dir.clone().map(|dir| {
        let (stop, mb) = (stop.clone(), a.load_mb_per_s);
        std::thread::spawn(move || io_load(&dir, mb, stop))
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
    for i in 0..total {
        let due = start + every.mul_f64(i as f64);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let key = format!("bench/{}", i % KEYS);
        let seen = match c.get(&key) {
            Ok(v) => v.map(|v| v.mod_rev),
            Err(e) => {
                failed = Some(e.to_string());
                break;
            }
        };
        let t = Txn::new()
            .when(Compare::ModRev(key.clone(), seen))
            .put(&key, Object::new(json!({ "i": i })));
        let r = c.txn(
            &t,
            &Request::new(
                &format!("bench-{run}-{i}"),
                "state-store-bench",
                "bench.cas",
            ),
        );
        match r {
            Ok(r) if r.succeeded => samples.push(due.elapsed().as_secs_f64() * 1_000.0),
            Ok(r) => {
                failed = Some(format!("CAS on {key} lost: {r:?}"));
                break;
            }
            Err(e) => {
                failed = Some(e.to_string());
                break;
            }
        }
    }
    stop.store(true, Ordering::SeqCst);
    if let Some(h) = load {
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("load generator: {e}")),
            Err(_) => return Err("load generator panicked".into()),
        }
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(quantiles(&mut samples)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
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
                json: true,
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
