#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/src/world.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
[ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set"
SIM="$SPIRA_RELEASE/bin/sim"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
REPO="$T/repo"
mkdir -p "$REPO"
tar -C "$HERE/.." --exclude=./target --exclude=./.git -cf - . | tar -xf - -C "$REPO"
git -C "$REPO" init -q --initial-branch=main && git -C "$REPO" add -A && git -C "$REPO" -c user.name=t -c user.email=t@t commit -q -m seed || echo "REPO FAIL"
NOLOC=(-u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)
W="$T/w"
(cd "$REPO" && env "${NOLOC[@]}" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$SIM" world up "$W") 2>&1 | tail -3
sed -i '/SPIRA_LIFECYCLE_ENFORCE/d' "$W/config/sim.env"
echo "SPIRA_HOME=$W/release" >> "$W/config/sim.env"
env "${NOLOC[@]}" "$SPIRA_RELEASE/bin/spira-config" set spira.batcher_enable 1 "$W/config/sim.toml"
in_world() {
    local -a kv=()
    local line
    while IFS= read -r line; do [ -n "$line" ] && kv+=("$line"); done < "$W/config/sim.env"
    (cd "$W/work" && env "${NOLOC[@]}" SPIRA_TOML="$W/config/sim.toml" "${kv[@]}" PATH="$W/bin:$W/release/bin:$PATH" "$@")
}
show() { echo "   state: $(in_world spira-lc show "$1" 2>&1 | tr -d '\n ' | grep -o '"state":"[A-Z_]*"' | head -1)"; }
run() { local o rc; o="$(in_world "$@" 2>&1)"; rc=$?; echo "## $* rc=$rc :: $(printf '%s' "$o" | tail -${TN:-2} | tr '\n' '|' | tr -s ' ' | cut -c1-${CW:-330})"; }
B=sp-h1
run spira-lc create-bead $B
run spira-lc event bead $B --expect READY --version 0 --actor sim --kind '{"Claim":{"holder":"aeon-1","lease_until":4102444800}}'
git -C "$W/work" worktree add -q -b spira/$B "$W/wt-$B" local/main
echo hi > "$W/wt-$B/h.txt"; git -C "$W/wt-$B" add h.txt
git -C "$W/wt-$B" -c user.name=a -c user.email=a@a commit -q -m "$B: sim commit"
run bash -c "cd $W/wt-$B && SPIRA_WORK_BEAD_ID=$B work submit"
show $B
for i in 1 2 3; do
for cmd in "landing-pass --pass" "gate-worker run" "batcher rounds" "queue publish-settle"; do
  run $cmd
  show $B
done
done
run git -C $W/work branch -a -v
run git -C $W/work tag
bad "dump" "forced"
tl_summary
