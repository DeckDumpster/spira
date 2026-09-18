#!/usr/bin/env bash
#
# test-id-prefix.sh — SPIRA_ID_PREFIX is honoured at every detection site.
#
# Two sites previously hardcoded "sp-" and silently returned "nothing found"
# for any installation with a non-default prefix:
#   - other_beads_on_conflicts in lib.sh (grep pattern)
#   - bead-id extractor in bd-close-focus-guard.sh (Python regex)
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): prove the
# detector fires before trusting the negative case.
#
# covers: spira/lib.sh spira/bd-close-focus-guard.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }
wantrc() { if [ "$3" -eq "$2" ]; then ok "$1"; else bad "$1" "wanted rc=$2, got rc=$3"; fi; }

echo "test-id-prefix.sh"

PREFIX=tt

# ============================================================================
echo
echo "other_beads_on_conflicts — non-default prefix"
# ============================================================================

TMP_REPO="$(mktemp -d)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_REPO" "$TMP_DIR"' EXIT INT TERM

git -C "$TMP_REPO" init -q
git -C "$TMP_REPO" config user.email "test@example.com"
git -C "$TMP_REPO" config user.name "Test"

echo "a" > "$TMP_REPO/f.txt"
git -C "$TMP_REPO" add f.txt
git -C "$TMP_REPO" commit -q -m "initial"

# Commit on base touching f.txt with a custom-prefix bead id in the subject.
echo "b" > "$TMP_REPO/f.txt"
git -C "$TMP_REPO" add f.txt
git -C "$TMP_REPO" commit -q -m "tt-abc123: fix the thing"
BASE="$(git -C "$TMP_REPO" rev-parse HEAD)"

# Branch from before that commit — represents the aeon's working branch.
git -C "$TMP_REPO" checkout -q -b "spira/tt-own" HEAD~1
echo "c" > "$TMP_REPO/f.txt"
git -C "$TMP_REPO" add f.txt
git -C "$TMP_REPO" commit -q -m "tt-own: branch work"

# POSITIVE CONTROL: with SPIRA_ID_PREFIX=tt, tt-abc123 must be found.
result="$(
    export SPIRA_ID_PREFIX="$PREFIX" SPIRA_HOME="$HERE" SPIRA_RUN="$TMP_DIR/run"
    export SPIRA_DB="$TMP_DIR/db"
    bash -c '. "$1/lib.sh"; other_beads_on_conflicts "$2" "spira/tt-own" "$3" "f.txt"' \
        -- "$HERE" "$TMP_REPO" "$BASE"
)"
want   "custom-prefix id found when branch prefix matches"          "tt-abc123" "$result"
nowant "own bead excluded from conflict result"                     "tt-own"    "$result"

# NEGATIVE CONTROL: a branch with sp- prefix must not match tt- ids on the base.
# The prefix is derived from the branch's own id, not SPIRA_ID_PREFIX.
result_sp="$(
    export SPIRA_HOME="$HERE" SPIRA_RUN="$TMP_DIR/run"
    export SPIRA_DB="$TMP_DIR/db"
    bash -c '. "$1/lib.sh"; other_beads_on_conflicts "$2" "spira/sp-own" "$3" "f.txt"' \
        -- "$HERE" "$TMP_REPO" "$BASE"
)"
nowant "sp-prefix branch does not match tt- ids on base"            "tt-abc123" "$result_sp"

# ============================================================================
echo
echo "bd-close-focus-guard — non-default prefix in close reason"
# ============================================================================

GUARD="$HERE/bd-close-focus-guard.sh"
[ -f "$GUARD" ] || { printf 'SKIP bd-close-focus-guard.sh not found at %s\n' "$GUARD"; exit 77; }

# Require at least two auto-summoned personas to form a focus period.
auto_count=0
for _f in "$HERE/chamber"/*.fayth; do
    [ -e "$_f" ] || continue
    _summon=auto
    while IFS= read -r _line; do
        case "$_line" in FAYTH_SUMMON=*) _summon="${_line#FAYTH_SUMMON=}"; break ;; esac
    done < "$_f"
    [ "$_summon" = "auto" ] && auto_count=$((auto_count+1))
done
unset _f _summon _line
if [ "$auto_count" -lt 2 ]; then
    printf 'SKIP need at least 2 auto-summoned personas; found %d\n' "$auto_count"
    exit 77
fi

FOCUS_PERSONA=""
for _f in "$HERE/chamber"/*.fayth; do
    [ -e "$_f" ] || continue
    _summon=auto
    while IFS= read -r _line; do
        case "$_line" in FAYTH_SUMMON=*) _summon="${_line#FAYTH_SUMMON=}"; break ;; esac
    done < "$_f"
    if [ "$_summon" = "auto" ]; then
        FOCUS_PERSONA="$(basename "$_f" .fayth)"
        break
    fi
done
unset _f _summon _line
[ -n "$FOCUS_PERSONA" ] || { printf 'SKIP could not pick a focus persona\n'; exit 77; }

FOCUS_CONF="$TMP_DIR/focus.conf"
printf 'SPIRA_FAYTHS = %s\n' "$FOCUS_PERSONA" > "$FOCUS_CONF"

make_payload() {
    python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$1"
}

run_guard() {
    local cmd="$1" id_prefix="$2"
    local rc=0 out
    out=$(make_payload "$cmd" | \
      env -i PATH="$PATH" HOME="$HOME" \
          SPIRA_AEON=1 \
          SPIRA_HOME="$HERE" \
          SPIRA_CONF="$FOCUS_CONF" \
          SPIRA_ID_PREFIX="$id_prefix" \
          bash "$GUARD" 2>&1) || rc=$?
    printf '%s' "$out"
    return "$rc"
}

# POSITIVE CONTROL: focus period + custom-prefix id present → guard allows.
# bd show will fail (no real db configured), guard fails open → exits 0.
WITH_ID="bd close tt-work --reason-file - <<'REASON'
finding tracked in tt-abc123
REASON"
rc=0; run_guard "$WITH_ID" "$PREFIX" >/dev/null 2>&1 || rc=$?
wantrc "guard allows when custom-prefix id is present in reason" 0 "$rc"

# NEGATIVE CONTROL: focus period + no id in reason → must block.
NO_ID="bd close tt-work --reason-file - <<'REASON'
finding present but no tracking reference filed
REASON"
out="$(run_guard "$NO_ID" "$PREFIX" 2>&1 || true)"
want "guard blocks when no custom-prefix id in reason" "BLOCK" "$out"

# REGRESSION PROOF: with sp- prefix, a tt- id must NOT satisfy the guard.
out_sp="$(run_guard "$WITH_ID" "sp" 2>&1 || true)"
want "sp- prefix does not match tt- id in reason (still blocks)" "BLOCK" "$out_sp"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
