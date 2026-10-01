//! Fixed `bash -c` seams into libraries this bead does not own (DESIGN.md §3):
//! `world.sh`'s `TIMER_PRIORITY` plus `ctrl.sh`'s suspension map. Every script text here is
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

pub struct Timers {
    /// Essential timer base names, in `world.sh`'s `TIMER_PRIORITY` order.
    pub priority: Vec<String>,
    /// Base names `ctrl.sh` has a recorded suspension for.
    pub suspended: std::collections::BTreeSet<String>,
}

// world.sh and ctrl.sh are the `world` and `aeons`/`ctrl` binaries now (sp-6onps) — a
// compiled binary cannot be sourced, so WORLD_LIB=1/CTRL_LIB=1 stopped being possible.
// `world.sh timer-priority` and `ctrl.sh suspended` are the machine-readable seams each
// binary grew for exactly this: callers that used to source them for a list/map now read
// one subcommand's stdout once instead. Both are invoked by bare name on the launcher's
// PATH (sp-gypjk) — `spira_home` is no longer part of the path, so it is accepted but
// unused, kept only so this function's signature does not ripple into its caller.
const TIMERS_SCRIPT: &str = r#"
set -uo pipefail
world.sh timer-priority || exit 97
printf '\0'
ctrl.sh suspended || exit 98
"#;

/// Reads `TIMER_PRIORITY` and the suspended-base set through one `bash -c`. `None` means
/// the seam itself failed (`world.sh timer-priority` or `ctrl.sh suspended` could not run)
/// — the caller treats that as "cannot check", not as "nothing is disabled"
/// (law-absence-needs-a-positive-control).
pub fn timer_priority_and_suspended(_spira_home: &str) -> Option<Timers> {
    let out = Command::new("bash").arg("-c").arg(TIMERS_SCRIPT).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let split = out.stdout.iter().position(|&b| b == 0)?;
    let (pri_bytes, rest) = out.stdout.split_at(split);
    let priority: Vec<String> = String::from_utf8_lossy(pri_bytes)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect();
    let suspended: std::collections::BTreeSet<String> = String::from_utf8_lossy(&rest[1..])
        .lines()
        .filter_map(|l| l.split('\t').next())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    Some(Timers {
        priority,
        suspended,
    })
}

pub struct PipelineProbe {
    /// `(fayth, aeon_count)` for every `spira_fayths` entry — the harness's own primitive
    /// for "how many aeons are alive", counted from pidfiles/`/proc`, never a `pgrep -f`
    /// substring scan (that failure mode is the whole reason `aeon_count` exists).
    pub fayth_counts: Vec<(String, u32)>,
    /// `(fayth, ready)` from `bulk_ready_by_fayth` — one `bd` round trip, only paid when the
    /// ledger file is readable (the common "no aeon has ever summoned here" case skips it).
    pub ready_by_fayth: Vec<(String, u32)>,
}

const PIPELINE_PROBE_SCRIPT: &str = r#"
set -uo pipefail
. "$1/lib.sh" >/dev/null 2>&1 || exit 97
for f in $(spira_fayths 2>/dev/null); do
    printf 'F\t%s\t%s\n' "$f" "$(aeon_count "$f" 2>/dev/null || echo 0)"
done
_ledger="${2:-}"
if [ -n "$_ledger" ] && [ -r "$_ledger" ]; then
    r="$(bulk_ready_by_fayth 2>/dev/null)" || r=""
    if [ -n "$r" ]; then
        while IFS=' ' read -r f n; do
            [ -n "$f" ] && printf 'R\t%s\t%s\n' "$f" "$n"
        done <<< "$r"
    fi
fi
"#;

