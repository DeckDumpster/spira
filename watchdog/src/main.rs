//! spira-watchdog — `pass` runs one watch; `render <os> <program> <every-secs>` prints the timer
//! files that run it on that platform.

use std::path::PathBuf;
use std::sync::Arc;

use runtime::ports::{Clock, WallClock};
use runtime::units::Units;
use watchdog::render::{render, Platform, TimerSpec};
use watchdog::{pass, Config, Deps, FileMarkerAge, Pager, SocketStatus};

/// The page is one line appended to the Concierge's durable inbox
/// (law-concierge-signals-ride-watchd-rows): a file append, so it needs neither the store nor
/// the scheduler, which is what is down when it is sent.
struct InboxPager(PathBuf);

impl Pager for InboxPager {
    fn page(&self, subject: &str, body: &str) -> Result<(), String> {
        use std::io::Write;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let line = inbox_rules::line(&tsd::iso_utc(now), &format!("{subject}: {body}"));
        if let Some(dir) = self.0.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.0).map_err(|e| format!("{}: {e}", self.0.display()))?;
        writeln!(f, "{line}").map_err(|e| format!("{}: {e}", self.0.display()))
    }
}

#[cfg(target_os = "linux")]
fn units(clock: Arc<dyn Clock>) -> Result<Box<dyn Units>, String> {
    runtime::units::SystemdUnits::connect(clock).map(|u| Box::new(u) as Box<dyn Units>).map_err(|e| e.0)
}

#[cfg(not(target_os = "linux"))]
fn units(_clock: Arc<dyn Clock>) -> Result<Box<dyn Units>, String> {
    Err("no Units backend reads an installed service on this platform yet".into())
}

/// The pass's line and whether the scheduler is down (an open incident).
fn run_pass() -> Result<(String, bool), String> {
    let cfg = Config::from_lookup(spira_config::process::cfg)?;
    let inbox = PathBuf::from(spira_config::process::cfg("SPIRA_CONCIERGE_INBOX")?);
    let clock: Arc<dyn Clock> = Arc::new(WallClock);
    let units = units(clock.clone())?;
    let deps = Deps { clock: &*clock, units: &*units, status: &SocketStatus(cfg.socket.clone()), marker: &FileMarkerAge(cfg.marker.clone()), pager: &InboxPager(inbox) };
    pass(&cfg, &deps).map(|o| (o.to_string(), o.scheduler_down()))
}

/// A pass that found the scheduler down exits with this, so a dead scheduler is never a
/// successful pass.
const EXIT_DOWN: i32 = 2;

fn run_render(args: &[String]) -> Result<String, String> {
    let [os, program, every] = args else { return Err("usage: spira-watchdog render <os> <program> <every-secs>".into()) };
    let every: u64 = every.parse().map_err(|_| format!("{every} is not a whole number of seconds"))?;
    let files = render(Platform::for_os(os), &TimerSpec::watchdog(PathBuf::from(program), every, vec![]))?;
    Ok(files.into_iter().map(|(name, text)| format!("==> {name} <==\n{text}")).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let op = args.first().cloned().unwrap_or_else(|| "usage".into());
    let metered = spira_otel::metrics::Run::begin(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "watchdog", &op);
    let result = match args.first().map(String::as_str) {
        Some("pass") => run_pass(),
        Some("render") => run_render(&args[1..]).map(|l| (l, false)),
        _ => Err("usage: spira-watchdog pass | render <os> <program> <every-secs>".into()),
    };
    metered.end(match &result {
        Ok((_, true)) => "down",
        Ok(_) => "ok",
        Err(_) => "error",
    });
    match result {
        Ok((line, down)) => {
            println!("{}", line.trim_end());
            if down {
                std::process::exit(EXIT_DOWN);
            }
        }
        Err(e) => {
            eprintln!("spira-watchdog: {e}");
            std::process::exit(1);
        }
    }
}
