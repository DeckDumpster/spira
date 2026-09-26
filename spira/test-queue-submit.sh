#!/usr/bin/env bash
# test-queue-submit.sh — queue.sh submit: certifies any branch the batcher can adopt,
# refuses the rest. Merged from test-submit.sh (sp-s088v.16, duplicate cluster #17,
# UC-27): cmd_submit never calls bd for a bead-less branch (only land_mark + gate.sh),
# so the merge drops test-submit.sh's testdb dependency along with the duplicated
# REPO/RMAP/run() scaffolding both files built independently.
#
# In queue mode, batch.sh builds only from refs/heads/spira/<id>. A branch not
# in that form must be refused before any state is written. The queue-dir write's
# exit status must also be checked so a write failure does not silently produce
# a "certified" result (gap G6).
#
# covers: spira/queue.sh spira/suites.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-queue-submit.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

GATE_LOG="$TMP/gate-calls.log"

cp -r "$HERE" "$TMP/spira"
cat > "$TMP/spira/gate.sh" <<FAKE
#!/usr/bin/env bash
printf 'gate-called branch=%s\n' "\${1:-}" >> "$GATE_LOG"
exit 0
FAKE
chmod +x "$TMP/spira/gate.sh"

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m "init"

RMAP="$TMP/repo-map"
printf 'fixq | %s | queue | main | | |\n' "$REPO" > "$RMAP"

run() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME_REPO=fixq \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_QUEUE_DIR="$TMP/run/queue" \
        SPIRA_REPO_MAP="$RMAP" \
        bash "$TMP/spira/queue.sh" "$@" 2>&1
}

git -C "$REPO" branch "spira/sp-abc01" main
git -C "$REPO" branch "spira-suite-state/test-foo-20260101000000" main

echo
echo "positive control — gate is reachable for a valid spira/<id> branch (bead-less, UC-27):"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
: > "$GATE_LOG"
out="$(run submit spira/sp-abc01)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 for valid branch" || bad "exit 0 for valid branch" "got rc=$rc out=$out"
want "gate stub was called for valid branch" "gate-called" "$(cat "$GATE_LOG")"
want "exit 0 for valid branch" "certified" "$out"
case "$(cat "$TMP/run/landstate/sp-abc01" 2>/dev/null)" in
    CERTIFIED*) ok "bead-less: landstate is CERTIFIED" ;;
    *)          bad "bead-less: landstate is CERTIFIED" "got: [$(cat "$TMP/run/landstate/sp-abc01" 2>/dev/null)]" ;;
esac
[ -f "$TMP/run/queue/sp-abc01" ] && ok "bead-less: queue record written" \
    || bad "bead-less: queue record written" "file missing: $TMP/run/queue/sp-abc01"
rm -rf "$TMP/run"

echo
echo "red branch fails submission; no landstate written:"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
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
[ ! -f "$TMP/run/landstate/sp-red01" ] && ok "red branch: no landstate" \
    || bad "red branch: no landstate" "got: [$(cat "$TMP/run/landstate/sp-red01" 2>/dev/null)]"
# Restore the passing gate stub for the remaining cases.
cat > "$TMP/spira/gate.sh" <<FAKE
#!/usr/bin/env bash
printf 'gate-called branch=%s\n' "\${1:-}" >> "$GATE_LOG"
exit 0
FAKE
chmod +x "$TMP/spira/gate.sh"
rm -rf "$TMP/run"

echo
echo "gap G6: queue-dir write fails — no 'certified', no silent success:"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
git -C "$REPO" branch "spira/sp-g6-01" main 2>/dev/null || true
# Pre-create the queue-dir target AS A DIRECTORY: the write is `printf ... > path`,
# which fails against a directory the same way a full disk or a permissions error
# would — the write's own exit status is what submit must check, not "did gate pass".
mkdir -p "$TMP/run/queue/sp-g6-01"
: > "$GATE_LOG"
out="$(run submit spira/sp-g6-01)"; rc=$?
[ "$rc" -ne 0 ] && ok "G6: exits non-zero when the queue-dir write fails" \
    || bad "G6: exits non-zero" "got rc=$rc out=$out"
