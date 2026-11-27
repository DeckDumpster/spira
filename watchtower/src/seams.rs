//! What the sweep asks of libraries it does not own (DESIGN.md §3), all in process.

/// The repo registry (`spira_config::repos::Registry::from_env`, sp-k6lku "wave 4.13"),
/// resolved once, in-process — no `bash -c '. lib.sh; repo_root ...'` subprocess, and no
/// one-shot snapshot subprocess either: `from_env` resolves
/// `SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`/`SPIRA_REPO_MAP` the same way
/// conf.sh does, in-process, when this (bare, unit-launched) process's own environment
/// lacks them (sp-z3eyk). `spira_home` is `lib_sh_dir()`'s own output (the directory
/// holding lib.sh), matching every other caller here.
pub fn registry(spira_home: &str) -> spira_config::repos::Registry {
    spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(spira_home))
}

pub struct PipelineProbe {
    /// `(rune, dwarf_count)` for every `spira_runes` entry — the harness's own primitive
    /// for "how many dwarves are alive", counted from pidfiles/`/proc`, never a `pgrep -f`
    /// substring scan (that failure mode is the whole reason `dwarf_count` exists).
    pub rune_counts: Vec<(String, u32)>,
    /// `(rune, ready)` from `bulk_ready_by_rune` — one `bd` round trip, only paid when the
    /// ledger file is readable (the common "no dwarf has ever called here" case skips it).
    pub ready_by_rune: Vec<(String, u32)>,
}

/// `None` means the probe itself failed (no home to read the runes from) — the caller
/// reports dwarves-alive and idle-while-ready as `?`, never as zero.
///
/// In-process: the roster is `spira_config::runes::spira_runes` and each count is
/// `strand::probe::dwarf_count` — the functions lib.sh's `spira_runes`/`dwarf_count` shims
/// called through `spira-config rune roster` and `strand dwarf-count`. A count that cannot be
/// read is 0, exactly as the shim's `|| echo 0` made it.
pub fn pipeline_probe(_lib_sh_dir: &str, ledger_path: Option<&str>) -> Option<PipelineProbe> {
    let home = spira_config::resolve::locate_home_for_process().ok()?;
    let roster = spira_config::process::cfg("SPIRA_RUNES").map(|r| spira_config::runes::spira_runes(&home, Some(&r))).unwrap_or_default();
    let strand_cfg = strand::config::Config::resolve(&strand::config::Live::load()).ok();
    let count = |rune: &str| strand_cfg.as_ref().map_or(0, |c| strand::probe::dwarf_count(c, rune, None));
    let ready = ledger_path.filter(|p| std::fs::File::open(p).is_ok()).and_then(|_| bulk_ready_by_rune());
    Some(probe_from(&roster, &count, ready.as_deref()))
}

/// `bulk-ready-by-rune`'s `<rune> <n>` lines; `None` when it could not answer.
fn bulk_ready_by_rune() -> Option<String> {
    crate::deadline::call("dwarves alive / ready (bd)", || spira_claim::cli::bulk_ready_by_rune().ok().map(|r| r.lines)).flatten()
}

/// The probe from the roster (space separated, priority order), a per-rune count, and the
/// ready lines when the ledger made them worth reading.
pub fn probe_from(roster: &str, count: &dyn Fn(&str) -> u32, ready: Option<&str>) -> PipelineProbe {
    let rune_counts = roster.split_whitespace().map(|f| (f.to_string(), count(f))).collect();
    let ready_by_rune = ready
        .unwrap_or("")
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(2, ' ');
            let f = it.next().filter(|f| !f.is_empty())?;
            Some((f.to_string(), it.next().unwrap_or("").trim().parse().unwrap_or(0)))
        })
        .collect();
    PipelineProbe { rune_counts, ready_by_rune }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tp3pfac)]
    #[test]
    fn the_roster_is_counted_in_order_and_ready_lines_are_parsed() {
        let p = probe_from(" builder ops", &|f| if f == "builder" { 2 } else { 0 }, Some("builder 5\nops 0\n"));
        assert_eq!(p.rune_counts, vec![("builder".to_string(), 2), ("ops".to_string(), 0)]);
        assert_eq!(p.ready_by_rune, vec![("builder".to_string(), 5), ("ops".to_string(), 0)]);
    }

    #[test_id(tpqcmnt)]
    #[test]
    fn no_ready_answer_is_no_ready_rows() {
        assert!(probe_from("builder", &|_| 1, None).ready_by_rune.is_empty());
    }
}
