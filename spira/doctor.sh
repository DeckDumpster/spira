#!/usr/bin/env bash
#
# doctor.sh — is the running harness healthy, right now.
#
#   doctor.sh    check every runtime vital sign, name every fault, exit 1 if any is fatal
#
# RUNTIME HEALTH ONLY. This is not a preflight for a box that has never been installed, and
# it is not a build check. A missing build input fails the build (make and the dependency
# manifest in conf.sh own that); a release that should not activate is refused by
# pre-activate.sh before the symlink flips. What is left for a box that is already running is
# read-only, fast questions, each its own function so watchtower can call them directly:
#
#   doctor_check_chamber_overlays — which operator overlay, if any, is in force
#   doctor_check_store            — is the store listener reachable
#   doctor_check_events_probe     — does a write/read round trip on the events substrate
#   doctor_check_failed_units     — are any spira-* systemd units in the failed state
#   doctor_check_snapshot_fresh   — is the cockpit collector still writing
#   doctor_check_operator_channel — can this operated instance actually reach its operator
#
# ONE WRITE. The events substrate probe writes a single sentinel event to the store and reads
# it back. That one write cannot collide with production state: the sentinel id is reserved
# and matches no real bead. Every other check here is read-only.
set -uo pipefail
# Tell conf.sh not to exit on a schema mismatch — doctor must survive to report the store
# section even when the schema is the fault, not die on the way in.
SPIRA_DOCTOR=1
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

