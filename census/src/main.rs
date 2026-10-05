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
    match std::env::var("SPIRA_HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => {
            eprintln!("census: SPIRA_HOME is not set and no spira/conf.sh sits beside the executable");
            std::process::exit(3)
        }
    }
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("sql") {
        return ExitCode::from(cmd_sql(&w, &argv[1..]).clamp(0, 255) as u8);
    }
    let with_suppressed = argv.first().map(String::as_str) == Some("--with-suppressed");
    let rc = census::run(&w, with_suppressed);
    ExitCode::from(rc.clamp(0, 255) as u8)
}

/// `census sql <events|handwritten|deliberate|class-fold-map|deliberate-causes>
/// [since_epoch_s]` — the `lib.sh` shim doors for `_census_events_sql`/
/// `_census_handwritten_sql`/`_census_deliberate_sql`/`_census_class_fold_map`/
/// `_census_deliberate_causes_sql_list` (wave4-decomposition.md row M, wave 4.35,
/// sp-kelr2). `census.sh` no longer exists for these to move "bash to bash" into — this
/// crate replaced it, in an earlier bead, deliberately leaving the SQL in `lib.sh`
/// (`ports.rs`'s own doc). The text itself lives in `census::sql`; this is only the CLI
/// door plus the epoch-to-UTC-string step `World::format_epoch_utc` already does for every
/// other caller in this crate.
fn cmd_sql(w: &dyn census::ports::World, args: &[String]) -> i32 {
    let since_formatted = |i: usize| -> Option<String> {
        args.get(i).and_then(|s| s.parse::<i64>().ok()).filter(|n| *n > 0).map(|n| w.format_epoch_utc(n))
    };
    match args.first().map(String::as_str) {
        Some("events") => {
            println!("{}", census::sql::events_sql(since_formatted(1).as_deref(), &w.deliberate_cause_names()));
            0
        }
        Some("handwritten") => {
            println!("{}", census::sql::handwritten_sql());
            0
        }
        Some("deliberate") => {
            println!("{}", census::sql::deliberate_sql(since_formatted(1).as_deref(), &w.deliberate_cause_names()));
            0
        }
        Some("class-fold-map") => {
            print!("{}", census::sql::class_fold_map());
            0
        }
        Some("deliberate-causes") => {
            println!("{}", census::sql::deliberate_causes_sql_list(&w.deliberate_cause_names()));
            0
        }
        // The retry-aware RUNNERS — `census_events_run_sql`/`census_handwritten_run_sql`/
        // `census_deliberate_run_sql`'s own shim doors (distinct from the pure SQL-text
        // subcommands above): these actually invoke `bd sql` through `World`, in-process,
        // exactly as `census::run`'s own pipeline already does. `events_run`'s epoch
        // parsing matches `events`'s own above, but here it's the `World` impl's job to
        // format it (it owns the retry loop too).
        Some("run-events") => match w.census_events_run_sql(epoch(1, args)) {
            Ok(out) => {
                println!("{out}");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Some("run-handwritten") => {
            println!("{}", w.census_handwritten_run_sql());
            0
        }
        Some("run-deliberate") => {
            println!("{}", w.census_deliberate_run_sql(epoch(1, args)));
            0
        }
        _ => {
            eprintln!(
                "usage: census sql <events|handwritten|deliberate|class-fold-map|deliberate-causes|run-events|run-handwritten|run-deliberate> [since_epoch_s]"
            );
            1
        }
    }
}

/// The raw epoch at `args[i]`, or `None` for anything unset/unparseable/`<= 0` — the same
/// `[ -n "${1:-}" ] && [ "${1:-0}" -gt 0 ]` guard `_census_events_sql`/`_census_deliberate_sql`
/// shared in the bash, now shared by the two pure-text subcommands and the two runners.
fn epoch(i: usize, args: &[String]) -> Option<i64> {
    args.get(i).and_then(|s| s.parse::<i64>().ok()).filter(|n| *n > 0)
}
