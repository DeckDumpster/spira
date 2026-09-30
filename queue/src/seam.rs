//! The lib.sh seam (DESIGN.md §6): `bash` reading a FIXED script from stdin, followed by
//! the operation's values, each NUL-terminated. argv is only `bash`; the environment is the
//! caller's own. The script reads every value first, detaches stdin, sources lib.sh
//! (and batch.sh where named) and calls exactly one function, whose name is
//! part of the fixed text, never data (law-payloads-go-on-stdin).
//!
//! Why this works: bash reading a non-seekable stdin reads it one byte at a time, so the
//! compound command `{ … }` is parsed to its closing brace and executed before any byte of
//! the data after it is consumed; the `read -d ''` loop inside then consumes the data, and
//! the `exit` inside the braces stops bash before it could read further.

/// One seam operation. The first value of every call is `$SPIRA_HOME` (lib.sh's directory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Context,
    Repos,
    TomlPath,
    Readback,
    LandMark,
    BeadReopen,
    CauseEvent,
    ReleaseClaim,
    CloseOnLand,
    GhCloseout,
    Comment,
    Notify,
    Event,
    Divergence,
    Push,
    Rebase,
    LandSubject,
    SortRows,
    CancelRuns,
    FormatBatch,
    BaseConflict,
    PfGate,
    CreateBug,
}

/// The record separator that precedes a seam's machine-readable answer on stdout, so any
/// lib.sh log line printed before it is never mistaken for the answer.
pub const MARK: char = '\u{1e}';

const PRELUDE: &str = r#"{
set -uo pipefail
__q=()
while IFS= read -r -d '' __v; do __q+=("$__v"); done
exec </dev/null
HERE="${__q[0]}"
set -- "${__q[@]:1}"
unset __q __v
. "$HERE/lib.sh" || exit 96
"#;

const CONTEXT: &str = r#"__n="${1:-}"; [ -n "$__n" ] || __n="$(spira_home_repo)"
__p="$(repo_root "$__n" 2>/dev/null)"; __pok=$?
__lr=""; [ "$__pok" -eq 0 ] && __lr="$(spira_landref "$__p" 2>/dev/null)"
__pf="$(spira_publish_forge "$__n" 2>/dev/null)"
__rem=""; [ "$__pok" -eq 0 ] && __rem="$(git -C "$__p" remote 2>/dev/null | tr '\n' ' ')"
__kv() { printf '%s=%s\0' "$1" "$2"; }
printf '\036'
__kv home "${SPIRA_HOME:-$HERE}"
__kv run "${SPIRA_RUN:-}"
__kv queue_dir "${SPIRA_QUEUE_DIR:-${SPIRA_RUN:-}/queue}"
__kv landstate "${LANDSTATE:-${SPIRA_RUN:-}/landstate}"
__kv releases "${SPIRA_RELEASES:-}"
__kv forge "${SPIRA_FORGE:-forge.sh}"
__kv repo_map "${SPIRA_REPO_MAP:-}"
__kv batcher_enable "${SPIRA_BATCHER_ENABLE:-1}"
__kv submitted_label "${SPIRA_SUBMITTED_LABEL:-spira-submitted}"
__kv pollsec "${SPIRA_QUEUE_TRANSITION_POLLSEC:-5}"
__kv maxsec "${SPIRA_QUEUE_TRANSITION_MAXSEC:-1800}"
__kv preflight "${SPIRA_PREFLIGHT_WALL_SECS:-240}"
__kv db "${SPIRA_DB:-}"
__kv bd "${SPIRA_BD:-bd}"
__kv home_repo "$(spira_home_repo)"
__kv name "$__n"
__kv path_ok "$__pok"
__kv path "$__p"
__kv mode "$(repo_land "$__n")"
__kv map_land "$(repo_field "$__n" land 2>/dev/null)"
__kv map_base "$(repo_field "$__n" base 2>/dev/null)"
__kv landref "$__lr"
__kv publish "$__pf"
__kv remotes "$__rem"
__K="$(printf '%s' "$__n" | tr 'a-z-' 'A-Z_')"
case "$__K" in *[!A-Z0-9_]*) __K="" ;; esac
__v="SPIRA_QUEUE_CI_MAXSEC_$__K"; __kv ci_maxsec "${!__v:-${SPIRA_QUEUE_CI_MAXSEC:-3600}}"
__v="SPIRA_QUEUE_CI_IDLE_SEC_$__K"; __kv ci_idle "${!__v:-${SPIRA_QUEUE_CI_IDLE_SEC:-600}}"
__kv infra_retries "${SPIRA_QUEUE_INFRA_RETRIES:-2}"
__kv lock_wait "${SPIRA_QUEUE_LOCK_WAIT:-90}"
__kv starve_max "${SPIRA_QUEUE_LOCK_STARVE_MAX:-5}"
__kv incident_priority "${SPIRA_INCIDENT_PRIORITY:-1}"
exit 0
"#;

