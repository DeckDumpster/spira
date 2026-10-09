#!/usr/bin/env bash
# test-queue-submit.sh — queue.sh submit: certifies any branch the batcher can adopt,
# refuses the rest. Merged from test-submit.sh (sp-s088v.16, duplicate cluster #17,
# UC-27): cmd_submit never calls bd for a bead-less branch (only the lifecycle certify + gate.sh),
# so the merge drops test-submit.sh's testdb dependency along with the duplicated
# REPO/RMAP/run() scaffolding both files built independently.
#
# In queue mode, batch.sh builds only from refs/heads/spira/<id>. A branch not
# in that form must be refused before any state is written. The lifecycle row is the one
# certification record: submit keeps no queue record of its own (sp-lck63 retired the
# per-branch `$SPIRA_QUEUE_DIR/<id>` file, and with it gap G6's write check), and a
# suite-state transition is a change bead on spira/<id> like any other — the bead-less
# `spira-suite-state/*` route is refused.
#
# tier: T2
# covers: queue/src/* testenv/src/suites/* spira/conf.sh UC-landing-merge-queue-27
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4), invoked by name: the tree under test's build is
# on the suite's PATH (sp-gypjk).

. "$HERE/testlib/lc-fixture.sh"

echo "test-queue-submit.sh"
TMP="$(mktemp -d)"; trap 'lcfix_down; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || { echo "test-queue-submit: could not build a lifecycle fixture"; exit 1; }
for _b in sp-abc01 sp-cso01 sp-def02 sp-ghi03; do lcfix_seed "$_b" WORKING; done
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

GATE_LOG="$TMP/gate-calls.log"

cp -r "$HERE" "$TMP/spira"
cat > "$TMP/spira/gate.sh" <<FAKE
#!/usr/bin/env bash
printf 'gate-called branch=%s suites=%s\n' "\${1:-}" "\${SPIRA_GATE_SUITES:-unset}" >> "$GATE_LOG"
exit 0
FAKE
chmod +x "$TMP/spira/gate.sh"

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m "init"

RMAP="$TMP/repo-map"
printf 'fixq | %s | queue | main | | |\n' "$REPO" > "$RMAP"

run() {
    tl_config SPIRA_HOME_REPO=fixq SPIRA_RUN="$TMP/run" SPIRA_QUEUE_DIR="$TMP/run/queue" \
        SPIRA_REPO_MAP="$RMAP"
    env -i $(lcfix_env) PATH="$TMP/spira:$PATH" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO="$REPO" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$TMP/spira" queue "$@" 2>&1
}

git -C "$REPO" branch "spira/sp-abc01" main
git -C "$REPO" branch "spira-suite-state/test-foo-20260101000000" main

echo
echo "certification honours SPIRA_CERTIFY_SUITES=off (fences only), as landing.sh does:"
mkdir -p "$TMP/run/queue"
: > "$GATE_LOG"
git -C "$REPO" branch "spira/sp-cso01" main
tl_config SPIRA_HOME_REPO=fixq SPIRA_RUN="$TMP/run" SPIRA_QUEUE_DIR="$TMP/run/queue" \
    SPIRA_REPO_MAP="$RMAP" SPIRA_CERTIFY_SUITES=off
env -i $(lcfix_env) PATH="$TMP/spira:$PATH" HOME="$TMP" SPIRA_CONF=/nonexistent \
    SPIRA_REPO="$REPO" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_HOME="$TMP/spira" queue submit spira/sp-cso01 >/dev/null 2>&1
want "the gate is handed suites=off" "suites=off" "$(cat "$GATE_LOG")"

echo
echo "positive control — gate is reachable for a valid spira/<id> branch (bead-less, UC-27):"
mkdir -p "$TMP/run/queue"
: > "$GATE_LOG"
out="$(run submit spira/sp-abc01)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 for valid branch" || bad "exit 0 for valid branch" "got rc=$rc out=$out"
want "gate stub was called for valid branch" "gate-called" "$(cat "$GATE_LOG")"
want "exit 0 for valid branch" "certified" "$out"
is "bead-less: the lifecycle row is CERTIFIED" "CERTIFIED" "$(lcfix_state sp-abc01)"
[ ! -e "$TMP/run/queue/sp-abc01" ] && ok "the queue keeps no certification record of its own" \
    || bad "the queue keeps no certification record of its own" "found: $TMP/run/queue/sp-abc01"
rm -rf "$TMP/run"

