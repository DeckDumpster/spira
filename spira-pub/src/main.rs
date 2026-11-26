//! `spira-pub --as <name> --topic <topic> < payload.json` — see `spira-pub -h`.

use std::io::Read;
use std::time::Duration;

use spira_pub::cli::{self, Args, Parsed};
use spira_pub::{topics, PubError, Publisher};

const FLUSH: Duration = Duration::from_secs(1);
const NOT_DELIVERED: u8 = 1;
const REFUSED: u8 = 2;

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match cli::parse(&argv) {
        Ok(Parsed::Help) => {
            print!("{}", cli::HELP);
            return std::process::ExitCode::SUCCESS;
        }
        Ok(Parsed::Publish(args)) => Some(args),
        Err(why) => {
            eprintln!("spira-pub: {why}");
            None
        }
    };
    spira_otel::run::begin("spira-pub", spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref());
    let code = args.map_or(REFUSED, publish);
    spira_otel::run::finish(i32::from(code));
    std::process::ExitCode::from(code)
}

fn publish(args: Args) -> u8 {
    let refused = |why: &dyn std::fmt::Display| {
        eprintln!("spira-pub: {why}");
        REFUSED
    };
    let mut text = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut text) {
        return refused(&format!("cannot read the payload from stdin: {e}"));
    }
    let payload: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return refused(&format!("the payload on stdin is not JSON: {e}")),
    };
    // Judged before the broker is looked for, so a bad topic is refused with the broker down.
    if let Err(e) = topics::check(&args.topic, args.class) {
        return refused(&e);
    }
    let publisher = match Publisher::new(&args.name) {
        Ok(p) => p,
        Err(e) => return refused(&e),
    };
    let sent = publisher.publish(&args.topic, &payload).and_then(|()| publisher.flush(FLUSH));
    match sent {
        Ok(()) => 0,
        Err(e) => report(&e),
    }
}

/// The library has logged a failed publish; a failed flush is logged only here.
fn report(e: &PubError) -> u8 {
    eprintln!("spira-pub: not delivered: {e}");
    if e.is_refusal() {
        REFUSED
    } else {
        NOT_DELIVERED
    }
}
