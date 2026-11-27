//! What the sweep asks of libraries it does not own (DESIGN.md §3), all in process.

/// The repo registry, resolved in-process from the home this process locates
/// (`SPIRA_HOME`, else `$SPIRA_RELEASE/spira`). `lib.sh`'s directory is not that home.
pub fn registry() -> Result<spira_config::repos::Registry, String> {
    let home = spira_config::resolve::locate_home_for_process()?;
    Ok(spira_config::repos::Registry::from_env(std::env::vars().collect(), &home))
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

    #[test_id(tjd5304)]
    #[test]
    fn the_registry_reads_the_located_home_with_no_lib_sh_on_path() {
        let d = testkit::TempDir::new("wt-registry");
        let home = spira_config::process::fixture_registry_home(d.path());
        let map = d.path().join("fixture.repos");
        std::fs::write(&map, format!("spira | {} | queue.local | local/main |  |\n", d.path().join("spira").display())).unwrap();
        let toml = spira_config::process::fixture_toml(d.path(), &[("SPIRA_REPO_MAP", map.to_str().unwrap())]);
        // The fixture's own repo blocks would answer for an unread map.
        let mut doc = spira_config::load(&toml).unwrap();
        doc.repo.clear();
        spira_config::serialize_and_write(&toml, &doc).unwrap();
        let no_tools = d.path().join("empty-path");
        std::fs::create_dir(&no_tools).unwrap();
        let _env = testkit::env(&[
            ("SPIRA_HOME", home.to_str()),
            ("SPIRA_TOML", toml.to_str()),
            ("SPIRA_REPO_MAP", None),
            ("SPIRA_HOME_REPO", None),
            ("PATH", no_tools.to_str()),
        ]);

        let reg = registry().expect("SPIRA_HOME locates the home");

        assert_eq!(reg.base("spira").as_deref(), Some("local/main"));
    }

    #[test_id(tk2iqt1)]
    #[test]
    fn a_home_that_cannot_be_located_is_refused_not_built_from_nothing() {
        let _env = testkit::env(&[("SPIRA_HOME", None), ("SPIRA_RELEASE", None)]);

        let refusal = registry().err().expect("no home is named");

        assert!(refusal.contains("SPIRA_HOME"), "{refusal}");
    }

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
