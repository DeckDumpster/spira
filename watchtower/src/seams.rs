//! Fixed `bash -c` seams into libraries this bead does not own (DESIGN.md §3). Every script text here is
//! a constant — nothing is ever interpolated into it; the only data that crosses the
//! boundary travels as argv to the fixed script or as NUL/tab-delimited stdout, the same
//! discipline `sentinel/src/seams.rs` documents for its own lib.sh seams. `repo_root`
//! (family U) is no longer one of these seams (sp-k6lku, "wave 4.13") — [`registry`] reads
//! it in-process through `spira_config::repos`.

use std::process::Command;

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

const PIPELINE_PROBE_SCRIPT: &str = r#"
set -uo pipefail
. "$1/lib.sh" >/dev/null 2>&1 || exit 97
for f in $(spira_runes 2>/dev/null); do
    printf 'F\t%s\t%s\n' "$f" "$(dwarf_count "$f" 2>/dev/null || echo 0)"
done
_ledger="${2:-}"
if [ -n "$_ledger" ] && [ -r "$_ledger" ]; then
    r="$(bulk_ready_by_rune 2>/dev/null)" || r=""
    if [ -n "$r" ]; then
        while IFS=' ' read -r f n; do
            [ -n "$f" ] && printf 'R\t%s\t%s\n' "$f" "$n"
        done <<< "$r"
    fi
fi
"#;

/// `None` means the seam itself failed (lib.sh could not be sourced) — the caller reports
/// dwarves-alive and idle-while-ready as `?`, never as zero.
pub fn pipeline_probe(spira_home: &str, ledger_path: Option<&str>) -> Option<PipelineProbe> {
    let out = crate::deadline::output(
        "dwarves alive / ready (bd)",
        Command::new("bash")
            .arg("-c")
            .arg(PIPELINE_PROBE_SCRIPT)
            .arg("_")
            .arg(spira_home)
            .arg(ledger_path.unwrap_or(""))
            // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
            // release's bin/+spira/ on the CHILD's PATH, never only inherited.
            .envs(spira_config::release_env::child_path_env_for_process()),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rune_counts = Vec::new();
    let mut ready_by_rune = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let n: u32 = parts[2].parse().unwrap_or(0);
        match parts[0] {
            "F" => rune_counts.push((parts[1].to_string(), n)),
            "R" => ready_by_rune.push((parts[1].to_string(), n)),
            _ => {}
        }
    }
    Some(PipelineProbe {
        rune_counts,
        ready_by_rune,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `registry()` now resolves for real when the environment lacks the four registry
    /// keys (`Registry::from_env`, sp-k6lku). Pinning `SPIRA_TOML` alone is not enough:
    /// `SPIRA_REPO_MAP`'s own default (`repo_map_candidate`, spira-config/src/resolve.rs)
    /// checks a LEGACY `spira.conf`'s directory too, found through `HOME`/
    /// `XDG_CONFIG_HOME` independent of the `SPIRA_TOML` pin — so both legacy-search
    /// inputs are pinned at a fixture directory that holds neither, to never resolve the
    /// real operator's own map file on the machine running the suite.
    #[test]
    fn repo_root_is_none_when_lib_sh_is_missing() {
        let d = testkit::TempDir::new("watchtower-repo-root-missing");
        let toml = d.join("no-such-config.toml");
        let xdg = d.join("no-such-xdg");
        let _g = testkit::env(&[
            ("SPIRA_TOML", toml.to_str()),
            ("HOME", d.to_str()),
            ("XDG_CONFIG_HOME", xdg.to_str()),
        ]);
        assert_eq!(registry("/does/not/exist").root("spira"), None);
    }

    #[test]
    fn pipeline_probe_reads_rune_counts_and_skips_ready_without_a_ledger() {
        let d = testkit::TempDir::new("wt-seams-probe");
        std::fs::write(
            d.join("lib.sh"),
            "spira_runes() { printf 'builder\\nops\\n'; }\ndwarf_count() { [ \"$1\" = builder ] && echo 2 || echo 0; }\nbulk_ready_by_rune() { echo 'builder 5'; }\n",
        )
        .unwrap();
        let p = pipeline_probe(d.to_str().unwrap(), None).unwrap();
        assert_eq!(
            p.rune_counts,
            vec![("builder".to_string(), 2), ("ops".to_string(), 0)]
        );
        assert!(p.ready_by_rune.is_empty());
    }

    #[test]
    fn pipeline_probe_reads_ready_when_the_ledger_exists() {
        let d = testkit::TempDir::new("wt-seams-probe-ledger");
        std::fs::write(
            d.join("lib.sh"),
            "spira_runes() { printf 'builder\\n'; }\ndwarf_count() { echo 1; }\nbulk_ready_by_rune() { echo 'builder 5'; }\n",
        )
        .unwrap();
        let ledger = d.join("dwarf-ledger.log");
        std::fs::write(&ledger, "x\n").unwrap();
        let p = pipeline_probe(d.to_str().unwrap(), Some(ledger.to_str().unwrap())).unwrap();
        assert_eq!(p.ready_by_rune, vec![("builder".to_string(), 5)]);
    }
}
