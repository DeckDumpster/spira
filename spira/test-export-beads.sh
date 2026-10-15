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

run() { env -i HOME="$TMP" PATH="$BIN:/usr/bin:/bin:$PATH" STUB_DIR="$TMP" SPIRA_WIKI="$WIKI" SPIRA_RUN="$RUN" bash "$HERE/export-beads.sh" 2>&1; }
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

r=$(env -i HOME="$TMP" PATH="$BIN:/usr/bin:/bin" SPIRA_WIKI= SPIRA_RUN="$RUN" bash "$HERE/export-beads.sh" 2>&1); wantrc "no wiki is a quiet no-op" 0 $?