fatal=0; warn=0
FAIL() { printf '  FAIL  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; fatal=$((fatal+1)); }
WARN() { printf '  warn  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; warn=$((warn+1)); }
OK()   { printf '  ok    %s\n' "$1"; }

CONF="${SPIRA_CONF_FILE:-}"

# --------------------------------------------------------------------------------------
# CHAMBER OVERLAYS. aeon.sh applies an operator overlay from SPIRA_CHAMBER_OVERLAY silently
# to a rendered brief; this is the only place that says one is in force at all, so a bead's
# history that disagrees with what an aeon was told can be checked against something.
# --------------------------------------------------------------------------------------
doctor_check_chamber_overlays() {
    local found
    found="$(find "$SPIRA_CHAMBER_OVERLAY" -maxdepth 2 -name '*.md' 2>/dev/null)"
    if [ -n "$found" ]; then
        while IFS= read -r f; do
            OK "active: ${f#"$SPIRA_CHAMBER_OVERLAY"/}"
        done <<<"$found"
    else
        OK "none active (checked $SPIRA_CHAMBER_OVERLAY)"
    fi
}

# --------------------------------------------------------------------------------------
# STORE LISTENER. Two questions: can bd reach $SPIRA_DB, and is whatever it depends on
# (a managed dolt-beads.service, or an independently-managed server) actually answering.
# "Managed independently" is a claim to check, not to assume — an unmanaged server that
# stopped answering fails exactly like a managed one that stopped.
#
# SPIRA_DOCTOR_INSTALLING softens the two not-yet-created cases to WARN: install.sh runs
# doctor.sh at phase 0, before the store exists (phase 3) and before the server unit is
# started (phase 4).
# --------------------------------------------------------------------------------------
doctor_check_store() {
    if [ -d "$SPIRA_DB/.beads" ]; then
        OK "$SPIRA_DB has a .beads"
        # A DATABASE THAT ANSWERS IS NOT THE SAME AS ONE THAT IS THERE. `bd` takes its
        # database from the path it is pointed at, so a wrong or moved path does not
        # error — it silently answers from some other store.
        local out
        if out="$(timeout 60 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --limit 1 --json 2>&1)"; then
            OK "bd can read it"
        elif [ -n "${SPIRA_DOCTOR_INSTALLING:-}" ] && [ -n "${SPIRA_DOLT_DATA:-}" ] \
                && ! systemctl --user is-active --quiet dolt-beads.service 2>/dev/null; then
            WARN "bd cannot read $SPIRA_DB — dolt-beads.service not active; install.sh will start it in phase 4" \
                 "$(printf '%s' "$out" | head -2)"
        else
            FAIL "bd cannot read $SPIRA_DB" "$(printf '%s' "$out" | head -2)
        A Dolt server may be down. Try: bd -C $SPIRA_DB dolt start"
        fi
        # EMBEDDED-MODE STORE. An embedded store holds one connection at a time; every bd
        # call waits in a line. Under this harness's concurrency, reads have taken minutes.
        local meta="$SPIRA_DB/.beads/metadata.json" mode
        if [ -f "$meta" ]; then
            mode="$(python3 -c '
import json,sys
try: print(json.load(open(sys.argv[1])).get("dolt_mode",""))
except Exception: print("")
' "$meta" 2>/dev/null || true)"
            if [ "${mode:-}" = embedded ]; then
                FAIL "store is in embedded mode — one client at a time, every bd call serialises on one lock" \
                     "Set SPIRA_DOLT_DATA in ${CONF:-spira.conf} and re-run install.sh to migrate to server mode."
            else
                OK "store mode: ${mode:-unknown}"
            fi
        fi
    elif [ -n "${SPIRA_DOCTOR_INSTALLING:-}" ]; then
        WARN "$SPIRA_DB has no .beads yet — install.sh will create it in phase 3" \
             "Set SPIRA_DB in ${CONF:-spira.conf} if this path is wrong."
    else
        FAIL "$SPIRA_DB has no .beads — the harness refuses to guess a database" \
             "Set SPIRA_DB in ${CONF:-spira.conf} and run install.sh to create it."
    fi

    if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
        if [ -d "$SPIRA_DOLT_DATA" ]; then
            OK "dolt data directory at $SPIRA_DOLT_DATA"
        elif [ -n "${SPIRA_DOCTOR_INSTALLING:-}" ]; then
            WARN "SPIRA_DOLT_DATA is set but $SPIRA_DOLT_DATA does not exist — install.sh will create it in phase 3" \
                 "Set SPIRA_DOLT_DATA in ${CONF:-spira.conf} if this path is wrong."
        else
            FAIL "SPIRA_DOLT_DATA is set but $SPIRA_DOLT_DATA does not exist" \
                 "Create it, or point SPIRA_DOLT_DATA at the directory dolt sql-server uses."
        fi
        if [ -f "$SPIRA_DOLT_DATA/dolt-server.yaml" ]; then
            OK "dolt-server.yaml at $SPIRA_DOLT_DATA/dolt-server.yaml"
        else
            WARN "no dolt-server.yaml at $SPIRA_DOLT_DATA/dolt-server.yaml" \
                 "The dolt-beads.service ExecStart expects this file. Without it the server
        cannot start. See the README for the minimal config."
        fi
        if systemctl --user is-active --quiet dolt-beads.service 2>/dev/null; then
            OK "dolt-beads.service is active"
        elif [ -n "${SPIRA_DOCTOR_INSTALLING:-}" ]; then
            WARN "dolt-beads.service is not active — install.sh will install and start it in phase 4" \
                 "phase 4 (systemd/install.sh) enables and starts dolt-beads.service."
        else
            FAIL "dolt-beads.service is not active" \
                 "The database is unreachable without the server. Start it:
        systemctl --user start dolt-beads.service
        Or run install.sh to enable and start it."
        fi
    else
        local meta_srv="$SPIRA_DB/.beads/metadata.json" host port
        if [ -f "$meta_srv" ]; then
            host="$(python3 -c '
import json,sys
try: print(json.load(open(sys.argv[1])).get("dolt_server_host","127.0.0.1"))
except Exception: print("127.0.0.1")
' "$meta_srv" 2>/dev/null || echo "127.0.0.1")"
            port="$(python3 -c '
import json,sys
try:
    v=json.load(open(sys.argv[1])).get("dolt_server_port",""); print(v)
except Exception: print("")
' "$meta_srv" 2>/dev/null || true)"
            if [ -n "$port" ]; then
                if (timeout 2 bash -c "(echo > /dev/tcp/${host}/${port}) 2>/dev/null") 2>/dev/null; then
                    OK "dolt server answering on ${host}:${port}"
                else
                    FAIL "SPIRA_DOLT_DATA is empty but no server answers on ${host}:${port}" \
                         "Start the dolt server, or set SPIRA_DOLT_DATA in ${CONF:-spira.conf} so
        install.sh can manage it."
                fi
            else
                OK "SPIRA_DOLT_DATA is empty — dolt server is managed independently"
            fi
        else
            OK "SPIRA_DOLT_DATA is empty — dolt server is managed independently"
        fi
    fi
}

# --------------------------------------------------------------------------------------
# EVENTS ROUND-TRIP PROBE. Write one sentinel event via bd sql and read it back. Passes
# exactly when the write path works. The probe anchors to a real bead because
# fk_events_issue enforces issue_id -> issues.id on a server-mode store; a phantom id is
# refused and the probe can never complete. Repeated runs append another probe row; the
# count only needs to be > 0.
# --------------------------------------------------------------------------------------
doctor_check_events_probe() {
    if [ ! -d "$SPIRA_DB/.beads" ]; then
        WARN "cannot probe events substrate — no database yet"
        return
    fi
    . "$SPIRA_HOME/lib.sh"
    local etype="__doctor_probe__" id count wrote path
    id="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --limit 1 --json 2>/dev/null \
        | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    rows = d if isinstance(d, list) else [d]
    print(rows[0].get("id", "") if rows else "")
except Exception:
    print("")
' 2>/dev/null)"

    if [ -z "$id" ]; then
        WARN "cannot probe events substrate — the store holds no issue to anchor a probe event to" \
             "This is expected on a store that has not been used yet. The probe resumes once a bead exists."
        return
    fi

    if _bump_write_event_try "$id" "$etype" "doctor" >/dev/null 2>&1; then wrote=1; else wrote=0; fi
    count="$(_counter_events_query "$id" "$etype")"
    # count is '?' (not a digit) when the read itself failed — that is a round-trip
    # failure too, not zero events, so the numeric guard below must not choke on it.
    if [ "${count:-0}" -gt 0 ] 2>/dev/null; then
        OK "events write/read round trip"
        return
    fi
    path="bd sql (server mode at ${SPIRA_DB})"
    # REFUSED AND DISCARDED ARE DIFFERENT FAULTS.
    if [ "$wrote" -eq 0 ]; then
        FAIL "events write/read round trip failed — the write via ${path} was refused (anchor bead: ${id})" \
             "Run the INSERT by hand to see the error; a foreign-key refusal means the anchor
        bead no longer exists, a permission error means ${SPIRA_DB} is not writable."
    else
        FAIL "events write/read round trip failed — writes via ${path} are discarded" \
             "The write reported success and did not come back. Check that ${SPIRA_DB} is writable."
    fi
}

# --------------------------------------------------------------------------------------
# FAILED UNITS (sp-niqjl). A unit that has exited into the failed state can sit there for
# days: nothing here restarts it, nothing before this check even looked. Dedup, age-since-
# failed and the anomaly it becomes are watchtower's job (sp-niqjl); doctor's job is the
# one-pass read of current state.
# --------------------------------------------------------------------------------------
doctor_check_failed_units() {
    local sc="${SPIRA_SYSTEMCTL:-systemctl}" out n=0 line unit
    if ! out="$("$sc" --user list-units --state=failed --no-legend 'spira-*' 2>&1)"; then
        FAIL "cannot query failed units: $(printf '%s' "$out" | head -1)" \
             "Check the systemd user manager: $sc --user status"
        return
    fi
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        unit="${line%% *}"
        FAIL "$unit is a failed systemd unit" \
             "Check: journalctl --user -u $unit -n 20"
        n=$((n+1))
    done <<< "$out"
    [ "$n" -eq 0 ] && OK "no failed spira-* units"
}

# --------------------------------------------------------------------------------------
# SNAPSHOT FRESHNESS. The collector writes cockpit.env on every tick; absence or a stale
# mtime means the supervisor is not writing — the exact condition that went undetected
# because nothing else checked it (sp-itsy). Uses SPIRA_SNAP_STALE_S as the threshold.
# --------------------------------------------------------------------------------------
doctor_check_snapshot_fresh() {
    local snap="$SPIRA_RUN/cockpit.env" age
    if [ ! -f "$snap" ]; then
        WARN "no cockpit snapshot at $snap — collector may not have run yet" \
             "Check: systemctl --user status $(spira_unit cockpit service)"
        return
    fi
    age=$(( $(date +%s) - $(stat --format='%Y' "$snap" 2>/dev/null || echo 0) ))
    if [ "$age" -gt "${SPIRA_SNAP_STALE_S:-60}" ]; then
        FAIL "cockpit snapshot stale — last written ${age}s ago (limit ${SPIRA_SNAP_STALE_S:-60}s)" \
             "The collector is not writing. Check: systemctl --user status $(spira_unit cockpit service)"
    else
        OK "cockpit snapshot fresh — $snap (${age}s old)"
    fi
}

# --------------------------------------------------------------------------------------
# OPERATOR CHANNEL. inotifywait, the configured mail client (COCKPIT_MAIL), hunk (when
# COCKPIT_SESSIONS uses it), and go are runtime health for an OPERATED instance, not build
# inputs: their absence does not stop the loop, it silently kills the path an operator
# reads and answers escalations through (law-answers-need-a-delivery-path). doctor-check.sh
# WARNs on these at image-build time because a container has no operator reading it; here,
# on a box that is actually running, SPIRA_OPERATED=1 (default) makes the same absence FAIL.
# SPIRA_OPERATED=0 downgrades to WARN for a headless fixture or CI box.
# --------------------------------------------------------------------------------------
doctor_check_operator_channel() {
    local level=FAIL; [ "${SPIRA_OPERATED:-1}" = 0 ] && level=WARN

    if command -v inotifywait >/dev/null 2>&1; then
        OK "inotifywait — $(command -v inotifywait)"
    else
        "$level" "inotifywait is not on PATH — no mail reaches you until a session starts" \
                  "Install: apt install inotify-tools"
    fi

    if [ -n "${COCKPIT_MAIL:-}" ]; then
        if command -v "$COCKPIT_MAIL" >/dev/null 2>&1; then
            OK "$COCKPIT_MAIL (COCKPIT_MAIL) — $(command -v "$COCKPIT_MAIL")"
        else
            "$level" "$COCKPIT_MAIL (COCKPIT_MAIL) is not on PATH — the cockpit mail pane is dead; escalations and answers have no delivery path" \
                      "Install $COCKPIT_MAIL, or set COCKPIT_MAIL in ${CONF:-spira.conf} to a mail client that is present."
        fi
    fi

    case " ${COCKPIT_SESSIONS:-} " in
        *" hunk "*)
            if command -v hunk >/dev/null 2>&1; then
                OK "hunk — $(command -v hunk)"
            else
                "$level" "hunk is not on PATH — the cockpit review pane is unavailable; rebuild.sh creates a hunk session with no program behind it" \
                          "Install: npm install -g --prefix ~/.local hunkdiff (bin lands at ~/.local/bin/hunk; ensure ~/.local/bin is in SPIRA_PATH in ${CONF:-spira.conf})"
            fi ;;
    esac

    local go_found="" c
    for c in "${GO:-}" "$HOME/.local/go/bin/go" "$(command -v go 2>/dev/null || true)"; do
        [ -n "$c" ] && [ -x "$c" ] && { go_found="$c"; break; }
    done
    if [ -n "$go_found" ]; then
        OK "go — $go_found"
    else
        local bd_ok=0 bd_ver arch prebuilt=0
        bd_ver="$(timeout 5 "${SPIRA_BD:-bd}" version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
        [ "${bd_ver:-}" = "${SPIRA_BD_TAG#v}" ] && bd_ok=1
        arch="$(uname -m)"
        case "$arch" in x86_64|aarch64) prebuilt=1 ;; esac
        if [ "$bd_ok" -eq 0 ] && [ "$prebuilt" -eq 0 ]; then
            "$level" "go is not on PATH — $(spira_bin_purpose go)" \
                      "bd is mismatched (need $SPIRA_BD_TAG) and no prebuilt exists for $arch. Install Go: https://go.dev/dl/ or use $HOME/.local/go/bin/go"
        else
            OK "go absent — bd $( [ "$bd_ok" -eq 1 ] && echo current || echo mismatched ); prebuilt $( [ "$prebuilt" -eq 1 ] && echo available || echo unavailable ) for $arch"
        fi
    fi
}

echo "spira doctor"

echo
echo "chamber overlays"
doctor_check_chamber_overlays

echo
echo "store"
doctor_check_store

echo
echo "events substrate"
doctor_check_events_probe

echo
echo "systemd units"
doctor_check_failed_units

echo
echo "the cockpit"
doctor_check_snapshot_fresh

echo
echo "operator channel"
doctor_check_operator_channel

echo
if [ "$fatal" -gt 0 ]; then
    printf '%d fatal, %d warnings — the harness is not healthy.\n' "$fatal" "$warn"
    exit 1
fi
printf '0 fatal, %d warnings — the harness is healthy.\n' "$warn"
exit 0
