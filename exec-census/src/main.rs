//! `exec-census [--poll-ms N] [--window-s N]` — runs until stopped, pushing execs per minute
//! by unit, the exact fork rate and coverage once per window to the configured metrics endpoint.

use exec_census::{ProcFs, Sampler, Window};
use std::time::{Duration, Instant, SystemTime};

const USAGE: &str = "usage: exec-census [--poll-ms N] [--window-s N] | check-config";

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    if std::env::args().nth(1).as_deref() == Some("check-config") {
        std::process::exit(spira_config::process::check_config_verb("exec-census", &[]));
    }
    spira_config::process::check_config(&[])?;
    let (mut poll_ms, mut window_s) = (50u64, 60u64);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let v = args.next().and_then(|v| v.parse::<u64>().ok()).filter(|&v| v > 0).ok_or(USAGE)?;
        match a.as_str() {
            "--poll-ms" => poll_ms = v,
            "--window-s" => window_s = v,
            _ => return Err(USAGE.to_string()),
        }
    }
    let sink = spira_config::process::metrics_sink("exec-census");
    let fs = ProcFs::new("/proc");
    let mut sampler = Sampler::new();
    let mut window = Window::default();
    let mut started = Instant::now();
    loop {
        window.add(sampler.tick(&fs));
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_secs(window_s) {
            let series = window.series(elapsed.as_secs_f64(), SystemTime::now());
            println!("execs={} forks={} coverage={:.2} top: {}", window.execs(), window.forks, window.coverage(), window.top_commands(5).join(" "));
            if let Err(e) = sink.try_push_series(&series) {
                eprintln!("exec-census: push failed: {e}");
            }
            window = Window::default();
            started = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(poll_ms)); // waits-on: nothing — a fixed sampling period is the method
    }
}
