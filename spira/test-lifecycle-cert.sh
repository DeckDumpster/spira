#!/usr/bin/env bash
#
# test-lifecycle-cert.sh — container-tier acceptance for certification onto events
# (sp-vd9dn): spira-lc certify/resubmit (lifecycle-cert.sh until sp-arpjt) and gate.sh's verdict() against a real, disposable
# control store.
#
# WHAT THIS PROVES:
#   - a GatePass whose tip does not match the row's own is refused — the mechanism the tip
#     invariant leans on (bead.rs's own TipMismatch, exercised through the shell wrapper).
#   - THE TIP INVARIANT ITSELF: lc_resubmit on a CERTIFIED row with a moved tip — a harness
#     rebase — sends the bead back to SUBMITTED with its gate key cleared.
#   - GateRed and GateInfra land on REWORK and (still) SUBMITTED respectively, and GateInfra
#     still advances the row's version (a recorded retry, not a no-op).
#   - a row the machine still shows WORKING is auto-submitted at the given tip before the
#     verdict is applied, so a caller never has to sequence Submit and GatePass itself.
#   - gate.sh's own verdict() calls through for real: a PASS certifies the row at the
#     branch's exact commit with a real, non-empty gate key; a FAIL sends it to REWORK.
#
# defect: sp-vd9dn
# tier: T2
# covers: spira-lc/src/callers.rs spira/gate.sh gate/src/engine.rs lifecycle/src/bead.rs
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"

. "$HERE/testlib/lc-fixture.sh"
. "$HERE/conf.sh"
TMP="$(mktemp -d)"
trap 'lcfix_down; rm -rf "$TMP"' EXIT INT TERM
REPO_ROOT="$(cd "$HERE/.." && pwd)"

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk).
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
lcfix_up || bail "the control store did not come up"

# seed_bead <id> <state> <tip|-> <gate_key|->
seed_bead() {
    local id="$1" state="$2" tip="$3" gk="$4"
    [ "$tip" = "-" ] && tip=""
    [ "$gk" = "-" ] && gk=""
    lcfix_seed "$id" "$state" ${tip:+"$tip"} ${gk:+"$gk"}
}
row_field() { lcfix_field "$1" "$2"; }

echo "test-lifecycle-cert.sh"

spira-lc state sp-inert >/dev/null 2>&1; is "the machine answers a bead with no row (1)" "1" "$?"

# ---------------------------------------------------------------------------------------
# A GatePass FOR A STALE TIP IS REFUSED (acceptance criterion 1).
# ---------------------------------------------------------------------------------------
echo
echo "certification for a stale tip is refused:"
seed_bead sp-stale SUBMITTED 4a0cde71aee7158542d013fc0c9f5acfc735c612 -
spira-lc certify sp-stale 6be712a6e73d979726c02a308b3457f9f5cf148e pass keyA >/dev/null 2>&1
is "GatePass for a different tip is refused" "3" "$?"
is "row state is unchanged" "SUBMITTED" "$(row_field sp-stale state)"
is "row tip is unchanged" "4a0cde71aee7158542d013fc0c9f5acfc735c612" "$(row_field sp-stale tip)"
is "row gate_key was never set" "" "$(row_field sp-stale gate_key)"

spira-lc certify sp-stale 4a0cde71aee7158542d013fc0c9f5acfc735c612 pass keyA >/dev/null 2>&1
is "POSITIVE CONTROL: GatePass for the matching tip applies" "0" "$?"
is "row is now CERTIFIED" "CERTIFIED" "$(row_field sp-stale state)"
is "row carries the gate key" "keyA" "$(row_field sp-stale gate_key)"

# ---------------------------------------------------------------------------------------
# THE TIP INVARIANT — a harness rebase of a certified branch sends it back to SUBMITTED
# (acceptance criterion 2). sp-stale is CERTIFIED at 4a0cde71aee7158542d013fc0c9f5acfc735c612 from the case above.
# ---------------------------------------------------------------------------------------
echo
echo "a harness rebase (moved tip) voids certification back to SUBMITTED:"
spira-lc resubmit sp-stale 926839139b8cf77fca0d1ccef742df1f23db3e81 >/dev/null 2>&1
is "lc_resubmit applies" "0" "$?"
is "row is back to SUBMITTED" "SUBMITTED" "$(row_field sp-stale state)"
is "row tip is the new (rebased) one" "926839139b8cf77fca0d1ccef742df1f23db3e81" "$(row_field sp-stale tip)"
is "row's gate key is cleared — the old verdict no longer answers for this tree" "" "$(row_field sp-stale gate_key)"

# ---------------------------------------------------------------------------------------
# GateRed and GateInfra.
# ---------------------------------------------------------------------------------------
echo
echo "GateRed sends a submitted bead to REWORK:"
seed_bead sp-red SUBMITTED 416797588a44ad7127cacb2471bc5ff549e15dff -
spira-lc certify sp-red 416797588a44ad7127cacb2471bc5ff549e15dff red "branch-red" >/dev/null 2>&1
is "GateRed applies" "0" "$?"
is "row is REWORK" "REWORK" "$(row_field sp-red state)"

