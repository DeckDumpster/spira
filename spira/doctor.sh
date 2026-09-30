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
#   doctor_check_release_tools    — is every Spira tool on PATH, by name (sp-gypjk)
#   doctor_check_hotfix           — does `release status` show a standing hotfix, and has
#                                    it stood past the configured alert threshold (sp-6p20x)
#   doctor_check_chamber_overlays — which operator overlay, if any, is in force
#   doctor_check_gate_compile_check — does every Rust repo's configured gate command still
#                                     reach a compile check (sp-1hmrm)
#   doctor_check_store            — is the store listener reachable
#   doctor_check_duckdb           — is run/tsd/'s query layer queryable
#   doctor_check_events_probe     — does a write/read round trip on the events substrate
#   doctor_check_failed_units     — are any spira-* systemd units in the failed state
#   doctor_check_orphan_units     — is any enabled watch unit's watcher gone from the manifest
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
# RELEASE TOOLS (sp-gypjk). Every Spira tool is invoked by its bare name on the PATH the
# launcher set — the release's bin/ and spira/ first. There is no resolver and no fallback,
# so the one question is whether each name resolves; each missing one is named.
# --------------------------------------------------------------------------------------
# The binaries come from deps.toml's release tier — one list, not a second literal here.
SPIRA_RELEASE_TOOLS="$(spira_deps_list release | tr '\n' ' ')mail.sh gate.sh world.sh"
doctor_check_release_tools() {
    local t missing="" n=0
    for t in $SPIRA_RELEASE_TOOLS; do
        n=$((n+1))
        command -v "$t" >/dev/null 2>&1 || missing="$missing $t"
    done
    if [ -n "$missing" ]; then
        FAIL "not on PATH:$missing" "PATH is $PATH — the launcher sets it to the release's bin/ and spira/"
    else
        OK "all $n release tools resolve on PATH"
    fi
}

# --------------------------------------------------------------------------------------
# HOTFIX (sp-6p20x). `release activate --hotfix` is the stop-the-world seam: the running
# system is on a commit that has not landed. `release status` is the one place that knows
# whether one stands and whether it has stood past the configured threshold (spira.
# hotfix_alert_hours, default 4h) — this reads its RUNNING UNLANDED / ALERT lines rather
# than re-deriving the age from $SPIRA_RUN/release/hotfix itself. WARN below the threshold
# (visible, not yet urgent); FAIL once ALERT fires, so a hotfix nobody has landed shows up
# fatal here the same way a failed unit does.
# --------------------------------------------------------------------------------------
doctor_check_hotfix() {
    local out line alert
    out="$(release status 2>/dev/null)" || out=""
    line="$(printf '%s\n' "$out" | grep '^RUNNING UNLANDED ')"
    alert="$(printf '%s\n' "$out" | grep '^ALERT ')"
    if [ -n "$alert" ]; then
        FAIL "$line" "$alert — land the fix (it supersedes automatically) or \`release rollback\`"
    elif [ -n "$line" ]; then
        WARN "$line" "land the fix or \`release rollback\` before it stands past threshold"
    else
        OK "no hotfix standing"
    fi
}

