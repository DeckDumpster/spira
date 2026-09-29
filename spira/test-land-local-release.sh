#!/usr/bin/env bash
#
# test-land-local-release.sh — sp-sf60f: under queue.local, a land packages the round head's
# source with the binaries its own tree's --with-bins corpus built and activates the result
# atomically; a land with no corpus is refused before anything changes; rollback re-activates
# the previous release and resets the ref to its archived head; skew.sh refresh, under
# queue.local, only checks current against local/main's head and never deploys.
#
# WHY THIS MATTERS: resetting a checkout advances the source but never touches whatever
# binary is already running, so the two can silently disagree about schema or protocol.
# Every assertion below is either "the activated bin/ is this round's own corpus, byte for
# byte" or "a mismatch is reported and nothing was deployed".
#
# tier: T1
# covers: queue/src/* spira/build-tarball.sh spira/activate.sh spira/skew.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4): the one conf.sh exports, else the tree under
# test's own build (testenv's SPIRA_ARTIFACTS), else this checkout's bin/.
QUEUE_BIN="${SPIRA_QUEUE_BIN:-${SPIRA_ARTIFACTS:-$HERE/../bin}/queue}"
[ -x "$QUEUE_BIN" ] || { echo "FAIL: the queue binary is not built at $QUEUE_BIN"; exit 1; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-land-local-release
TMP="$(mktemp -d)"
trap 'testdb_drop; chmod -R u+w "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
testdb_up llocrel || { echo "test-land-local-release: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-land-local-release.sh"

SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cat > "$SH/mail.sh" <<'EOFM'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
echo "mail sent" >&2
cat > "$MAIL_BODY_FILE"
EOFM
chmod +x "$SH/mail.sh"
MAIL_BODY_FILE="$TMP/mail-body"
rm -f "$MAIL_BODY_FILE"

# ---------------------------------------------------------------------------
# Fixture repo: a minimal cargo workspace (one [[bin]] target, no dependencies — cargo
# metadata resolves it with no network call) so build-tarball.sh --workspace can discover
# the binary NAME while a fake, content-stamped file supplies its BYTES from the
# --with-bins corpus directory, never from an actual `cargo build`.
# ---------------------------------------------------------------------------
REPO="$TMP/repo"
git init -q -b trunk "$REPO"
mkdir -p "$REPO/src"
cat > "$REPO/Cargo.toml" <<'EOF'
[package]
name = "fakebin"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "fakebin"
path = "src/main.rs"
EOF
echo 'fn main() {}' > "$REPO/src/main.rs"
git -C "$REPO" add Cargo.toml src
git -C "$REPO" commit -q -m base
git -C "$REPO" branch local/main trunk

RUN="$TMP/run"; QDIR="$RUN/queue"; RELEASES="$TMP/releases"
mkdir -p "$RUN/worktree" "$QDIR" "$RELEASES"
RMAP="$TMP/repo-map"
printf 'fixq | %s | queue.local | local/main | | |\n' "$REPO" > "$RMAP"

# ---------------------------------------------------------------------------
# Mock systemctl (sp-zt0ae, sections 8-9) — live-aeon fixture for the release step's
# guard. MOCK_AEONS, when exported, is echoed back as the live-unit list; unset (the
# default for sections 0-7) it reports none, same as a real systemctl with no aeons up.
# ---------------------------------------------------------------------------
SC_LOG="$TMP/sc.log"
MOCK_SC="$TMP/mock-sc"
cat > "$MOCK_SC" <<EOF
#!/usr/bin/env bash
printf 'SC: %s\n' "\$*" >> "$SC_LOG"
case "\$*" in
    *spira-aeon-*) printf '%s\n' "\${MOCK_AEONS:-}" ;;
    *list-units*)  printf 'spira-sentinel-prod.service loaded active running Sentinel\n' ;;
esac
exit 0
EOF
chmod +x "$MOCK_SC"