nowant "G6: never prints certified on a failed write" "certified" "$out"
want   "G6: names the failure" "failed to write queue entry" "$out"
rm -rf "$TMP/run"

echo
echo "spira-suite-state/... transition branch is accepted (not refused like non-spira/):"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
: > "$GATE_LOG"
out="$(run submit spira-suite-state/test-foo-20260101000000)"; rc=$?
want "gate stub was called for suite-state branch" "gate-called" "$(cat "$GATE_LOG")"
want "suite-state branch certified" "certified" "$out"
[ "$rc" -eq 0 ] && ok "exit 0 for suite-state branch" || bad "exit 0 for suite-state branch" "got rc=$rc"
rm -rf "$TMP/run"

echo
echo "submit refuses a non-spira/ branch in queue mode:"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
git -C "$REPO" branch "concierge/sp-swux6" main
out="$(run submit "concierge/sp-swux6")"; rc=$?
[ "$rc" -ne 0 ] && ok "exits non-zero" || bad "exits non-zero" "got rc=$rc"
want   "names required form" "spira/" "$out"
nowant "never says certified" "certified"   "$out"
[ ! -e "$TMP/run/landstate/concierge" ] \
    && ok "no landstate subdir created" \
    || bad "no landstate subdir created" "directory exists"
[ ! -e "$TMP/run/queue/concierge" ] \
    && ok "no queue subdir created" \
    || bad "no queue subdir created" "directory exists"
rm -rf "$TMP/run"

echo
echo "submit allows a spira-suite-state/* transition branch:"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
git -C "$REPO" branch "spira-suite-state/test-q.sh" main
: > "$GATE_LOG"
out="$(run submit spira-suite-state/test-q.sh)"; rc=$?
want "exit 0 for transition branch" "certified" "$out"
want "gate was called for transition branch" "gate-called" "$(cat "$GATE_LOG")"
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

mkdir -p "$TMP/run2/queue" "$TMP/run2/landstate"
: > "$GATE_LOG2"
run2() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME_REPO=holdhome \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$TMP/run2" \
        SPIRA_QUEUE_DIR="$TMP/run2/queue" \
        SPIRA_REPO_MAP="$RMAP2" \
        bash "$TMP/spira2/queue.sh" "$@" 2>&1
}

out="$(run2 submit spira/sp-def02 queuerepo)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 when repo arg names a queue-mode repo" \
    || bad "exit 0 when repo arg names a queue-mode repo" "got rc=$rc out=$out"
want "gate called with queue repo name" "repo=queuerepo" "$(cat "$GATE_LOG2")"
want "certified via named repo" "certified" "$out"
rm -rf "$TMP/run2"

echo
echo "submit uses home repo when no second arg — gate sees home repo name:"
mkdir -p "$TMP/run3/queue" "$TMP/run3/landstate"
: > "$GATE_LOG2"
git -C "$REPO" branch "spira/sp-ghi03" main 2>/dev/null || true

RMAP3="$TMP/repo-map3"
printf 'fixq | %s | queue | main | | |\n' "$REPO" > "$RMAP3"

run3() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME_REPO=fixq \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$TMP/run3" \
        SPIRA_QUEUE_DIR="$TMP/run3/queue" \
        SPIRA_REPO_MAP="$RMAP3" \
        bash "$TMP/spira2/queue.sh" "$@" 2>&1
}
out="$(run3 submit spira/sp-ghi03)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 with no repo arg" || bad "exit 0 with no repo arg" "got rc=$rc out=$out"
want "gate sees home repo name when no arg" "repo=fixq" "$(cat "$GATE_LOG2")"
rm -rf "$TMP/run3"

# ===========================================================================
# suites.sh transitions (quarantine/disable/activate) call queue.sh submit.
# _sts_transition does `cd "$HERE/.." ` internally, so its own spira/ must live
# INSIDE the repo it commits to — a separate fixture from the cases above.
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
git -C "$TREPO" push -q origin main
git -C "$TREPO" fetch -q origin

