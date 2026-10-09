//! `perf-watch --probes FILE [--limit-ms N] [--stats "CMD"] [--record FILE]`
//! Alarm lines go to stdout; a quiet pass prints nothing.

use perf_watch::{pass, parse_probes, Clock, Runner};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(30);

struct Wall(Instant);
impl Clock for Wall {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}

struct Spawn;
impl Runner for Spawn {
    fn run(&mut self, argv: &[String]) -> Result<String, String> {
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{}: {e}", argv[0]))?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() > DEADLINE => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{}: no answer in {}s", argv[0], DEADLINE.as_secs()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(format!("{}: {e}", argv[0])),
            }
        }
        let mut out = String::new();
        if let Some(mut so) = child.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut so, &mut out);
        }
        Ok(out)
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut probes, mut limit, mut stats, mut record) = (None, 500u64, "spira-lc stats".to_string(), None);
    while let Some(a) = args.next() {
        let v = args.next();
        match (a.as_str(), v) {
            ("--probes", Some(v)) => probes = Some(v),
            ("--limit-ms", Some(v)) => limit = v.parse().unwrap_or_else(|_| usage()),
            ("--stats", Some(v)) => stats = v,
            ("--record", Some(v)) => record = Some(v),
            _ => usage(),
        }
    }
    let Some(file) = probes else { usage() };
    let probes = match std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|t| parse_probes(&t)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("perf-watch: {file}: {e}");
            std::process::exit(2);
        }
    };
    let stats_argv: Vec<String> = stats.split_whitespace().map(String::from).collect();
    let (timings, lines) = pass(&probes, limit, &stats_argv, &Wall(Instant::now()), &mut Spawn);
    for l in &lines {
        println!("{l}");
    }
    if let Some(r) = record {
        let body: String = timings.iter().map(|t| format!("{}\t{}\n", t.name, t.millis)).collect();
        let _ = std::fs::write(r, body);
    }
}

fn usage() -> ! {
    eprintln!("usage: perf-watch --probes FILE [--limit-ms N] [--stats \"CMD\"] [--record FILE]");
    std::process::exit(2)
}