B() { bd -C "$SPIRA_DB" "$@"; }
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
v=d[0].get(sys.argv[1])
print(",".join(v) if isinstance(v,list) else (v or ""))' "$2" 2>/dev/null; }
seed() {
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":["plan","repo:fixq","spira-submitted"],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" | testdb_seed
}

run_q() {
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_HOME_REPO=fixq \
    SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" \
    SPIRA_QUEUE_DIR="$QDIR" \
    SPIRA_REPO_MAP="$RMAP" \
    SPIRA_RELEASES="$RELEASES" \
    SPIRA_LAND_UNGATED="fixture: hand-built heads no gate judged (queue/DESIGN.md §8 D12)" \
    SPIRA_SYSTEMCTL="$MOCK_SC" \
    MOCK_AEONS="${MOCK_AEONS:-}" \
        SPIRA_HOME="$SH" "$QUEUE_BIN" "$@" 2>&1
}
run_skew() {
    MAIL_BODY_FILE="$MAIL_BODY_FILE" \
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" \
    SPIRA_REPO_MAP="$RMAP" \
    SPIRA_RELEASES="$RELEASES" \
        bash "$SH/skew.sh" "$@" 2>&1
}
localmain() { git -C "$REPO" rev-parse local/main; }
current_name() { readlink "$RELEASES/current" 2>/dev/null; }

# mk_round <branch> <file> <content> -> commit on local/main's tip, print the head sha.
mk_round() {
    local br="$1" file="$2" content="$3"
    git -C "$REPO" checkout -qb "$br" local/main
    printf '%s\n' "$content" > "$REPO/$file"
    git -C "$REPO" add "$file"
    git -C "$REPO" commit -q -m "round: $file"
    local head; head="$(git -C "$REPO" rev-parse "$br")"
    git -C "$REPO" checkout -q trunk
    git -C "$REPO" branch -D "$br" >/dev/null 2>&1
    printf '%s' "$head"
}
# mk_bins <head> <content> -> populate the --with-bins corpus for <head>'s own tree.
# The round worktree queue land-local reads (--worktree, queue/DESIGN.md §8 D2/D3): one
# detached worktree per tree, at <head>; its target/release is the round's own build.
bins_wt() {   # bins_wt <head> -> the round worktree for <head>'s tree (created on first use)
    local head="$1" tree wt
    tree="$(git -C "$REPO" rev-parse "${head}^{tree}")"
    wt="$TMP/round-wt/$tree"
    [ -d "$wt" ] || git -C "$REPO" worktree add -q --detach "$wt" "$head" >/dev/null 2>&1
    printf '%s' "$wt"
}
mk_bins() {   # mk_bins <head> <content> -> the round's own release build in its worktree
    local head="$1" content="$2" dir
    dir="$(bins_wt "$head")/target/release"
    mkdir -p "$dir"
    printf '%s' "$content" > "$dir/fakebin"
    chmod +x "$dir/fakebin"
}

# Sections 0-7 exercise the release-activation path itself: the state sp-tkds8's cutover
# leaves production in, where $SPIRA_RELEASES/current already exists. A dangling symlink is
# enough — nothing here reads its target, only whether it is a symlink at all — and gives
# section 1 something to assert stays unchanged. Sections 8-9 (sp-zt0ae) below are what
# exercises today's pre-cutover state, where it does not exist yet.
ln -s spira-bootstrap "$RELEASES/current"

# ============================================================================
echo
echo "0 — rollback with no landed round is refused"
# ============================================================================
out="$(run_q rollback-local fixq)"; rc=$?
[ "$rc" -ne 0 ] && ok "0: exit non-zero with nothing to roll back to" \
    || bad "0: exit non-zero with nothing to roll back to" "got rc=$rc out=$out"
want "0: names the reason" "no landed round" "$out"

# ============================================================================
echo
echo "1 — a land with no built binaries is refused before anything changes"
# ============================================================================
testdb_reset
seed sp-lrel1
PRE_MAIN="$(localmain)"
HEAD1="$(mk_round round-1 one.txt v1)"

