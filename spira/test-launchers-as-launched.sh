#!/usr/bin/env bash
# test-launchers-as-launched.sh — the commands the harness hands to something outside itself,
# run the way that something runs them: the command text the harness wrote, in the environment
# the client gives it, never a path this suite assembled. Each case also runs the same command
# broken, because a launcher that passes either way proves nothing.
#
# hermetic-ok: its own TMUX_TMPDIR server and throwaway HOMEs; touches no operator state
# tier: T2
# covers: aerc/accounts.conf install/src/bin/install.rs mail/src/sendmail.rs release/src/session_hook.rs spira/ctx-meter.sh spira/hooks/session.sh cockpit/ops/src/layout.rs cockpit/ops/src/health_main.rs UC-operator-channel-46 UC-operator-channel-47 UC-operator-channel-48 UC-cockpit-observability-51
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

for b in mail spira-lc spira-config watchd release health layout cockpit-collect spira-install python3 tmux; do
    command -v "$b" >/dev/null 2>&1 || bail "$b is not on PATH"
done

echo "test-launchers-as-launched.sh"
TMP="$(mktemp -d)"
trap 'TMUX_TMPDIR="$TMP/tmux" tmux kill-server 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/releases" "$TMP/stubs" "$TMP/run" "$TMP/tmux"

R="$TMP/releases/rel1"
mkdir -p "$R/bin"
for b in mail spira-lc spira-config watchd release health layout cockpit-collect inbox-triage; do
    p="$(command -v "$b" 2>/dev/null)" && cp "$p" "$R/bin/$b"
done
ln -s "$ROOT/spira" "$R/spira"
ln -s "$R" "$TMP/releases/current"

printf 'probe|daemon|/bin/true\n' > "$TMP/watchers"
tl_config SPIRA_RELEASES="$TMP/releases" SPIRA_PROD="$R/spira" SPIRA_RUN="$TMP/run" \
    SPIRA_MAIL="$TMP/mail" SPIRA_DB="$TMP/nodb" SPIRA_WATCHERS="$TMP/watchers" \
    SPIRA_WATCHERS_OVERLAY="$TMP/no-overlay" SPIRA_CLIENT_SETTINGS="$TMP/settings.json" \
    SPIRA_COCKPIT="$TMP/cockpit"

note() { printf '# %s\n' "$*" >> "$TMP/notes"; }
client_env() { env -i HOME="$1" PATH=/usr/bin:/bin bash -c "$2"; }

# --- UC-operator-channel-46: aerc's outgoing line -------------------------------------------
for s in duckdb inotifywait; do printf '#!/bin/sh\nexit 0\n' > "$TMP/stubs/$s"; done
printf '#!/bin/sh\necho true\n' > "$TMP/stubs/dolt"
printf '#!/bin/sh\necho "  WebDAV: true"\n' > "$TMP/stubs/sccache"
printf '#!/bin/sh\necho "doctor stub: stop after the dependency phase"\nexit 1\n' > "$TMP/stubs/doctor"
chmod +x "$TMP/stubs/"*

AH="$TMP/aerc-home"
mkdir -p "$AH/.config/aerc" "$TMP/mail/concierge/"{new,cur,tmp} "$TMP/mail/operator/"{new,cur,tmp}
sed "s#maildir:///path/to/SPIRA_MAIL#maildir://$TMP/mail#" "$ROOT/aerc/accounts.conf" > "$AH/.config/aerc/accounts.conf"
env -i HOME="$AH" PATH="$TMP/stubs:/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" SPIRA_RELEASES="$TMP/releases" \
    SPIRA_INSTALL_AERC_CONSIDERED=1 "$(command -v spira-install)" >"$TMP/install.out" 2>&1
OUTGOING="$(sed -n 's/^outgoing *= *//p' "$AH/.config/aerc/accounts.conf" | head -1)"
case "$OUTGOING" in "$AH/.local/bin/spira-sendmail") ok "install repoints aerc's outgoing at spira-sendmail" ;;
    *) bad "install repoints aerc's outgoing at spira-sendmail" "outgoing=$OUTGOING; install said: $(tail -5 "$TMP/install.out")" ;; esac

