#!/usr/bin/env bash
#
# test-bead-sweep.sh — bead-sweep.sh raises each SUSPECT class once into the inbox, stays silent
# on a healthy board, re-raises a cleared class that returns, and reports a blind pass as unhealthy.
#
# tier: T1
# covers: spira/bead-sweep.sh spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-bead-sweep.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/bin" "$TMP/run"

cat > "$TMP/bin/spira-lc" <<'STUB'
#!/usr/bin/env bash
[ -e "$STUB_DIR/blind" ] && exit 1
case "$*" in
    "list --state LANDED"*) cat "$STUB_DIR/term.json" ;;
    "list --state"*) cat "$STUB_DIR/rows.json" ;;
    "content list"*) cat "$STUB_DIR/content.json" ;;
esac
STUB
chmod +x "$TMP/bin/spira-lc"
export STUB_DIR="$TMP/stub"; mkdir -p "$STUB_DIR"

: > "$TMP/inbox"
NOW="$(date +%s)"; OLD=$((NOW - 3 * 3600))
row() { # row <id> <state> <since> <holds-json> [blocked-json]
    printf '{"bead_id":"%s","state":"%s","since":%s,"holds":"%s","blocked_by":%s,"reason":"r"}' \
        "$1" "$2" "$3" "$(printf '%s' "$4" | sed 's/"/\\"/g')" "${5:-[]}"
}
cont() { printf '{"id":"%s","labels":%s,"issue_type":"task"}' "$1" "$2"; }

board() { # board <rows...> -- <content...>
    local r=() c=() into=r
    for a in "$@"; do [ "$a" = "--" ] && { into=c; continue; }; [ $into = r ] && r+=("$a") || c+=("$a"); done
    (IFS=,; echo "[${r[*]}]") > "$STUB_DIR/rows.json"
    (IFS=,; echo "[${c[*]}]") > "$STUB_DIR/content.json"
}
echo '[{"bead_id":"sp-done","state":"LANDED"}]' > "$STUB_DIR/term.json"

run() {
    tl_config ${SWEEP_HOME_REPO-SPIRA_HOME_REPO=widgets} SPIRA_RUN="$TMP/run" SPIRA_ASK_LABEL=needs-boss SPIRA_CONCIERGE_INBOX="$TMP/inbox"
    env -i PATH="$TMP/bin:$PATH" HOME="$TMP" SPIRA_RUN="$TMP/run" STUB_DIR="$STUB_DIR" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" SPIRA_REPO="${SWEEP_REPO:-$TMP/widgets}" \
        bash "$HERE/bead-sweep.sh" "$@" 2>&1
}

board "$(row sp-ok READY "$NOW" '[]')" -- "$(cont sp-ok '["repo:widgets"]')"
out="$(run --show)"
want "positive control: a ready bead is OK" "OK sp-ok READY" "$out"
case "$out" in *SUSPECT\ sp-*) bad=1 ;; *) bad=0 ;; esac
is "a healthy board has no SUSPECT" "0" "$bad"

board \
    "$(row sp-pois READY "$NOW" '["poison"]')" \
    "$(row sp-ask READY "$NOW" '["ask"]')" \
    "$(row sp-man READY "$OLD" '["manual"]')" \
    "$(row sp-edge READY "$NOW" '[]' '["sp-done"]')" \
    "$(row sp-rw REWORK "$OLD" '[]')" \
    "$(row sp-cert CERTIFIED "$OLD" '[]')" \
    "$(row sp-wait SUBMITTED "$OLD" '[]')" \
    "$(row sp-blk READY "$NOW" '[]' '["sp-pois"]')" \
    "$(row sp-askl READY "$NOW" '[]')" \
    -- \
    "$(cont sp-cert '["repo:other"]')" "$(cont sp-rowless '[]')" "$(cont sp-askl '["needs-boss"]')"
out="$(run --show)"
for c in "SUSPECT sp-pois READY POISONED" "SUSPECT sp-ask READY ASK HOLD" "SUSPECT sp-man READY STALE HOLD" \
         "SUSPECT sp-edge READY STALE EDGE" "SUSPECT sp-rw REWORK UNCLAIMED REWORK" \
         "SUSPECT sp-cert CERTIFIED CERTIFIED outside repo:widgets" "SUSPECT sp-wait SUBMITTED WAITING" \
         "SUSPECT sp-rowless ROWLESS" "BLOCKED sp-blk" "ASK sp-askl"; do
    want "class: $c" "$c" "$out"
done

FUT="$(date -u -d '+10 days' +%F)"; PAST="$(date -u -d '-3 days' +%F)"
rrow() { row "$1" "$2" "$3" "$4" | sed "s/\"reason\":\"r\"/\"reason\":\"$5\"/"; }
board "$(rrow sp-hfut READY "$OLD" '["manual"]' "review-by:$FUT waits on tenancy")" \
    "$(rrow sp-hpast READY "$OLD" '["manual"]' "review-by:$PAST waits on tenancy")" \
    "$(rrow sp-hnone READY "$OLD" '["manual"]' "waits on tenancy")" \
    "$(rrow sp-hnew READY "$NOW" '["manual"]' "waits on tenancy")" --
