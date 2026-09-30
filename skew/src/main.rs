//! `skew <check|units|refresh|gap|copies|foreign> [args...]` — see DESIGN.md §3. Argv shape
//! matches skew.sh's own `case` dispatch (the default subcommand with no args is `check`).

use skew::ports::World;
use skew::real::Real;
use std::path::PathBuf;
use std::process::ExitCode;

/// `$SPIRA_HOME` first when a caller set it explicitly and it actually holds `lib.sh`:
/// `_activated_release_cmd` (deploy.sh, sp-r15cf) pins it to the release just activated
/// specifically so a stale predecessor's own `$PATH` corruption cannot matter, and
/// `spira-skew.service` pins it to `@SPIRA_HOME@` rather than `@SPIRA_PROD@` on purpose in
/// split-checkout mode (so `SPIRA_REPO` derives to the dev checkout, not the release) —
/// both are deliberate overrides this binary must honor, not second-guess. Only when
/// nothing set it does this fall back to release-relative self-location
/// (`<release>/bin/skew` -> `<release>/spira/`), the equivalent of skew.sh's own
/// `$(dirname "$0")/lib.sh` for a bare interactive invocation with no `$SPIRA_HOME` at all.
fn resolve_home() -> PathBuf {
    if let Ok(h) = std::env::var("SPIRA_HOME") {
        if !h.is_empty() {
            let p = PathBuf::from(&h);
            if p.join("lib.sh").is_file() {
                return p;
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(release_dir) = bin_dir.parent() {
                let candidate = release_dir.join("spira");
                if candidate.join("lib.sh").is_file() {
                    return candidate;
                }
            }
        }
    }
    std::env::var("SPIRA_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}

fn main() -> ExitCode {
    let home = resolve_home();

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
                w.err("skew: no mapped repository carries a harness — the map or the matcher is wrong");
            }
            rc
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