echo
echo "red branch fails submission; no lifecycle row written:"
mkdir -p "$TMP/run/queue"
git -C "$REPO" branch "spira/sp-red01" main 2>/dev/null || true
cat > "$TMP/spira/gate.sh" <<FAKERED
#!/usr/bin/env bash
printf 'gate-called branch=%s\n' "\${1:-}" >> "$GATE_LOG"
exit 1
FAKERED
chmod +x "$TMP/spira/gate.sh"
: > "$GATE_LOG"
out="$(run submit spira/sp-red01)"; rc=$?
[ "$rc" -ne 0 ] && ok "red branch: exits non-zero" || bad "red branch: exits non-zero" "got rc=$rc"
want "red branch: failure reported" "failed the gate" "$out"
is "red branch: no lifecycle row" "" "$(lcfix_state sp-red01)"
# Restore the passing gate stub for the remaining cases.
cat > "$TMP/spira/gate.sh" <<FAKE
#!/usr/bin/env bash
printf 'gate-called branch=%s\n' "\${1:-}" >> "$GATE_LOG"
exit 0
FAKE
chmod +x "$TMP/spira/gate.sh"
rm -rf "$TMP/run"

echo
echo "a bead-less spira-suite-state/* branch is refused (sp-lck63: no route around spira-lc):"
mkdir -p "$TMP/run/queue"
: > "$GATE_LOG"
out="$(run submit spira-suite-state/test-foo-20260101000000)"; rc=$?
[ "$rc" -ne 0 ] && ok "suite-state branch: exits non-zero" || bad "suite-state branch: exits non-zero" "got rc=$rc out=$out"
want   "suite-state branch: names the required form" "requires a branch under spira/" "$out"
nowant "suite-state branch: never says certified" "certified" "$out"
is     "suite-state branch: the gate never ran" "" "$(cat "$GATE_LOG")"
[ ! -e "$TMP/run/queue/spira-suite-state" ] && ok "suite-state branch: no queue record" \
    || bad "suite-state branch: no queue record" "found $TMP/run/queue/spira-suite-state"
rm -rf "$TMP/run"

echo
echo "submit refuses a non-spira/ branch in queue mode:"
mkdir -p "$TMP/run/queue"
git -C "$REPO" branch "concierge/sp-swux6" main
out="$(run submit "concierge/sp-swux6")"; rc=$?
[ "$rc" -ne 0 ] && ok "exits non-zero" || bad "exits non-zero" "got rc=$rc"
want   "names required form" "spira/" "$out"
nowant "never says certified" "certified"   "$out"
[ ! -e "$TMP/run/queue/concierge" ] \
    && ok "no queue subdir created" \
    || bad "no queue subdir created" "directory exists"
rm -rf "$TMP/run"

echo
echo "submit uses the given repo name, not the home repo, for mode and gate:"
REPO2="$TMP/repo2"
git init -q -b main "$REPO2"
git -C "$REPO2" commit -q --allow-empty -m "init"
git -C "$REPO2" branch "spira/sp-def02" main

GATE_LOG2="$TMP/gate-calls2.log"
cp -r "$HERE" "$TMP/spira2"
cat > "$TMP/spira2/gate.sh" <<FAKE2
#!/usr/bin/env bash
printf 'gate-called branch=%s repo=%s\n' "\${1:-}" "\${2:-}" >> "$GATE_LOG2"
exit 0
FAKE2
chmod +x "$TMP/spira2/gate.sh"

RMAP2="$TMP/repo-map2"
printf 'holdhome | %s | hold | main | | |\n' "$REPO" > "$RMAP2"
printf 'queuerepo | %s | queue | main | | |\n' "$REPO2" >> "$RMAP2"

mkdir -p "$TMP/run2/queue"
: > "$GATE_LOG2"
run2() {
    tl_config SPIRA_HOME_REPO=holdhome SPIRA_RUN="$TMP/run2" SPIRA_QUEUE_DIR="$TMP/run2/queue" \
        SPIRA_REPO_MAP="$RMAP2"
    env -i $(lcfix_env) PATH="$TMP/spira2:$PATH" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO="$REPO" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$TMP/spira2" queue "$@" 2>&1
}

out="$(run2 submit spira/sp-def02 queuerepo)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 when repo arg names a queue-mode repo" \
    || bad "exit 0 when repo arg names a queue-mode repo" "got rc=$rc out=$out"
want "gate called with queue repo name" "repo=queuerepo" "$(cat "$GATE_LOG2")"
want "certified via named repo" "certified" "$out"
rm -rf "$TMP/run2"

