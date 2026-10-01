//! Fixed `bash -c` seams into libraries this bead does not own (DESIGN.md §3): `lib.sh`'s
//! `repo_root`, and `world.sh`'s `TIMER_PRIORITY` plus `ctrl.sh`'s suspension map. Every
//! script text here is a constant — nothing is ever interpolated into it; the only data
//! that crosses the boundary travels as argv to the fixed script or as NUL/tab-delimited
//! stdout, the same discipline `sentinel/src/seams.rs` documents for its own lib.sh seams.

use std::process::Command;

/// `. "$SPIRA_HOME/lib.sh"; repo_root "<repo>"` — the registered repository's filesystem
/// path, or `None` if lib.sh could not be loaded or the repo is unregistered.
pub fn repo_root(spira_home: &str, repo: &str) -> Option<String> {
    let out = Command::new("bash")
        .arg("-c")
        .arg(r#". "$1/lib.sh" >/dev/null 2>&1 && repo_root "$2""#)
        .arg("_")
        .arg(spira_home)
        .arg(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub struct Timers {
    /// Essential timer base names, in `world.sh`'s `TIMER_PRIORITY` order.
    pub priority: Vec<String>,
    /// Base names `ctrl.sh` has a recorded suspension for.
    pub suspended: std::collections::BTreeSet<String>,
}

const TIMERS_SCRIPT: &str = r#"
set -uo pipefail
WORLD_LIB=1 . "$1/world.sh" >/dev/null 2>&1 || exit 97
declare -A CTRL_SUSPENDED=()
CTRL_LIB=1 . "$1/ctrl.sh" >/dev/null 2>&1 || exit 98
ctrl_load_suspended CTRL_SUSPENDED
for b in "${TIMER_PRIORITY[@]}"; do printf 'T\t%s\n' "$b"; done
for k in "${!CTRL_SUSPENDED[@]}"; do printf 'S\t%s\n' "$k"; done
"#;

/// Reads `TIMER_PRIORITY` and the suspended-base set through one `bash -c`. `None` means
/// the seam itself failed (world.sh or ctrl.sh could not be sourced) — the caller treats
/// that as "cannot check", not as "nothing is disabled" (law-absence-needs-a-positive-control).
pub fn timer_priority_and_suspended(spira_home: &str) -> Option<Timers> {
    let out = Command::new("bash")
        .arg("-c")
        .arg(TIMERS_SCRIPT)
        .arg("_")
        .arg(spira_home)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut priority = Vec::new();
    let mut suspended = std::collections::BTreeSet::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("T\t") {
            priority.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("S\t") {
            suspended.insert(rest.to_string());
        }
    }
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

    #[test]
    fn repo_root_is_none_when_lib_sh_is_missing() {
        assert_eq!(repo_root("/does/not/exist", "spira"), None);
    }

    #[test]
    fn timer_priority_and_suspended_reads_the_fixture_shape() {
        let d = testkit::TempDir::new("wt-seams-timers");
        std::fs::write(
            d.join("world.sh"),
            "TIMER_PRIORITY=(spira-sentinel spira-watchtower)\n",
        )
        .unwrap();
        std::fs::write(
            d.join("ctrl.sh"),
            "ctrl_load_suspended() { local -n m=\"$1\"; m[spira-watchtower]=1; }\n",
        )
        .unwrap();
        let t = timer_priority_and_suspended(d.to_str().unwrap()).unwrap();
        assert_eq!(t.priority, vec!["spira-sentinel", "spira-watchtower"]);
        assert!(t.suspended.contains("spira-watchtower"));
        assert!(!t.suspended.contains("spira-sentinel"));
    }

    #[test]
    fn timer_priority_and_suspended_is_none_when_world_sh_is_missing() {
        let d = testkit::TempDir::new("wt-seams-timers-missing");
        assert!(timer_priority_and_suspended(d.to_str().unwrap()).is_none());
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
