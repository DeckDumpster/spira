//! The lib.sh seams this crate still needs, in the one shape every Rust crate in this
//! workspace already uses for it (sentinel/src/seams.rs, DESIGN.md §6): a FIXED script run
//! as `bash -c '<script>' <name>`, parameters arriving only through the environment
//! (never interpolated into the script text — law-payloads-go-on-stdin), stdout the only
//! channel back.
//!
//! lib.sh itself is explicitly out of this wave's scope (`remaining-bash-inventory.md`
//! group 4: "Ryan's standing instruction during the cutover was 'leave lib.sh alone.'");
//! its two chokepoints below are called, never re-implemented:
//!
//!   - `spira_lane_fayths` + `fayth_get`: a `.fayth` file is SOURCED as bash to read one
//!     field (aeons.sh's own `lane_caps`), and a fayth file may compute that field from
//!     other shell variables (groomer.fayth's `FAYTH_LABELS` does exactly this for a
//!     different field) — a hand-written Rust parser would silently disagree on the one
//!     fayth that does, which is the exact failure `aeons.sh` exists to prevent
//!     (law-bake-rules-into-tools: a program that refuses to be misread).
//!   - `bdq`/`bdjson`, `salvage`, `spira_destroy_worktree`/`_branch`, `spira_landref`,
//!     `content_landed`, `bead_repo`, `repo_root` (slay.sh steps 0, 4a, 5, 4b, 6): the one
//!     permitted door onto `git worktree remove`/`git branch -D` in the harness, and `bdq`
//!     carries its own fencing (repo-label validation, the destructive-vocabulary and
//!     schema-delete refusals, a dead-Dolt-connection retry) that a direct `bd` call would
//!     silently drop.

use std::path::Path;
use std::process::{Command, Stdio};

/// Run `<script>` as `bash -c '<script>' <name>`, with `SPIRA_HOME_LIB` pointing at
/// `lib.sh` (the seam's own prelude sources it) and every `(k, v)` in `envs` set besides
/// it. `stdin` is written and closed before the output is read. Returns
/// `(stdout, success)`; stderr is inherited so lib.sh's own chatter is visible, exactly as
/// sentinel's seams do.
pub fn run(lib_sh: &Path, name: &str, script: &str, envs: &[(&str, &str)], stdin: &str) -> (String, bool) {
    let full = format!("set -uo pipefail\n. \"$SPIRA_HOME_LIB\" >&2 || exit 97\n{script}\n");
    let mut cmd = Command::new("bash");
    cmd.args(["-c", &full, name]);
    cmd.env("SPIRA_HOME_LIB", lib_sh);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return (String::new(), false),
    };
    {
        use std::io::Write;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(stdin.as_bytes());
        }
    }
    match child.wait_with_output() {
        Ok(o) => (String::from_utf8_lossy(&o.stdout).into_owned(), o.status.success()),
        Err(_) => (String::new(), false),
    }
}

/// `aeons.sh status`'s lane report: every lane fayth's name and `FAYTH_MAX_CONCURRENT`,
/// NUL-separated `name\tmax_concurrent` records — the same two `fayth_get` reads
/// `lane_caps()` made, in one process instead of two lib.sh sourcings.
pub const LANE_CAPS: &str = r#"
for f in $(spira_lane_fayths); do
    printf '%s\t%s\0' "$(fayth_get "$f" FAYTH_NAME "$f")" "$(fayth_get "$f" FAYTH_MAX_CONCURRENT 1)"
done
"#;

/// Parse [`LANE_CAPS`]'s output into `(name, max_concurrent)` pairs. A field that fails to
/// parse as a number is skipped, matching bash's own arithmetic-context coercion of a bad
/// value to 0 rather than crashing the whole report.
pub fn parse_lane_caps(out: &str) -> Vec<(String, u32)> {
    out.split('\0')
        .filter(|r| !r.is_empty())
        .filter_map(|rec| {
            let (name, n) = rec.split_once('\t')?;
            Some((name.to_string(), n.trim().parse().unwrap_or(0)))
        })
        .collect()
}

