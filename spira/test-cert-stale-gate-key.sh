#!/usr/bin/env bash
#
# test-cert-stale-gate-key.sh — batch.sh skips a CERTIFIED branch whose stored gate
# key no longer matches the current gate command; landing.sh stores the key on cert.
#
# THE DEFECT (sp-4w1pp). CERTIFIED is written by paths that do not run the gate.
# When the gate command changes, old CERTIFIED records survive as if the new command
# had judged them. A batch built from such a record fails CI on the new check.
#
# FOUR CASES:
#
#   landing-writes-key: landing.sh writes a .gate-key file after certifying a branch.
#     Without this, no key is ever stored and the stale-key path is never triggered.
#
#   no-key (lenient): CERTIFIED landstate, no .gate-key file.
#     Old entries have no key; batch.sh does not block them (backward compatibility).
#
#   stale-key: .gate-key file does not match current CMD.
#     batch.sh skips the branch, clears the submitted record, reports stale-cert-key.
#
#   matching-key: .gate-key matches current CMD — branch is eligible.
#     This is the positive control: a detector that passes both the stale and
#     matching cases is not discriminating.
#
# tier: T2
# covers: spira/batch.sh spira/landing.sh spira/lib.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-cert-stale-gate-key
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up certgatekey || { echo "test-cert-stale-gate-key: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
SUBMITTED="$RUN/submitted"
QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$SUBMITTED" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"

GATE_COUNT="$TMP/gate-count"
: > "$GATE_COUNT"
# Gate stub: always pass, record invocations.
cat > "$SH/gate.sh" <<GSTUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$GATE_COUNT"
printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=none\n' "\$1" "\${2:-?}" >&2
exit 0
GSTUB
chmod +x "$SH/gate.sh"

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub gh 'exit 1'

# Forge stub: always succeed, never reach the network.
cat > "$SH/forge.sh" <<'FORGESTUB'
#!/usr/bin/env bash
cmd="${1:-}"; shift; shift
case "$cmd" in
    pr-create) cat > /dev/null; printf '42\n' ;;
    *) ;;
esac
FORGESTUB
chmod +x "$SH/forge.sh"

write_map_cmd1() {
    printf '%s | %s | queue | origin/main | | bash spira/gate.sh |\n' \
        "$REPONAME" "$REPO" > "$SH/repo-map"
}
write_map_cmd2() {
    printf '%s | %s | queue | origin/main | | bash spira/gate.sh --new-check |\n' \
        "$REPONAME" "$REPO" > "$SH/repo-map"
}
write_map_cmd1

landing() {
    rm -f "$RUN/landing.progress" "$GATE_COUNT"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" \
        bash "$SH/landing.sh" 2>&1
}

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_FORGE="$SH/forge.sh" \
    SPIRA_QUEUE_LOCAL_GATE=0 \
    SPIRA_QUEUE_BATCH_MAX=1 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
        bash "$SH/batch.sh" "$@" 2>&1
}

# Compute the current gate key for a branch by sourcing lib.sh in a subshell.
compute_key() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        bash -c ". '$SH/lib.sh'; compute_gate_key \"\$1\" \"\$2\" \"\$3\" \"\$4\"" \
        -- "$REPO" "$REPONAME" "$1" "origin/main" 2>/dev/null
}

seed() {
    testdb_reset
    rm -f "$LANDSTATE"/* "$SUBMITTED"/*
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

branch() {
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
}

tip_of()        { git -C "$REPO" rev-parse "spira/$1" 2>/dev/null; }
gate_key_file() { printf '%s/%s.gate-key' "$LANDSTATE" "$1"; }
sub_file()      { printf '%s/%s' "$SUBMITTED" "$1"; }
batch_open()    { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }

reset_queue() { rm -f "$(batch_open)"; }

echo "test-cert-stale-gate-key.sh"

# -----------------------------------------------------------------------------------------
# landing.sh writes .gate-key after a gate pass.
# -----------------------------------------------------------------------------------------
echo
echo "landing.sh stores gate key at certification:"
write_map_cmd1; seed; branch sp-gk-land
out="$(landing)"
want "certified sp-gk-land" "certified spira/sp-gk-land" "$out"
gkf="$(gate_key_file sp-gk-land)"
if [ -f "$gkf" ]; then
    ok "landing.sh wrote .gate-key file"
    gk="$(cat "$gkf")"
    is ".gate-key is non-empty" "1" "$([ -n "${gk:-}" ] && echo 1 || echo 0)"
else
    bad "landing.sh wrote .gate-key file" "absent: $gkf"
fi

# -----------------------------------------------------------------------------------------
# no-key (lenient): CERTIFIED without .gate-key — batch does not block the branch.
# -----------------------------------------------------------------------------------------
echo
echo "no .gate-key — lenient, branch eligible:"
reset_queue; seed; branch sp-no-key
tip="$(tip_of sp-no-key)"
printf 'CERTIFIED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/sp-no-key"
out="$(batch "$REPONAME")"
nowant "no stale-cert-key for sp-no-key" "stale-cert-key sp-no-key" "$out"

# -----------------------------------------------------------------------------------------
# stale-key: .gate-key does not match current CMD — batch skips, clears submitted.
# THE POSITIVE CONTROL is the matching-key case below. A detector that fires on both
# would not be discriminating.
# -----------------------------------------------------------------------------------------
echo
echo "stale .gate-key — batch skips branch, clears submitted:"
reset_queue; seed; branch sp-stale
tip="$(tip_of sp-stale)"
printf 'CERTIFIED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/sp-stale"
# Wrong key — simulates a gate-command change after the original certification.
printf 'deadbeef000000000000000000000000000000000000000000000000deadbeef\n' \
    > "$(gate_key_file sp-stale)"
# Submitted record — simulates the cmd_abandon path.
printf '%s %s certified 0\n' "$tip" "$(date +%s)" > "$(sub_file sp-stale)"
out="$(batch "$REPONAME")"
want "batch reports stale-cert-key" "stale-cert-key sp-stale" "$out"
is "no batch opened" "0" "$([ -f "$(batch_open)" ] && echo 1 || echo 0)"
if [ ! -f "$(sub_file sp-stale)" ]; then
    ok "submitted record cleared"
else
    bad "submitted record cleared" "file still present: $(sub_file sp-stale)"
fi

# -----------------------------------------------------------------------------------------
# matching-key: .gate-key matches current CMD — branch is eligible for batch.
# Positive control for stale-key: same branch, same repo-map, correct key is accepted.
# -----------------------------------------------------------------------------------------
echo
echo "matching .gate-key — positive control, branch eligible:"
reset_queue; seed; branch sp-match
tip="$(tip_of sp-match)"
printf 'CERTIFIED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/sp-match"
ck="$(compute_key "spira/sp-match" 2>/dev/null || true)"
if [ -n "${ck:-}" ]; then
    printf '%s\n' "$ck" > "$(gate_key_file sp-match)"
    out="$(batch "$REPONAME")"
    nowant "matching key not flagged as stale" "stale-cert-key sp-match" "$out"
else
    ok "matching-key: compute_key unavailable in test env — skipping"
fi
tl_summary