fn body(op: Op) -> &'static str {
    match op {
        Op::Context => CONTEXT,
        Op::Repos => "printf '\\036'\nspira_repos | while IFS= read -r __r; do [ -n \"$__r\" ] && printf '%s\\0' \"$__r\"; done\nexit \"${PIPESTATUS[0]}\"\n",
        Op::TomlPath => "printf '\\036%s' \"$(spira_toml_resolve 2>/dev/null)\"\nexit 0\n",
        Op::Readback => "printf '\\036%s\\0%s\\0' \"$(repo_land \"$1\")\" \"$(spira_landref \"$1\" 2>/dev/null)\"\nexit 0\n",
        Op::LandMark => "land_mark \"$1\" \"$2\" \"$3\" \"$4\"\nexit $?\n",
        Op::BeadReopen => "bead_reopen \"$1\" \"$2\" \"\" \"$3\"\nexit $?\n",
        Op::CauseEvent => "_bump_write_event \"$1\" reopen \"$2\"\nexit $?\n",
        Op::ReleaseClaim => "release_claim \"$1\"\nexit $?\n",
        Op::CloseOnLand => "bead_close_on_land \"$1\" \"$2\" || true\nexit 0\n",
        Op::GhCloseout => "gh_issue_closeout \"$1\" \"$2\" \"$3\" || true\nexit 0\n",
        Op::Comment => "printf '%s' \"$2\" | bdq comment \"$1\" --stdin >/dev/null 2>&1 || true\nexit 0\n",
        Op::Notify => "queue_notify_concierge \"$1\" \"$2\" \"$3\"\nexit 0\n",
        Op::Event => "spira_event \"$1\" - \"$2\" \"$3\" || true\nexit 0\n",
        Op::Divergence => "queue_local_check_divergence \"$1\" \"$2\" \"$3\" \"$4\" >/dev/null 2>&1\nexit $?\n",
        Op::Push => "spira_git_push \"$1\" -q \"$2\" \"$3\" 2>/dev/null\nexit $?\n",
        Op::Rebase => "rebase_branch \"$1\" \"$2\" \"$3\" \"$4\" 2>/dev/null; __rc=$?\nprintf '\\036%s' \"${REBASE_FAILURE:-}\"\nexit $__rc\n",
        Op::LandSubject => "printf '\\036%s' \"$(land_subject \"$1\")\"\nexit 0\n",
        Op::SortRows => "PRIO_JSON=\"$3\"\nprintf '\\036'\nprintf '%s' \"$4\" | queue_sort_rows \"$1\" \"$2\" | awk '{print $5, $6}'\nexit 0\n",
        Op::CancelRuns => "queue_cancel_branch_runs \"$1\" \"$2\" \"$3\" QUEUE || true\nexit 0\n",
        Op::FormatBatch => ". \"$HERE/batch.sh\" || exit 96\nformat_batch \"$1\" \"$2\" \"$3\"\nexit 0\n",
        Op::BaseConflict => ". \"$HERE/batch.sh\" || exit 96\n_base_conflict \"$1\" \"$2\" \"$3\"\nexit $?\n",
        Op::PfGate => ". \"$HERE/batch.sh\" || exit 96\n_PF_DEADLINE=$(( $(date +%s) + $4 ))\n__o=\"$(_pf_gate \"$1\" \"$2\" \"$3\")\"; __rc=$?\nprintf '\\036%s' \"$__o\"\nexit $__rc\n",
        // The body travels as a value and lands in a temp file inside the script: bd reads
        // it with --body-file, never argv (law-payloads-go-on-stdin).
        Op::CreateBug => "__f=\"$(mktemp)\" || exit 1\nprintf '%s' \"$5\" > \"$__f\"\n__id=\"$(BEADS_ACTOR=\"$1\" bdq create \"$2\" --type bug --priority \"$3\" --labels \"$4\" --body-file \"$__f\" --silent 2>/dev/null | tr -d '[:space:]')\"\nrm -f \"$__f\"\nprintf '\\036%s' \"$__id\"\nexit 0\n",
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
        v.extend_from_slice(val.as_bytes());
        v.push(0);
    }
    v
}