id="$(date +%s).$$.$RANDOM"
printf 'From: Operator <operator@spira>\nSubject: Please reply\nMessage-ID: <%s@spira>\n\nbody\n' "$id" > "$TMP/mail/concierge/cur/$id"
reply="$(printf 'In-Reply-To: <%s@spira>\nFrom: Concierge <concierge@spira>\nSubject: Re: Please reply\n\nDone.\n' "$id")"

(rm -f "$R/bin/mail"; out="$(printf '%s\n' "$reply" | client_env "$AH" "$OUTGOING -f concierge@spira operator@spira" 2>&1)"; rc=$?
 [ "$rc" != 0 ] && echo "ok-red" || echo "no-red:$out") > "$TMP/red46"
[ "$(cat "$TMP/red46")" = ok-red ] && ok "SEEN RED: with the release's mail gone the outgoing line fails" || bad "SEEN RED: with the release's mail gone the outgoing line fails" "$(cat "$TMP/red46")"
cp "$(command -v mail)" "$R/bin/mail"

out="$(printf '%s\n' "$reply" | client_env "$AH" "$OUTGOING -f concierge@spira operator@spira" 2>&1)"; rc=$?
wantrc "the outgoing line sends the reply from aerc's environment" 0 "$rc"
[ "$rc" = 0 ] || note "outgoing said: $out"
is "the original is marked answered" 1 "$(ls "$TMP/mail/concierge/cur" | grep -c ':2,.*R')"
is "the reply was delivered" 1 "$(grep -l '^Done\.$' "$TMP/mail"/*/new/* "$TMP/mail"/*/cur/* 2>/dev/null | wc -l | tr -d ' ')"

# --- UC-operator-channel-47/48: the commands session-hook registers -------------------------
out="$(env -i HOME="$TMP/home" PATH="$R/bin:/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" SPIRA_RELEASE="$R" release session-hook install 2>&1)"; rc=$?
wantrc "release session-hook install registers" 0 "$rc"
[ "$rc" = 0 ] || note "install said: $out"
registered() { python3 -I -c '
import json,sys
d=json.load(open(sys.argv[1]))
if sys.argv[2]=="status": print(d["statusLine"]["command"])
else: print(d["hooks"]["SessionStart"][0]["hooks"][0]["command"])' "$TMP/settings.json" "$1"; }
METER="$(registered status)"; HOOK="$(registered hook)"
[ -n "$METER" ] && ok "a statusLine command is registered" || bad "a statusLine command is registered" "$(cat "$TMP/settings.json" 2>&1)"
[ -n "$HOOK" ] && ok "a SessionStart command is registered" || bad "a SessionStart command is registered" "$(cat "$TMP/settings.json" 2>&1)"

mkdir -p "$TMP/home"
meter_in='{"context_window": {"current_usage": {}, "total_input_tokens": 12345}}'
out="$(printf '%s' "$meter_in" | client_env "$TMP/home" "$METER" 2>&1)"; rc=$?
wantrc "the statusLine command exits 0 as the client runs it" 0 "$rc"
[ "$rc" = 0 ] || note "statusLine said: $out; command: $METER"
[ -n "$out" ] && ok "and prints the meter" || bad "and prints the meter" "no output"
case "$out" in *SPIRA_TOML*) bad "and names no missing config" "$out" ;; *) ok "and names no missing config" ;; esac

unconfigured() { printf '%s' "$1" | sed -E 's# SPIRA_TOML=[^ ]*##'; }
out="$(printf '%s' "$meter_in" | client_env "$TMP/home" "$(unconfigured "$METER")" 2>&1)"; rc=$?
[ "$rc" != 0 ] && ok "SEEN RED: the same command without SPIRA_TOML refuses" || bad "SEEN RED: the same command without SPIRA_TOML refuses" "exit 0: $out"

