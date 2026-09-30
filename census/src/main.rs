//! `census [--with-suppressed]` — see DESIGN.md §3.

use census::real::Real;
use std::path::PathBuf;
use std::process::ExitCode;

/// An explicit, valid `$SPIRA_HOME` first — matching `skew`/`doctor`'s own priority
/// (their DESIGN.md §2: a caller that deliberately pins it, such as
/// `_activated_release_cmd`, sp-r15cf, must be honored, not second-guessed) — else
/// release-relative (`<release>/bin/census` -> `<release>/spira/`) for a bare invocation
/// with nothing set.
fn resolve_home() -> PathBuf {
    if let Ok(h) = std::env::var("SPIRA_HOME") {
        if !h.is_empty() {
            let p = PathBuf::from(&h);
            if p.join("conf.sh").is_file() {
                return p;
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(release_dir) = bin_dir.parent() {
                let candidate = release_dir.join("spira");
                if candidate.join("conf.sh").is_file() {
                    return candidate;
                }
            }
        }
    }
    std::env::var("SPIRA_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}

fn main() -> ExitCode {
    let w = Real::new(resolve_home());
    let with_suppressed = std::env::args().nth(1).as_deref() == Some("--with-suppressed");
    let rc = census::run(&w, with_suppressed);
    ExitCode::from(rc.clamp(0, 255) as u8)
}
