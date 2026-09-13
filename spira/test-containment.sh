#!/usr/bin/env bash
#
# test-containment.sh — a non-prod instance refuses repos outside its workspaces root
# and repos whose git remote is network-reachable.
#
# WHAT THIS SUITE COVERS
# ----------------------
# Containment is two independent refusals, both failing closed, for SPIRA_INSTANCE != prod:
#
#   1. Any repo-map path not under SPIRA_WORKSPACES is refused by name.
#   2. Any repo-map path whose git remote resolves to a network URL is refused.
#
# Prod is entirely unaffected: neither check runs when SPIRA_INSTANCE is 'prod' (or
# absent), so the seven real repos load unchanged.
#
# THE POSITIVE CONTROL COMES FIRST. Before asserting that a bad config is refused, prove
# that a good one passes: a test-instance map with local paths and no remotes must load
# without error. Silence on a check that was never reached looks identical to a check that
# passed; the positive control is the only thing that distinguishes them.
#
# ACCEPTANCE (from bead sp-g2vl):
#   A — a test config naming a path outside SPIRA_WORKSPACES is refused by name.
#   B — a test clone that still has a real origin is refused.
#   C — a prod config with the (example) repos loads unchanged.
#
# covers: spira/lib.sh spira/conf.sh
# defect: sp-g2vl
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
must_pass()  { "$@" >/dev/null 2>&1 && ok "rc=0: $1" || bad "rc=0: $1" "exited non-zero"; }
must_fail()  { "$@" >/dev/null 2>&1 && bad "rc!=0: $1" "unexpectedly succeeded" || ok "rc!=0: $1"; }

echo "test-containment.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Build a minimal harness tree that conf.sh + lib.sh can resolve from.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
cp "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
cp "$HERE/lib.sh"  "$HARNESS/spira/lib.sh"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"
# lib.sh starts a git repo at its own location — give it one.
git -C "$HARNESS" init -q && git -C "$HARNESS" commit -q --allow-empty -m "root"

# Workspaces root for test fixtures.
WS="$TMP/workspaces"
mkdir -p "$WS"

# Helper: source lib.sh in an isolated subprocess with a given repo-map and instance.
# Returns the exit status of that subprocess.
load() {       # load <instance> <map-path>
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS/spira" \
        SPIRA_REPO="$HARNESS" \
        SPIRA_CONF=/nonexistent \
        SPIRA_INSTANCE="${1:-prod}" \
        SPIRA_WORKSPACES="$WS" \
        SPIRA_REPO_MAP="${2:-}" \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        bash -c ". '$HARNESS/spira/conf.sh'; . '$HARNESS/spira/lib.sh'; echo loaded" 2>&1
}

# ---------------------------------------------------------------------------
# Build fixture repos: two clones in the allowed workspaces root.
# ---------------------------------------------------------------------------
REPO_A="$WS/repo-a"; git init -q "$REPO_A" && git -C "$REPO_A" commit -q --allow-empty -m "a"
REPO_B="$WS/repo-b"; git init -q "$REPO_B" && git -C "$REPO_B" commit -q --allow-empty -m "b"

# A clone that has a REAL remote (simulated with a file:// path pointing outside ws).
# A real remote is any URL that is NOT a local absolute path or file://. We simulate
# one by adding an https-style remote, which the containment check recognises as real.
REPO_REMOTE="$WS/repo-remote"
git init -q "$REPO_REMOTE" && git -C "$REPO_REMOTE" commit -q --allow-empty -m "r"
git -C "$REPO_REMOTE" remote add origin "https://github.com/example/repo.git"

# A repo OUTSIDE the workspaces root.
REPO_OUTSIDE="$TMP/outside/repo-outside"
mkdir -p "$TMP/outside"
git init -q "$REPO_OUTSIDE" && git -C "$REPO_OUTSIDE" commit -q --allow-empty -m "out"

# ---------------------------------------------------------------------------
# Repo-map fixtures.
# ---------------------------------------------------------------------------
MAP_GOOD="$TMP/map-good"        # two local repos inside WS, no remotes -> must pass
MAP_OUTSIDE="$TMP/map-outside"  # one repo outside WS -> must fail
MAP_REAL_REMOTE="$TMP/map-remote" # repo inside WS but with real remote -> must fail
MAP_PROD="$TMP/map-prod"        # same bad config, but instance=prod -> must pass

cat >"$MAP_GOOD" <<EOF
# A clean test map: both paths are under SPIRA_WORKSPACES and have no remotes.
repo-a  | $REPO_A  | push | origin/main | |
repo-b  | $REPO_B  | push | origin/main | |
EOF

cat >"$MAP_OUTSIDE" <<EOF
# One entry is outside the workspaces root — must be refused.
repo-a   | $REPO_A       | push | origin/main | |
repo-out | $REPO_OUTSIDE  | push | origin/main | |
EOF

cat >"$MAP_REAL_REMOTE" <<EOF
# This clone has a real remote — must be refused regardless of path.
repo-a      | $REPO_A      | push | origin/main | |
repo-remote | $REPO_REMOTE | push | origin/main | |
EOF