hook_in='{"hook_event_name":"SessionStart","source":"startup"}'
out="$(printf '%s' "$hook_in" | client_env "$TMP/home" "$HOOK" 2>&1)"; rc=$?
wantrc "the SessionStart command exits 0 as the client runs it" 0 "$rc"
[ "$rc" = 0 ] || note "SessionStart said: $out; command: $HOOK"
want "and prints the watcher summary" "probe" "$out"
out="$(printf '%s' "$hook_in" | client_env "$TMP/home" "$(unconfigured "$HOOK")" 2>&1)"
nowant "SEEN RED: the same command without SPIRA_TOML prints no summary" "probe" "$out"

# --- UC-cockpit-observability-51: the ops pane's start command -------------------------------
printf '#!/bin/sh\ncase "$*" in *is-enabled*) exit 0 ;; *is-active*) echo active ;; esac\nexit 0\n' > "$TMP/stubs/systemctl"
chmod +x "$TMP/stubs/systemctl"
mkdir -p "$TMP/cockpit"
SNAP="$TMP/snapshot.env"
env -i PATH="$R/bin:/usr/bin:/bin" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_RELEASE="$R" SPIRA_COCKPIT_FORCE=1 \
    cockpit-collect once >/dev/null 2>&1
note "snapshot: $(wc -l < "$TMP/run/cockpit.env" 2>&1) lines: $(head -c 600 "$TMP/run/cockpit.env" 2>&1 | tr '\n' ' ')"
[ -s "$TMP/run/cockpit.env" ] && ok "the collector wrote a snapshot for the pane to read" || bad "the collector wrote a snapshot for the pane to read" "no $TMP/run/cockpit.env"

pane() {  # pane <name> <toml-for-the-server-or-empty> — `layout up` in a fresh server, print the health pane
    local sock="$TMP/tmux-$1" toml="$2"
    mkdir -p "$sock"
    local base=(HOME="$TMP/home" PATH="$R/bin:$TMP/stubs:/usr/bin:/bin" TMUX_TMPDIR="$sock" SPIRA_SYSTEMCTL="$TMP/stubs/systemctl")
    local srv=("${base[@]}")
    [ -n "$toml" ] && srv+=(SPIRA_TOML="$toml")
    env -i "${srv[@]}" tmux new-session -d -s c -x 200 -y 60
    env -i "${base[@]}" SPIRA_TOML="$SPIRA_TOML" SPIRA_RELEASE="$R" SPIRA_REPO="$TMP" "$R/bin/layout" up --window c:0 >"$TMP/layout-$1.out" 2>&1
    sleep 4
    TMUX_TMPDIR="$sock" tmux list-panes -t c:0 -F '#{pane_id} #{@cockpit}' | awk '$2=="health"{print $1}' \
        | while read -r p; do TMUX_TMPDIR="$sock" tmux capture-pane -p -t "$p"; done
    TMUX_TMPDIR="$sock" tmux kill-server 2>/dev/null
}

OUT="$(pane good "$SPIRA_TOML")"
[ -n "$(printf '%s' "$OUT" | tr -d '[:space:]')" ] && ok "the ops pane rendered" || bad "the ops pane rendered" "empty pane; layout said: $(cat "$TMP/layout-good.out")"
for label in ATTN BEADS LAND GATE; do
    printf '%s\n' "$OUT" | grep -qE "^ ?${label}( |$)" && ok "the $label section rendered" || bad "the $label section rendered" "$OUT"
done
nowant "no STOPPED banner" "STOPPED" "$OUT"
nowant "no refusal line" "config unresolved" "$OUT"
printf '%s\n' "$OUT" | grep -qE 'pass [0-9]+s' && ok "the pane shows values the collector wrote to the configured run directory" \
    || bad "the pane shows values the collector wrote to the configured run directory" "$OUT"

OUT="$(pane bad "")"
want "SEEN RED: a pane started with no config says so instead of rendering a default" "config unresolved" "$OUT"

echo
[ -s "$TMP/notes" ] && cat "$TMP/notes"
tl_summary
