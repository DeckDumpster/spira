//! `census [--with-suppressed]` — see DESIGN.md §3.

use census::real::Real;
use std::path::PathBuf;
use std::process::ExitCode;

/// An explicit, valid `$SPIRA_HOME` first — matching `skew`/`doctor`'s own priority
/// (their DESIGN.md §2: a caller that deliberately pins it, such as
/// `_activated_release_cmd`, sp-r15cf, must be honored, not second-guessed) — else
/// release-relative (`<release>/bin/census` -> `<release>/spira/`) for a bare invocation
/// with nothing set.
///
/// THAT FALLBACK READS `argv[0]`, NEVER `current_exe()`. `current_exe()` canonicalizes
/// every symlink in the path; testenv's own release staging (and the gate's fixture
/// release) link `bin/<tool>` to wherever cargo actually built it and `spira/` to the real
/// checkout — two unrelated directories once resolved, so a `current_exe()`-based fallback
/// missed silently there (empty output, exit code standing in for a verdict, caught live
/// by testenv running the skew/doctor/census suites, sp-yyk47). `argv[0]` is exactly the
/// path PATH search resolved to, unresolved further — bash's own `$0` never re-resolved it.
fn resolve_home() -> PathBuf {
    if let Ok(h) = std::env::var("SPIRA_HOME") {
        if !h.is_empty() {
            let p = PathBuf::from(&h);
            if p.join("conf.sh").is_file() {
                return p;
            }
        }
    }
    if let Some(exe) = argv0_path() {
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

/// `argv[0]`, resolved to where it actually sits, with every symlink component left
/// exactly as invoked — see `resolve_home`'s own comment.
///
/// A bare name (no `/`) is NOT already the resolved path the way it is when a shell execs
/// a PATH-found command: bash rewrites its own `$0` to the full path PATH search landed on
/// before exec, but a non-shell caller — `env PATH=... census`, `posix_spawnp`, a test
/// harness's own `env -i PATH="$STUBBIN:$PATH" ...` — calls `execvp` directly, which
/// resolves the PATH search internally but passes argv[0] through unchanged: still the
/// bare string "census". Joining that onto the current directory (as if shell-relative)
/// lands on nothing real. Caught live by testenv's test-skew-local-release.sh exercising
/// the identical pattern in `skew` (sp-yyk47) — ported here for the same reason.
fn argv0_path() -> Option<PathBuf> {
    let arg0 = std::env::args_os().next()?;
    let p = PathBuf::from(&arg0);
    if p.components().count() > 1 {
        return if p.is_absolute() { Some(p) } else { Some(std::env::current_dir().ok()?.join(p)) };
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&arg0);
        if is_exec(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_exec(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

fn main() -> ExitCode {
    let w = Real::new(resolve_home());
    let with_suppressed = std::env::args().nth(1).as_deref() == Some("--with-suppressed");
    let rc = census::run(&w, with_suppressed);
    ExitCode::from(rc.clamp(0, 255) as u8)
}
