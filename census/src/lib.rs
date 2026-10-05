//! `census` — Maechen census: failure classes ranked by frequency, with open-remedy
//! suppression. Replaces `spira/census.sh` (238 lines). See DESIGN.md for the contract;
//! written from the script's intent (bd sp-yyk47). The SQL itself and the six
//! `census/*.py` pipeline scripts are unchanged (DESIGN.md §2) — this crate is the bash
//! orchestration around them, moved to Rust.

pub mod ports;
pub mod real;
pub mod sql;
#[cfg(test)]
mod tests;

use ports::World;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// Runs the whole census and prints it via `World::out`/`World::err`, exactly as
/// census.sh's case dispatch did. Returns the process exit code.
pub fn run(w: &dyn World, with_suppressed: bool) -> i32 {
    let run_dir = w.env("SPIRA_RUN").unwrap_or_default();
    let watermark_file = PathBuf::from(&run_dir).join("maechen.watermark");
    let watermark_ts = read_watermark(w, &watermark_file);

    // CLOCK SKEW GUARD (law-a-control-that-cannot-check-must-refuse).
    let tolerance = w.env("SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S").and_then(|v| v.parse::<i64>().ok()).unwrap_or(120);
    let host_epoch = w.host_utc_epoch();
    let row = match w.bd_sql_utc_now_row() {
        Ok(r) => r,
        Err(e) => {
            w.err(&format!("census: cannot verify the substrate clock against UTC — refusing to rank blind\n{e}"));
            return 1;
        }
    };
    let substrate_utc = row.split('|').next().unwrap_or("").trim().to_string();
    let Some(substrate_epoch) = w.parse_utc_to_epoch(&substrate_utc) else {
        w.err(&format!("census: cannot verify the substrate clock against UTC — refusing to rank blind\n{row}"));
        return 1;
    };
    let skew = substrate_epoch - host_epoch;
    if skew.abs() > tolerance {
        w.err(&format!(
            "census: substrate clock skew is {skew}s (tolerance {tolerance}s) — refusing to rank a windowed query over a clock that disagrees with UTC"
        ));
        w.err(&format!("census: substrate utc_fn={substrate_utc} host_utc={}", w.format_epoch_utc(host_epoch)));
        return 1;
    }

    // RANKED CENSUS: since-watermark when the watermark is valid, all-time otherwise.
    let Ok(all_time) = census_counts(w, None) else {
        w.err("census: events substrate is unreachable — cannot produce a census");
        return 1;
    };
    let ranked = if watermark_ts > 0 {
        let Ok(since_wm) = census_counts(w, Some(watermark_ts)) else {
            w.err("census: events substrate is unreachable — cannot produce a census");
            return 1;
        };
        w.merge_py(&all_time, &since_wm)
    } else {
        all_time
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let beads = it.next()?;
                let events = it.next()?;
                let class = it.next()?;
                let beads_part = it.next().map(|d| format!(", {d} beads")).unwrap_or_default();
                Some(format!("{beads} {class} ({events} detections{beads_part})"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let fold_map = w.census_class_fold_map();
    let covers_pattern = "covers:*";

    let suppressed: HashSet<String> = {
        let json = w.bd_list_json("open,in_progress,blocked,deferred", covers_pattern);
        w.covers_py(&json, &fold_map).lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
    };

    let mut suppressed_closed: HashSet<String> = HashSet::new();
    let mut orphaned_by_class: BTreeMap<String, Vec<String>> = BTreeMap::new();
    {
        let json = w.bd_list_json("closed", covers_pattern);
        let closed_out = w.covers_closed_py(&json, &fold_map);
        let repo = w.repo_root();
        for line in closed_out.lines() {
            let mut it = line.splitn(2, ' ');
            let (Some(bead_id), Some(class)) = (it.next(), it.next()) else { continue };
            if bead_id.is_empty() || class.is_empty() {
                continue;
            }
            match w.lc_landed(bead_id) {
                1 => {
                    let has_branch = repo.as_deref().map(|r| w.git_branch_exists_matching(r, &format!("*{bead_id}*"))).unwrap_or(false);
                    if has_branch {
                        suppressed_closed.insert(class.to_string());
                    } else {
                        orphaned_by_class.entry(class.to_string()).or_default().push(bead_id.to_string());
                    }
                }
                2 => w.err(&format!("census: remedy {bead_id}: land status unknown, not suppressing {class}")),
                _ => {}
            }
        }
    }

    for line in ranked.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (count, class, rest) = split3(line);
        if class.is_empty() {
            continue;
        }
        let rest_suffix = if rest.is_empty() { String::new() } else { format!(" {rest}") };
        if suppressed.contains(&class) {
            if with_suppressed {
                w.out(&format!("{count} {class}{rest_suffix} [suppressed]"));
            }
        } else if suppressed_closed.contains(&class) {
            if with_suppressed {
                w.out(&format!("{count} {class}{rest_suffix} [suppressed: remedy closed, not landed]"));
            }
        } else if let Some(ids) = orphaned_by_class.get(&class) {
            w.out(&format!("{count} {class}{rest_suffix} [orphaned remedy {}: closed, nothing in flight]", ids.join(",")));
        } else {
            w.out(&format!("{count} {class}{rest_suffix}"));
        }
    }

    if with_suppressed {
        let hw = w.handwritten_py(&w.census_handwritten_run_sql());
        print_lines(w, &hw);
        let del_sql = w.census_deliberate_run_sql(if watermark_ts > 0 { Some(watermark_ts) } else { None });
        let del = w.deliberate_py(&del_sql);
        print_lines(w, &del);
    }

    0
}

fn print_lines(w: &dyn World, text: &str) {
    let trimmed = text.trim_end_matches('\n');
    if trimmed.is_empty() {
        return;
    }
    for l in trimmed.lines() {
        w.out(l);
    }
}

/// `IFS=' ' read -r count class rest` — exactly 3 space-delimited pieces, `rest` carrying
/// everything after the second space verbatim (it may itself contain spaces, e.g.
/// `"(18 detections, 2 all-time)"`).
fn split3(line: &str) -> (String, String, String) {
    let mut it = line.splitn(3, ' ');
    (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string())
}

/// `_census_raw` (all-time) / the since-watermark variant: `census_events_run_sql [since] |
/// python3 count.py`.
fn census_counts(w: &dyn World, since: Option<i64>) -> Result<String, String> {
    let tabular = w.census_events_run_sql(since)?;
    w.count_py(&tabular)
}

/// Reads and validates the watermark file exactly as census.sh did: a missing file and an
/// unreadable/non-numeric one print DIFFERENT stderr messages (both fall back to 0 = all-time).
fn read_watermark(w: &dyn World, path: &std::path::Path) -> i64 {
    match w.read_to_string(path) {
        Some(raw) => {
            let trimmed: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
            if trimmed.is_empty() || !trimmed.chars().all(|c| c.is_ascii_digit()) {
                w.err(&format!("census: watermark at {} is unreadable; falling back to all-time counts", path.display()));
                0
            } else {
                trimmed.parse().unwrap_or(0)
            }
        }
        None => {
            w.err(&format!("census: no watermark file at {}; reporting all-time counts", path.display()));
            0
        }
    }
}
