//! spira-watchdog — `pass` runs one watch; `render <os> <program> <every-secs>` prints the timer
//! files that run it on that platform.

use std::path::PathBuf;
use std::sync::Arc;

use runtime::ports::{Clock, WallClock};
use runtime::units::Units;
use watchdog::render::{render, Platform, TimerSpec};
use watchdog::{pass, Config, Deps, FileMarkerAge, Pager, SocketStatus};

struct MailPager(mail::env::Env);

impl Pager for MailPager {
    fn page(&self, subject: &str, body: &str) -> Result<(), String> {
        let args = mail::cmds::SendArgs { mailbox: "operator", from: Some("Spira Watchdog <watchdog@spira>"), subject, kind: "note", default: "", class: "", bead: "", urgent: true, digest: false, dry_run: false, requested_by: "" };
        let bd = mail::bead::BdCli { bdq: String::new(), conn_retries: 1 };
        mail::cmds::send(&bd, &self.0, &args, body.to_string()).map(|_| ())
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

fn run_pass() -> Result<String, String> {
    let cfg = Config::from_lookup(spira_config::process::cfg)?;
    let mut env = mail::env::Env::load().map_err(|e| format!("mail environment: {e}"))?;
    env.db.clear();
    let clock: Arc<dyn Clock> = Arc::new(WallClock);
    let units = units(clock.clone())?;
    let deps = Deps { clock: &*clock, units: &*units, status: &SocketStatus(cfg.socket.clone()), marker: &FileMarkerAge(cfg.marker.clone()), pager: &MailPager(env) };
    pass(&cfg, &deps).map(|o| o.to_string())
}

fn run_render(args: &[String]) -> Result<String, String> {
    let [os, program, every] = args else { return Err("usage: spira-watchdog render <os> <program> <every-secs>".into()) };
    let every: u64 = every.parse().map_err(|_| format!("{every} is not a whole number of seconds"))?;
    let files = render(Platform::for_os(os), &TimerSpec::watchdog(PathBuf::from(program), every, vec![]))?;
    Ok(files.into_iter().map(|(name, text)| format!("==> {name} <==\n{text}")).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("pass") => run_pass(),
        Some("render") => run_render(&args[1..]),
        _ => Err("usage: spira-watchdog pass | render <os> <program> <every-secs>".into()),
    };
    match result {
        Ok(line) => println!("{}", line.trim_end()),
        Err(e) => {
            eprintln!("spira-watchdog: {e}");
            std::process::exit(1);
        }
    }
}
