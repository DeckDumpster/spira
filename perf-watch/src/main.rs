//! `perf-watch --probes FILE [--stats "CMD"] [--record FILE] [--health FILE]`
//! Budget, interval and reminder come from config. Alarm lines go to stdout; a quiet pass
//! prints nothing. `--health` is rewritten each pass that probes.

use perf_watch::happy::{self, Sim};
use perf_watch::{parse_probes, run_pass, Clock, Health, Runner, Settings};
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
    let r = if std::env::args().nth(1).as_deref() == Some("happy") { run_happy() } else { run() };
    if let Err(e) = r {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

const USAGE: &str = "usage: perf-watch --probes FILE [--stats \"CMD\"] [--record FILE] [--health FILE]";

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut probes, mut stats, mut record, mut health) = (None, "spira-lc stats".to_string(), None, None);
    while let Some(a) = args.next() {
        let v = args.next();
        match (a.as_str(), v) {
            ("--probes", Some(v)) => probes = Some(v),
            ("--stats", Some(v)) => stats = v,
            ("--record", Some(v)) => record = Some(v),
            ("--health", Some(v)) => health = Some(v),
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
    let Some(out) = run_pass(&cfg, &probes, &stats_argv, &prior, &Wall(Instant::now()), &mut Spawn) else { return Ok(()) };
    for l in &out.lines {
        println!("{l}");
    }
    if let Some(r) = record {
        let body: String = out.timings.iter().map(|t| format!("{}\t{}\n", t.name, t.millis)).collect();
        let _ = std::fs::write(r, body);
    }
    if let Some(h) = health {
        std::fs::write(&h, out.health.render()).map_err(|e| format!("perf-watch: {h}: {e}"))?;
    }
    Ok(())
}

const HAPPY_USAGE: &str = "usage: perf-watch happy --repo DIR --sim BIN --scratch DIR --history FILE [--scenario NAME] [--seed N] [--goal-ms N] [--run-limit-s N]";
const PRODUCTION_LOCATORS: [&str; 4] = ["SPIRA_LC_PASSWORD_FILE", "SPIRA_LC_SOCKET", "SPIRA_RUN", "SPIRA_DB"];

struct RealSim {
    repo: String,
    sim: String,
    scratch: String,
    scenario: String,
    seed: u64,
    limit: Duration,
}

impl RealSim {
    fn sim(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.sim);
        c.args(args).current_dir(&self.repo).env("TMPDIR", &self.scratch).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        for k in PRODUCTION_LOCATORS {
            c.env_remove(k);
        }
        if let Ok(r) = std::env::var("SPIRA_RELEASE") {
            c.env("SPIRA_SIM_RELEASE", r);
        }
        c
    }
}

impl Sim for RealSim {
    fn round_open(&mut self) -> Result<bool, String> {
        let out = Command::new("timeout").args(["5", "queue", "round", "status"]).stdin(Stdio::null()).stderr(Stdio::null()).output().map_err(|e| format!("queue: {e}"))?;
        if !out.status.success() {
            return Err(format!("queue round status exited {}", out.status));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        match text.lines().find_map(|l| l.strip_prefix("round=")) {
            Some("open") => Ok(true),
            Some("none") => Ok(false),
            other => Err(format!("queue round status said {other:?}")),
        }
    }

    fn run(&mut self) -> Result<String, String> {
        std::fs::create_dir_all(&self.scratch).map_err(|e| e.to_string())?;
        let seed = self.seed.to_string();
        let mut child = self.sim(&["run", &self.scenario, "--seed", &seed, "--keep"]).spawn().map_err(|e| format!("{}: {e}", self.sim))?;
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Ok(s),
                Ok(None) if start.elapsed() > self.limit => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!("sim run did not finish in {}s", self.limit.as_secs()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => break Err(e.to_string()),
            }
        };
        let suffix = format!("-{seed}");
        let mut log = Err("sim kept no world".to_string());
        for e in std::fs::read_dir(&self.scratch).map_err(|e| e.to_string())?.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("sim-run-") && n.ends_with(&suffix) {
                log = std::fs::read_to_string(e.path().join("exec.log")).map_err(|e| format!("exec.log: {e}"));
                let _ = self.sim(&["world", "down", &e.path().to_string_lossy()]).status();
            }
        }
        let s = status?;
        if !s.success() {
            return Err(format!("sim run exited {s}"));
        }
        log
    }
}

fn run_happy() -> Result<(), String> {
    let mut args = std::env::args().skip(2);
    let (mut repo, mut sim, mut scratch, mut history) = (None, None, None, None);
    let (mut scenario, mut seed, mut goal, mut limit) = ("happy-path".to_string(), 7u64, 60_000u64, 280u64);
    while let Some(a) = args.next() {
        let v = args.next().ok_or(HAPPY_USAGE)?;
        let n = |v: &str| v.parse::<u64>().map_err(|_| HAPPY_USAGE.to_string());
        match a.as_str() {
            "--repo" => repo = Some(v),
            "--sim" => sim = Some(v),
            "--scratch" => scratch = Some(v),
            "--history" => history = Some(v),
            "--scenario" => scenario = v,
            "--seed" => seed = n(&v)?,
            "--goal-ms" => goal = n(&v)?,
            "--run-limit-s" => limit = n(&v)?,
            _ => return Err(HAPPY_USAGE.to_string()),
        }
    }
    let (Some(repo), Some(sim), Some(scratch), Some(history)) = (repo, sim, scratch, history) else { return Err(HAPPY_USAGE.to_string()) };
    let mut real = RealSim { repo, sim, scratch, scenario, seed, limit: Duration::from_secs(limit) };
    let outcome = happy::measure(&Wall(Instant::now()), &mut real);
    if let happy::Outcome::Measured { millis, .. } = &outcome {
        let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&history).map_err(|e| format!("{history}: {e}"))?;
        std::io::Write::write_all(&mut f, happy::history_line(epoch, *millis).as_bytes()).map_err(|e| e.to_string())?;
    }
    let recent = happy::trend(&std::fs::read_to_string(&history).unwrap_or_default(), 5);
    if let Some(l) = happy::alarm(&outcome, goal, &recent) {
        println!("{l}");
    }
    Ok(())
}
