//! `skew <check|units|refresh|gap|copies|foreign> [args...]` — see DESIGN.md §3. Argv shape
//! matches skew.sh's own `case` dispatch (the default subcommand with no args is `check`).

use skew::ports::World;
use skew::real::Real;
use std::path::PathBuf;
use std::process::ExitCode;

/// The declared harness home (`SPIRA_HOME`, else `$SPIRA_RELEASE/spira`), holding `lib.sh`; a
/// refusal names what is missing. Never searched for beside the executable.
fn resolve_home() -> Result<PathBuf, String> {
    let home = spira_config::resolve::locate_home_for_process().map_err(|e| format!("skew: {e}"))?;
    if home.join("lib.sh").is_file() {
        Ok(home)
    } else {
        Err(format!("skew: {} holds no lib.sh", home.display()))
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    skew::telemetry::metered_exit(&argv, run_main)
}

fn run_main() -> ExitCode {
    let home = match resolve_home() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(3);
        }
    };

    // THE SHARED INIT GUARD (skew.sh's own `trap ... EXIT` around sourcing lib.sh, which
    // itself sources conf.sh): a conf.sh that cannot even source — e.g. `bd migrate schema`
    // failing on a locked database — must be "could not check" (exit 3), never read as a
    // real verdict from whichever subcommand happens to run next on empty data. Checked
    // once, before dispatch, exactly where skew.sh's own trap fired: before its `case`.
    if !skew::real::lib_sh_sources(&home) {
        return ExitCode::from(3);
    }

    let w = Real::new(home);

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = argv.first().map(String::as_str).unwrap_or("check");

    let rc = match cmd {
        "check" => {
            let escalate = argv.iter().skip(1).any(|a| a == "--escalate");
            let unknown = argv.iter().skip(1).find(|a| a.as_str() != "--escalate");
            if let Some(u) = unknown {
                w.err(&format!("skew: unknown flag: {u}"));
                1
            } else {
                skew::check(&w, escalate)
            }
        }
        "units" => skew::units(&w),
        "refresh" => skew::refresh(&w, argv.get(1).map(String::as_str)),
        "gap" => skew::gap(&w, argv.get(1).map(String::as_str)),
        "copies" => {
            let rc = skew::copies(&w);
            if rc != 0 {
                // bash: `copies || { echo ... >&2; exit 3; }` — the CLI dispatch converts
                // copies()'s internal not-found (1) to exit 3 at this layer, never inside
                // copies() itself. Caught live by testenv's "no harness" case, sp-yyk47.
                w.err("skew: no mapped repository carries a harness — the map or the matcher is wrong");
                3
            } else {
                0
            }
        }
        "foreign" => skew::foreign(
            &w,
            argv.get(1).map(String::as_str).unwrap_or(""),
            argv.get(2).map(String::as_str).unwrap_or(""),
            argv.get(3).map(String::as_str).unwrap_or(""),
        ),
        _ => {
            eprintln!(
                "skew — skew check [--escalate] | units | refresh [repo] | gap [repo] | copies | foreign <repo> <base> <ref>"
            );
            2
        }
    };
    ExitCode::from(rc.clamp(0, 255) as u8)
}
