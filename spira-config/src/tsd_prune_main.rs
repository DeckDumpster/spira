//! tsd-prune — apply the run/tsd/ retention bound: `tsd-prune [--dry-run]`.
//! Bounds come from SPIRA_TSD_KEEP_DAYS and SPIRA_TSD_KEEP_DAYS_BY_FAMILY, floored per family by
//! `tsd::prune::READER_WINDOW_DAYS`. `tsd` cannot read config (it is a dependency of this
//! crate), so the config door lives here.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(e) => {
            eprintln!("tsd-prune: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<usize, String> {
    let dry = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--dry-run") => true,
        Some(other) => return Err(format!("unknown argument {other:?}; usage: tsd-prune [--dry-run]")),
    };
    let run_dir = PathBuf::from(spira_config::process::cfg("SPIRA_RUN")?);
    if run_dir.as_os_str().is_empty() {
        return Err("SPIRA_RUN is empty — refusing to prune a relative path".into());
    }
    let default_days: u32 = spira_config::process::cfg_parse("SPIRA_TSD_KEEP_DAYS")?;
    let overrides = tsd::prune::parse_overrides(&spira_config::process::cfg("SPIRA_TSD_KEEP_DAYS_BY_FAMILY")?)
        .map_err(|e| format!("SPIRA_TSD_KEEP_DAYS_BY_FAMILY: {e}"))?;
    let now = tsd::parse_ts(&tsd::now_iso()).ok_or("cannot read the clock")?;
    if dry {
        for (family, days) in family_bounds(&run_dir, default_days, &overrides) {
            println!("{family}\tkeep {days}d");
        }
        return Ok(0);
    }
    let mut failed = 0;
    for (family, days, r) in tsd::prune::prune_root(&run_dir, default_days, &overrides, now) {
        match r {
            Ok(s) => println!("{family}\tkeep {days}d\tkept {}\tdropped {}", s.kept, s.dropped),
            Err(e) => {
                failed += 1;
                eprintln!("{family}\tkeep {days}d\tFAILED {e}");
            }
        }
    }
    Ok(failed)
}

fn family_bounds(run: &std::path::Path, default_days: u32, overrides: &std::collections::BTreeMap<String, u32>) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = std::fs::read_dir(run.join("tsd"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".jsonl").map(str::to_string))
        .filter(|f| tsd::valid_family(f))
        .map(|f| (f.clone(), tsd::prune::keep_days(&f, default_days, overrides)))
        .collect();
    v.sort();
    v
}
