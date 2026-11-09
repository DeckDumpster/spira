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
    BeadReopen,
    CauseEvent,
    ReleaseClaim,
    Comment,
    Event,
    Rebase,
    LandSubject,
    FormatBatch,
    BaseConflict,
    PfGate,
    CreateBug,
    AmendBug,
    BeadLive,
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

fn body(op: Op) -> &'static str {
    match op {
        // spira_landref dropped (sp-o88bx, "wave 4.12") and repo_land with it (sp-k6lku,
        // "wave 4.13"): readback() resolves both in-process through spira_config::repos
        // now, so there is no Readback op left to call here.
        Op::BeadReopen => "bead_reopen \"$1\" \"$2\" \"\" \"$3\"\nexit $?\n",
        Op::CauseEvent => "_bump_write_event \"$1\" reopen \"$2\"\nexit $?\n",
        Op::ReleaseClaim => "release_claim \"$1\"\nexit $?\n",
        Op::Comment => "printf '%s' \"$2\" | bdq comment \"$1\" --stdin >/dev/null 2>&1 || true\nexit 0\n",
        Op::Event => "spira_event \"$1\" - \"$2\" \"$3\" || true\nexit 0\n",
        Op::Rebase => "rebase_branch \"$1\" \"$2\" \"$3\" \"$4\" 2>/dev/null; __rc=$?\nprintf '\\036%s' \"${REBASE_FAILURE:-}\"\nexit $__rc\n",
        Op::LandSubject => "printf '\\036%s' \"$(land_subject \"$1\")\"\nexit 0\n",
        // FormatBatch/BaseConflict/PfGate used to source batch.sh for these bodies (`format_batch`,
        // `_base_conflict`, `_pf_gate`/`_pf_run`) — inlined here, batch.sh deleted, sp-uwhx0. Bodies
        // are otherwise unchanged from batch.sh's own (same reviewed shape), except PfGate: batch.sh
        // shared one `_PF_DEADLINE` wall across a sequence of steps (gate, attribution, re-gate) that
        // no longer exists here — open-batch calls pf_gate exactly once, so the wall is just `$4`
        // (`wall_secs`) with no shared clock to thread through.
        Op::FormatBatch => concat!(
            "__wt=\"$1\" __base=\"$2\" __name=\"$3\"\n",
            "__cmd=\"$(repo_format \"$__name\" 2>/dev/null)\"\n",
            "if [ -z \"$__cmd\" ]; then exit 0; fi\n",
            "if ! ( cd \"$__wt\" && env -i PATH=\"$HOME/.cargo/bin:$PATH\" HOME=\"$HOME\" TERM=dumb \\\n",
            "         timeout \"${SPIRA_FORMAT_TIMEOUT:-300}\" bash -c \"$__cmd\" ) >/dev/null 2>&1; then\n",
            "    log \"format: $__name's formatter failed on batch — leaving it unformatted\"\n",
            "    git -C \"$__wt\" checkout -q -- . 2>/dev/null\n",
            "    exit 0\n",
            "fi\n",
            "__paths=()\n",
            "while IFS= read -r -d '' __f; do\n",
            "    [ -f \"$__wt/$__f\" ] && __paths+=(\"$__f\")\n",
            "done < <(git -C \"$__wt\" diff -z --name-only \"$__base\" HEAD 2>/dev/null)\n",
            "[ \"${#__paths[@]}\" -gt 0 ] && git -C \"$__wt\" add -- \"${__paths[@]}\" 2>/dev/null\n",
            "git -C \"$__wt\" checkout -q -- . 2>/dev/null\n",
            "__staged=0\n",
            "git -C \"$__wt\" diff --cached --quiet 2>/dev/null || __staged=1\n",
            "[ \"$__staged\" = 1 ] || exit 0\n",
            "git -C \"$__wt\" -c \"user.name=${SPIRA_GIT_NAME:-spira}\" -c \"user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}\" commit -q -F - <<EOF 2>/dev/null\n",
            "spira: format batch\n",
            "\n",
            "Batch assembly merges branches without re-running $__name's formatter, so the\n",
            "assembled tree may fail the required layout check. Formatted with: $__cmd\n",
            "EOF\n",
            "log \"format: formatted $__name batch\"\n",
            "exit 0\n",
        ),
        Op::BaseConflict => concat!(
            "__repo=\"$1\" __base=\"$2\" __tip=\"$3\" __rc=1\n",
            "__wt=\"$SPIRA_RUN/worktree/.batch-ck-$$\"\n",
            "git -C \"$__repo\" worktree add -q --detach \"$__wt\" \"$__base\" 2>/dev/null || exit 2\n",
            "git -C \"$__wt\" merge --no-commit --no-ff \"$__tip\" >/dev/null 2>&1 || __rc=0\n",
            "git -C \"$__wt\" merge --abort 2>/dev/null || true\n",
            "git -C \"$__repo\" worktree remove -f \"$__wt\" 2>/dev/null || true\n",
            "exit $__rc\n",
        ),
        Op::PfGate => concat!(
            "__br=\"$1\" __name=\"$2\" __stamp=\"$3\" __secs=\"$4\"\n",
            "__o=\"$(\n",
            "    if ! [ \"$__secs\" -gt 0 ] 2>/dev/null; then exit 124; fi\n",
            "    __flag=\"$(mktemp)\"; rm -f \"$__flag\"\n",
            "    SPIRA_GATE_BEAD=\"batch-$__stamp\" setsid gate.sh \"$__br\" \"$__name\" 2>&1 &\n",
            "    __pid=$!\n",
            "    ( sleep \"$__secs\" && { : > \"$__flag\"; kill -TERM -\"$__pid\" 2>/dev/null; sleep 15 && kill -KILL -\"$__pid\" 2>/dev/null; } ) \\\n",
            "        </dev/null >/dev/null 2>&1 &\n",
            "    __dog=$!\n",
            "    wait \"$__pid\"; __rc=$?\n",
            "    pkill -P \"$__dog\" 2>/dev/null; kill \"$__dog\" 2>/dev/null; wait \"$__dog\" 2>/dev/null\n",
            "    if [ -e \"$__flag\" ]; then rm -f \"$__flag\"; exit 124; fi\n",
            "    exit \"$__rc\"\n",
            ")\"\n",
            "__rc=$?\n",
            "printf '\\036%s' \"$__o\"\n",
            "exit $__rc\n",
        ),
        // The body travels as a value and lands in a temp file inside the script: bd reads
        // it with --body-file, never argv (law-payloads-go-on-stdin).
        Op::CreateBug => "__f=\"$(mktemp)\" || exit 1\nprintf '%s' \"$5\" > \"$__f\"\n__id=\"$(BEADS_ACTOR=\"$1\" bdq create \"$2\" --type bug --priority \"$3\" --labels \"$4\" --body-file \"$__f\" --silent 2>/dev/null | tr -d '[:space:]')\"\nrm -f \"$__f\"\nprintf '\\036%s' \"$__id\"\nexit 0\n",
        Op::AmendBug => r#"__r="$(lc_bead_row "$2" 2>/dev/null)" || { printf '\036'; exit 0; }
case "${__r%%$'\t'*}" in ""|LANDED|SUPERSEDED|DROPPED|DONE) printf '\036'; exit 0 ;; esac
__f="$(mktemp)" || exit 1
printf '%s' "$3" > "$__f"
if BEADS_ACTOR="$1" bdq note "$2" --file "$__f" >/dev/null 2>&1; then printf '\036amended'; else printf '\036'; fi
rm -f "$__f"
exit 0
"#,
        Op::BeadLive => r#"__r="$(lc_bead_row "$1" 2>/dev/null)" || { printf '\036'; exit 0; }
case "${__r%%$'\t'*}" in ""|LANDED|SUPERSEDED|DROPPED|DONE) printf '\036' ;; *) printf '\036live' ;; esac
exit 0
"#,
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

/// Parse a `key=value\0` answer.
pub fn parse_kv0(answer: &str) -> std::collections::BTreeMap<String, String> {
    answer
        .split('\0')
        .filter_map(|r| {
            r.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_script_is_one_braced_command_with_no_nul() {
        for op in [
            Op::BeadReopen,
            Op::CauseEvent,
            Op::ReleaseClaim,
            Op::Comment,
            Op::Event,
            Op::Rebase,
            Op::LandSubject,
            Op::FormatBatch,
            Op::BaseConflict,
            Op::PfGate,
            Op::CreateBug,
            Op::AmendBug,
            Op::BeadLive,
        ] {
            let s = script(op);
            assert!(s.starts_with("{\n") && s.ends_with("}\n"), "{op:?}");
            assert!(!s.contains('\0'), "{op:?}");
            assert!(s.contains("exit"), "{op:?} must exit inside the braces");
        }
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
