//! `perf-watch --probes FILE [--stats "CMD"] [--record FILE] [--slow-log FILE] [--health FILE] [--inbox FILE]`
//! Budget, interval and reminder come from config. Alarm lines go to stdout; a quiet pass
//! prints nothing. `--health` is rewritten each pass that probes.

use perf_watch::{parse_probes, run_pass, Clock, Health, Runner, Settings};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(5);

struct Wall(Instant);
impl Clock for Wall {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
    fn epoch_secs(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }
}

const TIMER_PREFIX: &str = "PERFWATCH_US=";

// The probe runs in its own transient scope, outside this unit's CPU quota, and times itself
// there: wall time across the throttled parent would read the throttle, not the store.
// bash's own clock, so no process start of a clock is charged to the probe.
const SCOPED: &str = "t0=$EPOCHREALTIME; \"$@\"; rc=$?; t1=$EPOCHREALTIME; echo \"PERFWATCH_US=$(( ${t1//[.,]/} - ${t0//[.,]/} ))\" >&2; exit $rc";

struct Spawn {
    scoped: bool,
    measured: Option<u64>,
    phases: Option<String>,
}

impl Spawn {
    fn new() -> Spawn {
        // FORK-EDGE: needs a scope run on the Units port that hands back the child's stdout and
        // stderr (the probe is timed outside this unit's CPU quota); this checks one exists.
        let scoped = Command::new("timeout").args(["5", "systemd-run", "--version"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        Spawn { scoped, measured: None, phases: None }
    }
}

impl Runner for Spawn {
    fn measured_us(&self) -> Option<u64> {
        self.measured
    }

    fn phases(&self) -> Option<String> {
        self.phases.clone()
    }

    fn run(&mut self, argv: &[String]) -> Result<String, String> {
        self.measured = None;
        self.phases = None;
        let mut cmd = if self.scoped {
            // FORK-EDGE: needs a scope run on the Units port with captured output (as above).
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "--collect", "--", "bash", "-c", SCOPED, "perf-probe"]).args(argv);
            c
        } else {
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        };
        let mut child = cmd
            .env(spira_otel::run::PHASES_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(if self.scoped { Stdio::piped() } else { Stdio::null() })
            .spawn()
            .map_err(|e| format!("{}: {e}", argv[0]))?;
        let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_string(&mut buf);
                }
                buf
            })
        };
        let (so, se) = (drain(child.stdout.take().map(|p| Box::new(p) as _)), drain(child.stderr.take().map(|p| Box::new(p) as _)));
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) if start.elapsed() > DEADLINE => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{}: no answer in {}s", argv[0], DEADLINE.as_secs()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(format!("{}: {e}", argv[0])),
            }
        };
        let out = so.join().unwrap_or_default();
        let err = se.join().unwrap_or_default();
        if self.scoped {
            self.measured = err.lines().rev().find_map(|l| l.strip_prefix(TIMER_PREFIX)).and_then(|n| n.trim().parse().ok());
            self.phases = err.lines().rev().find_map(|l| l.strip_prefix(spira_otel::run::PHASES_PREFIX)).map(str::to_string);
        }
        if self.scoped && status.code() == Some(127) {
            return Err(format!("{}: not found", argv[0]));
        }
        if !status.success() {
            let said = format!("{err} {out}");
            return Err(format!("{}: {status}: {}", argv[0], said.split_whitespace().take(40).collect::<Vec<_>>().join(" ")));
        }
        Ok(out)
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

const USAGE: &str = "usage: perf-watch --probes FILE [--stats \"CMD\"] [--record FILE] [--slow-log FILE] [--health FILE] [--inbox FILE]";

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut probes, mut stats, mut record, mut health, mut inbox, mut slow_log) = (None, "spira-lc stats".to_string(), None, None, None, None);
    while let Some(a) = args.next() {
        let v = args.next();
        match (a.as_str(), v) {
            ("--probes", Some(v)) => probes = Some(v),
            ("--stats", Some(v)) => stats = v,
            ("--record", Some(v)) => record = Some(v),
            ("--slow-log", Some(v)) => slow_log = Some(v),
            ("--health", Some(v)) => health = Some(v),
            ("--inbox", Some(v)) => inbox = Some(v),
            _ => return Err(USAGE.to_string()),
        }
    }
    let Some(file) = probes else { return Err(USAGE.to_string()) };
    let probes = std::fs::read_to_string(&file)
        .map_err(|e| e.to_string())
        .and_then(|t| parse_probes(&t))
        .map_err(|e| format!("perf-watch: {file}: {e}"))?;
    let cfg = Settings {
        budget_ms: spira_config::process::cfg_parse("SPIRA_READ_WATCH_BUDGET_MS")?,
        interval_secs: spira_config::process::cfg_parse("SPIRA_READ_WATCH_INTERVAL")?,
        remind_secs: spira_config::process::cfg_parse("SPIRA_READ_WATCH_REMIND")?,
    };
    let prior = health.as_deref().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| Health::parse(&t)).unwrap_or_default();
    let stats_argv: Vec<String> = stats.split_whitespace().map(String::from).collect();
    let Some(out) = run_pass(&cfg, &probes, &stats_argv, &prior, &Wall(Instant::now()), &mut Spawn::new()) else { return Ok(()) };
    let inbox = match inbox {
        Some(p) => p,
        None => spira_config::process::cfg("SPIRA_CONCIERGE_INBOX")?,
    };
    let now = Wall(Instant::now()).epoch_secs();
    for l in &out.lines {
        println!("{l}");
        perf_watch::append_inbox(&inbox, &perf_watch::inbox_line(now, l));
    }
    if let Some(f) = slow_log {
        for l in &out.slow {
            perf_watch::append_inbox(&f, l);
        }
    }
    perf_watch::push_timings(&spira_config::process::metrics_sink("perf-watch"), &out.timings);
    if let Some(r) = record {
        let body: String = out.timings.iter().map(|t| format!("{}\t{}\n", t.name, t.millis)).collect();
        let _ = std::fs::write(r, body);
    }
    if let Some(h) = health {
        std::fs::write(&h, out.health.render()).map_err(|e| format!("perf-watch: {h}: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_that_exits_non_zero_is_an_error_not_an_answer() {
        let mut spawn = Spawn { scoped: false, measured: None, phases: None };
        let argv = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let err = spawn.run(&argv(&["sh", "-c", "echo refused >&2; echo refused; exit 3"])).unwrap_err();
        assert!(err.contains("refused"), "{err}");
        assert_eq!(spawn.run(&argv(&["sh", "-c", "echo fine"])).unwrap().trim(), "fine");
    }
}