# GateRedReason (lifecycle/src/reason.rs) is a closed enum — spira-lc refuses to parse a
# --kind whose reason is not one of its six kebab-case variants. gate.sh's own verdict()
# reasons (branch-red, syntax, beads-data, foreign-harness, ...) are free text, so
# lc_certify's red arm must map every one of them, including one this table has never seen,
# onto a variant the machine accepts — not pass the raw string through (SEEN RED: it did,
# and "branch-red" itself — the one gate.sh actually emits — failed to parse).
echo
echo "GateRed with an unrecognized raw reason still applies (mapped, not passed through):"
seed_bead sp-red2 SUBMITTED 9dfe5bb4211c8967d10dc13590acc1ad8da9ee7c -
spira-lc certify sp-red2 9dfe5bb4211c8967d10dc13590acc1ad8da9ee7c red "a-reason-spira-lc-has-never-heard-of" >/dev/null 2>&1
is "GateRed with an unmapped reason still applies" "0" "$?"
is "row is REWORK" "REWORK" "$(row_field sp-red2 state)"

echo
echo "GateInfra leaves a submitted bead SUBMITTED but still advances its version:"
seed_bead sp-infra SUBMITTED f6c4bf78c47ab305e4a99ca66e20ef812819705c -
v0="$(row_field sp-infra version)"
spira-lc certify sp-infra f6c4bf78c47ab305e4a99ca66e20ef812819705c infra - >/dev/null 2>&1
is "GateInfra applies" "0" "$?"
is "row is still SUBMITTED" "SUBMITTED" "$(row_field sp-infra state)"
is "row's version advanced (a recorded retry, not a silent no-op)" "$((v0 + 1))" "$(row_field sp-infra version)"

# ---------------------------------------------------------------------------------------
# A WORKING bead is auto-submitted before the verdict lands — a caller of lc_certify never
# has to sequence Submit and GatePass itself.
# ---------------------------------------------------------------------------------------
echo
echo "a WORKING bead is auto-submitted at the given tip, then certified:"
seed_bead sp-working WORKING - -
spira-lc certify sp-working 53aebfaae14bcb2e0fd283d18ee2638ad7d55013 pass keyB >/dev/null 2>&1
is "lc_certify from WORKING applies" "0" "$?"
is "row is CERTIFIED" "CERTIFIED" "$(row_field sp-working state)"
is "row tip is the one it was certified at" "53aebfaae14bcb2e0fd283d18ee2638ad7d55013" "$(row_field sp-working tip)"

# ---------------------------------------------------------------------------------------
# gate.sh's OWN verdict() calls through, for real — no stub, real git, real gate command.
# (acceptance criterion 3's mechanism: a gate that runs is the only path to a certification.)
# ---------------------------------------------------------------------------------------
echo
echo "gate.sh: a real PASS certifies the row at the branch's exact commit and gate key:"
gate_fixture_init "$TMP/gatefx"
gate_fixture_branch spira/sp-gpass
printf 'repo | %s | push | origin/main |  | true\n' "$REPO" > "$MAP"
TIP_PASS="$(git -C "$REPO" rev-parse spira/sp-gpass)"
seed_bead sp-gpass WORKING - -
gout="$(gate_fixture_run spira/sp-gpass repo PATH="$PATH" \
    SPIRA_GATE_BEAD=sp-gpass $(lcfix_decl))"
want "gate.sh: reports PASS" "VERDICT=PASS" "$gout"
is "gate.sh PASS: row is CERTIFIED" "CERTIFIED" "$(row_field sp-gpass state)"
is "gate.sh PASS: row tip is the branch's exact commit" "$TIP_PASS" "$(row_field sp-gpass tip)"
gk_gpass="$(row_field sp-gpass gate_key)"
[ -n "$gk_gpass" ] && ok "gate.sh PASS: row carries a real (non-empty) gate key" \
    || bad "gate.sh PASS: row carries a real (non-empty) gate key" "gate_key was empty"

echo
echo "gate.sh: a real FAIL sends the row to REWORK, not CERTIFIED:"
# The gate command must fail on the BRANCH and pass on the BASE — "false" for both would
# read as the BASE failing too (BASE_FAIL/infra, not the branch's own fault). "bad.txt"
# exists only on the branch, so `test ! -f bad.txt` fails there and passes on origin/main.
gate_fixture_branch spira/sp-gred bad.txt trip
printf 'repo | %s | push | origin/main |  | test ! -f bad.txt\n' "$REPO" > "$MAP"
seed_bead sp-gred WORKING - -
gout2="$(gate_fixture_run spira/sp-gred repo PATH="$PATH" \
    SPIRA_GATE_BEAD=sp-gred $(lcfix_decl))"
want "gate.sh: reports FAIL" "VERDICT=FAIL" "$gout2"
is "gate.sh FAIL: row is REWORK, not CERTIFIED" "REWORK" "$(row_field sp-gred state)"

tl_summary
