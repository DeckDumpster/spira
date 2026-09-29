// bd-meter — the IO seam of testenv::bdmeter (see there, and testenv DESIGN.md §3.4).
//
//   bd-meter --install <home>     make a suite HOME and link bd / bd-embedded to this binary
//   bd … | bd-embedded …          (as a link) run the real one, log its wall to $SPIRA_BD_LOG

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Instant;
use testenv::bdmeter;

fn main() -> ExitCode {
    let mut argv = std::env::args_os();
    let arg0 = PathBuf::from(argv.next().unwrap_or_default());
    let args: Vec<String> = argv.map(|a| a.to_string_lossy().into_owned()).collect();
    let me = std::env::current_exe().unwrap_or_else(|_| arg0.clone());

    let name = arg0
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !bdmeter::SHIMMED.contains(&name.as_str()) {
        return match args.as_slice() {
            [flag, home] if flag == "--install" => {
                match bdmeter::install(&PathBuf::from(home), &me) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("bd-meter: install into {home}: {e}");
                        ExitCode::from(1)
                    }
                }
            }
            _ => {
                eprintln!("usage: bd-meter --install <home>   (or invoke as bd / bd-embedded)");
                ExitCode::from(2)
            }
        };
    }

    let path = std::env::var_os("PATH").unwrap_or_default();
    let Some(real) = bdmeter::find_real(&name, &path, &me) else {
        eprintln!("{name}: command not found (bd-meter: no {name} on PATH past the meter)");
        return ExitCode::from(127);
    };
    let t = Instant::now();
    let rc = match Command::new(&real).args(&args).status() {
        Ok(st) => bdmeter::shell_status(st),
        Err(e) => {
            eprintln!("bd-meter: {}: {e}", real.display());
            126
        }
    };
    if let Some(log) = std::env::var_os("SPIRA_BD_LOG").filter(|l| !l.is_empty()) {
        bdmeter::append(
            &PathBuf::from(log),
            &bdmeter::log_line(t.elapsed().as_millis(), rc, &args),
        );
    }
    ExitCode::from(rc.clamp(0, 255) as u8)
}
