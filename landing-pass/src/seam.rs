//! The lib.sh seam (DESIGN.md §6): `bash` with no arguments, reading a FIXED script from
//! stdin followed by the operation's values, each NUL-terminated. argv is only `bash`; the
//! environment is the caller's own. The script reads every value first, detaches stdin,
//! sources lib.sh, defines `progress` and `act` — which
//! lib.sh functions call and which only the landing pass itself used to define — and calls
//! exactly one function, whose name is part of the fixed text, never data
//! (law-payloads-go-on-stdin).
//!
//! Why this works: bash reading a non-seekable stdin reads it one byte at a time, so the
//! compound command `{ … }` is parsed to its closing brace and executed before any byte of
//! the data after it is consumed; the `read -d ''` loop inside consumes the data, and the
//! `exit` inside the braces stops bash before it could read further.

/// One seam operation. The first value of every call is `$SPIRA_HOME` (lib.sh's directory).
///
/// `ConflictNote`, `OtherBeads`, `PrMerged`, `LandSubject` and `CloseOnLand` are retired
/// (sp-81t4d, "wave 4.17": family R, landed verification) — `conflict_reopen_note`,
/// `other_beads_on_conflicts`, `pr_merged`, `land_subject` and the close-on-land are all
/// native now (`land_verify.rs`), reached through `RealLib` directly, never this seam.
/// `Closeout` and `GhUnlandedScan` are retired too (sp-j3fim, "wave 4.31": family AB,
/// GitHub closeout) — `gh_issue_closeout`/`_gh_unlanded_scan` moved into gh-intake;
/// `RealLib` shells to the compiled `gh-intake closeout`/`unlanded-scan` binary instead.
/// `Note`, the five `Deliver*` and `PruneWorktrees` are retired too (sp-ipwlic): the store's
/// `bd-note` and `deliver` verbs over `LcClient`, and `hel::reap::prune_worktrees`, in-process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Context,
    Reopen,
    Event,
    Incident,
    Rebase,
    Recut,
    BumpRequeue,
    RequeuesOf,
    Push,
    /// `spira_git_push <repo> -q --force-with-lease -u <remote> <branch>` — `land_pr`'s push,
    /// distinct from `Push`'s plain `-q <remote> <refspec>`.
    ForcePush,
}

pub const ALL: &[Op] = &[
    Op::Context,
    Op::Reopen,
    Op::Event,
    Op::Incident,
    Op::Rebase,
    Op::Recut,
    Op::BumpRequeue,
    Op::RequeuesOf,
    Op::Push,
    Op::ForcePush,
];

/// Precedes a seam's machine-readable answer on stdout.
pub const MARK: char = '\u{1e}';
/// Starts a line that is a movement (`progress`), not a log line.
pub const PROGRESS: char = '\u{1f}';
/// Separates the fields of one repository record in the context answer.
pub const FIELD: char = '\u{1d}';

const PRELUDE: &str = r#"{
set -uo pipefail
__q=()
while IFS= read -r -d '' __v; do __q+=("$__v"); done
exec </dev/null
HERE="${__q[0]}"
set -- "${__q[@]:1}"
unset __q __v
. "$HERE/lib.sh" || exit 96
progress() { printf '\037%s\n' "$*"; }
act() { log "$*"; }
"#;

/// Settings only — the repository list itself (family U: `spira_home_repo`/`spira_repos`/
/// `repo_root`/`repo_land`) and the base-ref columns (family W: `spira_landref`/
/// `ref_remote`/`ref_branch`/`qualify_base_ref`/`spira_publish_forge`) are both resolved
/// in-process now by `real.rs`'s `load_context`/`parse_context`, through
/// `spira_config::repos` (family W since sp-o88bx "wave 4.12"; family U since sp-k6lku
/// "wave 4.13") — not by a loop in this script shelling into each of those, every one of
/// them an extra spira-config subprocess, once per function per repository, inside this
/// one already-running bash seam call.
const CONTEXT: &str = r#"__kv() { printf '%s=%s\0' "$1" "$2"; }
printf '\036'
__kv home "${SPIRA_HOME:-$HERE}"
__kv run "${SPIRA_RUN:-}"
__kv repo "${SPIRA_REPO:-}"
__kv bd_timeout "${BD_TIMEOUT:-180}"
__kv id_prefix "${SPIRA_ID_PREFIX:-sp}"
__kv land_maxsec "${SPIRA_LAND_MAXSEC:-3600}"
__kv gate_reserve "${SPIRA_LAND_GATE_RESERVE:-1200}"
__kv gate_timeout "${SPIRA_GATE_TIMEOUT:-2700}"
__kv gate_lock_wait "${SPIRA_GATE_LOCK_WAIT:-}"
__kv certify_par "${SPIRA_CERTIFY_PAR:-}"
__kv gate_worker "${SPIRA_GATE_WORKER:-1}"
__kv verdict_ttl "${SPIRA_VERDICT_TTL:-0}"
__kv verdicts "${SPIRA_VERDICTS:-${SPIRA_RUN:-}/verdicts}"
__kv deferral_at "${SPIRA_DEFERRAL_ESCALATE_AT:-5}"
__kv cutover_label "${SPIRA_CUTOVER_ROUND_LABEL:-cutover-round}"
__kv submitted_label "${SPIRA_SUBMITTED_LABEL:-spira-submitted}"
__kv rebase_escalate_at "${SPIRA_REBASE_ESCALATE_AT:-3}"
__kv git_name "${SPIRA_GIT_NAME:-spira}"
__kv git_email "${SPIRA_GIT_EMAIL:-spira@spira.invalid}"
__kv incident "${SPIRA_INCIDENT:-incident.sh}"
__kv scope_label "${SPIRA_SCOPE_LABEL:-}"
__kv queue_dir "${SPIRA_QUEUE_DIR:-${SPIRA_RUN:-}/queue}"
__kv halt_grace "${SPIRA_HALT_GRACE:-30}"
__kv path "${PATH:-}"
__kv bdjson_fixture "${SPIRA_BDJSON_FIXTURE:-}"
__kv toml "${SPIRA_TOML_FILE:-}"
__kv pr_refresh_max "${SPIRA_PR_REFRESH_MAX:-5}"
__kv batcher_enable "${SPIRA_BATCHER_ENABLE:-1}"
__kv batcher_forge_enable "${SPIRA_BATCHER_FORGE_ENABLE:-1}"
__kv ask_label "${SPIRA_ASK_LABEL:-}"
__kv noverdict_max "${SPIRA_NOVERDICT_MAX:-3}"
__kv noverdict_class_window "${SPIRA_NOVERDICT_CLASS_WINDOW:-86400}"
__kv gate_backoff "${SPIRA_GATE_BACKOFF:-300}"
__kv rebase_decompose_files "${SPIRA_REBASE_DECOMPOSE_FILES:-4}"
__kv rebase_generated_files "${SPIRA_REBASE_GENERATED_FILES:-}"
__kv repo_map_ok "$([ -r "${SPIRA_REPO_MAP:-}" ] && printf 1 || printf 0)"
exit 0
"#;

