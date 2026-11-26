//! `spira-sub <run|add|drop|list|follow>` — see `spira-sub -h`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use ports::{Clock, VirtualClock, WallClock};
use spira_config::process::cfg;
use spira_otel::metrics::MetricsSink;
use spira_pub::topics;
use spira_sub::cli::{self, Command, Parsed};
use spira_sub::client;
use spira_sub::protocol::{Reply, Request};
use spira_sub::run::{self, Options};

const FAILED: u8 = 1;
const REFUSED: u8 = 2;

fn fail(code: u8, why: &str) -> u8 {
    eprintln!("spira-sub: {why}");
    code
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let parsed = cli::parse(&argv);
    if parsed == Ok(Parsed::Help) {
        print!("{}", cli::HELP);
        return ExitCode::SUCCESS;
    }
    spira_otel::run::begin("spira-sub", cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref());
    let code = match parsed {
        Ok(Parsed::Command { name, command }) => {
            spira_otel::run::operation(argv.first().map_or("run", String::as_str));
            dispatch(name, command)
        }
        Ok(Parsed::Help) => 0,
        Err(why) => fail(REFUSED, &why),
    };
    spira_otel::run::finish(i32::from(code));
    ExitCode::from(code)
}

fn control_dir() -> Result<PathBuf, String> {
    spira_config::sub::control_dir(&cfg("SPIRA_RUN")?, &cfg).map(PathBuf::from)
}

fn dispatch(name: String, command: Command) -> u8 {
    let control_dir = match control_dir() {
        Ok(dir) => dir,
        Err(why) => return fail(REFUSED, &why),
    };
    let socket = run::socket_path(&control_dir, &name);
    let request = match command {
        Command::Run => return serve(name, control_dir),
        Command::Follow => {
            let mut note = |what: &str| eprintln!("spira-sub: {name}: {what}");
            return match client::follow(&socket, &mut std::io::stdout(), &mut note) {
                Ok(()) => 0,
                Err(why) => fail(FAILED, &why),
            };
        }
        // Judged before the client is looked for, so a bad filter is refused with it down.
        Command::Add { filter, .. } if !topics::filter_matches_any(&filter) => {
            return fail(REFUSED, &format!("filter {filter:?} matches no topic family declared in spira/topics.toml"))
        }
        Command::Add { filter, note, once } => Request::Add { filter, note, once },
        Command::At { at_ms, note } => Request::At { at_ms, note },
        Command::Drop { id } => Request::Drop { id },
        Command::List => Request::List,
    };
    match client::call(&socket, &request) {
        Ok(Reply::Done { out, warning }) => {
            if let Some(warning) = warning {
                eprintln!("spira-sub: {warning}");
            }
            if !out.is_empty() {
                println!("{}", out.trim_end());
            }
            0
        }
        Ok(Reply::Refused(why)) => fail(REFUSED, &why),
        Ok(Reply::Failed(why)) | Err(why) => fail(FAILED, &why),
    }
}

fn serve(name: String, control_dir: PathBuf) -> u8 {
    let broker = cfg("SPIRA_RUN").and_then(|run| spira_config::broker::socket_path(&run, &cfg));
    let broker = match broker {
        Ok(broker) => PathBuf::from(broker),
        Err(why) => return fail(REFUSED, &why),
    };
    let clock: Arc<dyn Clock> = match spira_config::vtime::override_epoch() {
        Some(secs) => Arc::new(VirtualClock::new(secs.saturating_mul(1000))),
        None => Arc::new(WallClock),
    };
    let metrics = cfg("SPIRA_METRICS_ENDPOINT").ok().map(|e| MetricsSink::for_component(Some(&e), "spira-sub"));
    match run::run(Options { name, broker, control_dir, clock, metrics }) {
        Ok(()) => 0,
        Err(why) => fail(FAILED, &why),
    }
}
