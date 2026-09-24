#!/usr/bin/env bash
#
# test-gate-unit.sh — the gate-verdict seams gate-lib.sh exists to make T1: verdict()'s
# shape and its no-evidence downgrade, gate_key_hash(), cache_fresh(), gate_tree_key() and
# gate_attribute(). Every row here is a pure function call — no git, no worktree, no lock,
# no gate.sh trial. What previously needed six-plus full gate runs (two git trials each) to
# exercise one branch of one function is a sub-second table here.
#
# host-reason: sources gate-lib.sh and conf.sh only; no database, no systemd, no git
# tier: T1
# covers: spira/gate-lib.sh spira/lib.sh UC-gate-verdict-01 UC-gate-verdict-02 UC-gate-verdict-11 UC-gate-verdict-12 UC-gate-verdict-15 UC-gate-verdict-16
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-gate-unit.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# AN EXPLICIT, MINIMAL ENVIRONMENT. SPIRA_CONF points nowhere so no real spira.conf on this
# box can leak in; SPIRA_HOME is a scratch directory conf.sh's derived defaults resolve
# against, never the checkout this suite itself lives in.
export SPIRA_CONF="$TMP/nonexistent.conf"
export SPIRA_HOME="$HERE"

# --- 1. verdict()'s shape: exactly one anchored VERDICT= line, and the right exit code ---
# (UC-gate-verdict-01)
#
# verdict() is sourced and called in a SUBSHELL, because it calls exit(). gate_meter and
# yield_note stay undefined — verdict() only calls them through `command -v`, which is the
# seam the plan names ("source with gate_meter/yield_note undefined").
_run_verdict() {         # _run_verdict <status> <reason> [msg...] -> sets _V_OUT, _V_RC
    _V_OUT="$(
        . "$HERE/conf.sh"; . "$HERE/gate-lib.sh"
        BR="spira/sp-unit"; REPO_NAME="spira"
        "$@" 2>&1
    )"
    _V_RC=$?
}

_run_verdict verdict 0 syntax-only ""
wantrc "verdict(): PASS exits 0"                      0  "$_V_RC"
want   "verdict(): PASS line is anchored VERDICT=PASS" "gate: VERDICT=PASS reason=syntax-only branch=spira/sp-unit repo=spira suite=-" "$_V_OUT"

_run_verdict verdict 1 branch-red "gate: something failed"
wantrc "verdict(): FAIL exits 1"                       1 "$_V_RC"
want   "verdict(): FAIL line says VERDICT=FAIL"        "gate: VERDICT=FAIL reason=branch-red" "$_V_OUT"

_run_verdict verdict "$SPIRA_GATE_BASEFAIL" base-red "gate: base is red too"
wantrc "verdict(): BASE_FAIL exits 76"                 76 "$_V_RC"
want   "verdict(): BASE_FAIL line says VERDICT=BASE_FAIL" "gate: VERDICT=BASE_FAIL reason=base-red" "$_V_OUT"

_run_verdict verdict "$SPIRA_GATE_NOVERDICT" no-base "gate: cannot resolve a ref"
wantrc "verdict(): NO_VERDICT exits 75"                75 "$_V_RC"
want   "verdict(): NO_VERDICT line says VERDICT=NO_VERDICT" "gate: VERDICT=NO_VERDICT reason=no-base" "$_V_OUT"

# The whole VERDICT line is printed exactly once (checked against the PASS case above,
# which produced no other message) — not duplicated, not swallowed.
_run_verdict verdict 0 syntax-only ""
n_lines="$(printf '%s\n' "$_V_OUT" | grep -c '^gate: VERDICT=')"
is "verdict(): exactly one VERDICT= line" "1" "$n_lines"

# --- 2. no-evidence downgrade (UC-gate-verdict-02, gap: previously untested) -------------
# A FAIL (or BASE_FAIL) that carries no message is downgraded to NO_VERDICT with
# reason=no-evidence:<original reason> — a failure that says nothing is the machinery
# failing to run a check, not the branch failing one (sp-io5j backstop).
_run_verdict verdict 1 branch-red ""
wantrc "no-evidence downgrade: exits 75 (NO_VERDICT), not 1"  75 "$_V_RC"
want   "no-evidence downgrade: reason is prefixed no-evidence:" "reason=no-evidence:branch-red" "$_V_OUT"
want   "no-evidence downgrade: says the machinery failed, not the branch" \
    "the gate returned a failure with no output at all" "$_V_OUT"

_run_verdict verdict 1 branch-red "   "
wantrc "no-evidence downgrade: whitespace-only message also downgrades" 75 "$_V_RC"