/// `None` means the seam itself failed (lib.sh could not be sourced) — the caller reports
/// aeons-alive and idle-while-ready as `?`, never as zero.
pub fn pipeline_probe(spira_home: &str, ledger_path: Option<&str>) -> Option<PipelineProbe> {
    let out = Command::new("bash")
        .arg("-c")
        .arg(PIPELINE_PROBE_SCRIPT)
        .arg("_")
        .arg(spira_home)
        .arg(ledger_path.unwrap_or(""))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fayth_counts = Vec::new();
    let mut ready_by_fayth = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let n: u32 = parts[2].parse().unwrap_or(0);
        match parts[0] {
            "F" => fayth_counts.push((parts[1].to_string(), n)),
            "R" => ready_by_fayth.push((parts[1].to_string(), n)),
            _ => {}
        }
    }
    Some(PipelineProbe {
        fayth_counts,
        ready_by_fayth,
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
        std::env::set_var("SPIRA_TOML", d.join("no-such-config.toml"));
        std::env::set_var("HOME", d.path());
        std::env::set_var("XDG_CONFIG_HOME", d.join("no-such-xdg"));
        assert_eq!(registry("/does/not/exist").root("spira"), None);
    }

    /// world.sh/ctrl.sh are the `world`/`ctrl` binaries now (sp-6onps): the seam calls
    /// them by bare name on PATH, so the fixture is a fake PATH entry, not a sourceable
    /// bash library — `world.sh timer-priority` and `ctrl.sh suspended` are the only
    /// contract this function depends on.
    fn with_fake_path<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let saved = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{saved}", dir.display()));
        let r = f();
        std::env::set_var("PATH", saved);
        r
    }

    #[test]
    fn timer_priority_and_suspended_reads_the_fixture_shape() {
        let d = testkit::TempDir::new("wt-seams-timers");
        testkit::write_exe(
            d.join("world.sh"),
            "#!/usr/bin/env bash\nprintf 'spira-sentinel\\nspira-watchtower\\n'\n",
        );
        testkit::write_exe(
            d.join("ctrl.sh"),
            "#!/usr/bin/env bash\nprintf 'spira-watchtower\\tbecause\\n'\n",
        );
        let t = with_fake_path(&d, || timer_priority_and_suspended("unused")).unwrap();
        assert_eq!(t.priority, vec!["spira-sentinel", "spira-watchtower"]);
        assert!(t.suspended.contains("spira-watchtower"));
        assert!(!t.suspended.contains("spira-sentinel"));
    }

    #[test]
    fn timer_priority_and_suspended_is_none_when_world_sh_is_missing() {
        let d = testkit::TempDir::new("wt-seams-timers-missing");
        // A confined PATH (never the inherited one) — this box's own release may well
        // have a real world.sh/ctrl.sh on it, which would defeat "missing" entirely.
        let saved = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:/usr/bin:/bin", d.display()));
        let r = timer_priority_and_suspended("unused");
        std::env::set_var("PATH", saved);
        assert!(r.is_none());
    }

    #[test]
    fn pipeline_probe_reads_fayth_counts_and_skips_ready_without_a_ledger() {
        let d = testkit::TempDir::new("wt-seams-probe");
        std::fs::write(
            d.join("lib.sh"),
            "spira_fayths() { printf 'builder\\nops\\n'; }\naeon_count() { [ \"$1\" = builder ] && echo 2 || echo 0; }\nbulk_ready_by_fayth() { echo 'builder 5'; }\n",
        )
        .unwrap();
        let p = pipeline_probe(d.to_str().unwrap(), None).unwrap();
        assert_eq!(
            p.fayth_counts,
            vec![("builder".to_string(), 2), ("ops".to_string(), 0)]
        );
        assert!(p.ready_by_fayth.is_empty());
    }

    #[test]
    fn pipeline_probe_reads_ready_when_the_ledger_exists() {
        let d = testkit::TempDir::new("wt-seams-probe-ledger");
        std::fs::write(
            d.join("lib.sh"),
            "spira_fayths() { printf 'builder\\n'; }\naeon_count() { echo 1; }\nbulk_ready_by_fayth() { echo 'builder 5'; }\n",
        )
        .unwrap();
        let ledger = d.join("aeon-ledger.log");
        std::fs::write(&ledger, "x\n").unwrap();
        let p = pipeline_probe(d.to_str().unwrap(), Some(ledger.to_str().unwrap())).unwrap();
        assert_eq!(p.ready_by_fayth, vec![("builder".to_string(), 5)]);
    }
}