echo
echo "submit uses home repo when no second arg — gate sees home repo name:"
mkdir -p "$TMP/run3/queue"
: > "$GATE_LOG2"
git -C "$REPO" branch "spira/sp-ghi03" main 2>/dev/null || true

RMAP3="$TMP/repo-map3"
printf 'fixq | %s | queue | main | | |\n' "$REPO" > "$RMAP3"

run3() {
    tl_config SPIRA_HOME_REPO=fixq SPIRA_RUN="$TMP/run3" SPIRA_QUEUE_DIR="$TMP/run3/queue" \
        SPIRA_REPO_MAP="$RMAP3"
    env -i $(lcfix_env) PATH="$TMP/spira2:$PATH" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO="$REPO" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$TMP/spira2" queue "$@" 2>&1
}
out="$(run3 submit spira/sp-ghi03)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 with no repo arg" || bad "exit 0 with no repo arg" "got rc=$rc out=$out"
want "gate sees home repo name when no arg" "repo=fixq" "$(cat "$GATE_LOG2")"
rm -rf "$TMP/run3"

# ===========================================================================
# testenv suites transitions (quarantine/disable/activate) call queue submit
# (testenv/DESIGN-suites.md §9: suites.sh is `testenv suites` now). The harness root is
# pinned to the fixture repo (SPIRA_TESTENV_HARNESS), so its spira/ lives INSIDE the repo it
# commits to — a separate fixture from the cases above.
# ===========================================================================
TREPO="$TMP/trepo"
TSH="$TREPO/spira"
TREMOTE="$TMP/tremote.git"
TRUN="$TMP/trun"
TGATE_LOG="$TMP/tgate-calls.log"