# --------------------------------------------------------------------------------------
# CONFIG FILES. Once spira.toml exists it is the only file conf.sh reads (spira_toml_resolve,
# sp-usxfl) and the only one any harness writer targets (spira_config_set) — a spira.conf
# left beside it is inert, not backing anything up, and an operator hand-editing it would see
# no effect at all. Warn so it gets noticed and removed, per the bead's own acceptance: "the
# Concierge retires this box's spira.conf" once this lands.
# --------------------------------------------------------------------------------------
doctor_check_config_files() {
    local toml="${SPIRA_TOML_FILE:-}"
    if [ -n "$CONF" ] && [ -n "$toml" ]; then
        WARN "both $CONF and $toml exist — spira.conf is no longer read or written" \
             "Confirm $toml carries everything you need, then remove $CONF."
    elif [ -n "$toml" ]; then
        OK "spira.toml only — $toml"
    elif [ -n "$CONF" ]; then
        OK "spira.conf only (legacy) — $CONF"
    else
        OK "no config file found — running on derived defaults"
    fi

    # PARSING IS spira-config's JOB, NOT DOCTOR'S OWN: a hand-rolled well-formedness check
    # here would be a second parser of spira.toml, exactly the duplication this file exists
    # to retire (sp-4bw2i). validate is the one authority on whether the document in force
    # is well-formed.
    if [ -n "$toml" ]; then
        local out
        if ! command -v spira-config >/dev/null 2>&1; then
            FAIL "cannot validate $toml — spira-config is not on PATH"
        else
            # sp-oppza ONE-TIME UPGRADE MIGRATION, ahead of validate: a box whose config
            # predates sp-k6m1m (goal set, no id_prefix) is repaired in place instead of
            # failing validation on every such box. Idempotent — a no-op once id_prefix is
            # set, which includes production's own state, set by hand — so unconditional.
            local mig
            mig="$(spira-config migrate "$toml" 2>&1)"
            [ -n "$mig" ] && printf '%s\n' "$mig"
            if out="$(spira-config validate "$toml" 2>&1)"; then
                OK "spira.toml validates — $toml"
            else
                FAIL "spira.toml fails validation — $toml" "$out"
            fi
        fi
    fi
}

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
# OPERATOR OVERRIDES (sp-qdh0x). overrides.sh doctor names the two conditions that used to be
# invisible to everything but a log a queue monitor happened to be tailing: a spec that failed
# to apply or retire, and a spec whose bead closed over a day ago without HEAD ever carrying
# its `spira: land` commit — the harness's own answer, not a model of it kept here.
# --------------------------------------------------------------------------------------
doctor_check_overrides() {
    local out rc
    out="$(overrides.sh doctor 2>&1)"; rc=$?
    if [ "$rc" = 0 ]; then
        OK "no problems (checked $SPIRA_OVERRIDES)"
        return
    fi
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        FAIL "override: $line"
    done <<< "$out"
}

