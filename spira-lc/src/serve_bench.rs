//! `spira-lc serve-bench`: the same requests (`show` and `list --live`, or with `--open-items`
//! that verb alone) put to a running server's two sockets, one after the other, and each one's
//! answers per second and p99 reported.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use spira_otel::metrics::Point;

const USAGE: &str = "usage: spira-lc serve-bench --socket A --loop-socket B --clients N --secs S [--open-items REPO]";

pub(crate) struct Opts {
    socket: PathBuf,
    loop_socket: PathBuf,
    clients: usize,
    secs: u64,
    open_items: Option<String>,
}

pub(crate) fn parse(args: &[String]) -> Result<Opts, String> {
    let take = |flag: &str| -> Result<&str, String> {
        let at = args.iter().position(|a| a == flag).ok_or_else(|| format!("{flag} is required; {USAGE}"))?;
        args.get(at + 1).map(String::as_str).ok_or_else(|| format!("{flag} takes a value; {USAGE}"))
    };
    let number = |flag: &str| -> Result<u64, String> { take(flag)?.parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(|| format!("{flag} is a whole number above 0")) };
    let open_items = args.iter().any(|a| a == "--open-items").then(|| take("--open-items").map(str::to_string)).transpose()?;
    Ok(Opts { socket: take("--socket")?.into(), loop_socket: take("--loop-socket")?.into(), clients: number("--clients")? as usize, secs: number("--secs")?, open_items })
}

/// The bead ids in a `list --live` answer.
pub(crate) fn ids_of(listing: &str) -> Vec<String> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(listing).unwrap_or_default();
    rows.iter().filter_map(|r| r["bead_id"].as_str().or_else(|| r["id"].as_str())).map(str::to_string).collect()
}

/// The 99th percentile by nearest rank; zero for no samples.
pub(crate) fn p99(samples: &mut [Duration]) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    samples.sort();
    samples[(samples.len() * 99).div_ceil(100) - 1]
}

#[derive(Debug, PartialEq)]
pub(crate) struct Tally {
    pub(crate) answers: u64,
    pub(crate) failures: u64,
    pub(crate) p99: Duration,
    pub(crate) per_sec: f64,
}

/// What the clients ask for.
pub(crate) enum Load<'a> {
    /// `show` and `list --live`, alternating over these ids.
    ShowAndList(&'a [String]),
    /// `work open-items --repo <name>`, every time.
    OpenItems(&'a str),
}

/// The request client `c` puts as its `n`th.
fn request(load: &Load, c: usize, n: u64) -> Vec<String> {
    let ids = match load {
        Load::ShowAndList(ids) => ids,
        Load::OpenItems(repo) => return ["work", "-", "open-items", "--repo", repo].map(String::from).to_vec(),
    };
    match ids.get((c + n as usize) / 2 % ids.len().max(1)) {
        Some(id) if (c as u64 + n).is_multiple_of(2) => vec!["show".to_string(), id.clone()],
        _ => vec!["list".to_string(), "--live".to_string()],
    }
}

/// `clients` threads calling until `over` says so; `call` is one request's success.
pub(crate) fn drive(load: &Load, clients: usize, over: &(dyn Fn() -> bool + Sync), call: &(dyn Fn(&[String]) -> bool + Sync)) -> Tally {
    let began = Instant::now();
    let failures = AtomicU64::new(0);
    let mut samples: Vec<Duration> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..clients)
            .map(|c| {
                let failures = &failures;
                scope.spawn(move || {
                    let mut mine = Vec::new();
                    let mut n = 0u64;
                    while !over() {
                        let argv = request(load, c, n);
                        let sent = Instant::now();
                        if call(&argv) {
                            mine.push(sent.elapsed());
                        } else {
                            failures.fetch_add(1, Ordering::SeqCst);
                        }
                        n += 1;
                    }
                    mine
                })
            })
            .collect();
        threads.into_iter().flat_map(|t| t.join().expect("a bench client ends")).collect()
    });
    let took = began.elapsed().as_secs_f64().max(f64::EPSILON);
    Tally { answers: samples.len() as u64, failures: failures.load(Ordering::SeqCst), p99: p99(&mut samples), per_sec: samples.len() as f64 / took }
}

fn call_on(socket: &Path, argv: &[String]) -> bool {
    let Ok(stream) = spira_store::lc_call::connect(socket) else { return false };
    spira_store::lc_call::exchange_once(stream, argv, None).is_ok_and(|r| r.code == 0)
}

fn report(label: &str, t: &Tally) -> serde_json::Value {
    serde_json::json!({ label: { "answers": t.answers, "failures": t.failures, "per_sec": t.per_sec, "p99_ms": t.p99.as_secs_f64() * 1000.0 } })
}

fn live_ids(socket: &Path) -> Result<Vec<String>, String> {
    let stream = spira_store::lc_call::connect(socket).map_err(spira_store::lc_call::Fail::into_text);
    match stream.and_then(|s| spira_store::lc_call::exchange(s, &["list".to_string(), "--live".to_string()], None)) {
        Ok(r) if r.code == 0 => Ok(ids_of(&r.out)),
        Ok(r) => Err(format!("list --live: {}", r.why())),
        Err(e) => Err(format!("{}: {e}", socket.display())),
    }
}

