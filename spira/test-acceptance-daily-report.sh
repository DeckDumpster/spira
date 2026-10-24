#!/usr/bin/env bash
#
# test-acceptance-daily-report.sh — a red daily acceptance run files one issue per failing
# check carrying the FAIL line verbatim, and a repeat appends instead of filing again.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): case 1 must see an issue created
# before the append case's "no second issue" is believed.
#
# tier: T1
# covers: spira/acceptance-daily-report.sh .github/workflows/acceptance-daily.yml spira/acceptance-ci.sh
# host-reason: fake gh in an isolated PATH dir; no container or database dependency
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SCRIPT="$HERE/acceptance-daily-report.sh"
. "$HERE/testlib.sh"

echo "test-acceptance-daily-report.sh"

[ -x "$SCRIPT" ] || { bad "self-check: acceptance-daily-report.sh must exist and be executable" "missing"; tl_summary; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
BIN="$TMP/bin"; STORE="$TMP/store"
mkdir -p "$BIN" "$STORE"

cat > "$BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
# issue store: $STORE/<n>.title, <n>.body (appended), <n>.labels
case "$1 $2" in
    "label create") exit 0 ;;
    "issue list")
        for t in "$STORE"/*.title; do
            [ -f "$t" ] || continue
            n="$(basename "$t" .title)"
            printf '%s\t%s\n' "$n" "$(cat "$t")"
        done
        exit 0 ;;
    "issue create")
        title=""; labels=""
        shift 2
        while [ $# -gt 0 ]; do
            case "$1" in --title) title="$2"; shift 2 ;; --label) labels="$labels $2"; shift 2 ;; *) shift ;; esac
        done
        n=$(( $(ls "$STORE"/*.title 2>/dev/null | wc -l) + 1 ))
        printf '%s' "$title" > "$STORE/$n.title"
        printf '%s' "$labels" > "$STORE/$n.labels"
        cat > "$STORE/$n.body"
        exit 0 ;;
    "issue comment")
        n="$3"; cat >> "$STORE/$n.body"; printf 'COMMENT\n' >> "$STORE/$n.body"; exit 0 ;;
esac
exit 1
GHEOF
chmod +x "$BIN/gh"

run_report() { env -i PATH="$BIN:/usr/bin:/bin" HOME="$TMP" STORE="$STORE" GH_TOKEN="${TOK-tok}" ${EXTRA:-} \
    bash "$SCRIPT" --log "$1" --sha abc123def456 --run-url https://example.invalid/runs/7 --repo acme/spira; }

LOG="$TMP/red.log"
printf '  PASS  install\n  FAIL  sentinel summons: Access denied for bead content\n  FAIL  probe closes: timed out\n' > "$LOG"

run_report "$LOG" >/dev/null 2>&1; rc=$?
is "red run exits 0 when everything was filed" 0 "$rc"
is "one issue per distinct failing check" 2 "$(ls "$STORE"/*.title | wc -l)"
want "title names the check" "acceptance-daily: sentinel summons" "$(cat "$STORE"/*.title)"
want "body carries the FAIL line verbatim" "FAIL  sentinel summons: Access denied for bead content" "$(cat "$STORE"/*.body)"
N="$(grep -l 'sentinel summons' "$STORE"/*.title | head -1 | xargs basename | sed 's/\.title//')"
want "body carries the commit" "abc123def456" "$(cat "$STORE/$N.body")"
want "body carries the run link" "https://example.invalid/runs/7" "$(cat "$STORE/$N.body")"
want "issue carries spira:accept" "spira:accept" "$(cat "$STORE/$N.labels")"
nowant "body does not carry another check's line" "probe closes" "$(cat "$STORE/$N.body")"

run_report "$LOG" >/dev/null 2>&1
is "a repeat files no second issue" 2 "$(ls "$STORE"/*.title | wc -l)"
want "a repeat appends a comment" "COMMENT" "$(cat "$STORE/$N.body")"

printf '  FAIL  brand new check: nope\n' > "$TMP/red2.log"
run_report "$TMP/red2.log" >/dev/null 2>&1
is "a different failure files its own issue" 3 "$(ls "$STORE"/*.title | wc -l)"

rm -f "$STORE"/*
printf 'building…\nacceptance: container never came up\n' > "$TMP/novd.log"
run_report "$TMP/novd.log" >/dev/null 2>&1
is "no verdict files one issue" 1 "$(ls "$STORE"/*.title | wc -l)"
want "no-verdict issue carries the log tail" "container never came up" "$(cat "$STORE/1.body")"

rm -f "$STORE"/*
for i in 1 2 3 4 5 6 7; do printf '  FAIL  check%s: x\n' "$i"; done > "$TMP/many.log"
run_report "$TMP/many.log" >/dev/null 2>&1
is "issues per run are capped" 5 "$(ls "$STORE"/*.title | wc -l)"

rm -f "$STORE"/*
TOK="" run_report "$LOG" >"$TMP/o" 2>"$TMP/e"; rc=$?
is "no token exits 1" 1 "$rc"
want "no token is named" "ALARM" "$(cat "$TMP/e")"
is "no token files nothing" 0 "$(ls "$STORE"/*.title 2>/dev/null | wc -l)"

WF="$(cat "$HERE/../.github/workflows/acceptance-daily.yml")"
want "workflow runs on a schedule" "schedule:" "$WF"
want "workflow can be dispatched" "workflow_dispatch:" "$WF"
want "workflow reports through the script" "spira/acceptance-daily-report.sh" "$WF"
want "workflow runs fresh-install acceptance from a tarball" "--tarball" "$WF"
nowant "workflow passes no predecessor" "--prev-tag" "$WF"
nowant "workflow never deletes a release" "release delete" "$WF"
nowant "workflow never deletes a tag" "git/refs/tags" "$WF"

tl_summary
