//! The lib.sh seam (DESIGN.md §4): `bash` with no arguments, reading a FIXED script from
//! stdin followed by the operation's values, each NUL-terminated — the construction
//! landing-pass uses (law-payloads-go-on-stdin). The first two values of every call are
//! `$SPIRA_HOME` (lib.sh's directory) and the `--status-from` file (empty for none), so a
//! suite's status seam governs EVERY witness a destruction asks, as it did when one bash
//! process ran the whole sweep.
//!
//! Only lib.sh's chokepoints go through here: context, the holder witnesses, the bead's
//! record, and the verified deletions. Every decision is Rust (sweep.rs).

/// Precedes a seam's machine-readable answer on stdout; everything before it is lib.sh's
/// own output (its `log` lines, salvage notes), passed through to ours.
pub const MARK: char = '\u{1e}';
/// Separates the fields of one record in an answer.
pub const FIELD: char = '\u{1d}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Context,
    Base,
    Witness,
    Bead,
    Send,
    CloseOnLand,
    DestroyWorktree,
    Prune,
    LabelAdd,
}

#[cfg(test)]
pub const ALL: &[Op] = &[
    Op::Context,
    Op::Base,
    Op::Witness,
    Op::Bead,
    Op::Send,
    Op::CloseOnLand,
    Op::DestroyWorktree,
    Op::Prune,
    Op::LabelAdd,
];

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

/// Settings, then one record per repository: `name FIELD has-root FIELD root FIELD queued`.
const CONTEXT: &str = r#"printf '\036'
printf 'run=%s\0' "${SPIRA_RUN:-}"
printf 'reaplog=%s\0' "${SPIRA_REAPLOG:-}"
printf 'submitted=%s\0' "${SPIRA_SUBMITTED_LABEL:-spira-submitted}"
printf 'gh=%s\0' "${SPIRA_GH:-gh}"
printf 'gh_timeout=%s\0' "${GH_TIMEOUT:-120}"
for __n in $(spira_repos); do
    __h=1; __p="$(repo_root "$__n" 2>/dev/null)" || { __h=0; __p=""; }
    __q=0; repo_land_queued "$__n" && __q=1
    printf 'repo=%s\035%s\035%s\035%s\0' "$__n" "$__h" "$__p" "$__q"
done
exit 0
"#;

/// `landref FIELD landrefs FIELD remote` for one checkout; exit 1 when the land ref does not
/// resolve (the repository is skipped, loudly).
const BASE: &str = r#"__lr="$(spira_landref "$1")" || exit 1
__refs="$(spira_landrefs "$1" 2>/dev/null)" || __refs="$__lr"
__rm="$(ref_remote "$__lr" "$1")" || __rm=""
printf '\036%s\035%s\035%s' "$__lr" "$__refs" "$__rm"
exit 0
"#;

/// exit 0 and the reason when somebody may be home; exit 1 when nobody is.
const WITNESS: &str = r#"__why="$(spira_holder_witnesses "$1")" || exit 1
printf '\036%s' "$__why"
exit 0
"#;

/// `bdjson show <id>` verbatim.
const BEAD: &str = r#"printf '\036%s' "$(bdjson show "$1" 2>/dev/null)"
exit 0
"#;

/// send_branch's recheck and the verified deletion, in one process so nothing can claim the
/// bead between the two: exit 10 + why when held; else spira_reap_landed_branch's own
/// status with SPIRA_REAP_ERR as the answer.
const SEND: &str = r#"if __why="$(spira_holder_witnesses "$1")"; then printf '\036%s' "$__why"; exit 10; fi
spira_reap_landed_branch "$1" "$2" "$3" "$4" "$5"; __rc=$?
printf '\036%s' "${SPIRA_REAP_ERR:-}"
exit $__rc
"#;

fn body(op: Op) -> &'static str {
    match op {
        Op::Context => CONTEXT,
        Op::Base => BASE,
        Op::Witness => WITNESS,
        Op::Bead => BEAD,
        Op::Send => SEND,
        Op::CloseOnLand => "bead_close_on_land \"$1\" \"$2\" || true\nexit 0\n",
        Op::DestroyWorktree => "spira_destroy_worktree \"$1\" \"$2\" \"$3\" \"$4\"\nexit $?\n",
        Op::Prune => "spira_prune_worktrees \"$1\"\nexit 0\n",
        Op::LabelAdd => "bdq label add \"$1\" \"$2\" >/dev/null 2>&1 || true\nexit 0\n",
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