# POSITIVE CONTROL: the SAME status with a real message is NOT downgraded — the downgrade
# fires on "no evidence", not on every FAIL (law-absence-needs-a-positive-control).
_run_verdict verdict 1 branch-red "gate: real diagnostic output here"
wantrc "positive control: FAIL with a message stays FAIL (exit 1)" 1 "$_V_RC"
nowant "positive control: reason is not prefixed no-evidence:" "no-evidence:" "$_V_OUT"

# PASS and BASE_FAIL are exempt: an empty message on either is not downgraded (verdict()
# only downgrades a nonzero, non-NOVERDICT, non-BASEFAIL status).
_run_verdict verdict 0 syntax-only ""
nowant "PASS with no message is never downgraded" "no-evidence:" "$_V_OUT"

# --- 3. gate_tree_key(): a pure, filesystem-safe munge of the branch name -----------------
# (part of the UC-gate-verdict-16 seam — test-gate-tree.sh calls this instead of its own
# copy, per the plan's duplicate-cluster #4/seam table; that rewrite is tracked separately)
. "$HERE/gate-lib.sh"
is "gate_tree_key(): slashes become dashes"        "spira-sp-04yh0" "$(gate_tree_key "spira/sp-04yh0")"
is "gate_tree_key(): repeatable for the same input" "$(gate_tree_key "spira/sp-x")" "$(gate_tree_key "spira/sp-x")"
nowant "gate_tree_key(): never emits a slash (unsafe as a path component)" "/" "$(gate_tree_key "a/b/c")"
is "gate_tree_key(): an unusual char (space) becomes a dash" "a-b" "$(gate_tree_key "a b")"

# --- 4. gate_key_hash(): pure hashing — each input moves the key (UC-gate-verdict-15) -----
base_key="$(gate_key_hash spira treeA filesA cmdA harnessA on none)"
is   "gate_key_hash(): deterministic for identical inputs" \
    "$base_key" "$(gate_key_hash spira treeA filesA cmdA harnessA on none)"
nk="$(gate_key_hash spira treeB filesA cmdA harnessA on none)"
isnz_tree="$([ "$nk" != "$base_key" ] && echo 1 || echo 0)"
is "gate_key_hash(): a different tree moves the key"          "1" "$isnz_tree"
nk="$(gate_key_hash spira treeA filesB cmdA harnessA on none)"
is "gate_key_hash(): a different files-hash moves the key"    "1" "$([ "$nk" != "$base_key" ] && echo 1 || echo 0)"
nk="$(gate_key_hash spira treeA filesA cmdB harnessA on none)"
is "gate_key_hash(): a different command moves the key"       "1" "$([ "$nk" != "$base_key" ] && echo 1 || echo 0)"
nk="$(gate_key_hash spira treeA filesA cmdA harnessB on none)"
is "gate_key_hash(): a different harness hash moves the key"  "1" "$([ "$nk" != "$base_key" ] && echo 1 || echo 0)"
nk="$(gate_key_hash spira treeA filesA cmdA harnessA off none)"
is "gate_key_hash(): SPIRA_GATE_SUITES (suites=on vs off) moves the key" \
    "1" "$([ "$nk" != "$base_key" ] && echo 1 || echo 0)"
nk="$(gate_key_hash spira treeA filesA cmdA harnessA on sp-other)"
is "gate_key_hash(): a different bead id moves the key" \
    "1" "$([ "$nk" != "$base_key" ] && echo 1 || echo 0)"

# --- 5. cache_fresh(): TTL/eval hardening (UC-gate-verdict-15; gap #10, previously eval'd) -
entry="$TMP/entry"
now=1000000000

printf 'when=2026-01-01T00:00:00Z\nby=aeon-x\nat=%s\n' "$((now - 5))" > "$entry"
out="$(cache_fresh "$entry" 10 "$now")"; rc=$?
wantrc "cache_fresh(): a fresh entry (age 5, ttl 10) returns 0" 0 "$rc"
want   "cache_fresh(): fresh entry reports when|by"             "2026-01-01T00:00:00Z|aeon-x" "$out"

printf 'when=old\nby=aeon-y\nat=%s\n' "$((now - 20))" > "$entry"
cache_fresh "$entry" 10 "$now" >/dev/null; rc=$?
wantrc "cache_fresh(): an expired entry (age 20, ttl 10) returns 1" 1 "$rc"

printf 'when=x\nby=y\n' > "$entry"
cache_fresh "$entry" 10 "$now" >/dev/null; rc=$?
wantrc "cache_fresh(): no at= line returns 1 (gap: fails toward re-running)" 1 "$rc"

printf 'when=x\nby=y\nat=not-a-number\n' > "$entry"
cache_fresh "$entry" 10 "$now" >/dev/null; rc=$?
wantrc "cache_fresh(): non-numeric at= returns 1" 1 "$rc"

printf 'when=x\nby=y\nat=%s\n' "$((now - 5))" > "$entry"
cache_fresh "$entry" not-a-number "$now" >/dev/null; rc=$?
wantrc "cache_fresh(): non-numeric TTL returns 1 (gap #9, previously '0')" 1 "$rc"