out="$(run_q land-local fixq --head "$HEAD1" --members "sp-lrel1:$HEAD1" --worktree "$(bins_wt "$HEAD1")")"; rc=$?
[ "$rc" -ne 0 ] && ok "1: exit non-zero with no corpus for the round's tree" \
    || bad "1: exit non-zero with no corpus for the round's tree" "got rc=$rc out=$out"
want "1: names the missing corpus"       "no built binaries" "$out"
is   "1: local/main is unchanged"        "$PRE_MAIN" "$(localmain)"
is   "1: current is unchanged"           "spira-bootstrap" "$(current_name)"
is   "1: the bead is left open"          open "$(field sp-lrel1 status)"

# ============================================================================
echo
echo "2 — the same head, corpus now built: packages, activates, lands"
# ============================================================================
mk_bins "$HEAD1" v1-binary

out="$(run_q land-local fixq --head "$HEAD1" --members "sp-lrel1:$HEAD1" --worktree "$(bins_wt "$HEAD1")")"; rc=$?
[ "$rc" -eq 0 ] && ok "2: exit 0 once the corpus exists" \
    || bad "2: exit 0 once the corpus exists" "got rc=$rc out=$out"
want "2: reports the activated release" "activated spira-$HEAD1" "$out"
is   "2: local/main advances to the round head" "$HEAD1" "$(localmain)"
is   "2: current is activated to this round's release" "spira-$HEAD1" "$(current_name)"
is   "2: bin/fakebin is this round's own corpus, byte for byte" \
     "v1-binary" "$(cat "$RELEASES/spira-$HEAD1/bin/fakebin" 2>/dev/null)"
want "2: MANIFEST records the round head commit" "commit $HEAD1" "$(cat "$RELEASES/spira-$HEAD1/MANIFEST" 2>/dev/null)"
is   "2: the bead is closed"             closed "$(field sp-lrel1 status)"
want "2: close reason declares landed"   "OUTCOME: landed" "$(field sp-lrel1 close_reason)"

# ============================================================================
echo
echo "3 — a second round lands its own, different corpus"
# ============================================================================
seed sp-lrel3
HEAD2="$(mk_round round-2 two.txt v2)"
mk_bins "$HEAD2" v2-binary

out="$(run_q land-local fixq --head "$HEAD2" --members "sp-lrel3:$HEAD2" --worktree "$(bins_wt "$HEAD2")")"; rc=$?
[ "$rc" -eq 0 ] && ok "3: second round lands" || bad "3: second round lands" "got rc=$rc out=$out"
is   "3: current advances to the second round's release" "spira-$HEAD2" "$(current_name)"
is   "3: bin/fakebin is the second round's own corpus" \
     "v2-binary" "$(cat "$RELEASES/spira-$HEAD2/bin/fakebin" 2>/dev/null)"
[ -d "$RELEASES/spira-$HEAD1" ] && ok "3: the first release directory is still present" \
    || bad "3: the first release directory is still present" "missing $RELEASES/spira-$HEAD1"

# ============================================================================
echo
echo "4 — skew refresh under queue.local: matched — checks, deploys nothing"
# ============================================================================
out="$(run_skew refresh "$REPO")"; rc=$?
is   "4: exit 0 when running matches local/main" "0" "$rc"
want "4: reports nothing to deploy" "nothing to deploy" "$out"
is   "4: current is unchanged"      "spira-$HEAD2" "$(current_name)"

# ============================================================================
echo
echo "5 — skew refresh under queue.local: mismatched — alarms, deploys nothing"
# ============================================================================
# Simulate a stray write to local/main that bypassed queue.sh land-local (the one writer):
# no release was ever packaged for this head, so running (still HEAD2's release) now
# disagrees with local/main.
STRAY="$(mk_round round-stray stray.txt strayed)"
git -C "$REPO" update-ref refs/heads/local/main "$STRAY"
REPO_HEAD_BEFORE="$(git -C "$REPO" rev-parse trunk)"