pub(crate) fn run(args: &[String]) -> i32 {
    let opts = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("spira-lc serve-bench: {e}");
            return 2;
        }
    };
    let ids = match &opts.open_items {
        Some(_) => Vec::new(),
        None => match live_ids(&opts.socket) {
            Ok(ids) => ids,
            Err(why) => {
                eprintln!("spira-lc serve-bench: {why}");
                return 2;
            }
        },
    };
    let load = opts.open_items.as_deref().map_or(Load::ShowAndList(&ids), Load::OpenItems);
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    let metered = spira_otel::metrics::Run::begin(endpoint.as_deref(), "lc", "serve-bench");
    let mut result = serde_json::json!({ "clients": opts.clients, "secs": opts.secs, "beads": ids.len() });
    let mut gauges = Vec::new();
    for (label, socket, rate, tail) in [("server", &opts.socket, "spira_lc_bench_server_answers_per_second", "spira_lc_bench_server_p99_seconds"), ("loop", &opts.loop_socket, "spira_lc_bench_loop_answers_per_second", "spira_lc_bench_loop_p99_seconds")] {
        let until = Instant::now() + Duration::from_secs(opts.secs);
        let tally = drive(&load, opts.clients, &|| Instant::now() >= until, &|argv| call_on(socket, argv));
        result[label] = report(label, &tally)[label].clone();
        gauges.extend([Point::Gauge { name: rate, value: tally.per_sec }, Point::Gauge { name: tail, value: tally.p99.as_secs_f64() }]);
    }
    println!("{result}");
    metered.end_with("ok", &gauges);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test_id(thi000h)]
    #[test]
    fn p99_is_the_nearest_rank_of_the_sorted_samples() {
        let mut hundred: Vec<Duration> = (1..=100).rev().map(ms).collect();
        assert_eq!(p99(&mut hundred), ms(99));
        let mut two = vec![ms(5), ms(1)];
        assert_eq!(p99(&mut two), ms(5));
        let mut one_slow_in_two_hundred: Vec<Duration> = (0..199).map(|_| ms(1)).chain([ms(900)]).collect();
        assert_eq!(p99(&mut one_slow_in_two_hundred), ms(1));
        let mut none: Vec<Duration> = Vec::new();
        assert_eq!(p99(&mut none), Duration::ZERO);
    }

    #[test_id(tsl0rby)]
    #[test]
    fn the_drive_counts_answers_and_failures_and_alternates_show_with_list_live() {
        let ids = vec!["sp-a".to_string(), "sp-b".to_string()];
        let budget = AtomicUsize::new(40);
        let seen = std::sync::Mutex::new(Vec::new());
        let tally = drive(&Load::ShowAndList(&ids), 1, &|| budget.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_err(), &|argv| {
            seen.lock().unwrap().push(argv.to_vec());
            argv[0] == "show"
        });
        assert_eq!((tally.answers, tally.failures), (20, 20));
        let seen = seen.into_inner().unwrap();
        assert!(seen.chunks(2).all(|p| p[0][0] == "show" && p[1] == ["list", "--live"]), "{seen:?}");
        assert!(seen.iter().any(|a| a[1] == "sp-b"), "every listed bead is asked for");
    }

    #[test_id(tuaqbou)]
    #[test]
    fn the_arguments_name_both_sockets_and_a_positive_client_count_and_duration() {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ok = parse(&words(&["--socket", "a", "--loop-socket", "b", "--clients", "50", "--secs", "60"])).unwrap();
        assert_eq!((ok.socket, ok.loop_socket, ok.clients, ok.secs), ("a".into(), "b".into(), 50, 60));
        assert!(parse(&words(&["--socket", "a", "--loop-socket", "b", "--clients", "0", "--secs", "60"])).is_err());
        assert!(parse(&words(&["--socket", "a", "--clients", "5", "--secs", "6"])).is_err());
    }

    #[test_id(ty44yrh)]
    #[test]
    fn with_open_items_every_request_is_that_verb_for_the_named_repository() {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let base = ["--socket", "a", "--loop-socket", "b", "--clients", "1", "--secs", "1"];
        let opts = parse(&words(&[&base[..], &["--open-items", "spira"]].concat())).unwrap();
        let repo = opts.open_items.as_deref().unwrap();
        for n in 0..3 {
            assert_eq!(request(&Load::OpenItems(repo), 0, n), words(&["work", "-", "open-items", "--repo", "spira"]));
        }
        assert!(parse(&words(&base)).unwrap().open_items.is_none());
        assert!(parse(&words(&[&base[..], &["--open-items"]].concat())).is_err());
    }

    #[test_id(trz2gby)]
    #[test]
    fn the_listing_yields_its_bead_ids() {
        assert_eq!(ids_of(r#"[{"bead_id":"sp-a","state":"READY"},{"id":"sp-b"},{"x":1}]"#), ["sp-a", "sp-b"]);
        assert!(ids_of("not json").is_empty());
    }
}