git init -q --bare -b main "$TREMOTE"
git init -q -b main "$TREPO"
mkdir -p "$TSH" "$TRUN/worktree"
cp "$HERE"/*.sh "$HERE"/*.py "$TSH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$TSH/"
printf '#!/usr/bin/env bash\n# covers: spira/queue.sh\nset -uo pipefail\nprintf ok\n' > "$TSH/test-q.sh"; chmod +x "$TSH/test-q.sh"
printf '#!/usr/bin/env bash\n# covers: spira/queue.sh\nset -uo pipefail\nprintf ok\n' > "$TSH/test-d.sh"; chmod +x "$TSH/test-d.sh"
printf '#!/usr/bin/env bash\n# covers: spira/queue.sh\nset -uo pipefail\nprintf ok\n' > "$TSH/test-a.sh"; chmod +x "$TSH/test-a.sh"
: > "$TSH/suite-state"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TSH/confine.sh"; chmod +x "$TSH/confine.sh"
cat > "$TSH/gate.sh" <<TFAKE
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$TGATE_LOG"
exit 0
TFAKE
chmod +x "$TSH/gate.sh"
cat > "$TSH/repo-map" <<TRMAP
tfixq | $TREPO | queue | origin/main | | |
TRMAP

git -C "$TREPO" add -A
git -C "$TREPO" commit -q --allow-empty -m base
git -C "$TREPO" remote add origin "$TREMOTE"
timeout 5 git -C "$TREPO" push -q origin main
timeout 5 git -C "$TREPO" fetch -q origin

transition() {
    tl_config SPIRA_HOME_REPO=tfixq SPIRA_RUN="$TRUN" SPIRA_QUEUE_DIR="$TRUN/queue" \
        SPIRA_REPO_MAP="$TSH/repo-map"
    env -i $(lcfix_env) PATH="$TSH:$PATH" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$TSH" \
        SPIRA_REPO="$TREPO" \
        SPIRA_TESTENV_HARNESS="$TREPO" \
        SPIRA_TOML="$SPIRA_TOML" \
        testenv suites "$@" 2>&1
}

# sp-lck63: the transition files its change bead through bead.sh (a stub here: no beads
# store in this fixture), claims it through lib.sh's lc_claim_bead against the lifecycle
# fixture, and submits spira/<bead>.
TBEAD_LOG="$TMP/tbead-calls.log"
cat > "$TSH/bead.sh" <<TBEAD
#!/usr/bin/env bash
n=\$(( \$(cat "$TMP/tbead-n" 2>/dev/null || echo 0) + 1 ))
printf '%s\n' "\$n" > "$TMP/tbead-n"
printf '%s\n' "\$*" >> "$TBEAD_LOG"
printf 'advisory: similar to sp-decoy1\n{"id":"sp-ssc%02d","title":"t"}\n' "\$n"
TBEAD
chmod +x "$TSH/bead.sh"

echo
echo "quarantine: files a change bead, claims it, commits on spira/<bead> and certifies it on spira-lc:"
mkdir -p "$TRUN/queue"
: > "$TGATE_LOG"
git -C "$TREPO" checkout -q main 2>/dev/null || true
_thead="$(git -C "$TREPO" rev-parse HEAD)"
out="$(transition quarantine test-q.sh sp-xyz "flaky test")"
is "quarantine: the fixture checkout's HEAD did not move (D2: no checkout)" \
   "$_thead" "$(git -C "$TREPO" rev-parse HEAD)"
is "quarantine: gate was called" "1" "$(wc -l < "$TGATE_LOG" | tr -d ' ')"
_qbr="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
is "quarantine: the change bead's branch on stdout" "spira/sp-ssc01" "$_qbr"
want "quarantine: the bead was filed --submitted for ops in the home repo" \
    "file suite-state: test-q.sh -> quarantined --for ops --repo tfixq --submitted" "$(cat "$TBEAD_LOG" 2>/dev/null)"
_qtip="$(git -C "$TREPO" rev-parse -q --verify "refs/heads/$_qbr" 2>/dev/null)"
[ -n "$_qtip" ] && ok "quarantine: spira/<bead> exists in the fixture repo" \
    || bad "quarantine: spira/<bead> exists in the fixture repo" "no ref refs/heads/$_qbr"
want "quarantine: the commit carries the suite's new row" "test-q.sh | quarantined" \
    "$(git -C "$TREPO" show "$_qtip:spira/suite-state" 2>/dev/null)"
want "quarantine: the commit cites the change bead" "sp-ssc01: suite-state: test-q.sh -> quarantined" \
    "$(git -C "$TREPO" log -1 --format=%s "$_qtip" 2>/dev/null)"
is "quarantine: the change bead's lifecycle row is CERTIFIED" "CERTIFIED" "$(lcfix_state sp-ssc01)"
is "quarantine: certified at the branch tip" "$_qtip" "$(lcfix_tip sp-ssc01)"
[ ! -e "$TRUN/queue/sp-ssc01" ] && ok "quarantine: the queue keeps no record of its own" \
    || bad "quarantine: the queue keeps no record of its own" "found $TRUN/queue/sp-ssc01"
is "quarantine: no bead-less spira-suite-state/* branch" "" \
    "$(git -C "$TREPO" for-each-ref --format='%(refname)' refs/heads/spira-suite-state/)"

echo
echo "disable: a handed change bead (--change-bead) is claimed and certified; none is filed:"
: > "$TGATE_LOG"
: > "$TBEAD_LOG"
git -C "$TREPO" checkout -q main 2>/dev/null || true
out="$(transition disable test-d.sh "unsafe in CI" --change-bead sp-sshand1)"
is "disable: gate was called" "1" "$(wc -l < "$TGATE_LOG" | tr -d ' ')"
_dbr="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
is "disable: the handed bead's branch on stdout" "spira/sp-sshand1" "$_dbr"
is "disable: no bead was filed" "" "$(cat "$TBEAD_LOG")"
is "disable: the handed bead's lifecycle row is CERTIFIED" "CERTIFIED" "$(lcfix_state sp-sshand1)"
is "disable: certified at the branch tip" "$(git -C "$TREPO" rev-parse "refs/heads/$_dbr" 2>/dev/null)" "$(lcfix_tip sp-sshand1)"

echo
echo "activate: same pattern, a second filed bead:"
git -C "$TREPO" checkout -q main 2>/dev/null || true
printf 'test-a.sh | disabled | 2026-01-01T00:00:00Z | | fixture\n' >> "$TSH/suite-state"
git -C "$TREPO" add "$TSH/suite-state"
git -C "$TREPO" commit -q --no-gpg-sign -m "fixture: disable test-a.sh for activate test"
: > "$TGATE_LOG"
out="$(transition activate test-a.sh)"
is "activate: gate was called" "1" "$(wc -l < "$TGATE_LOG" | tr -d ' ')"
_abr="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
is "activate: the change bead's branch on stdout" "spira/sp-ssc02" "$_abr"
is "activate: the change bead's lifecycle row is CERTIFIED" "CERTIFIED" "$(lcfix_state sp-ssc02)"

echo
tl_summary