/// slay.sh step 0: refuse unless the store answers with EXACTLY `$SLAY_ID` — never a
/// prefix-matched orphan child (`bd show sp-ofeb` resolving `sp-ofeb.1`), and never just a
/// nonzero exit check.
pub const SLAY_EXISTS: &str = r#"
bdq show "$SLAY_ID" --json 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    d = d[0] if isinstance(d, list) else d
    print(d.get("id",""))
except Exception:
    print("")'
"#;

/// slay.sh steps 4a (release the claim before anything is destroyed), 5 (salvage, park or
/// delete the work), 4b (the note) and the final `bdjson show` — everything downstream of
/// "the aeon/hold is stopped" that still has to go through lib.sh's own chokepoints.
/// Reads `$SLAY_ID $SLAY_WHY $SLAY_MODE $SLAY_REASON $SLAY_KEEP $SLAY_NAME $SLAY_PID
/// $SLAY_UNIT` from the environment (never interpolated into this script's text) and
/// prints slay.sh's own narration lines verbatim, ending with one machine-readable line
/// this crate strips before relaying the rest: `___SLAY_RESULT___\t<0|1>`.
pub const SLAY_FINISH: &str = r#"
ID="$SLAY_ID"; WHY="$SLAY_WHY"; MODE="$SLAY_MODE"; REASON="$SLAY_REASON"
KEEP="$SLAY_KEEP"; name="$SLAY_NAME"; pid="$SLAY_PID"; unit="$SLAY_UNIT"
fail=0
say() { printf '%s\n' "$*"; }

status_of() { bdjson show "$ID" | python3 -c 'import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("status","") if d else "")' 2>/dev/null; }
st="$(status_of)"
if [ "$st" = in_progress ]; then
    spira-lc holder-dead "$ID" slay >/dev/null 2>&1 || true
fi
bdq update "$ID" --assignee "" --force >/dev/null 2>&1 || bdq update "$ID" --assignee "" >/dev/null 2>&1

repo_name="$(bead_repo "$ID" 2>/dev/null)"; repo=""
[ -n "$repo_name" ] && repo="$(repo_root "$repo_name" 2>/dev/null)"
br="spira/$ID"; wt="$SPIRA_RUN/worktree/$ID"; tip=""; nuked=""; saved=""; WIP_COMMITTED=0
if [ -n "$repo" ] && git -C "$repo" show-ref --verify -q "refs/heads/$br"; then
    tip="$(git -C "$repo" rev-parse --short "$br")"
fi
if [ "$KEEP" = 1 ]; then
    say "work: kept — branch $br${tip:+ at $tip}, worktree $wt"
elif [ -n "$repo" ]; then
    if [ -d "$wt" ]; then
        git -C "$wt" rebase --abort >/dev/null 2>&1 || true
        git -C "$wt" merge --abort  >/dev/null 2>&1 || true
        if salvage "$ID" "$wt"; then
            saved="${SALVAGED:-}"
            if [ -n "$saved" ]; then
                say "work: uncommitted changes salvaged to $saved"
                if git -C "$wt" add -A >/dev/null 2>&1 \
                    && SPIRA_ALLOW_DIRTY_STAGE=1 \
                       git -C "$wt" -c user.name=slay -c user.email=slay@spira.local -c commit.gpgsign=false \
                           commit -m "$ID: wip — salvaged at slay ($WHY)" >/dev/null 2>&1; then
                    WIP_COMMITTED=1
                    tip="$(git -C "$repo" rev-parse --short "$br" 2>/dev/null || echo "${tip:-?}")"
                    say "work: wip committed to $br at $tip — branch stays in refs/heads"
                else
                    say "work: could not make wip commit — patch is the only copy"
                fi
            fi
        else
            say "work: could not salvage $wt — leaving it in place"; fail=1
        fi
        if [ "$fail" = 0 ]; then
            if spira_destroy_worktree "$ID" "$wt" "$repo" "slain: $WHY"; then
                say "work: worktree $wt removed"
            else
                say "work: could not remove $wt — see $SPIRA_RUN/reap.log"; fail=1
            fi
        fi
    fi
    parked=""
    if [ -n "$tip" ] && [ "$fail" = 0 ] && [ "$WIP_COMMITTED" = 0 ]; then
        if base="$(spira_landref "$repo" 2>/dev/null)"; then
            ahead="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo 0)"
            if [ "${ahead:-0}" -gt 0 ] && ! content_landed "$repo" "$br" "$base" 2>/dev/null; then
                if git -C "$repo" update-ref "refs/slain/$ID" "$br" 2>/dev/null; then
                    parked="refs/slain/$ID"
                    say "work: $br carries work $base does not — parked at $parked"
                else
                    say "work: could not park $br at refs/slain/$ID — REFUSING to delete it"
                    fail=1
                fi
            fi
        fi
    fi
    if [ -n "$tip" ] && [ "$fail" = 0 ] && [ "$WIP_COMMITTED" = 0 ]; then
        if spira_destroy_branch "$ID" "$br" "$repo" "slain: $WHY" slain; then
            nuked="branch $br deleted at $tip${parked:+, kept at $parked}"; say "work: $nuked"
        else
            say "work: could not delete $br${SPIRA_DESTROY_ERR:+ — $SPIRA_DESTROY_ERR}"; fail=1
        fi
    fi