/// The answer after the last [`MARK`] on stdout, and everything printed before it (lib.sh
/// log lines, passed through to queue's own stdout).
pub fn split_answer(stdout: &str) -> (&str, &str) {
    match stdout.rfind(MARK) {
        Some(i) => (&stdout[..i], &stdout[i + MARK.len_utf8()..]),
        None => (stdout, ""),
    }
}

/// Parse a `name\0` list (the Repos answer): empty names dropped, first occurrence kept.
pub fn parse_names0(answer: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in answer.split('\0').map(str::trim).filter(|n| !n.is_empty()) {
        if !out.iter().any(|o| o == n) {
            out.push(n.to_string());
        }
    }
    out
}

/// Parse the Context answer: `key=value\0` records.
pub fn parse_kv0(answer: &str) -> std::collections::BTreeMap<String, String> {
    answer
        .split('\0')
        .filter_map(|r| r.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[test]
    fn every_script_is_one_braced_command_with_no_nul() {
        for op in [
            Op::Context, Op::TomlPath, Op::Readback, Op::LandMark, Op::BeadReopen, Op::CauseEvent, Op::ReleaseClaim,
            Op::CloseOnLand, Op::GhCloseout, Op::Comment, Op::Notify, Op::Event, Op::Divergence, Op::Push, Op::Rebase,
            Op::LandSubject, Op::SortRows, Op::CancelRuns, Op::FormatBatch, Op::BaseConflict, Op::PfGate,
            Op::CreateBug,
        ] {
            let s = script(op);
            assert!(s.starts_with("{\n") && s.ends_with("}\n"), "{op:?}");
            assert!(!s.contains('\0'), "{op:?}");
            assert!(s.contains("exit"), "{op:?} must exit inside the braces");
        }
    }

    #[test]
    fn values_travel_on_stdin_with_newlines_and_empties_intact() {
        let _serial = crate::testutil::serial();
        // The seam's own mechanism, against a stand-in lib.sh that defines land_mark as
        // "print my arguments": proves argv is only `bash` and every value arrives whole.
        let dir = crate::testutil::tmpdir("seam");
        std::fs::write(dir.join("lib.sh"), "land_mark() { printf '[%s]' \"$@\"; }\n").unwrap();
        let home = dir.to_str().unwrap();
        let mut child = Command::new("bash").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&stdin_bytes(Op::LandMark, &[home, "sp-a", "RED", "", "line one\nline $(two) `x`"])).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "[sp-a][RED][][line one\nline $(two) `x`]");
    }

    #[test]
    fn answer_follows_the_last_mark() {
        let (logs, ans) = split_answer("2026 spira: log\n\u{1e}spira: land sp-a");
        assert_eq!(logs, "2026 spira: log\n");
        assert_eq!(ans, "spira: land sp-a");
        let kv = parse_kv0("a=1\0b=x=y\0");
        assert_eq!(kv["b"], "x=y");
    }
}
