//! The lib.sh seam (DESIGN.md §4): `bash` with no arguments, reading a FIXED script from
//! stdin followed by the operation's values, each NUL-terminated — the construction
//! landing-pass uses (law-payloads-go-on-stdin). The first two values of every call are
//! `$SPIRA_HOME` (lib.sh's directory) and the `--status-from` file (empty for none), so a
//! suite's status seam governs every bd question this still asks through bash.
//!
//! sp-9envm moved the destruction chokepoint itself (the holder witnesses' liveness half,
//! salvage, the verified deletions, the reap log) into Rust — `reap.rs`, called in-process
//! from `real.rs` now, not through here. Base (family W — `spira_landref`/`spira_landrefs`/
//! `ref_remote`) moved too (sp-o88bx, "wave 4.12"): `real.rs`'s `base()` calls
//! `spira_config::repos` directly, no `Op::Base` seam left to retire-rather-than-port
//! around. Context's own repository list (family U: `spira_repos`/`repo_root`/
//! `repo_land_queued`) moved the same way (sp-k6lku, "wave 4.13") — `Real::new` builds it
//! from `repo_registry()`, not this seam. What is LEFT going through bash is only what has
//! not moved yet: context's plain settings, the bead's own record and the two bd questions
//! the liveness witness still needs (`spira_bead_status`/`spira_db_reachable`, family
//! A/B) — and the label mutations, which are one-line bdq calls not worth a Rust
//! reimplementation yet.

/// Precedes a seam's machine-readable answer on stdout; everything before it is lib.sh's
/// own output (its `log` lines, salvage notes), passed through to ours.
pub const MARK: char = '\u{1e}';
/// Separates the fields of one record in an answer.
pub const FIELD: char = '\u{1d}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Context,
    Bead,
    /// `spira_db_reachable` + `spira_bead_status`, in one process: the one bd question the
    /// native `holder_witnesses` (reap.rs) still cannot answer itself (families A/B).
    Status,
    CloseOnLand,
    LabelAdd,
    LabelRemove,
}

#[cfg(test)]
pub const ALL: &[Op] = &[Op::Context, Op::Bead, Op::Status, Op::CloseOnLand, Op::LabelAdd, Op::LabelRemove];

const PRELUDE: &str = r#"{
set -uo pipefail
__q=()
while IFS= read -r -d '' __v; do __q+=("$__v"); done
exec </dev/null
HERE="${__q[0]}"
__sf="${__q[1]}"
set -- "${__q[@]:2}"
unset __q __v
. "$HERE/lib.sh" || exit 96
[ -n "$__sf" ] && spira_status_seam "$__sf"
progress() { log "$*"; }
act() { log "$*"; }
"#;

/// Settings only — the repository list (family U: `spira_repos`/`repo_root`/
/// `repo_land_queued`) moved in-process (sp-k6lku, "wave 4.13"): `Real::new` builds it from
/// `repo_registry()` after this seam call instead of this script's own loop, which paid one
/// extra lib.sh-shim subprocess fork per mapped repository.
const CONTEXT: &str = r#"printf '\036'
printf 'run=%s\0' "${SPIRA_RUN:-}"
printf 'reaplog=%s\0' "${SPIRA_REAPLOG:-}"
printf 'submitted=%s\0' "${SPIRA_SUBMITTED_LABEL:-spira-submitted}"
printf 'gh=%s\0' "${SPIRA_GH:-gh}"
printf 'gh_timeout=%s\0' "${GH_TIMEOUT:-120}"
exit 0
"#;

/// `bdjson show <id>` verbatim.
const BEAD: &str = r#"printf '\036%s' "$(bdjson show "$1" 2>/dev/null)"
exit 0
"#;

/// `<0|1>` (db reachable) FIELD `<status>` — the two bd-backed questions
/// `reap::holder_witnesses` asks through `BdProbe`, calling the still-bash
/// `spira_db_reachable`/`spira_bead_status` so the status seam (a suite's
/// `spira_status_seam`) keeps governing both exactly as it did before the chokepoint moved.
const STATUS: &str = r#"__ok=0; spira_db_reachable && __ok=1
__st="$(spira_bead_status "$1" 2>/dev/null)"
printf '\036%s\035%s' "$__ok" "$__st"
exit 0
"#;

fn body(op: Op) -> &'static str {
    match op {
        Op::Context => CONTEXT,
        Op::Bead => BEAD,
        Op::Status => STATUS,
        Op::CloseOnLand => "bead_close_on_land \"$1\" \"$2\" || true\nexit 0\n",
        Op::LabelAdd => "bdq label add \"$1\" \"$2\" >/dev/null 2>&1 || true\nexit 0\n",
        Op::LabelRemove => "bdq label remove \"$1\" \"$2\" >/dev/null 2>&1 || true\nexit 0\n",
    }
}

/// The complete fixed script for `op`.
pub fn script(op: Op) -> String {
    format!("{PRELUDE}{}}}\n", body(op))
}

/// The bytes written to the seam's stdin: the script, then every value NUL-terminated.
pub fn stdin_bytes(op: Op, values: &[&str]) -> Vec<u8> {
    let mut v = script(op).into_bytes();
    for val in values {
        v.extend_from_slice(val.split('\0').next().unwrap_or("").as_bytes());
        v.push(0);
    }
    v
}

/// Split a seam's stdout into lib.sh's own output (before the last MARK) and the answer.
pub fn split(stdout: &str) -> (&str, &str) {
    match stdout.rfind(MARK) {
        Some(i) => (&stdout[..i], &stdout[i + MARK.len_utf8()..]),
        None => (stdout, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_script_is_one_braced_command_that_exits_inside_it() {
        for op in ALL {
            let s = script(*op);
            assert!(s.starts_with("{\n") && s.ends_with("}\n"), "{op:?}");
            assert!(!s.contains('\0'), "{op:?}");
            assert!(body(*op).contains("exit"), "{op:?} must exit inside the braces");
        }
    }

    #[test]
    fn split_passes_log_lines_through_and_takes_the_last_answer() {
        assert_eq!(split("a log\n\u{1e}answer"), ("a log\n", "answer"));
        assert_eq!(split("x\u{1e}y\u{1e}z"), ("x\u{1e}y", "z"));
        assert_eq!(split("no mark"), ("no mark", ""));
    }
}