const INCIDENT: &str = r#"case "$1" in
    */*) [ -r "$1" ] || exit 2; __p="$1" ;;
    *) __p="$(command -v "$1")" || exit 2 ;;
esac
if [ -n "$2" ]; then export SPIRA_INCIDENT_LABELS="$2"; else unset SPIRA_INCIDENT_LABELS; fi
__id="$(SPIRA_INCIDENT_TYPE=bug SPIRA_INCIDENT_PRIORITY=1 SPIRA_INCIDENT_ACTOR=landing \
    SPIRA_INCIDENT_REPO="$3" SPIRA_INCIDENT_REF="$4" \
    SPIRA_INCIDENT_CAUSE=base-suite-red bash "$__p" file "$5" - <<< "$6")" || exit 1
printf '\036%s' "$__id"
exit 0
"#;

fn body(op: Op) -> &'static str {
    match op {
        Op::Context => CONTEXT,
        Op::Reopen => "bead_reopen \"$1\" \"$2\" \"$3\" || true\nexit 0\n",
        Op::Event => "spira_event \"$1\" \"$2\" \"$3\" \"$4\" || true\nexit 0\n",
        Op::Incident => INCIDENT,
        Op::Rebase => "rebase_branch \"$1\" \"$2\" \"$3\" \"$4\"; __rc=$?\nprintf '\\036%s\\035%s\\035%s' \"${REBASE_FAILURE:-}\" \"${REBASE_CONFLICTS:-}\" \"${REBASE_REFUSED_REASON:-}\"\nexit $__rc\n",
        Op::Recut => "recut_onto \"$1\" \"$2\" \"$3\" \"$4\"; __rc=$?\nprintf '\\036%s\\035%s' \"${RECUT_APPLIED_COUNT:-0}\" \"${RECUT_CONFLICTS:-}\"\nexit $__rc\n",
        Op::BumpRequeue => "bump_requeue \"$1\" \"$2\" >/dev/null 2>&1\nexit 0\n",
        Op::RequeuesOf => "printf '\\036%s' \"$(requeues_of \"$1\")\"\nexit 0\n",
        Op::Push => "__e=\"$(spira_git_push \"$1\" -q \"$2\" \"$3\" 2>&1 >/dev/null)\"; __rc=$?\nprintf '\\036%s' \"$__e\"\nexit $__rc\n",
        Op::ForcePush => "__e=\"$(spira_git_push \"$1\" -q --force-with-lease -u \"$2\" \"$3\" 2>&1 >/dev/null)\"; __rc=$?\nprintf '\\036%s' \"$__e\"\nexit $__rc\n",
    }
}

/// The complete fixed script for `op`.
pub fn script(op: Op) -> String {
    format!("{PRELUDE}{}}}\n", body(op))
}

/// The bytes written to the seam's stdin: the script, then every value NUL-terminated.
/// A value may not itself contain NUL; one that does is cut at it (it could not travel).
pub fn stdin_bytes(op: Op, values: &[&str]) -> Vec<u8> {
    let mut v = script(op).into_bytes();
    for val in values {
        let cut = val.split('\0').next().unwrap_or("");
        v.extend_from_slice(cut.as_bytes());
        v.push(0);
    }
    v
}

/// What one seam call printed: the lines before the answer (lib.sh log lines and
/// `progress` movements) and the answer after the last MARK.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Printed {
    pub logs: Vec<String>,
    pub progress: Vec<String>,
    pub answer: String,
}

pub fn split(stdout: &str) -> Printed {
    let (before, answer) = match stdout.rfind(MARK) {
        Some(i) => (&stdout[..i], &stdout[i + MARK.len_utf8()..]),
        None => (stdout, ""),
    };
    let mut p = Printed { answer: answer.to_string(), ..Default::default() };
    for line in before.split('\n') {
        if line.is_empty() {
            continue;
        }
        match line.strip_prefix(PROGRESS) {
            Some(m) => p.progress.push(m.to_string()),
            None => p.logs.push(line.to_string()),
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;



    #[test]
    fn every_script_is_one_braced_command_with_no_nul() {
        for op in ALL {
            let s = script(*op);
            assert!(s.starts_with("{\n") && s.ends_with("}\n"), "{op:?}");
            assert!(!s.contains('\0'), "{op:?}");
            assert!(s.contains("exit"), "{op:?} must exit inside the braces");
        }
    }
}
