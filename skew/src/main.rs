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
///
/// THAT FALLBACK READS `argv[0]`, NEVER `current_exe()`. `current_exe()` canonicalizes
/// every symlink in the path; testenv's own release staging (and the gate's fixture
/// release) link `bin/<tool>` to wherever cargo actually built it and `spira/` to the real
/// checkout — two unrelated directories once resolved. `current_exe()` there returns
/// cargo's own build output path, whose "sibling" spira/ doesn't exist, so the fallback
/// always missed silently (empty stdout, exit 3 — indistinguishable from a real
/// CANNOT-VERIFY until someone reads the exit code). `argv[0]` is exactly the path PATH
/// search resolved to, unresolved further — bash's own `$0` never re-resolves it either.
fn resolve_home() -> Result<PathBuf, String> {
    if let Ok(h) = std::env::var("SPIRA_HOME") {
        if !h.is_empty() {
            let p = PathBuf::from(&h);
            if p.join("lib.sh").is_file() {
                return Ok(p);
            }
        }
    }
    if let Some(exe) = argv0_path() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(release_dir) = bin_dir.parent() {
                let candidate = release_dir.join("spira");
                if candidate.join("lib.sh").is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    match std::env::var("SPIRA_HOME") {
        Ok(h) if !h.is_empty() => Ok(PathBuf::from(h)),
        _ => Err("skew: SPIRA_HOME is not set and no spira/lib.sh sits beside the executable".to_string()),
    }
}

/// `argv[0]`, resolved to where it actually sits, but with every symlink component left
/// exactly as invoked — never `current_exe()`'s fully resolved path. See `resolve_home`'s
/// own comment for why that distinction is load-bearing.
///
/// A bare name (no `/`) is NOT already the resolved path the way it is when a shell execs
/// a PATH-found command: bash rewrites its own `$0` to the full path PATH search landed on
/// before exec, but a non-shell caller — `env PATH=... skew check`, `posix_spawnp`, a test
/// harness's own `env -i PATH="$STUBBIN:$PATH" ... skew check` — calls `execvp` directly,
/// which resolves the PATH search internally but passes argv[0] through to the new process
/// completely unchanged: still the bare string "skew". Joining that onto the current
/// directory (as if it were shell-relative) lands on nothing real. Caught live by testenv's
/// test-skew-local-release.sh, whose fixture invokes exactly that way (sp-yyk47).
fn argv0_path() -> Option<PathBuf> {
    let arg0 = std::env::args_os().next()?;
    let p = PathBuf::from(&arg0);
    if p.components().count() > 1 {
        // Has a directory component (relative or absolute) — this IS where it was found.
        return if p.is_absolute() { Some(p) } else { Some(std::env::current_dir().ok()?.join(p)) };
    }
    // Bare name: redo the PATH search ourselves, exactly as execvp did, but keep the
    // first matching entry's path unresolved (no canonicalize).
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
