#!/usr/bin/env bash
#
# test-cockpit-landed.sh — the worked/landed row is 24h-scoped; anomaly = closed, branch,
# no landstate; landing detection uses subject forms only (law-aeon-commits-name-their-bead).
#
#   ./test-cockpit-landed.sh
#
# covers: spira/cockpit.sh cockpit/health.sh
# scar: the worked/landed row counted all-time under a 24h header, producing nonsense against scoped counts; and never-landed falsely flagged healthy landing queues as irrecoverable.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# RUNNER-EXPORTED VARIABLES. conf.sh exports SPIRA_DB pointing at the runner's production
# database (which may need a running Dolt server). When this suite sources testdb.sh,
# conf.sh checks SPIRA_DB/.beads and runs bd migrate schema — failing before testdb_up
# can replace SPIRA_DB with the fixture. Unset it so conf.sh derives a default with no
# .beads and skips the check. suites.sh also strips it via RUNNER_VARS, but suites run
# directly in a runner context (e.g. for debugging) need this too.
unset SPIRA_DB
. "$HERE/testdb.sh"
testdb_require cockpit-landed
testdb_up cockpit-landed || exit 1

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# Git identity for fixture commits — required in the container (no ~/.gitconfig).
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"
BD_PATH="${SPIRA_PATH:-}"
REAL_BD="$(PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" command -v bd)"
[ -n "$REAL_BD" ] || { echo "SKIP cockpit-landed: no bd binary" >&2; exit 77; }
# After testdb_up, PATH has TESTDB_BIN prepended; command -v bd returns the full absolute
# path to the embedded binary symlink there (TESTDB_BIN/bd). Using the production binary
# (CGO_ENABLED=0, the REAL_BD) fails the conf.sh migrate schema check on embedded stores.
TESTDB_BD_PATH="$(command -v bd)"

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"                               # hermetic-ok: bare remote the fixture pushes to
git init -q "$REPO"                                    # hermetic-ok: throwaway fixture repo in $TMP
git init -q --bare "$REMOTE"                           # hermetic-ok: bare remote for spira_landrefs
git -C "$REPO" remote add origin "$REMOTE"             # hermetic-ok: give the fixture a real remote
git -C "$REPO" commit --allow-empty -m "init" -q       # hermetic-ok: seed commit for the fixture
BASE_BR="$(git -C "$REPO" branch --show-current)"

MAP="$TMP/repo-map"
# Base left empty; spira_landref resolves via rung 3 (remote set-head --auto) to
# origin/$BASE_BR after the push below — the same resolution path production uses.
# Never rely on rung 4 (HEAD) here: that path exists for repos with no remote, but the
# production harness always has one, and the fixture must exercise the same code path.
cat > "$MAP" <<MAP
# name | path | land | base | format | gate
work | $REPO | push | | |
MAP
RUN="$TMP/run"; mkdir -p "$RUN"

# --- fixture: four closed beads, three within 24h, one outside ---
# closed_at timestamps: "now" for the 24h cases, 48h ago for the old one.
NOW_ISO="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
OLD_ISO="$(date -u -d '48 hours ago' '+%Y-%m-%dT%H:%M:%SZ')"

testdb_seed <<JSONL
{"id":"sp-land","title":"landed work","status":"closed","closed_at":"$NOW_ISO","labels":["spira","plan","repo:work"]}
{"id":"sp-wait","title":"awaiting work","status":"closed","closed_at":"$NOW_ISO","labels":["spira","plan","repo:work"]}
{"id":"sp-gone","title":"lost work","status":"closed","closed_at":"$NOW_ISO","labels":["spira","plan","repo:work"]}
{"id":"sp-oldd","title":"old closed","status":"closed","closed_at":"$OLD_ISO","labels":["spira","plan","repo:work"]}
JSONL

# All four have aeon logs (the evidence a session ran).
for _id in sp-land sp-wait sp-gone sp-oldd; do
    touch "$RUN/$_id.log"
done

# sp-land: a commit on the base branch names it → landed (using <id>: prefix form).
git -C "$REPO" checkout -q "$BASE_BR"
git -C "$REPO" commit --allow-empty -m "sp-land: fix" -q   # hermetic-ok: fixture commit

# sp-wait: a branch exists but no commit on the base branch names it → awaiting landing.
git -C "$REPO" checkout -q -b spira/sp-wait
git -C "$REPO" commit --allow-empty -m "sp-wait work" -q   # hermetic-ok: fixture commit
git -C "$REPO" checkout -q "$BASE_BR"

# sp-gone: no commit anywhere, no branch → never landed (irrecoverable).
# (nothing to do — the absence is the fixture)

# sp-oldd: a commit names it on the base branch, but it was closed >24h ago.
git -C "$REPO" commit --allow-empty -m "sp-oldd fix" -q   # hermetic-ok: fixture commit

# Push the base branch to the bare remote so origin/$BASE_BR has the landing commits.
# cockpit.sh reads git log against origin/$BASE_BR (the remote-tracking ref from rung 3),
# not the local branch — matching the production invariant that nothing advances the shared
# checkout's default branch except a sentinel push.
git -C "$REPO" push -q origin "$BASE_BR":"$BASE_BR" 2>/dev/null   # hermetic-ok: push to fixture remote

# ======================================================================================
echo "closed-vs-landed — three-way classification and 24h scoping:"

out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=work \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_PATH="$BD_PATH" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"

val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

# --- the 24h scoping: sp-oldd is excluded, so 3 beads not 4 ---
is "SP_CLOSED is 24h-scoped" "3" "$(val SP_CLOSED)"

# --- landed vs anomaly ---
is "SP_LANDED counts committed work"              "1" "$(val SP_LANDED)"
is "SP_UNLANDED_N counts anomalies only"          "1" "$(val SP_UNLANDED_N)"

# --- positive control: the probe found something ---
want "output contains SP_AT"             "SP_AT=" "$out"
want "output contains SP_UNLANDED_N"     "SP_UNLANDED_N=" "$out"

echo
printf 'test-cockpit-landed: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