out="$(run_skew refresh "$REPO")"; rc=$?
[ "$rc" -ne 0 ] && ok "5: exit non-zero on a running/local-main mismatch" \
    || bad "5: exit non-zero on a running/local-main mismatch" "got rc=$rc out=$out"
want "5: names the condition" "LOCAL-SKEW" "$out"
want "5: escalation reported on stdout" "escalated" "$out"
[ -s "$MAIL_BODY_FILE" ] && ok "5: an alarm mail was actually sent" \
    || bad "5: an alarm mail was actually sent" "no mail body file was written"
is   "5: current is still the second round's release — nothing was deployed" \
     "spira-$HEAD2" "$(current_name)"
is   "5: the checkout HEAD was never touched" "$REPO_HEAD_BEFORE" "$(git -C "$REPO" rev-parse trunk)"
is   "5: local/main is left exactly as found — refresh never resets it either way" \
     "$STRAY" "$(localmain)"

# ============================================================================
echo
echo "6 — rollback-local re-activates the previous release and resets local/main"
# ============================================================================
git -C "$REPO" update-ref refs/heads/local/main "$HEAD2"   # undo the stray write above

out="$(run_q rollback-local fixq)"; rc=$?
[ "$rc" -eq 0 ] && ok "6: rollback exits 0" || bad "6: rollback exits 0" "got rc=$rc out=$out"
want "6: reports the re-activated release" "spira-$HEAD1" "$out"
is   "6: current rolls back to the first round's release" "spira-$HEAD1" "$(current_name)"
is   "6: bin/fakebin is the first round's corpus again" \
     "v1-binary" "$(cat "$RELEASES/spira-$HEAD1/bin/fakebin" 2>/dev/null)"
is   "6: local/main resets to the first round's archived head" "$HEAD1" "$(localmain)"
is   "6: the first round's bead is still landed (bead state is untouched)" \
     closed "$(field sp-lrel1 status)"

# ============================================================================
echo
echo "7 — GitHub diverged in both directions never reaches refresh's verdict"
# ============================================================================
# This is the interim override's guarded scenario (skew-keep-local, tagged to retire when
# this bead lands): the pre-fix refresh() fetched the land ref's remote and reset the
# checkout onto it whenever that remote held a commit HEAD lacked. Once production runs
# ahead of a round nothing has published yet, "GitHub holds a commit production lacks" is
# routine, not an error — so that reset would drop every locally landed, unpublished round.
# Build exactly that divergence and confirm the queue.local branch never gets near it: it
# resolves purely from running vs local/main, and GitHub is never fetched or written to.
GITHUB="$TMP/github.git"
git clone -q --bare "$REPO" "$GITHUB"
GHWORK="$TMP/ghwork"
git clone -q "$GITHUB" "$GHWORK" >/dev/null 2>&1
git -C "$GHWORK" checkout -q local/main
echo github-only > "$GHWORK/github-only.txt"
git -C "$GHWORK" add github-only.txt
git -C "$GHWORK" commit -q -m "a commit GitHub has that production never landed"
git -C "$GHWORK" push -q origin local/main
git -C "$REPO" remote add origin "$GITHUB"

# production lands a round of its own that it never publishes — holding a commit GitHub
# lacks, on top of already lacking the one GitHub just gained above.
STRAY2="$(mk_round round-stray2 stray2.txt strayed2)"
git -C "$REPO" update-ref refs/heads/local/main "$STRAY2"

PRE_CURRENT="$(current_name)"
PRE_TRUNK="$(git -C "$REPO" rev-parse trunk)"
PRE_GITHUB_MAIN="$(git -C "$GITHUB" rev-parse local/main)"