# Prod uses the same 'outside' map — containment must not fire on prod.
cat >"$MAP_PROD" <<EOF
repo-a   | $REPO_A       | push | origin/main | |
repo-out | $REPO_OUTSIDE  | push | origin/main | |
EOF

# ===========================================================================
echo
echo "POSITIVE CONTROL — a good test map loads without error:"
# ===========================================================================
# Prove the check is reachable and passes on a valid config before asserting refusal.
out="$(load test "$MAP_GOOD" 2>&1)"
rc=$?
is "good map loads (rc=0)" "0" "$rc"
want "good map prints 'loaded'" "loaded" "$out"

# ===========================================================================
echo
echo "A — path outside SPIRA_WORKSPACES is refused by name:"
# ===========================================================================
# The check must fire before 'loaded' is printed, so rc!=0.
out="$(load test "$MAP_OUTSIDE" 2>&1)"
rc=$?
is "outside-path map exits non-zero" "1" "$rc"
want "error names the repo entry"     "repo-out"          "$out"
want "error mentions containment"     "containment"       "$out"
want "error mentions the instance"    "test"              "$out"

# ===========================================================================
echo
echo "B — a clone with a real remote is refused:"
# ===========================================================================
out="$(load test "$MAP_REAL_REMOTE" 2>&1)"
rc=$?
is "real-remote map exits non-zero" "1" "$rc"
want "error names the repo entry"    "repo-remote"        "$out"
want "error mentions the remote URL" "github.com"         "$out"
want "error mentions containment"    "containment"        "$out"

# ===========================================================================
echo
echo "C — prod instance is not affected by the same maps:"
# ===========================================================================
# The 'outside' map is deliberately bad for a test instance. Prod must load it cleanly.
out="$(load prod "$MAP_PROD" 2>&1)"
rc=$?
is "prod loads map with outside paths (rc=0)" "0" "$rc"
want "prod map prints 'loaded'" "loaded" "$out"

# A real-remote map is also fine for prod.
out="$(load prod "$MAP_REAL_REMOTE" 2>&1)"
rc=$?
is "prod loads map with real remote (rc=0)" "0" "$rc"
want "prod real-remote map prints 'loaded'" "loaded" "$out"

# Unset SPIRA_INSTANCE behaves identically to prod.
out="$(env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS/spira" \
        SPIRA_REPO="$HARNESS" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WORKSPACES="$WS" \
        SPIRA_REPO_MAP="$MAP_OUTSIDE" \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        bash -c ". '$HARNESS/spira/conf.sh'; . '$HARNESS/spira/lib.sh'; echo loaded" 2>&1)"
rc=$?
is "unset SPIRA_INSTANCE behaves like prod (rc=0)" "0" "$rc"
want "unset-instance map prints 'loaded'" "loaded" "$out"

# ===========================================================================
echo
echo "D — workspace root / passes containment for all absolute paths:"
# ===========================================================================
# Regression for sp-5jg4: when ws_real is "/" the old code built the glob "//*"
# ("$ws_real"/* with ws_real="/") — this matches no absolute path because a single
# leading slash cannot match two.  The fix strips the trailing slash so the pattern
# becomes "/*", which correctly matches every absolute path.

load_ws() {  # load_ws <workspaces> <instance> <map-path>
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS/spira" \
        SPIRA_REPO="$HARNESS" \
        SPIRA_CONF=/nonexistent \
        SPIRA_INSTANCE="${2:-prod}" \
        SPIRA_WORKSPACES="$1" \
        SPIRA_REPO_MAP="${3:-}" \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        bash -c ". '$HARNESS/spira/conf.sh'; . '$HARNESS/spira/lib.sh'; echo loaded" 2>&1
}

# POSITIVE CONTROL — prove the workspace check is reachable when ws != /.
# Repos are under $TMP; they must pass when SPIRA_WORKSPACES=$TMP.
out_d0="$(load_ws "$TMP" test "$MAP_GOOD" 2>&1)"
rc_d0=$?
is "D0: workspace=$TMP: repos under it pass containment (rc=0)" "0" "$rc_d0"
want "D0: workspace=$TMP prints 'loaded'" "loaded" "$out_d0"

# REGRESSION — workspace=/ must accept any absolute path (all live under /).
MAP_ROOT="$TMP/map-root"
cat >"$MAP_ROOT" <<EOF
repo-a  | $REPO_A  | push | origin/main | |
repo-b  | $REPO_B  | push | origin/main | |
EOF

out_d1="$(load_ws "/" test "$MAP_ROOT" 2>&1)"
rc_d1=$?
is "D1: workspace=/: absolute paths pass containment (rc=0)" "0" "$rc_d1"
want "D1: workspace=/ prints 'loaded'" "loaded" "$out_d1"

# AND IT MUST STILL BE A CHECK. Accepting every absolute path must not silently disable
# the real-remote refusal — that arm is independent and must still fire.
out_d2="$(load_ws "/" test "$MAP_REAL_REMOTE" 2>&1)"
rc_d2=$?
is   "D2: ws=/ still refuses a clone with a real remote" "1" "$rc_d2"
want "D2: ws=/ refusal mentions containment" "containment" "$out_d2"

# ===========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
