//! `channel-spike --socket <path>` — see `channel-spike -h`.

use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;

const HELP: &str = "\
channel-spike — throwaway MCP channel server for the live wake proof.

usage: channel-spike --socket <path> [--metrics-endpoint <url>]

Speaks MCP over stdin and stdout. After `initialize`, each delivery of the `spira-sub run` on
the socket is sent as one notifications/claude/channel. stdout carries only JSON-RPC; logs go
to stderr. It reads no configuration.

arguments
  --socket <path>           the control socket of `spira-sub run --as <name>`:
                            <SPIRA_SUB_CONTROL_DIR>/<name>.sock
  --metrics-endpoint <url>  where to send this run's metrics; none are sent without it
  -h, --help                this text

exit codes:
  0  the client closed its end
  1  failed: the client could not be read or written, or `follow` could not run
  2  refused: usage
";

const FAILED: u8 = 1;
const REFUSED: u8 = 2;

struct Args {
    socket: PathBuf,
    metrics: Option<String>,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let (mut socket, mut metrics) = (None, None);
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        let slot = match word.as_str() {
            "--socket" => &mut socket,
            "--metrics-endpoint" => &mut metrics,
            other if other.starts_with('-') => return Err(format!("unknown flag {other:?}; see -h")),
            bare => return Err(format!("unexpected bare argument {bare:?}: should it follow --socket?")),
        };
        let value = words.next().ok_or_else(|| format!("{word} needs a value"))?;
        if slot.replace(value.clone()).is_some() {
            return Err(format!("{word} was given twice"));
        }
    }
    Ok(Args { socket: socket.ok_or("--socket <path> is required")?.into(), metrics })
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let args = match parse(&argv) {
        Ok(args) => args,
        Err(why) => {
            eprintln!("channel-spike: {why}");
            return ExitCode::from(REFUSED);
        }
    };
    spira_otel::run::begin("channel-spike", args.metrics.as_deref());
    let code = match channel_spike::relay(&args.socket, BufReader::new(std::io::stdin()), std::io::stdout()) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("channel-spike: {why}");
            FAILED
        }
    };
    spira_otel::run::finish(i32::from(code));
    ExitCode::from(code)
}