out="$(run_skew refresh "$REPO")"; rc=$?
[ "$rc" -ne 0 ] && ok "7: exit non-zero — running no longer matches local/main" \
    || bad "7: exit non-zero — running no longer matches local/main" "got rc=$rc out=$out"
want "7: names the local mismatch, not a GitHub one" "LOCAL-SKEW" "$out"
is   "7: current is untouched — nothing was deployed" "$PRE_CURRENT" "$(current_name)"
is   "7: local/main is left exactly as found" "$STRAY2" "$(localmain)"
is   "7: the checkout's HEAD (trunk) is never moved" "$PRE_TRUNK" "$(git -C "$REPO" rev-parse trunk)"
is   "7: GitHub's own ref is never written to" "$PRE_GITHUB_MAIN" "$(git -C "$GITHUB" rev-parse local/main)"
[ ! -e "$REPO/.git/FETCH_HEAD" ] && ok "7: refresh never fetched GitHub" \
    || bad "7: refresh never fetched GitHub" "FETCH_HEAD exists"

# ============================================================================
echo
echo "8 — no current symlink, aeons live: release step is skipped, not refused (sp-zt0ae)"
# ============================================================================
# Production has not run the installed-release cutover (sp-tkds8) yet, so nothing reads
# $SPIRA_RELEASES/current. Absent that symlink, the land must skip packaging/activation
# entirely — including its live-aeon refusal — rather than fail every land the loop makes,
# since the loop always has aeons live.
rm -f "$RELEASES/current"
seed sp-lrel8
HEAD8="$(mk_round round-8 eight.txt v8)"
mk_bins "$HEAD8" v8-binary
export MOCK_AEONS="spira-aeon-abc-prod.service"

out="$(run_q land-local fixq --head "$HEAD8" --members "sp-lrel8:$HEAD8" --worktree "$(bins_wt "$HEAD8")")"; rc=$?
unset MOCK_AEONS
[ "$rc" -eq 0 ] && ok "8: exit 0 despite live aeons — skipped, not refused" \
    || bad "8: exit 0 despite live aeons — skipped, not refused" "got rc=$rc out=$out"
want   "8: names the skip"                   "release step skipped: production runs a checkout" "$out"
nowant "8: never reports an activation"      "activated" "$out"
is     "8: local/main advances to the round head" "$HEAD8" "$(localmain)"
is     "8: still nothing activated — no current symlink" "" "$(current_name)"
is     "8: the bead is closed"               closed "$(field sp-lrel8 status)"

# ============================================================================
echo
echo "9 — current symlink present, aeons live: activates without refusing (sp-zt0ae)"
# ============================================================================
# Once production runs from a release (current exists), the release step must run even
# with aeons live — the loop never has zero live aeons — so land-local's own call into
# activate.sh must not hit the default live-aeon guard.
ln -s "spira-$HEAD1" "$RELEASES/current"
seed sp-lrel9
HEAD9="$(mk_round round-9 nine.txt v9)"
mk_bins "$HEAD9" v9-binary
export MOCK_AEONS="spira-aeon-abc-prod.service"

out="$(run_q land-local fixq --head "$HEAD9" --members "sp-lrel9:$HEAD9" --worktree "$(bins_wt "$HEAD9")")"; rc=$?
unset MOCK_AEONS
[ "$rc" -eq 0 ] && ok "9: exit 0 — the release step runs despite live aeons" \
    || bad "9: exit 0 — the release step runs despite live aeons" "got rc=$rc out=$out"
want   "9: reports the activated release"    "activated spira-$HEAD9" "$out"
nowant "9: never refuses on the live-aeon guard" "refusing" "$out"
is     "9: current advances to this round's release" "spira-$HEAD9" "$(current_name)"
is     "9: bin/fakebin is this round's own corpus" \
       "v9-binary" "$(cat "$RELEASES/spira-$HEAD9/bin/fakebin" 2>/dev/null)"
is     "9: the bead is closed"               closed "$(field sp-lrel9 status)"

tl_summary