else
    say "work: bead names no resolvable repository — nothing to remove"
fi

if [ -n "$nuked" ]; then bdq label remove "$ID" "branch:$br" >/dev/null 2>&1 || true; fi
_work_msg="${nuked:-work kept}${saved:+; uncommitted changes salvaged to $saved}"
[ "$WIP_COMMITTED" = 1 ] && _work_msg="wip committed to $br; branch kept in refs/heads; patch at $saved"
note="Slain by the operator: $WHY. Aeon ${name:-?}${pid:+ (pid $pid)} stopped${unit:+ via $unit}. ${_work_msg}. No attempt charged."
case "$MODE" in
    close)  spira-lc drop "$ID" "$REASON — $note" slay >/dev/null 2>&1 || true
            bdq note "$ID" "$REASON — $note" >/dev/null 2>&1 || true ;;
    reopen) bdq note "$ID" "$note" >/dev/null 2>&1 || true ;;
esac
bdjson show "$ID" | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; b=d[0] if d else {}
print("bead: %s status=%s assignee=%s labels=%s" % (b.get("id"), b.get("status"), b.get("assignee") or "-", ",".join(b.get("labels") or [])))' 2>/dev/null

[ "$KEEP" = 1 ] || [ "$WIP_COMMITTED" = 1 ] || [ -z "$repo" ] || ! git -C "$repo" show-ref --verify -q "refs/heads/$br" || { say "verify: $br still exists"; fail=1; }
printf '___SLAY_RESULT___\t%s\n' "$fail"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lane_caps_reads_nul_separated_records() {
        let out = "groomer\t1\0ops\t3\0";
        assert_eq!(parse_lane_caps(out), vec![("groomer".to_string(), 1), ("ops".to_string(), 3)]);
    }

    #[test]
    fn parse_lane_caps_defaults_bad_number_to_zero() {
        let out = "x\tnotanumber\0";
        assert_eq!(parse_lane_caps(out), vec![("x".to_string(), 0)]);
    }

    #[test]
    fn parse_lane_caps_of_empty_output_is_empty() {
        assert!(parse_lane_caps("").is_empty());
    }

    #[test]
    fn run_against_a_stub_lib_reaches_the_function_with_env_not_argv() {
        let d = testkit::TempDir::new("seam-run");
        std::fs::write(d.join("lib.sh"), "spira_lane_fayths() { printf 'groomer\\n'; }\nfayth_get() { case \"$2\" in FAYTH_NAME) printf 'groomer';; *) printf '2';; esac; }\n").unwrap();
        let (out, ok) = run(&d.join("lib.sh"), "test-lane-caps", LANE_CAPS, &[], "");
        assert!(ok, "{out}");
        assert_eq!(parse_lane_caps(&out), vec![("groomer".to_string(), 2)]);
    }

    #[test]
    fn run_reports_failure_when_lib_sh_is_missing() {
        let d = testkit::TempDir::new("seam-run-missing");
        let (_out, ok) = run(&d.join("no-such-lib.sh"), "test", "true", &[], "");
        assert!(!ok);
    }
}
