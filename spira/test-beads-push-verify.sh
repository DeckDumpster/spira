#!/usr/bin/env bash
#
# test-beads-push-verify.sh — beads-push.sh never reports a push it did not make.
#
#   ./test-beads-push-verify.sh
#
# THE SCAR. On 2026-09-15 the store's config table held 144 uncommitted rows —
# the entire statute book — while beads-push.service exited 0/SUCCESS twice and
# the remote sat four and a half hours behind. Two independent defects combined:
#
#   1. _bp_dirty_count returned 0 for BOTH "clean" and "my query did not work",
#      and the caller read 0 as clean. Its embedded fallback probed a directory
#      (.beads/embeddeddolt) that does not exist on a server-mode store, so a
#      store whose location neither path knew reported itself clean.
#
#   2. Success was declared from the push command's own output — "Push complete"
#      — which is a claim about a command, not about the remote. Nothing ever
#      re-read the remote to see whether it had moved.
#
# THE ENGINES DISAGREE, which is why (1) matters more than it looks. On that
# store `bd sql "SELECT COUNT(*) FROM dolt_status"` answered 0 while the dolt CLI
# reading the same database answered 1 (config, 144 diff rows). A probe that
# picks the wrong engine is not merely unlucky; it is confidently wrong. So the
# CLI is preferred where a data dir is configured, and the engine that answered
# the dirty probe is the engine that must verify the push.
#
# WHAT THIS SUITE REQUIRES
#   A. A probe that cannot answer is a FAILURE, never a clean store.
#   B. A dirty store is committed, and a commit that does not clear the working
#      set is a failure — not a shrug followed by a push.
#   C. Success is asserted from the REMOTE's head, not the push's output. A push
#      that prints "Push complete" and leaves the remote behind is a failure.
#   D. The happy path still passes, or the three checks above are just a way to
#      make the job always red.
#
# tier: T1
# covers: beads-push.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
REPO="$(cd "$HERE/.." && pwd -P)"

echo "test-beads-push-verify.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"; DB="$TMP/db"; DD="$TMP/doltdata"
mkdir -p "$BIN" "$DB/.beads" "$DD/spira" "$TMP/run"
printf 'sync.remote: "git+ssh://git@example.invalid/x.git"\n' > "$DB/.beads/config.yaml"
printf '{"dolt_database":"spira","dolt_mode":"server"}\n'     > "$DB/.beads/metadata.json"

# STATE FILES drive the stubs. The test writes them; the stubs read them. This is
# how one fixture produces a clean store, a dirty one, and a broken probe.
st() { printf '%s' "$2" > "$TMP/state.$1"; }
st dirty 0; st local_head aaaa; st remote_head aaaa; st probe ok; st commit_clears yes

# ---- stub dolt: the authoritative engine -----------------------------------
cat > "$BIN/dolt" <<'STUB'
#!/usr/bin/env bash
S="$(dirname "$0")/../state"
q=""; for a in "$@"; do case "$a" in *dolt_status*|*dolt_log*|*active_branch*|*DOLT_COMMIT*) q="$a" ;; esac; done
[ "$(cat "${S}.probe")" = "broken" ] && { echo "dolt: cannot open database" >&2; exit 1; }
case "$*" in
    *fetch*) exit 0 ;;
esac
case "$q" in
    *active_branch*) printf 'b\nmain\n'; exit 0 ;;
    *dolt_status*)   printf 'n\n%s\n' "$(cat "${S}.dirty")"; exit 0 ;;
    *DOLT_COMMIT*)
        [ "$(cat "${S}.commit_clears")" = "yes" ] && printf '0' > "${S}.dirty"
        printf 'status\n0\n'; exit 0 ;;
    *"dolt_log('main')"*)          printf 'commit_hash\n%s\n' "$(cat "${S}.local_head")";  exit 0 ;;
    *dolt_log*remotes*|*remotes*)  printf 'commit_hash\n%s\n' "$(cat "${S}.remote_head")"; exit 0 ;;
esac
exit 0
STUB
chmod +x "$BIN/dolt"

# ---- stub bd: the engine that was confidently wrong -------------------------
# It always answers "clean" and "nothing to commit", exactly as the real one did.
cat > "$BIN/bd" <<'STUB'
#!/usr/bin/env bash
S="$(dirname "$0")/../state"
case "$*" in
    *"dolt push"*)  printf 'Push complete\n'; exit 0 ;;
    *"dolt commit"*) printf 'Nothing to commit.\n'; exit 0 ;;
    *"sql"*"dolt_status"*) printf -- '-\n0\n(1 rows)\n'; exit 0 ;;
    *memories*)     printf '{}\n'; exit 0 ;;
esac
exit 0
STUB
chmod +x "$BIN/bd"

run_push() {
    env -i PATH="$BIN:/usr/local/bin:/usr/bin:/bin" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent SPIRA_PATH="$BIN" \
        SPIRA_DB="$DB" SPIRA_RUN="$TMP/run" SPIRA_DOLT_DATA="$DD" \
        SPIRA_REPO_MAP=/nonexistent SPIRA_INSTANCE=prod \
        bash "$REPO/beads-push.sh" 2>&1
}

# ===========================================================================
echo
echo "D. happy path — a clean store whose remote matches is a success:"
# ===========================================================================
st dirty 0; st local_head aaaa; st remote_head aaaa; st probe ok
out="$(run_push)"; rc=$?
wantrc "clean+matching remote exits 0" 0 "$rc"
want   "and says so"                   "OK" "$out"

# ===========================================================================
echo
echo "A. a probe that cannot answer is a failure, not a clean store:"
# ===========================================================================
# THE SCAR, DIRECTLY. bd answers 0 no matter what; if the trustworthy engine
# cannot be reached the job must refuse rather than inherit that 0.
st probe broken
out="$(run_push)"; rc=$?
wantrc "broken probe exits non-zero"       1 "$rc"
nowant "and never claims success"          "OK" "$out"
want   "and names what it could not do"    "uncommitted" "$out"
st probe ok

# ===========================================================================
echo
echo "B. a dirty store is committed, and a commit that does not clear it fails:"
# ===========================================================================
st dirty 1; st commit_clears yes; st local_head bbbb; st remote_head bbbb
out="$(run_push)"; rc=$?
wantrc "dirty store commits then pushes"   0 "$rc"
want   "and reports what it committed"     "committed" "$out"

st dirty 1; st commit_clears no
out="$(run_push)"; rc=$?
wantrc "a commit that clears nothing fails" 1 "$rc"
nowant "and never claims success"           "OK" "$out"
st dirty 0; st commit_clears yes

# ===========================================================================
echo
echo "C. success comes from the remote's head, not the push's own output:"
# ===========================================================================
# The stub bd prints "Push complete" unconditionally — the exact string the old
# code trusted. The remote is behind, so this must still fail.
st local_head cccc; st remote_head aaaa
out="$(run_push)"; rc=$?
wantrc "remote behind local is a failure"   1 "$rc"
nowant "and never claims success"           "OK" "$out"
want   "and names the disagreement"         "remote" "$out"

echo
tl_summary
