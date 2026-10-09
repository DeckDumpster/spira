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
in_world() {
    local -a kv=()
    local line
    while IFS= read -r line; do [ -n "$line" ] && kv+=("$line"); done < "$W/config/sim.env"
    (cd "$W/work" && env "${NOLOC[@]}" SPIRA_TOML="$W/config/sim.toml" "${kv[@]}" PATH="$W/bin:$W/release/bin:$PATH" "$@")
}
show() { echo "   state: $(in_world spira-lc show "$1" 2>&1 | tr -d '\n ' | grep -o '"state":"[A-Z_]*"' | head -1)"; }
run() { local o rc; o="$(in_world "$@" 2>&1)"; rc=$?; echo "## $* rc=$rc :: $(printf '%s' "$o" | tail -${TN:-3} | tr '\n' '|' | tr -s ' ' | cut -c1-${CW:-300})"; }
FX="$(cat $W/db.fixture)"
echo "fixture=$FX; bd=$(command -v bd); $(ls $FX | tr '\n' ' ')"
run bd --version
run bash -c "cd $FX && bd create 'sim t' --id sp-h1 -l branch:spira/sp-h1 -l repo:sim -l spira -t task -p 2 --json"
run bash -c "cd $FX && bd list --json | head -c 300"
TN=6 CW=700 run bead file "sim title" --for builder --repo sim --json
run spira-lc list
bad "dump" "forced"
tl_summary
