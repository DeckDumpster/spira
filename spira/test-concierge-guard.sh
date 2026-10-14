#!/usr/bin/env bash
#
# test-concierge-guard.sh — the guard's timer is registered and fenced, and the built binary,
# against a fixture /proc, writes one inbox row on the second consecutive miss and never invokes tmux,
# and mails the operator once when the lapse persists past --mail-after.
#
# tier: T0
# covers: concierge-guard/* systemd/spira-concierge-guard.* spira/watchers install/src/manifest.rs install/tests/units_lint.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-concierge-guard.service"
TMR="$UNIT_DIR/spira-concierge-guard.timer"

echo "=== units ==="
want "it is a oneshot" "Type=oneshot" "$(grep '^Type=' "$SVC" 2>/dev/null)"
want "ExecStart runs the guard's check" "concierge-guard check --state @SPIRA_RUN@/concierge-guard.state" "$(grep '^ExecStart=' "$SVC" 2>/dev/null)"
_quota_re='^CPUQuota=[0-9]+%$'
is "positive control: the quota matcher finds a planted CPUQuota" "1" "$(printf 'CPUQuota=35%%\n' | grep -cE "$_quota_re")"
is "it is CPU-fenced" "1" "$(grep -cE "$_quota_re" "$SVC" 2>/dev/null)"
want "timer repeats" "OnUnitActiveSec=3min" "$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null)"
want "timer arms itself at boot" "OnBootSec=" "$(grep '^OnBootSec=' "$TMR" 2>/dev/null)"
want "timer names the service" "spira-concierge-guard.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"
if command -v systemd-analyze >/dev/null 2>&1; then
    want "the repeat interval parses as a timespan" "μs" "$(systemd-analyze timespan "$(grep '^OnUnitActiveSec=' "$TMR" | cut -d= -f2-)" 2>&1)"
fi
want "the watchers file has a row with a health command" "concierge-guard|log|@SPIRA_RUN@/concierge-guard.log|concierge-guard health" "$(grep '^concierge-guard|' "$HERE/watchers")"

union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "service is in UNITS" "spira-concierge-guard.service" "$units_block"
want "timer is enabled" "spira-concierge-guard.timer" "$enable_block"

echo "=== binary ==="
BIN=concierge-guard
command -v "$BIN" >/dev/null 2>&1 || { bad "concierge-guard is on PATH" "not found"; tl_summary; exit 1; }
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
PROC="$T/proc"; ST="$T/state"; KEYS="$T/keys.log"; MAILS="$T/mails.log"

printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> %q\n' "$KEYS" > "$T/tmux"
printf '#!/usr/bin/env bash\nprintf "ARGS %%s\\n" "$*" >> %q\ncat >> %q\nprintf "ENV %%s\\n" "${SPIRA_MAIL_OPERATOR_CONSIDERED:-}" >> %q\n' "$MAILS" "$MAILS" "$MAILS" > "$T/mail"
chmod +x "$T/tmux" "$T/mail"
printf 'older\nlast inbox line\n' > "$T/inbox.log"

mkproc() {   # mkproc <pid> <ppid> <argv...>
    local pid="$1" ppid="$2"; shift 2
    mkdir -p "$PROC/$pid"
    printf '%s (x y) S %s 0 0\n' "$pid" "$ppid" > "$PROC/$pid/stat"
    printf '%s\0' "$@" > "$PROC/$pid/cmdline"
}
session() {  # session <name>
    rm -rf "$PROC"; mkdir -p "$PROC"
    mkproc 100 1 claude --model x --remote-control "$1"
    printf 'TMUX=%s,4242,0\0TMUX_PANE=%%9\0' "$T/tmux.sock" > "$PROC/100/environ"
}
guard() {
    PATH="$T:$PATH" "$BIN" check --proc-root "$PROC" --state "$ST" --session concierge --waiter inbox-next --waiter "watchd next concierge-inbox" \
        --inbox-text "re-arm now" --mail-after 600 --inbox "$T/inbox.log" --mail "$T/mail" --from "Guard <g@spira>"
}
sets() { sed -i "s/^$1=.*/$1=$2/" "$ST"; }
rows() { wc -l < "$T/inbox.log" | tr -d ' '; }

session concierge; mkproc 101 100 bash -c snapshot; mkproc 102 101 bash /run/concierge-notes/inbox-next.sh
guard >/dev/null; guard >/dev/null
is "a waiter descendant: no inbox row" "2" "$(rows)"
want "listening is recorded" "listening=yes" "$(cat "$ST")"

session concierge; rm -f "$ST"
guard >/dev/null
is "no waiter, first check: no row" "2" "$(rows)"
guard >/dev/null
is "second check: one inbox row" "3" "$(rows)"
want "the row carries the text" "re-arm now" "$(tail -1 "$T/inbox.log")"
guard >/dev/null
is "third check: no second row" "3" "$(rows)"
is "no mail yet" "" "$(cat "$MAILS" 2>/dev/null)"

sets missing_since "$(( $(date +%s) - 700 ))"
guard >/dev/null
want "a lapse ten minutes on mails the operator a note" "--kind note" "$(cat "$MAILS" 2>/dev/null)"
want "the mail carries the last inbox lines" "re-arm now" "$(cat "$MAILS" 2>/dev/null)"
want "the mail names the consideration that lets it reach the operator" "ENV concierge-guard" "$(cat "$MAILS" 2>/dev/null)"
guard >/dev/null
is "exactly one mail" "1" "$(grep -c '^ARGS' "$MAILS")"
is "tmux was never invoked, through the whole lapse" "" "$(cat "$KEYS" 2>/dev/null)"
is "positive control: the tmux stub records an invocation" "x" "$(PATH="$T:$PATH" tmux x; cat "$KEYS" | tr -d '\n')"
: > "$KEYS"

echo "=== positive control: a differing argv is not the session ==="
session other; rm -f "$ST" "$KEYS"
guard >/dev/null; guard >/dev/null
is "no wake for another session" "" "$(cat "$KEYS" 2>/dev/null)"
want "and no session is recorded" "session=0" "$(cat "$ST")"

echo "=== health ==="
session concierge; mkproc 101 100 inbox-next.sh; rm -f "$ST"; guard >/dev/null
"$BIN" health --state "$ST" --max-age 600 >/dev/null; wantrc "listening and fresh: health passes" 0 "$?"
sets listening no
"$BIN" health --state "$ST" --max-age 600 >/dev/null; wantrc "not listening: health fails" 1 "$?"
sets listening yes; sets checked 1
"$BIN" health --state "$ST" --max-age 600 >/dev/null; wantrc "a stale check: health fails" 1 "$?"

tl_summary