transition() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$TSH" \
        SPIRA_HOME_REPO=tfixq \
        SPIRA_REPO="$TREPO" \
        SPIRA_RUN="$TRUN" \
        SPIRA_QUEUE_DIR="$TRUN/queue" \
        SPIRA_REPO_MAP="$TSH/repo-map" \
        bash "$TSH/suites.sh" "$@" 2>&1
}
tlandstate() { cat "$TRUN/landstate/${1:-}" 2>/dev/null; }
tqueue_rec()  { cat "$TRUN/queue/${1:-}" 2>/dev/null; }

echo
echo "quarantine: creates a branch, commits, and submits it (certified via queue.sh):"
mkdir -p "$TRUN/queue" "$TRUN/landstate"
: > "$TGATE_LOG"
git -C "$TREPO" checkout -q main 2>/dev/null || true
out="$(transition quarantine test-q.sh sp-xyz "flaky test")"
is "quarantine: gate was called" "1" "$(wc -l < "$TGATE_LOG" | tr -d ' ')"
_qid="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
case "$_qid" in
    spira-suite-state/*) ok "quarantine: branch name on stdout" ;;
    *) bad "quarantine: branch name on stdout" "got: [$_qid]" ;;
esac
case "$(tlandstate "$_qid")" in
    CERTIFIED*) ok "quarantine: transition branch certified" ;;
    *)          bad "quarantine: transition branch certified" "got: [$(tlandstate "$_qid")]" ;;
esac
[ -f "$TRUN/queue/$_qid" ] && ok "quarantine: queue record written" \
    || bad "quarantine: queue record written" "file missing: $TRUN/queue/$_qid"

echo
echo "disable: same pattern:"
: > "$TGATE_LOG"
git -C "$TREPO" checkout -q main 2>/dev/null || true
out="$(transition disable test-d.sh "unsafe in CI")"
is "disable: gate was called" "1" "$(wc -l < "$TGATE_LOG" | tr -d ' ')"
_did="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
case "$_did" in
    spira-suite-state/*) ok "disable: branch name on stdout" ;;
    *) bad "disable: branch name on stdout" "got: [$_did]" ;;
esac
case "$(tlandstate "$_did")" in
    CERTIFIED*) ok "disable: transition branch certified" ;;
    *)          bad "disable: transition branch certified" "got: [$(tlandstate "$_did")]" ;;
esac
[ -f "$TRUN/queue/$_did" ] && ok "disable: queue record written" \
    || bad "disable: queue record written" "file missing: $TRUN/queue/$_did"

echo
echo "activate: same pattern:"
git -C "$TREPO" checkout -q main 2>/dev/null || true
printf 'test-a.sh | disabled | 2026-01-01T00:00:00Z | | fixture\n' >> "$TSH/suite-state"
git -C "$TREPO" add "$TSH/suite-state"
git -C "$TREPO" commit -q --no-gpg-sign -m "fixture: disable test-a.sh for activate test"
: > "$TGATE_LOG"
out="$(transition activate test-a.sh)"
is "activate: gate was called" "1" "$(wc -l < "$TGATE_LOG" | tr -d ' ')"
_aid="$(printf '%s' "$out" | tail -1 | tr -d '[:space:]')"
case "$_aid" in
    spira-suite-state/*) ok "activate: branch name on stdout" ;;
    *) bad "activate: branch name on stdout" "got: [$_aid]" ;;
esac
case "$(tlandstate "$_aid")" in
    CERTIFIED*) ok "activate: transition branch certified" ;;
    *)          bad "activate: transition branch certified" "got: [$(tlandstate "$_aid")]" ;;
esac
[ -f "$TRUN/queue/$_aid" ] && ok "activate: queue record written" \
    || bad "activate: queue record written" "file missing: $TRUN/queue/$_aid"

echo
tl_summary