# --------------------------------------------------------------------------------------
# GATE COMPILE CHECK (sp-1hmrm). A hand-cut of a repo-map row's gate command is live,
# operator state — no committed tree carries it, so no container-run suite ever sees it.
# sp-0inic certified green and broke the build for its whole round because the row's gate
# had been cut to three fences with no compile check at all (build-fence.sh, wired into
# gate-touched.sh, was reachable only when the row's gate command still named one of them).
# This is the runtime check for that: ask the gate binary for the gate the repository's
# landing ref defines (its tree's gate.steps since sp-quu2w, else the repo-map column), and
# for every repo carrying Rust crates, fail unless it still names build-fence.sh.
# (gate-touched.sh no longer calls it, sp-aprxm, so naming only that is not enough; under
# gate_mode=unit the gate drops the step and its build phase is the check.)
# --------------------------------------------------------------------------------------
doctor_check_gate_compile_check() {
    . "$SPIRA_HOME/lib.sh"
    local name path gate checked=0
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        path="$(repo_field "$name" path 2>/dev/null)"
        [ -n "$path" ] && [ -d "$path" ] || continue
        find "$path" -maxdepth 2 -name Cargo.toml 2>/dev/null | grep -q . || continue
        checked=$((checked + 1))
        # The gate the landing ref defines: its checked-in gate.steps, or the row's column
        # for a repository that never adopted one (sp-quu2w) — resolved by the gate itself.
        gate="$(gate --home "$SPIRA_HOME" --definition "$name" 2>&1)" \
            || gate="<unresolved: ${gate:-no output}>"
        case "$gate" in
            *build-fence.sh*)
                OK "$name: gate command reaches a compile check" ;;
            *)
                FAIL "$name: gate command has no reachable compile check" \
                     "gate: ${gate:-<empty>}
        A Rust compile break in $path certifies green. Add \`step bash spira/build-fence.sh\`
        to the repository's gate.steps (or a call to it to the row's gate column)." ;;
        esac
    done <<< "$(repo_names 2>/dev/null)"
    [ "$checked" -gt 0 ] || \
        OK "no repository in the map carries Rust crates (checked ${SPIRA_REPO_MAP:-<unset>})"
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
# DUCKDB (sp-ujhmm). Runtime-tier in deps.toml: the query layer over run/tsd/'s JSONL
# families that tsd-query.sh and reconciler-flow both shell out to. Its absence does not
# crash the loop — every flow invariant just logs unobservable forever, which reads
# identically to "nothing to report" until someone reads reconciler-status.jsonl by hand.
# FAIL, not warn, so a blind detector shows up here first.
# --------------------------------------------------------------------------------------
doctor_check_duckdb() {
    local duckdb_bin="${SPIRA_DUCKDB_BIN:-duckdb}"
    local found=""
    if [[ "$duckdb_bin" == */* ]]; then
        [[ -x "$duckdb_bin" ]] && found="$duckdb_bin"
    else
        found="$(command -v "$duckdb_bin" 2>/dev/null || true)"
    fi
    if [[ -n "$found" ]]; then
        OK "duckdb — $found"
    else
        FAIL "duckdb is not on PATH" \
             "tsd-query.sh refuses every query and every reconciler-flow invariant logs
        unobservable until this is installed. See deps.toml's duckdb entry."
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
#
# SPIRA_DOCTOR_INSTALLING softens a failed unit to WARN (sp-r15cf). This check runs at
# install.sh's phase 0 — its OWN preflight, before phase 0 has touched a single unit — so
# every failure it finds here necessarily PREDATES this install run: it cannot be something
# this install broke, because this install has not acted yet. Refusing the install outright
# over a unit that was already down (an aged box's own leftover failure, unrelated to the
# release being installed) blocks the one thing that might repair it — phase 4 re-renders
# and restarts. Outside SPIRA_DOCTOR_INSTALLING (the ordinary runtime read doctor.sh and
# deploy.sh's own health check use) this stays a FAIL: a unit failed while the box was
# already up IS live evidence something broke, with no "before phase 0" innocence to claim.
doctor_check_failed_units() {
    local sc="${SPIRA_SYSTEMCTL:-systemctl}" out n=0 line unit
    # --plain: without it systemctl prefixes each failed unit with a "● " status bullet,
    # and the first field below was the bullet, not the unit ("● is a failed systemd unit").
    if ! out="$("$sc" --user list-units --state=failed --no-legend --plain 'spira-*' 2>&1)"; then
        FAIL "cannot query failed units: $(printf '%s' "$out" | head -1)" \
             "Check the systemd user manager: $sc --user status"
        return
    fi
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        unit="${line%% *}"
        if [ -n "${SPIRA_DOCTOR_INSTALLING:-}" ]; then
            WARN "$unit is a failed systemd unit — pre-existing, predates this install" \
                 "Check: journalctl --user -u $unit -n 20"
        else
            FAIL "$unit is a failed systemd unit" \
                 "Check: journalctl --user -u $unit -n 20"
        fi
        n=$((n+1))
    done <<< "$out"
    [ "$n" -eq 0 ] && OK "no failed spira-* units"
}

# --------------------------------------------------------------------------------------
# ORPHAN WATCH UNITS (sp-07yxy). `watchd prune` disables and removes a retired daemon
# row's unit; this is the check for when prune was never run — an enabled
# spira-watch-*-<instance>.service whose watcher name has no `daemon` row in the manifest
# runs on against a target that no longer exists, and fails, unnoticed, until this asks.
# --------------------------------------------------------------------------------------
doctor_check_orphan_units() {
    local sc="${SPIRA_SYSTEMCTL:-systemctl}" inst="${SPIRA_INSTANCE:-prod}" out rows
    if ! rows="$(watchd manifest 2>/dev/null)"; then
        FAIL "cannot read the watcher manifest" \
             "Check: watchd manifest"
        return
    fi
    local known=" " name kind rest
    while IFS='|' read -r name kind rest; do
        [ -n "$name" ] && [ "$kind" = daemon ] && known="$known$name "
    done <<< "$rows"

    # NO MATCH IS NOT A FAILED PROBE. systemd (255, Ubuntu 24.04) exits 1 and prints
    # NOTHING when no unit file matches the pattern — the state of every fresh install,
    # which has enabled no watcher yet. Read as a failed query it made install.sh's own
    # phase-0 preflight refuse every fresh install. A manager that cannot be reached says
    # so ("Failed to connect to bus: ..."), so a non-zero exit WITH output is still a FAIL.
    if ! out="$("$sc" --user list-unit-files --no-legend --state=enabled \
                    "spira-watch-*-${inst}.service" 2>&1)"; then
        if [ -n "$out" ]; then
            FAIL "cannot query watch unit files: $(printf '%s' "$out" | head -1)" \
                 "Check the systemd user manager: $sc --user status"
            return
        fi
        out=""
    fi
    local n=0 line unit wname
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        unit="${line%% *}"
        wname="${unit#spira-watch-}"; wname="${wname%-"$inst".service}"
        case "$known" in *" $wname "*) continue ;; esac
        FAIL "$unit is enabled but '$wname' has no daemon row in the manifest" \
             "Check: watchd prune"
        n=$((n+1))
    done <<< "$out"
    [ "$n" -eq 0 ] && OK "no orphan spira-watch units"
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

# --------------------------------------------------------------------------------------
# CONCIERGE SINGLETON (sp-rig42). Two live processes answering to the same Remote Control
# name "concierge" is invisible from the phone — both show as the one session — and the
# mail wake only ever reaches the managed one, so an operator talking to the stray never
# sees a reply arrive. concierge.sh refuses to START a second one; this is the read-only
# check for a stray that got there some other way (a bare `claude --remote-control
# concierge` typed by hand into a dead cockpit pane, which is how this happened both times
# it was seen).
# --------------------------------------------------------------------------------------
doctor_check_concierge_singleton() {
    local conc="$SPIRA_REPO/concierge.sh" stray
    if [ ! -x "$conc" ]; then
        WARN "no concierge.sh at $conc — cannot check for a second concierge"
        return
    fi
    stray="$(bash "$conc" _stray-holders 2>/dev/null)"
    if [ -n "$stray" ]; then
        FAIL "a second concierge is live outside the managed session" \
             "pid(s): $(printf '%s' "$stray" | tr '\n' ' ')
        Mail wakes reach only the managed session; this one will never see a reply. Inspect:
        ps -o pid,tty,lstart,cmd -p <pid>. If it should be the managed one, kill it and run:
        $conc start"
    else
        OK "Remote Control name 'concierge' held by no more than the managed session"
    fi
}

echo "spira doctor"

echo
echo "release tools"
doctor_check_release_tools

echo
echo "hotfix"
doctor_check_hotfix

echo
echo "config files"
doctor_check_config_files

echo
echo "chamber overlays"
doctor_check_chamber_overlays

echo
echo "operator overrides"
doctor_check_overrides

echo
echo "gate compile check"
doctor_check_gate_compile_check

echo
echo "store"
doctor_check_store

echo
echo "time series query layer"
doctor_check_duckdb

echo
echo "events substrate"
doctor_check_events_probe

echo
echo "systemd units"
doctor_check_failed_units
doctor_check_orphan_units

echo
echo "the cockpit"
doctor_check_snapshot_fresh

echo
echo "operator channel"
doctor_check_operator_channel

echo
echo "concierge"
doctor_check_concierge_singleton

echo
if [ "$fatal" -gt 0 ]; then
    printf '%d fatal, %d warnings — the harness is not healthy.\n' "$fatal" "$warn"
    exit 1
fi
printf '0 fatal, %d warnings — the harness is healthy.\n' "$warn"
exit 0
