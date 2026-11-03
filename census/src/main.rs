//! `census [--with-suppressed]` — see DESIGN.md §3.

use census::real::Real;

use std::process::ExitCode;

fn main() -> ExitCode {
    let w = match spira_config::resolve::locate_home_for_process().and_then(Real::new) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("census: {e}");
            return ExitCode::from(3);
        }
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("pipeline") {
        return ExitCode::from(cmd_pipeline(&w, &argv[1..]).clamp(0, 255) as u8);
    }
    if argv.first().map(String::as_str) == Some("sql") {
        return ExitCode::from(cmd_sql(&w, &argv[1..]).clamp(0, 255) as u8);
    }
    let with_suppressed = argv.first().map(String::as_str) == Some("--with-suppressed");
    let rc = census::run(&w, with_suppressed);
    ExitCode::from(rc.clamp(0, 255) as u8)
}

/// `census pipeline <cluster|cluster-merge|covers|covers-closed|handwritten|deliberate> [file...]`
/// — the classification stages on their own, reading stdin (and, for `cluster-merge`, two files;
/// for `covers`/`covers-closed`, a fold-map file), for the suites that feed them canned input.
fn cmd_pipeline(w: &dyn census::ports::World, args: &[String]) -> i32 {
    use std::io::Read;
    let mut stdin = String::new();
    let file = |i: usize| args.get(i).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let mut read_stdin = || {
        let _ = std::io::stdin().read_to_string(&mut stdin);
        std::mem::take(&mut stdin)
    };
    let out = match args.first().map(String::as_str) {
        Some("cluster") => w.cluster(&read_stdin()).unwrap_or_default(),
        Some("cluster-merge") => w.cluster_merge(&file(1), &file(2)),
        Some("covers") => w.covers(&read_stdin(), &file(1)),
        Some("covers-closed") => w.covers_closed(&read_stdin(), &file(1)),
        Some("handwritten") => w.handwritten(&read_stdin()),
        Some("deliberate") => w.deliberate(&read_stdin()),
        _ => {
            eprintln!("usage: census pipeline <cluster|cluster-merge|covers|covers-closed|handwritten|deliberate> [file...]");
            return 1;
        }
    };
    print!("{out}");
    0
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
        Some("event-rows") => {
            println!("{}", census::sql::event_rows_sql(since_formatted(1).as_deref(), &w.deliberate_cause_names(), &[]));
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
        Some("run-event-rows") => match w.census_event_rows_run_sql(epoch(1, args)) {
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
