#!/usr/bin/env bash
# tier: T2
# covers: skew/src/* landing-pass/* queue/src/* release/src/* UC-instance-lifecycle-37
#
# test-skew-refresh.sh — stage-and-swap refresh advances regardless of live aeon leases;
# running processes keep their old inode; dirty tracked files are stashed; gap reports
# commits behind with a positive control that verifies the ref is resolvable; a queue-mode
# repo's checkout is advanced by the landing pass's own refresh loop; and, under queue.local,
# a round landed through queue.sh land-local is picked up by skew refresh as check-only
# (never a reset), and queue.sh rollback-local re-activates the previous release and moves
# local/main back to its archived head — the container-tier design's own three cases
# (wiki/projects/spira/designs/local-main-2026-09-27.md, "Test strategy"), run here against
# the real queue, release and skew, not fixture stand-ins for them (sp-gkfg1: a landing
# publishes a release through the release binary).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. It and queue (queue/DESIGN.md §7.4) are called by name on PATH.

echo "test-skew-refresh.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ── Fixture: a git remote with two commits, REPO left one behind ─────────────────────────
ORIGIN="$TMP/origin"
REPO="$TMP/repo"
git init -q "$ORIGIN"
git -C "$ORIGIN" config user.email "test@test"
git -C "$ORIGIN" config user.name "test"
mkdir -p "$ORIGIN/spira"
printf '#!/usr/bin/env bash\n# OLD\n' > "$ORIGIN/spira/lib.sh"
printf '# OLD_CANARY\n'               > "$ORIGIN/spira/canary.sh"
printf '# tracked\n'                  > "$ORIGIN/tracked.txt"
git -C "$ORIGIN" add spira/ tracked.txt
git -C "$ORIGIN" commit -q -m "base"
BASE_COMMIT="$(git -C "$ORIGIN" rev-parse HEAD)"

printf '#!/usr/bin/env bash\n# NEW\n' > "$ORIGIN/spira/lib.sh"
printf '# NEW_CANARY\n'               > "$ORIGIN/spira/canary.sh"
git -C "$ORIGIN" add spira/
git -C "$ORIGIN" commit -q -m "advance"
AHEAD_COMMIT="$(git -C "$ORIGIN" rev-parse HEAD)"

git clone -q "$ORIGIN" "$REPO"
git -C "$REPO" config user.email "test@test"
git -C "$REPO" config user.name "test"
git -C "$REPO" remote set-head origin --auto >/dev/null 2>&1 || true

reset_repo() { git -C "$REPO" reset -q --hard "$BASE_COMMIT"; }

# Stub `release` for skew's release-mode refresh (release build/verify/activate), so the
# refresh path runs without cargo. First on PATH only in run_skew_release.
STUBBIN="$TMP/stubbin"; mkdir -p "$STUBBIN"
cat > "$STUBBIN/release" <<'STUBREL'
#!/usr/bin/env bash
rel=""; a=()
while [ $# -gt 0 ]; do
    case "$1" in
        --releases) rel="$2"; shift 2 ;;
        --repo|--landed-ref) shift 2 ;;
        *) a+=("$1"); shift ;;
    esac
done
case "${a[0]}" in
    build)    mkdir -p "$rel/${a[1]}"; printf 'commit %s\n' "${a[1]}" > "$rel/${a[1]}/MANIFEST"; printf '%s\n' "${a[1]}" ;;
    verify)   printf 'release %s verifies\n' "${a[1]}" ;;
    activate) ln -sfn "${a[1]}" "$rel/.current.new.$$" && mv -T "$rel/.current.new.$$" "$rel/current"
              printf 'stub-release: current -> %s\n' "${a[1]}" ;;
esac
STUBREL
chmod +x "$STUBBIN/release"
reset_repo