# THE HARDENING ROW (gap #10). The original inline check built `eval` from this file's own
# text. A `when=` holding a command substitution must never execute — it is a label to
# print, nothing else. The canary file must not appear.
canary="$TMP/canary-$$"
rm -f "$canary"
printf 'when=$(touch %s)\nby=y\nat=%s\n' "$canary" "$((now - 5))" > "$entry"
out="$(cache_fresh "$entry" 10 "$now")"; rc=$?
wantrc "cache_fresh(): a hostile when= is still treated as fresh (it's just text)" 0 "$rc"
is     "cache_fresh(): the hostile when= comes back verbatim, unevaluated" \
    "\$(touch $canary)|y" "$out"
canary_exists="$([ -e "$canary" ] && echo yes || echo no)"
is "cache_fresh(): the command substitution in when= was NEVER EXECUTED" "no" "$canary_exists"

# --- 6. gate_attribute(): the whole base-attribution rule (UC-gate-verdict-12) ------------
# Pure function of two trials' captured output — no git, no worktree, no second gate run.
mkbr() { printf '%s' "$1" > "$TMP/branch.out"; }
mkbase() { printf '%s' "$1" > "$TMP/base.out"; }

# base could not be tried at all (exited 75/124, or checkout failed) -> base-untestable
mkbr "x"; mkbase ""
cls="$(gate_attribute 1 "$TMP/branch.out" 0 1 "$TMP/base.out")"
is "gate_attribute(): base did not run -> base-untestable -" "base-untestable -" "$cls"

# base ran and PASSED cleanly -> branch-red, suite named from the branch's own reds
mkbr "spira/test-foo.sh RED spira/test-bar.sh FAILED"; mkbase "all green"
cls="$(gate_attribute 1 "$TMP/branch.out" 1 0 "$TMP/base.out")"
is "gate_attribute(): base passed -> branch-red, first branch-red suite" \
    "branch-red spira/test-foo.sh" "$cls"

# base ran and is red on the SAME suite as the branch, no branch-only red -> base-red
mkbr "spira/test-foo.sh RED"; mkbase "spira/test-foo.sh RED"
cls="$(gate_attribute 1 "$TMP/branch.out" 1 1 "$TMP/base.out")"
is "gate_attribute(): same suite red on both -> base-red" "base-red spira/test-foo.sh" "$cls"

# base red on one suite, branch red on that suite AND another it alone owns -> branch-red,
# excused only for the shared suite
mkbr "spira/test-foo.sh RED spira/test-only.sh RED"; mkbase "spira/test-foo.sh RED"
cls="$(gate_attribute 1 "$TMP/branch.out" 1 1 "$TMP/base.out")"
is "gate_attribute(): a suite red only on the branch -> branch-red, names that suite" \
    "branch-red spira/test-only.sh" "$cls"

# base's ONLY reds are timeouts -> inconclusive -> base-timeout
mkbr "spira/test-foo.sh RED"; mkbase "spira/test-foo.sh TIMEOUT"
cls="$(gate_attribute 1 "$TMP/branch.out" 1 1 "$TMP/base.out")"
is "gate_attribute(): base's only reds are timeouts -> base-timeout" \
    "base-timeout spira/test-foo.sh" "$cls"

# base has a genuine red AND a timeout -> the genuine one still counts as base-red
mkbr "spira/test-foo.sh RED spira/test-slow.sh RED"; mkbase "spira/test-foo.sh RED spira/test-slow.sh TIMEOUT"
cls="$(gate_attribute 1 "$TMP/branch.out" 1 1 "$TMP/base.out")"
is "gate_attribute(): a genuine base red beside a timeout -> base-red" \
    "base-red spira/test-foo.sh" "$cls"

# an unnamed red on both (no filename this convention recognises) -> base-red, suite "-"
mkbr "the command exploded"; mkbase "the command exploded"
cls="$(gate_attribute 1 "$TMP/branch.out" 1 1 "$TMP/base.out")"
is "gate_attribute(): an unnamed red on both -> base-red -" "base-red -" "$cls"

# --- 7. host_cores() (UC-gate-verdict-11): getconf, immune to cgroup-limited nproc --------
real_cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null)"
got="$(bash -c '
    mkdir -p "'"$TMP"'/stubpath"
    printf "#!/usr/bin/env bash\necho 1\n" > "'"$TMP"'/stubpath/nproc"
    chmod +x "'"$TMP"'/stubpath/nproc"
    PATH="'"$TMP"'/stubpath:$PATH" bash -c ". \"'"$HERE"'/lib.sh\" 2>/dev/null; host_cores"
')"
is "host_cores(): returns getconf's count even with nproc stubbed to 1" "$real_cores" "$got"

tl_summary
