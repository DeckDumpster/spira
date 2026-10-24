//! pilgrim-login <mcp-server-name> <claude-project-dir> [--login-url BASE]
//!
//! BASE defaults to SPIRA_IDENTITY_LOGIN_URL; the broker's login code is appended to it.

use pilgrim_login::{run, Tools};
use std::process::ExitCode;

const USAGE: &str = "usage: pilgrim-login <mcp-server-name> <claude-project-dir> [--login-url BASE]";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (pos, flag): (Vec<&String>, Option<&String>) = match argv.as_slice() {
        [a, b] => (vec![a, b], None),
        [a, b, f, v] if f == "--login-url" => (vec![a, b], Some(v)),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let base = match flag {
        Some(v) => v.clone(),
        None => match spira_config::process::cfg("SPIRA_IDENTITY_LOGIN_URL") {
            Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
            Ok(_) => {
                eprintln!("pilgrim-login: SPIRA_IDENTITY_LOGIN_URL is empty in the config file — refusing");
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("pilgrim-login: {e}");
                return ExitCode::from(1);
            }
        },
    };
    match run(&Tools::default(), pos[0], std::path::Path::new(pos[1]), &base) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pilgrim-login: {e}");
            ExitCode::from(1)
        }
    }
}