out="$(run --show)"
want "a manual hold before its review date is HELD, with its reason" "HELD sp-hfut READY manual hold until review $FUT: waits on tenancy" "$out"
want "a manual hold past its review date is SUSPECT" "SUSPECT sp-hpast READY HOLD REVIEW OVERDUE since $PAST (waits on tenancy)" "$out"
want "a manual hold with no review date keeps the stale rule" "SUSPECT sp-hnone READY STALE HOLD manual" "$out"
want "a young hold with no review date stays HELD" "HELD sp-hnew READY holds=['manual']" "$out"

SHA="$TMP/d9f47a657061e16a9df8df8911340ecc293ad1ab"; mkdir -p "$SHA"
MID=$((NOW - 15 * 60))
board "$(row sp-mine CERTIFIED "$MID" '[]')" "$(row sp-theirs CERTIFIED "$MID" '[]')" -- \
    "$(cont sp-mine '["repo:widgets"]')" "$(cont sp-theirs '["repo:other"]')"
out="$(SWEEP_REPO="$SHA" run --show)"
case "$out" in *"SUSPECT sp-mine"*) bad=1 ;; *) bad=0 ;; esac
is "a sha-named release dir raises no SUSPECT on a home-repo bead" "0" "$bad"
want "a foreign CERTIFIED bead still raises it" "SUSPECT sp-theirs CERTIFIED CERTIFIED outside repo:widgets" "$out"
out="$(SWEEP_REPO="$SHA" SWEEP_HOME_REPO=SPIRA_HOME_REPO= run --show)"
want "an undeclared home repo refuses" "SWEEP FAILED" "$out"
board "$(row sp-pois READY "$NOW" '["poison"]')" "$(row sp-ask READY "$NOW" '["ask"]')" \
    "$(row sp-man READY "$OLD" '["manual"]')" "$(row sp-edge READY "$NOW" '[]' '["sp-done"]')" \
    "$(row sp-rw REWORK "$OLD" '[]')" "$(row sp-cert CERTIFIED "$OLD" '[]')" \
    "$(row sp-wait SUBMITTED "$OLD" '[]')" "$(row sp-blk READY "$NOW" '[]' '["sp-pois"]')" \
    "$(row sp-askl READY "$NOW" '[]')" -- \
    "$(cont sp-cert '["repo:other"]')" "$(cont sp-rowless '[]')" "$(cont sp-askl '["needs-boss"]')"
run watch --interval 5 --ticks 1 >/dev/null
n="$(grep -c 'watch:bead-sweep. SUSPECT' "$TMP/inbox")"
is "each SUSPECT reached the inbox once" "8" "$n"
want "inbox line carries the watcher tag" "[watch:bead-sweep] SUSPECT sp-pois" "$(cat "$TMP/inbox")"
case "$(cat "$TMP/inbox")" in *"BLOCKED"*|*"ASK sp-askl"*) bad=1 ;; *) bad=0 ;; esac
is "only SUSPECT is announced" "0" "$bad"

: > "$TMP/inbox"
board "$(row sp-ok READY "$NOW" '[]')" -- "$(cont sp-ok '["repo:widgets"]')"
run watch --interval 5 --ticks 1 >/dev/null
is "a healthy board announces nothing" "0" "$(wc -l < "$TMP/inbox")"

touch "$STUB_DIR/blind"
rm -f "$TMP/run/watchd/bead-sweep.health"
out="$(run watch --interval 5 --ticks 1)"
want "a blind pass says so" "SWEEP FAILED" "$out"
run health >/dev/null; is "a blind sweep is never healthy" "1" "$?"
rm -f "$STUB_DIR/blind"

run watch --interval 7 --ticks 1 >/dev/null
run health >/dev/null; is "health passes after a good pass" "0" "$?"
echo "ok 1 100 7" > "$TMP/run/watchd/bead-sweep.health"
run health >/dev/null; is "health fails once the last pass is stale" "1" "$?"

grep -q '^bead-sweep|daemon|' "$HERE/watchers" && ok=0 || ok=1
is "watchers manifest carries the bead-sweep row" "0" "$ok"


printf 'prrepo | %s/pr | pr | origin/main | | |\n' "$TMP" > "$TMP/land-map"
printf 'widgets | %s/widgets | queue | origin/main | | |\n' "$TMP" >> "$TMP/land-map"
tl_config SPIRA_REPO_MAP="$TMP/land-map"
board "$(row sp-prw SUBMITTED "$OLD" '[]')" "$(row sp-qw SUBMITTED "$OLD" '[]')" \
    -- "$(cont sp-prw '["repo:prrepo"]')" "$(cont sp-qw '["repo:widgets"]')"
out="$(run --show)"
want "positive control: a queue-mode bead past the wait is still WAITING" "SUSPECT sp-qw SUBMITTED WAITING" "$out"
case "$out" in *sp-prw*WAITING*) bad=1 ;; *) bad=0 ;; esac
is "a pr-mode bead is not waiting for a round" "0" "$bad"

tl_summary
