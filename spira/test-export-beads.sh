#!/usr/bin/env bash
# test-export-beads.sh — the wiki bead mirror: content joined to lifecycle state, sorted, committed only on change.
#
# tier: T1
# covers: spira/export-beads.sh systemd/spira-export-beads.* install/src/manifest.rs auron/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-export-beads.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
WIKI="$TMP/wiki"; BIN="$TMP/bin"; RUN="$TMP/run"; mkdir -p "$WIKI" "$BIN" "$RUN"
git -C "$WIKI" init -q && git -C "$WIKI" config user.name t && git -C "$WIKI" config user.email t@t

cat >"$BIN/spira-lc" <<'STUB'
#!/bin/sh
case "$1 $2" in
  "content list") cat "$STUB_DIR/content.json" ;;
  "list-all "*|"list-all ") cat "$STUB_DIR/state.tsv" ;;
  "content memories") echo '{"law-x":"text"}' ;;
  *) exit 2 ;;
esac
STUB
chmod +x "$BIN/spira-lc"
export STUB_DIR="$TMP"
printf '[{"id":"sp-b","title":"B","status":"open"},{"id":"sp-a","title":"A","status":"open"},{"id":"other-1","title":"x"}]' >"$TMP/content.json"
printf 'sp-a\tDROPPED\t\nsp-b\tLANDED\t\nbead:\tDROPPED\t\n' >"$TMP/state.tsv"

run() { PATH="$BIN:$PATH" STUB_DIR="$TMP" SPIRA_TOML="$(tl_layer SPIRA_WIKI="${WIKI_FOR_RUN-$WIKI}" SPIRA_RUN="$RUN")" bash "$HERE/export-beads.sh" 2>&1; }
OUT="$WIKI/raw/spira-beads/spira.jsonl"

r=$(run); wantrc "first run exits 0" 0 $?
is "ids sorted, foreign ids dropped" "sp-a sp-b" "$(grep '"id":"sp-' "$OUT" | python3 -I -c 'import sys,json;print(" ".join(json.loads(l)["id"] for l in sys.stdin))')"
want "lifecycle state joined" '"lifecycle_state":"DROPPED"' "$(grep '"id":"sp-a"' "$OUT")"
want "memories follow" '"_type":"memory"' "$(cat "$OUT")"
is "one commit" 1 "$(git -C "$WIKI" rev-list --count HEAD)"

touch -d '2 days ago' "$OUT"; r=$(run)
want "unchanged pass says so" "unchanged" "$r"
is "no new commit when unchanged" 1 "$(git -C "$WIKI" rev-list --count HEAD)"
age=$(( $(date +%s) - $(stat -c %Y "$OUT") ))
[ "$age" -lt 60 ] && ok "unchanged pass refreshes the mtime the alarm reads" || bad "mtime not refreshed" "age=$age"

printf 'sp-a\tLANDED\t\nsp-b\tLANDED\t\n' >"$TMP/state.tsv"; run >/dev/null
is "state change commits" 2 "$(git -C "$WIKI" rev-list --count HEAD)"

printf '[{"id":"sp-a"}]' >"$TMP/content.json"; before=$(cat "$OUT"); r=$(run); rc=$?
[ "$rc" -ne 0 ] && ok "collapsed store refused" || bad "collapsed store accepted"
is "mirror untouched on refusal" "$before" "$(cat "$OUT")"

printf '[]' >"$TMP/content.json"; r=$(run); rc=$?
[ "$rc" -ne 0 ] && ok "empty store refused" || bad "empty store accepted"

r=$(WIKI_FOR_RUN= run); wantrc "no wiki is a quiet no-op" 0 $?

printf '[{"id":"sp-b","title":"B2"},{"id":"sp-a","title":"A"}]' >"$TMP/content.json"
before=$(git -C "$WIKI" rev-list --count HEAD)
touch "$WIKI/.git/index.lock"; (sleep 2; rm -f "$WIKI/.git/index.lock") &
r=$(SPIRA_EXPORT_GIT_RETRY_WAIT=1 run); wantrc "index.lock held briefly: export still succeeds" 0 $?
wait
is "commit landed after the lock cleared" $((before + 1)) "$(git -C "$WIKI" rev-list --count HEAD)"

printf '[{"id":"sp-b","title":"B3"},{"id":"sp-a","title":"A"}]' >"$TMP/content.json"
git -C "$WIKI" config user.useConfigOnly true; git -C "$WIKI" config --unset user.email
r=$(GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null EMAIL= SPIRA_EXPORT_GIT_RETRY_WAIT=1 run); rc=$?
[ "$rc" -ne 0 ] && ok "real commit failure exits non-zero" || bad "real failure swallowed"
want "journal line names the git command" "git commit failed" "$r"
want "journal line carries git's stderr" "email" "$r"

git -C "$WIKI" config user.email t@t
branch=$(git -C "$WIKI" symbolic-ref --short HEAD)
git init -q --bare "$TMP/origin.git"
git -C "$WIKI" remote add origin "$TMP/origin.git"
timeout 5 git -C "$WIKI" push -q -u origin "$branch" 2>&1
timeout 5 git clone -q "$TMP/origin.git" "$TMP/other" && timeout 5 git -C "$TMP/other" config user.name o && timeout 5 git -C "$TMP/other" config user.email o@o
echo x >"$TMP/other/ingest.txt"; timeout 5 git -C "$TMP/other" add ingest.txt; timeout 5 git -C "$TMP/other" commit -qm "ingest"; timeout 5 git -C "$TMP/other" push -q origin "$branch" 2>&1
printf '[{"id":"sp-b","title":"B4"},{"id":"sp-a","title":"A"}]' >"$TMP/content.json"
r=$(run); wantrc "origin ahead: export integrates it and publishes" 0 $?
is "remote tip is the mirror commit" "$(git -C "$WIKI" log -1 --format=%s)" "$(git -C "$TMP/origin.git" log -1 --format=%s "$branch")"
is "remote's own commit kept beneath it" "ingest" "$(git -C "$TMP/origin.git" log -1 --format=%s "$branch~1")"

timeout 5 git -C "$TMP/other" pull -q --rebase origin "$branch" 2>&1
echo "hand edit" >"$TMP/other/raw/spira-beads/spira.jsonl"
timeout 5 git -C "$TMP/other" commit -qam "hand edit mirror"; timeout 5 git -C "$TMP/other" push -q origin "$branch" 2>&1
printf '[{"id":"sp-b","title":"B5"},{"id":"sp-a","title":"A"}]' >"$TMP/content.json"
r=$(run); rc=$?
[ "$rc" -ne 0 ] && ok "conflicting mirror edit on origin is a refusal" || bad "conflict swallowed"
want "refusal carries git's output" "export-beads: git" "$r"
is "no rebase left half-done" "" "$(ls -d "$WIKI/.git/rebase-merge" "$WIKI/.git/rebase-apply" 2>/dev/null)"
