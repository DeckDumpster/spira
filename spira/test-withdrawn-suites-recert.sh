#!/usr/bin/env bash
# test-withdrawn-suites-recert.sh — a bead withdrawn for suite reds re-certifies against
# those suites, not fences-only (sp-hkfdp).
#
# THE DEFECT. Self-certification (queue.sh submit) runs with SPIRA_CERTIFY_SUITES=off —
# fences only, right for a first submission because the Concierge's full-corpus round
# catches suite reds. But a bead the batcher withdrew for a reproduced suite red carried
# no record of which suite, so the aeon's very next resubmission re-ran fences-only and
# rediscovered the same red, over and over, at a full corpus's cost each time.
#
# bead_reopen now writes the suites a withdrawal is known to have reddened to
# $LANDSTATE/<id>.ejected — the sidecar gate.sh already reads unconditionally for the
# EJECTED case (verdict.sh's _attr_eject) — so a WITHDRAWN bead gets the same forcing.
#
# THE GATE COMMAND HERE ONLY SIMULATES test-x.sh: it never runs a real suite, it reports
# whether it was TOLD to via SPIRA_GATE_EJECTED_SUITES. This is what "red first" against
# unpatched lib.sh/queue.sh means for this file: run it before bead_reopen learned to
# write the sidecar and case B fails (gate passes fences-only, oblivious to the red).
#
# tier: T1
# covers: spira/queue.sh spira/lib.sh spira/batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-withdrawn-suites-recert.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; SH="$TMP/spira"
mkdir -p "$RUN/worktree" "$RUN/landstate" "$RUN/queue/fixq" "$SH"
cp "$HERE/queue.sh" "$HERE/gate.sh" "$HERE/gate-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" \
   "$HERE/exclude.sh" "$HERE/skew.sh" "$HERE/yield.sh" "$HERE/suite-covers.sh" \
   "$HERE/gate-sweep.sh" "$SH/"

git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" branch spira/sp-wsx main

# The repo's own gate command: never runs a real suite, only reports what it was handed —
# so every assertion below is about the forcing wiring, not about a suite fixture agreeing
# with itself. Exits 1 (red) exactly when told to force test-x.sh, modelling a suite that
# is genuinely red for this branch.
GATELOG="$TMP/gate-ejected.log"
cat > "$SH/gate-stub.sh" <<STUB
#!/usr/bin/env bash
printf 'ejected=%s\n' "\${SPIRA_GATE_EJECTED_SUITES:-}" >> "$GATELOG"
case "\${SPIRA_GATE_EJECTED_SUITES:-}" in
    *test-x.sh*) exit 1 ;;
esac
exit 0
STUB
chmod +x "$SH/gate-stub.sh"

RMAP="$TMP/repo-map"
printf 'fixq | %s | queue | main |  | %s\n' "$REPO" "$SH/gate-stub.sh" > "$RMAP"

submit() {
    : > "$GATELOG"
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$SH" \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$RUN" \
        SPIRA_DB=/nonexistent \
        SPIRA_HOME_REPO=fixq \
        SPIRA_REPO_MAP="$RMAP" \
        SPIRA_QUEUE_DIR="$RUN/queue" \
        SPIRA_CERTIFY_SUITES=off \
        bash "$SH/queue.sh" submit spira/sp-wsx 2>&1
}

echo
echo "A. positive control — a bead with no withdrawal history certifies fences-only, gate never told about test-x.sh:"
rm -f "$RUN/landstate/sp-wsx" "$RUN/landstate/sp-wsx.ejected" "$RUN/queue/sp-wsx"
out="$(submit)"; rc=$?
[ "$rc" -eq 0 ] && ok "A: exit 0, no withdrawal history" || bad "A: exit 0" "rc=$rc out=$out"
want "A: certified" "certified" "$out"
want "A: the gate command actually ran" "ejected=" "$(cat "$GATELOG")"
nowant "A: gate command was never told to force test-x.sh" "test-x.sh" "$(cat "$GATELOG")"

echo
echo "B. THE DEFECT, seen to fail: a bead withdrawn with suites=test-x.sh must re-certify with test-x.sh forced:"
rm -f "$RUN/landstate/sp-wsx" "$RUN/landstate/sp-wsx.ejected" "$RUN/queue/sp-wsx"
(
    export SPIRA_RUN="$RUN" SPIRA_CONF=/nonexistent SPIRA_DB=/nonexistent SPIRA_BD=/nonexistent
    # shellcheck disable=SC1090
    . "$SH/lib.sh"
    printf 'CERTIFIED %s %s\n' "deadbeef" "$(date +%s)" > "$RUN/landstate/sp-wsx"
    bead_reopen sp-wsx batch-eject "reproduced failure" "test-x.sh" >/dev/null 2>&1
)
st="$(awk '{print $1}' "$RUN/landstate/sp-wsx" 2>/dev/null || true)"
[ "$st" = "WITHDRAWN" ] && ok "B: landstate WITHDRAWN after bead_reopen with suites" \
    || bad "B: landstate WITHDRAWN" "got $st"
[ "$(cat "$RUN/landstate/sp-wsx.ejected" 2>/dev/null)" = "test-x.sh" ] \
    && ok "B: .ejected sidecar carries test-x.sh" \
    || bad "B: .ejected sidecar" "got [$(cat "$RUN/landstate/sp-wsx.ejected" 2>/dev/null)]"

out="$(submit)"; rc=$?
want "B: the gate command was told to force test-x.sh" "ejected=test-x.sh" "$(cat "$GATELOG")"
[ "$rc" -ne 0 ] && ok "B: re-certification RED — the withdrawn suite is caught, not skipped" \
    || bad "B: re-certification RED" "rc=$rc out=$out"
want "B: failure reported to the aeon" "failed the gate" "$out"
[ ! -f "$RUN/queue/sp-wsx" ] && ok "B: not admitted to the queue on a red re-cert" \
    || bad "B: not admitted to the queue" "queue record exists"

echo
tl_summary