run_skew_cmd() {
    local run_dir="$1"; shift
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        skew "$@" 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "gap — POSITIVE CONTROL: HEAD behind origin/main must be reported:"
# ===========================================================================

RUN1="$(mktemp -d "$TMP/run-XXXXX")"
gap_behind_out="$(run_skew_cmd "$RUN1" gap "$REPO")"; gap_behind_rc=$?
is   "gap behind: exits 1"                    "1"                "$gap_behind_rc"
want "gap behind: reports commit count"       "commit(s) behind" "$gap_behind_out"
want "gap behind: names the base ref"         "origin/"          "$gap_behind_out"

# ===========================================================================
echo
echo "gap — at-base: HEAD at origin/main exits 0:"
# ===========================================================================

REMOTE_MAIN="$(git -C "$REPO" symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null)"
git -C "$REPO" merge --ff-only -q "$REMOTE_MAIN"
RUN2="$(mktemp -d "$TMP/run-XXXXX")"
gap_current_out="$(run_skew_cmd "$RUN2" gap "$REPO")"; gap_current_rc=$?
is   "gap current: exits 0"                   "0"                "$gap_current_rc"
want "gap current: reports 0 commits behind"  "0 commits behind" "$gap_current_out"
reset_repo

# ===========================================================================
echo
echo "refresh — POSITIVE CONTROL: live lease does NOT block advance:"
# ===========================================================================
# stage-and-swap is safe while aeons run — each file replaced atomically, old inodes kept.
# This test fails if the refusal approach is ever restored, which is the discriminating signal.

RUN3="$(mktemp -d "$TMP/run-XXXXX")"
mkdir -p "$RUN3/aeon"
FAKE_BEAD="sp-testaa"
printf '%s' "$(($(date +%s) + 3600))" > "$RUN3/aeon/$FAKE_BEAD.lease"
reset_repo

out3="$(run_skew_cmd "$RUN3" refresh "$REPO")"; rc3=$?
is   "lease held: exits 0"               "0"             "$rc3"
want "lease held: reports refreshed"     "refreshed"     "$out3"
HEAD3="$(git -C "$REPO" rev-parse HEAD)"
is   "lease held: checkout advanced"     "$AHEAD_COMMIT" "$HEAD3"

# ===========================================================================
echo
echo "refresh — inode preservation: held fd reads old content; path reads new:"
# ===========================================================================
# Open canary.sh before the refresh (simulating a bash session that sourced it). After the
# atomic mv the path points to a new inode; the held fd still references the old one.

reset_repo
exec 9< "$REPO/spira/canary.sh"
INODE_BEFORE="$(stat -c '%i' "$REPO/spira/canary.sh")"

RUN4="$(mktemp -d "$TMP/run-XXXXX")"
out4="$(run_skew_cmd "$RUN4" refresh "$REPO")"; rc4=$?
is   "inode test: refresh exits 0"         "0"    "$rc4"

INODE_AFTER="$(stat -c '%i' "$REPO/spira/canary.sh")"
[ "$INODE_BEFORE" != "$INODE_AFTER" ] \
    && ok "stage-and-swap created a new inode at the path" \
    || bad "stage-and-swap created a new inode at the path" "inode unchanged ($INODE_BEFORE)"

old_content="$(cat <&9)"; exec 9<&-
want   "held fd reads old content"  "OLD_CANARY" "$old_content"
nowant "held fd does not see new"   "NEW_CANARY" "$old_content"
want   "path now has new content"   "NEW_CANARY" "$(cat "$REPO/spira/canary.sh")"

# ===========================================================================
echo
echo "refresh — dirty tracked files are stashed, not refused:"
# ===========================================================================

reset_repo
printf '# machine-written\n' >> "$REPO/tracked.txt"

RUN5="$(mktemp -d "$TMP/run-XXXXX")"
out5="$(run_skew_cmd "$RUN5" refresh "$REPO")"; rc5=$?
is   "dirty: exits 0 (stashed and advanced)"   "0"         "$rc5"
want "dirty: reports stash"                    "stashed"   "$out5"
want "dirty: reports refreshed"               "refreshed" "$out5"
HEAD5="$(git -C "$REPO" rev-parse HEAD)"
is   "dirty: checkout advanced despite dirty"  "$AHEAD_COMMIT" "$HEAD5"
STASH_COUNT="$(git -C "$REPO" stash list 2>/dev/null | wc -l | tr -d ' ')"
[ "$STASH_COUNT" -ge 1 ] \
    && ok "dirty: stash entry created" \
    || bad "dirty: stash entry created" "stash list shows $STASH_COUNT entries"

# ===========================================================================
echo
echo "refresh — release mode: fetch + release build/verify/activate + symlink flip:"
# ===========================================================================
# In release mode (SPIRA_RELEASES set and releases/current is a symlink), refresh
# must: fast-forward the checkout, run release build/verify/activate, and flip current.
# A stub 'release' creates the release dir and manifest so cargo is not required.

RELEASES="$TMP/releases"
OLD_SHA="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
mkdir -p "$RELEASES/$OLD_SHA"
ln -sf "$OLD_SHA" "$RELEASES/current"

reset_repo

run_skew_release() {
    local run_dir="$1"; shift
    env -i PATH="$STUBBIN:$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_RELEASES="$RELEASES" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        skew "$@" 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# T1a: refresh in release mode when behind — must advance, publish a release, flip symlink
reset_repo  # puts REPO at BASE_COMMIT, one behind AHEAD_COMMIT
RUN6="$(mktemp -d "$TMP/run-XXXXX")"
out6="$(run_skew_release "$RUN6" refresh "$REPO")"; rc6=$?
is   "release-mode refresh: exits 0"            "0"          "$rc6"
want "release-mode refresh: reports install"    "refreshed"  "$out6"
HEAD6="$(git -C "$REPO" rev-parse HEAD)"
is   "release-mode refresh: checkout advanced"  "$AHEAD_COMMIT" "$HEAD6"

NEW_CURRENT="$(readlink "$RELEASES/current")"
is   "release-mode refresh: current flipped to new sha" "$AHEAD_COMMIT" "$NEW_CURRENT"

# T1b: old release dir still present (not deleted)
[ -d "$RELEASES/$OLD_SHA" ] \
    && ok "release-mode refresh: old release dir preserved" \
    || bad "release-mode refresh: old release dir preserved" "missing $RELEASES/$OLD_SHA"

# T1c: new release dir was created with MANIFEST
[ -f "$RELEASES/$AHEAD_COMMIT/MANIFEST" ] \
    && ok "release-mode refresh: new release has MANIFEST" \
    || bad "release-mode refresh: new release has MANIFEST" "missing $RELEASES/$AHEAD_COMMIT/MANIFEST"

# T1d: refresh when already up-to-date — must exit 0 and publish nothing
git -C "$REPO" merge --ff-only -q "$(git -C "$REPO" symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null)"
ln -sf "$AHEAD_COMMIT" "$RELEASES/current"  # already current

RUN7="$(mktemp -d "$TMP/run-XXXXX")"
out7="$(run_skew_release "$RUN7" refresh "$REPO")"; rc7=$?
is   "release-mode already-current: exits 0"        "0"       "$rc7"
want "release-mode already-current: reports current" "already" "$out7"
nowant "release-mode already-current: no release call"  "stub-release" "$out7"

# ===========================================================================
echo
echo "landing pass — a queue-mode repo's checkout is advanced by the refresh loop:"
# ===========================================================================
# verdict.sh advances a queue-mode repo's base by a fast-forward push; nothing else pulls
# the shared checkout, so it lags origin/<base> until landing.sh's own end-of-pass loop
# (landing.sh: "ADVANCE THE CHECKOUT HUMANS READ") calls skew refresh on it — push and
# queue repos both, because those are the two modes that advance the base through Spira.
#
# MINIMUM LANDSTATE: one repo-map row with land=queue and no spira/* branches. land_repo
# reads the branch list first and returns immediately when it is empty (before touching
# bd, gate.sh or confine.sh), so reaching the refresh loop needs none of those — just
# landing.sh, lib.sh, conf.sh and the real skew, wired through a repo-map whose only
# row is the queue repo itself, doubling as the home repo so nothing else is visited.
#
# THE POSITIVE CONTROL IS THE CONSTRUCTION. origin/main is advanced by one commit while
# the queue repo's checkout stays behind; a false-clean result — HEAD already at
# origin/main before the pass — is impossible because the extra commit is added
# explicitly, so silence means the refresh loop never fired.
QORIGIN="$TMP/qorigin.git"; QREPO="$TMP/qland-repo"; QSH="$TMP/qland-spira"; QRUN="$TMP/qland-run"
git init -q --bare -b main "$QORIGIN"
git init -q -b main "$QREPO"
git -C "$QREPO" config user.email "test@test"
git -C "$QREPO" config user.name "test"
git -C "$QREPO" commit -q --allow-empty -m "queue base"
git -C "$QREPO" remote add origin "$QORIGIN"
git -C "$QREPO" push -q origin main
git -C "$QREPO" fetch -q origin
git -C "$QREPO" remote set-head origin --auto >/dev/null 2>&1 || true

# Advance origin/main while QREPO's checkout stays behind.
_QCLONE="$(mktemp -d "$TMP/qclone-XXXXX")"
git clone -q "$QORIGIN" "$_QCLONE"
git -C "$_QCLONE" commit -q --allow-empty -m "origin advances"
git -C "$_QCLONE" push -q origin main
rm -rf "$_QCLONE"
git -C "$QREPO" fetch -q origin

QUEUE_NEW="$(git -C "$QREPO" rev-parse origin/main)"
[ "$(git -C "$QREPO" rev-parse HEAD)" != "$QUEUE_NEW" ] \
    || bad "queue-mode refresh setup" "checkout is already at origin/main before the pass"

mkdir -p "$QSH" "$QRUN"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$QSH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$QSH/"
# `skew` is a compiled binary now (sp-yyk47): landing-pass's own `skew_refresh` resolves it
# by bare name on PATH, which is `$QSH` first here, so the binary must actually be there.
cp "$(command -v skew)" "$QSH/skew"
cat > "$QSH/repo-map" <<MAP
qfixture | $QREPO | queue | |
MAP

q_out="$(env -i PATH="$PATH" \
    HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$QSH" PATH="$QSH:$PATH" \
    SPIRA_RUN="$QRUN" \
    SPIRA_DB="$TMP/qland-no-db" \
    SPIRA_REPO="$QREPO" \
    SPIRA_HOME_REPO=qfixture \
    SPIRA_REPO_MAP="$QSH/repo-map" \
    SPIRA_DOLT_DATA="" \
    SPIRA_TESTDB_DATA="" \
    landing-pass land 2>&1)"

QUEUE_AFTER="$(git -C "$QREPO" rev-parse HEAD)"
[ "$QUEUE_AFTER" = "$QUEUE_NEW" ] \
    && ok  "a queue-mode repo's checkout is advanced to origin/main by the landing pass" \
    || bad "queue-mode refresh" "checkout at $(git -C "$QREPO" rev-parse --short HEAD), expected $(printf '%.7s' "$QUEUE_NEW")"
want "and the pass reports the refresh" "skew: refreshed to" "$q_out"

# ===========================================================================
echo
echo "queue.local — land then refresh, then rollback by ref move:"
# ===========================================================================
# The container tier's own three cases: skew refresh following local/main in a real
# harness checkout; land then refresh; rollback by ref move. Every assertion below runs
# against the real queue and release binaries and skew and mail (both compiled binaries
# now, sp-yyk47 and sp-ooh1k) copied verbatim into a scratch SPIRA_HOME — no stand-in for
# any of them but systemctl.
LSH="$TMP/local-spira"; mkdir -p "$LSH"
cp "$HERE"/lib.sh "$HERE"/conf.sh "$HERE"/suite-covers.sh "$LSH/" 2>/dev/null
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$LSH/"
cp "$(command -v skew)" "$LSH/skew"
cp "$(command -v mail)" "$LSH/mail"

LREPO="$TMP/local-repo"
git init -q -b trunk "$LREPO"
git -C "$LREPO" config user.email "test@test"
git -C "$LREPO" config user.name "test"
mkdir -p "$LREPO/fakebin/src"
# A one-member workspace: release reads the [[bin]] targets from [workspace].members.
printf '[workspace]\nmembers = ["fakebin"]\n' > "$LREPO/Cargo.toml"
printf '[package]\nname = "fakebin"\nversion = "0.1.0"\nedition = "2021"\n' > "$LREPO/fakebin/Cargo.toml"
echo 'fn main() {}' > "$LREPO/fakebin/src/main.rs"
# What release verify needs of a release: its own pre-activate.sh and a systemd/ directory.
mkdir -p "$LREPO/spira" "$LREPO/systemd"
printf '#!/bin/sh\nexit 0\n' > "$LREPO/spira/pre-activate.sh"
chmod +x "$LREPO/spira/pre-activate.sh"
printf '[Service]\nExecStart=@SPIRA_RELEASE@/bin/fakebin\n' > "$LREPO/systemd/spira-fake.service"
git -C "$LREPO" add Cargo.toml fakebin spira systemd
git -C "$LREPO" commit -q -m base
git -C "$LREPO" branch local/main trunk

LRUN="$TMP/local-run"; LQDIR="$LRUN/queue"; LRELEASES="$TMP/local-releases"
mkdir -p "$LRUN/worktree" "$LQDIR" "$LRELEASES"
LRMAP="$TMP/local-repo-map"
printf 'lfixq | %s | queue.local | local/main | | |\n' "$LREPO" > "$LRMAP"
ln -s spira-bootstrap "$LRELEASES/current"   # production runs a release, not a checkout
# release activate daemon-reloads; a mock stands in for systemctl.
printf '#!/bin/sh\nexit 0\n' > "$TMP/mock-sc"; chmod +x "$TMP/mock-sc"

run_lq() {
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$LSH" PATH="$LSH:$PATH" \
        SPIRA_HOME_REPO=lfixq \
        SPIRA_REPO="$LREPO" \
        SPIRA_RUN="$LRUN" \
        SPIRA_QUEUE_DIR="$LQDIR" \
        SPIRA_REPO_MAP="$LRMAP" \
        SPIRA_RELEASES="$LRELEASES" \
        SPIRA_SYSTEMCTL="$TMP/mock-sc" \
        SPIRA_DB="$TMP/local-no-db" \
        SPIRA_LAND_UNGATED="fixture: hand-built heads no gate judged (queue/DESIGN.md §8 D12)" \
        SPIRA_HOME="$LSH" PATH="$LSH:$PATH" queue "$@" 2>&1
}
run_lskew() {
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$LSH" PATH="$LSH:$PATH" \
        SPIRA_REPO="$LREPO" \
        SPIRA_RUN="$LRUN" \
        SPIRA_REPO_MAP="$LRMAP" \
        SPIRA_RELEASES="$LRELEASES" \
        "$LSH/skew" "$@" 2>&1
}
lround() {  # lround <branch> <file> <content> -> commit on local/main's tip, print the head sha
    local br="$1" file="$2" content="$3"
    git -C "$LREPO" checkout -qb "$br" local/main
    printf '%s\n' "$content" > "$LREPO/$file"
    git -C "$LREPO" add "$file"
    git -C "$LREPO" commit -q -m "round: $file"
    local head; head="$(git -C "$LREPO" rev-parse "$br")"
    git -C "$LREPO" checkout -q trunk
    git -C "$LREPO" branch -D "$br" >/dev/null 2>&1
    printf '%s' "$head"
}
# The round worktree queue land-local reads (--worktree, queue/DESIGN.md §8 D2/D3): one
# detached worktree per tree, at <head>; its target/release is the round's own build.
lbins_wt() {   # lbins_wt <head> -> the round worktree for <head>'s tree (created on first use)
    local head="$1" tree wt
    tree="$(git -C "$LREPO" rev-parse "${head}^{tree}")"
    wt="$TMP/round-wt/$tree"
    [ -d "$wt" ] || git -C "$LREPO" worktree add -q --detach "$wt" "$head" >/dev/null 2>&1
    printf '%s' "$wt"
}
lbins() {   # lbins <head> <content> -> the round's own release build in its worktree
    local head="$1" content="$2" dir
    dir="$(lbins_wt "$head")/target/release"
    mkdir -p "$dir"
    printf '%s' "$content" > "$dir/fakebin"
    chmod +x "$dir/fakebin"
}

LHEAD1="$(lround r1 f1.txt round1)"
lbins "$LHEAD1" bin1
lout1="$(run_lq land-local lfixq --head "$LHEAD1" --members "sp-lskw1:$LHEAD1" --worktree "$(lbins_wt "$LHEAD1")")"; lrc1=$?
is   "land 1: exits 0"                    "0"      "$lrc1"
want "land 1: activates the round's release" "activated release $LHEAD1" "$lout1"
is   "land 1: local/main fast-forwards to the round head" "$LHEAD1" "$(git -C "$LREPO" rev-parse local/main)"

echo
echo "queue.local refresh — matched: checks, deploys nothing:"
rout1="$(run_lskew refresh "$LREPO")"; rrc1=$?
is   "refresh (matched): exits 0"          "0"                "$rrc1"
want "refresh (matched): nothing to deploy" "nothing to deploy" "$rout1"

echo
echo "queue.local refresh — stray write to local/main: alarms, never resets it:"
LHEAD2="$(lround r2 f2.txt round2)"
lbins "$LHEAD2" bin2
lout2="$(run_lq land-local lfixq --head "$LHEAD2" --members "sp-lskw2:$LHEAD2" --worktree "$(lbins_wt "$LHEAD2")")"; lrc2=$?
is   "land 2: exits 0"                     "0"      "$lrc2"
is   "land 2: local/main fast-forwards to the second round head" "$LHEAD2" "$(git -C "$LREPO" rev-parse local/main)"

# Simulate a stray write to local/main that bypassed queue.sh land-local (the one writer):
git -C "$LREPO" update-ref refs/heads/local/main "$LHEAD1"
rout2="$(run_lskew refresh "$LREPO")"; rrc2=$?
is     "refresh (stray write): exits 1"        "1"          "$rrc2"
want   "refresh (stray write): reports LOCAL-SKEW" "LOCAL-SKEW" "$rout2"
is     "refresh (stray write): local/main is left exactly as found — refresh never resets it" \
       "$LHEAD1" "$(git -C "$LREPO" rev-parse local/main)"
# Restore for the rollback case below — a real skew refresh never touches this ref either way.
git -C "$LREPO" update-ref refs/heads/local/main "$LHEAD2"

echo
echo "queue.local rollback-local — re-activates the previous release, resets by ref move:"
lout3="$(run_lq rollback-local lfixq)"; lrc3=$?
is   "rollback: exits 0"                       "0"           "$lrc3"
want "rollback: re-activates the first round's release" "activated release $LHEAD1" "$lout3"
is   "rollback: local/main resets to the archived first-round head" \
     "$LHEAD1" "$(git -C "$LREPO" rev-parse local/main)"
is   "rollback: current symlink points at the first round's release" \
     "$LHEAD1" "$(readlink "$LRELEASES/current")"

echo
tl_summary
