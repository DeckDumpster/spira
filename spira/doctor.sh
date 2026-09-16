#!/usr/bin/env bash
#
# doctor.sh — can this harness run on this box, and if not, exactly what is missing.
#
#   doctor.sh          check everything, name every fault, exit 1 if any is fatal
#   doctor.sh --paths  print the resolved configuration and stop
#
# WHY IT EXISTS. A harness that dies with `bd: command not found` from a systemd timer has
# told the operator nothing: not which program, not what it is for, not where to get it, and
# not into a log anyone reads. Every fault here is named, in one pass, with what to do about
# it — because the alternative is discovering them one restart at a time.
#
# FATAL versus WARN is the difference between "the loop cannot run" and "one feature is off".
# A missing `cargo` is a warning: the loop runs fine without the attention panel. A missing
# `bd` is fatal, because beads is the substrate.
#
# ONE WRITE. The events substrate probe (in the "events substrate" section below) writes a
# single sentinel event to the store and reads it back. That one write cannot collide with
# production state: the sentinel id is reserved and matches no real bead. Every other check
# here is read-only.
set -uo pipefail
# Tell conf.sh not to exit on schema mismatch so the "bd schema" section below can
# report it cleanly. Without this, conf.sh exits before doctor.sh prints any FAIL line.
SPIRA_DOCTOR=1
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

fatal=0; warn=0
FAIL() { printf '  FAIL  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; fatal=$((fatal+1)); }
WARN() { printf '  warn  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; warn=$((warn+1)); }
OK()   { printf '  ok    %s\n' "$1"; }

CONF="${SPIRA_CONF_FILE:-}"

if [ "${1:-}" = "--paths" ]; then
    printf 'config file   %s\n' "${CONF:-<none — every value is a default>}"
    for k in SPIRA_HOME SPIRA_REPO $SPIRA_CONF_KEYS; do
        printf '%-18s %s\n' "$k" "${!k:-}"
    done
    printf '%-18s %s\n' PATH "$PATH"
    exit 0
fi

echo "spira doctor"
echo
echo "configuration"
if [ -n "$CONF" ]; then OK "reading $CONF"
else WARN "no spira.conf found — every value is a default" \
          "looked in $SPIRA_REPO/spira.conf, \${XDG_CONFIG_HOME:-\$HOME/.config}/spira/, /etc/spira/"
fi
OK "harness at $SPIRA_HOME (in $SPIRA_REPO)"

echo
echo "programs"
# ADDING A NAME TO EITHER `for b in` LOOP BELOW IS A CHANGE TO THE CONTAINER IMAGE.
# spira/testenv/doctor-check.sh PARSES these two lines out of this file and fails the image
# build for any program that is neither installed in the image nor listed in
# spira/testenv/doctor-waivers with a reason. That is deliberate — it stops the image and the
# harness drifting apart — but nothing here said so, and adding `jq` and `zstd` to the WARN
# loop broke the image build, which took testenv-batch.sh down with it and left every suite
# running on the host. Add the program to the Containerfile, or waive it with a reason.
# FATAL: the loop cannot run without these. `flock` is one of them because the landing gate
# serialises on the tree it extracts a branch into, and a gate that cannot take that lock
# refuses rather than judging — so every branch would fail its gate and every finished bead
# would be reopened.
for b in bd git python3 flock; do
    if command -v "$b" >/dev/null 2>&1; then OK "$b — $(command -v "$b")"
    else FAIL "$b is not on PATH — $(spira_bin_purpose "$b")" \
              "PATH is $PATH. If it is installed elsewhere, set SPIRA_PATH in ${CONF:-spira.conf}."; fi
done
# dolt: WARN only. The file fallback (db-wx4) handles events reads and writes on an embedded
# store when dolt is absent, so the missing-binary check cannot detect a broken write path.
# Whether the path works is verified by the round-trip probe in the "events substrate" section
# below. This line names which path is in use so the probe failure message is not the first
# mention of dolt.
if command -v dolt >/dev/null 2>&1; then
    OK "dolt — $(command -v dolt)"
else
    WARN "dolt is not on PATH — $(spira_bin_purpose dolt)" \
         "If it is installed elsewhere, set SPIRA_PATH in ${CONF:-spira.conf}."
fi

# WARN: each disables one feature, named, rather than the loop.
# SPIRA_AGENT is used here rather than a literal: an operator who sets it to a different
# binary name gets a useful message about that binary, not about a product they did not install.
# jq and zstd each have a working fallback (python3 for JSON, gzip for archives), so their
# absence is a warning about a slower or less convenient path, never about a broken one.
# They are checked here because they are DECLARED — test-bin-manifest.sh fails if anything
# in SPIRA_BINS is examined by nothing, which is how bd-embedded went a week unnoticed.
for b in gh "${SPIRA_AGENT:-claude}" tmux node jq zstd inotifywait aerc hunk; do
    if command -v "$b" >/dev/null 2>&1; then OK "$b — $(command -v "$b")"
    else WARN "$b is not on PATH — $(spira_bin_purpose "$b")" \
              "If it is installed elsewhere, set SPIRA_PATH in ${CONF:-spira.conf}."; fi
done

# CARGO VERSION CHECK. cargo absent is a WARN — the loop runs fine without the panel.
# cargo present but below 1.78.0 is a FAIL: loom/Cargo.lock is version 4, which only
# parses on rustc >= 1.78.0. A stale toolchain fails silently in build.sh with "lock file
# version 4 requires -Znext-lockfile-bump" and leaves the panel unbuildable with no
# indication that the toolchain is the fault (sp-tst3). Presence alone is not enough.
if command -v cargo >/dev/null 2>&1; then
    _cargo_ver="$(cargo --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
    _cargo_major="$(printf '%s\n' "${_cargo_ver:-0.0.0}" | cut -d. -f1)"
    _cargo_minor="$(printf '%s\n' "${_cargo_ver:-0.0.0}" | cut -d. -f2)"
    if [ -z "$_cargo_ver" ]; then
        FAIL "cargo is on PATH but its version could not be determined" \
             "Run 'cargo --version' to inspect it."
    elif [ "$_cargo_major" -gt 1 ] \
      || { [ "$_cargo_major" -eq 1 ] && [ "$_cargo_minor" -ge 78 ]; }; then
        OK "cargo — $(command -v cargo) ($_cargo_ver)"
    else
        FAIL "cargo $_cargo_ver is below the required minimum 1.78.0" \
             "loom/Cargo.lock is version 4, which requires rustc >= 1.78.0. A build with
        this toolchain fails immediately: 'lock file version 4 requires -Znext-lockfile-bump'.
        Update the Rust toolchain: rustup update, or install via https://rustup.rs/"
    fi
    unset _cargo_ver _cargo_major _cargo_minor
else
    WARN "cargo is not on PATH — $(spira_bin_purpose cargo)" \
         "If it is installed elsewhere, set SPIRA_PATH in ${CONF:-spira.conf}."
fi

# EVERY bd ON PATH, with its version. When more than one is present, PATH order decides
# which one an unconfigured caller picks — and that order differs between a login shell, a
# systemd unit and an aeon's confined environment. SPIRA_BD (set in conf.sh) is the pin that
# makes them agree; the configured binary is labelled here so a mismatch in "bd schema" below
# immediately names the offender (sp-s2zvn, scar from 2026-09-08).
# --------------------------------------------------------------------------------------
# DEVELOPMENT DEPENDENCIES — what it takes to TEST Spira, not to run it.
#
# Reported in their own section and never counted as faults, because a production box is
# correct without any of them. `--dev` promotes a missing one to a warning; that is the mode
# for a box where suites are expected to run.
#
# THE EXCEPTION IS A SILENT DOWNGRADE. A program whose absence changes behaviour rather than
# stopping it gets its consequence printed in EVERY mode, including on a production box —
# because "tests quietly run on a different and much worse engine" is not a fact an operator
# can be expected to infer from a line that says a binary is missing. That is precisely how
# bd-embedded went missing here for a week while every check reported healthy.
# --------------------------------------------------------------------------------------
_dr_dev_mode=0
[ "${1:-}" = "--dev" ] && _dr_dev_mode=1
echo
echo "development dependencies (not needed to run the loop)"
for b in $SPIRA_BINS; do
    [ "$(spira_bin_tier "$b")" = dev ] || continue
    # RESOLVE THE WAY THE CONSUMER RESOLVES IT. A dev tool is not always taken from PATH:
    # build-bd.sh invokes $HOME/.local/go/bin/go directly, so a PATH-only check calls a box
    # broken that is not. Each entry is looked for where the thing that needs it looks.
    _dr_found=""
    case "$b" in
        go) for _c in "${GO:-}" "$HOME/.local/go/bin/go" "$(command -v go 2>/dev/null || true)"; do
                [ -n "$_c" ] && [ -x "$_c" ] && { _dr_found="$_c"; break; }
            done ;;
        *)  _dr_found="$(command -v "$b" 2>/dev/null || true)" ;;
    esac
    if [ -n "$_dr_found" ]; then
        OK "$b — $_dr_found"
        continue
    fi
    _dr_absent="$(spira_bin_absent "$b")"
    if [ "$_dr_dev_mode" = 1 ]; then
        WARN "$b is not on PATH — $(spira_bin_purpose "$b")" "${_dr_absent:-}"
    elif [ -n "$_dr_absent" ]; then
        # Not a fault on this box, but its absence is doing something, so say what.
        WARN "$b is not on PATH — $(spira_bin_purpose "$b")" "$_dr_absent"
    else
        printf '  info  %s is not on PATH — %s\n' "$b" "$(spira_bin_purpose "$b")"
    fi
done
unset _dr_absent

# A STRAY .beads ABOVE THE TEMP DIRECTORY DISABLES EMBEDDED FIXTURES SILENTLY.
# bd walks UP from the working directory looking for a workspace. Every fixture builder —
# testdb.sh's embedded probe, and build-bd.sh's own verification — works inside `mktemp -d`,
# which lives under TMPDIR. One abandoned `.beads` there (a `bd init` that once ran with
# cwd=/tmp) makes every one of them refuse with "legacy Dolt workspace detected".
#
# The consequence is not a visible failure. testdb.sh reads the refusal as "this box has no
# embedded engine" and falls back to the shared Dolt server; build-bd.sh reads it as a bad
# binary and declines to install a good one. Both are correct responses to a wrong answer.
# An empty directory nobody could see cost a week of test failures that read as code defects.
_dr_tmp="${TMPDIR:-/tmp}"
_dr_stray=""
_dr_d="$_dr_tmp"
while [ -n "$_dr_d" ] && [ "$_dr_d" != / ]; do
    [ -e "$_dr_d/.beads" ] && _dr_stray="$_dr_d/.beads"
    _dr_d="$(dirname "$_dr_d")"
done
[ -e "/.beads" ] && _dr_stray="/.beads"
if [ -n "$_dr_stray" ]; then
    WARN "a beads workspace sits above the temp directory: $_dr_stray" \
         "bd searches parent directories, so every fixture built in $_dr_tmp refuses with \"legacy Dolt workspace detected\". Embedded fixtures silently fall back to a shared Dolt server and build-bd.sh refuses to install a good binary. Remove it if nothing owns it."
else
    OK "no stray beads workspace above $_dr_tmp"
fi
unset _dr_tmp _dr_stray _dr_d

echo
echo "bd binaries on PATH"
_spira_dr_bd_count=0
_spira_dr_bd_first=""
IFS=: read -ra _spira_dr_path_dirs <<< "$PATH"
for _spira_dr_bd_dir in "${_spira_dr_path_dirs[@]}"; do
    [ -x "${_spira_dr_bd_dir}/bd" ] || continue
    _spira_dr_bd_count=$((_spira_dr_bd_count + 1))
    _spira_dr_bd_full="${_spira_dr_bd_dir}/bd"
    _spira_dr_bd_ver="$(timeout 5 "$_spira_dr_bd_full" version 2>/dev/null | head -1 || printf '?')"
    [ "$_spira_dr_bd_count" -eq 1 ] && _spira_dr_bd_first="$_spira_dr_bd_full"
    if [ "$_spira_dr_bd_full" = "${SPIRA_BD:-}" ]; then
        OK "$_spira_dr_bd_full — $_spira_dr_bd_ver  ← SPIRA_BD (configured)"
    else
        OK "$_spira_dr_bd_full — $_spira_dr_bd_ver"
    fi
done
if [ "$_spira_dr_bd_count" -eq 0 ]; then
    FAIL "no bd on PATH" "PATH is $PATH"
elif [ "$_spira_dr_bd_count" -gt 1 ] && \
     [ -n "${SPIRA_BD:-}" ] && [ "${SPIRA_BD:-}" != "$_spira_dr_bd_first" ]; then
    WARN "SPIRA_BD is not the first bd on PATH" \
         "An unconfigured tool would pick $_spira_dr_bd_first instead of $SPIRA_BD.
    PATH order is determined by SPIRA_PATH in ${CONF:-spira.conf}."
elif [ "$_spira_dr_bd_count" -gt 1 ] && [ -z "${SPIRA_BD:-}" ]; then
    WARN "$_spira_dr_bd_count bd binaries on PATH and SPIRA_BD is not set" \
         "Set SPIRA_BD in ${CONF:-spira.conf} to pin the binary explicitly."
fi
unset _spira_dr_bd_count _spira_dr_bd_first _spira_dr_path_dirs _spira_dr_bd_dir \
      _spira_dr_bd_full _spira_dr_bd_ver

echo
echo "the database"
if [ -d "$SPIRA_DB/.beads" ]; then
    OK "$SPIRA_DB has a .beads"
    # A DATABASE THAT ANSWERS IS NOT THE SAME AS ONE THAT IS THERE. `bd` takes its database
    # from the path it is pointed at, so a wrong or moved path does not error — it silently
    # answers from some other store.
    if out="$(timeout 60 bd -C "$SPIRA_DB" list --limit 1 --json 2>&1)"; then
        OK "bd can read it"
    else
        FAIL "bd cannot read $SPIRA_DB" "$(printf '%s' "$out" | head -2)
        A Dolt server may be down. Try: bd -C $SPIRA_DB dolt start"
    fi
    # EMBEDDED-MODE STORE. An embedded store holds one connection at a time; every bd call
    # waits in a line. Under this harness's concurrency, reads have taken minutes.
    _dr_meta="$SPIRA_DB/.beads/metadata.json"
    if [ -f "$_dr_meta" ]; then
        _dr_dolt_mode="$(python3 -c '
import json,sys
try: print(json.load(open(sys.argv[1])).get("dolt_mode",""))
except Exception: print("")
' "$_dr_meta" 2>/dev/null || true)"
        if [ "${_dr_dolt_mode:-}" = embedded ]; then
            FAIL "store is in embedded mode — one client at a time, every bd call serialises on one lock" \
                 "Set SPIRA_DOLT_DATA in ${CONF:-spira.conf} and re-run install.sh to migrate to server mode."
        else
            OK "store mode: ${_dr_dolt_mode:-unknown}"
        fi
        unset _dr_dolt_mode
    fi
    unset _dr_meta
else
    if [ -n "${SPIRA_DOCTOR_INSTALLING:-}" ]; then
        WARN "$SPIRA_DB has no .beads yet — install.sh will create it in phase 3" \
             "Set SPIRA_DB in ${CONF:-spira.conf} if this path is wrong."
    else
        FAIL "$SPIRA_DB has no .beads — the harness refuses to guess a database" \
             "Set SPIRA_DB in ${CONF:-spira.conf} and run install.sh to create it."
    fi
fi
case "$SPIRA_DB" in
    "$SPIRA_REPO"/*|"$SPIRA_REPO")
        WARN "the database is inside the harness checkout" \
             "It accumulates internal notes and agent memories and must never be committed.
        Move it out, or make certain it is gitignored (law-beads-is-never-public)." ;;
esac

echo
echo "the dolt server"
# SPIRA_DOLT_DATA is either set (this installation manages the Dolt server) or empty
# (the operator runs it another way). When set, verify the service is active. When
# empty and metadata names a port, verify a server is answering — "managed
# independently" is a claim to check, not a pass.
if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
    if [ -d "$SPIRA_DOLT_DATA" ]; then
        OK "dolt data directory at $SPIRA_DOLT_DATA"
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
    else
        FAIL "dolt-beads.service is not active" \
             "The database is unreachable without the server. Start it:
        systemctl --user start dolt-beads.service
        Or run install.sh to enable and start it."
    fi
else
    _dr_meta_srv="$SPIRA_DB/.beads/metadata.json"
    if [ -f "$_dr_meta_srv" ]; then
        _dr_srv_host="$(python3 -c '
import json,sys
try: print(json.load(open(sys.argv[1])).get("dolt_server_host","127.0.0.1"))
except Exception: print("127.0.0.1")
' "$_dr_meta_srv" 2>/dev/null || echo "127.0.0.1")"
        _dr_srv_port="$(python3 -c '
import json,sys
try:
    v=json.load(open(sys.argv[1])).get("dolt_server_port",""); print(v)
except Exception: print("")
' "$_dr_meta_srv" 2>/dev/null || true)"
        if [ -n "$_dr_srv_port" ]; then
            if (timeout 2 bash -c "(echo > /dev/tcp/${_dr_srv_host}/${_dr_srv_port}) 2>/dev/null") 2>/dev/null; then
                OK "dolt server answering on ${_dr_srv_host}:${_dr_srv_port}"
            else
                FAIL "SPIRA_DOLT_DATA is empty but no server answers on ${_dr_srv_host}:${_dr_srv_port}" \
                     "Start the dolt server, or set SPIRA_DOLT_DATA in ${CONF:-spira.conf} so
        install.sh can manage it."
            fi
        else
            OK "SPIRA_DOLT_DATA is empty — dolt server is managed independently"
        fi
        unset _dr_srv_host _dr_srv_port
    else
        OK "SPIRA_DOLT_DATA is empty — dolt server is managed independently"
    fi
    unset _dr_meta_srv
fi

echo
echo "events substrate"
# ROUND-TRIP PROBE. Write one sentinel event and read it back. This is the positive control
# the dolt-on-PATH proxy (removed by db-9dh) could not provide: it passes exactly when the
# write path works, and fails when writes vanish silently. The file fallback introduced by
# db-wx4 means embedded-store installs without dolt still have a working events path, and the
# dolt-on-PATH check would have FAILed those correctly-configured boxes for four days.
#
# THE PROBE MUST ANCHOR TO A REAL BEAD. A server-mode store carries fk_events_issue
# (events.issue_id -> issues.id), so a sentinel issue_id matching no bead is REFUSED with
# Error 1452 and the round trip can never complete. This probe used "__doctor_probe__" as
# the issue id and therefore reported "writes are discarded" on every run of every
# correctly-working server-mode box, while real event writes against real bead ids worked
# the whole time. A positive control that cannot pass is worse than none: it is a permanent
# red, and a check that is always red is a check nobody reads.
#
# So the sentinel moves from the issue_id to the EVENT TYPE, and the row is anchored to an
# issue that actually exists. The embedded file fallback has no foreign key and no issues
# table to ask, so it keeps the old sentinel and the old behaviour.
#
# Repeated runs append another probe row; the count grows but the check only needs > 0.
#
# lib.sh is sourced here rather than at the top because loading it sooner would require
# making the repo-map conditional unconditional, or duplicating the source. The events
# functions are self-contained: they need only SPIRA_DB and SPIRA_BD (set by conf.sh).
if [ -d "$SPIRA_DB/.beads" ]; then
    . "$SPIRA_HOME/lib.sh"
    _probe_etype="__doctor_probe__"
    _probe_doltdb="${SPIRA_DB}/.beads/embeddeddolt"
    _probe_id="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --limit 1 --json 2>/dev/null \
        | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    rows = d if isinstance(d, list) else [d]
    print(rows[0].get("id", "") if rows else "")
except Exception:
    print("")
' 2>/dev/null)"

    if [ -z "$_probe_id" ] && [ ! -d "$_probe_doltdb" ]; then
        # A SQL-backed store with no issues at all. The foreign key leaves nothing to
        # anchor to, so the round trip cannot be performed. Honest ignorance beats a
        # failure the operator cannot act on (law-alerts-must-be-actionable).
        WARN "cannot probe events substrate — the store holds no issue to anchor a probe event to" \
             "This is expected on a store that has not been used yet. The probe resumes once a bead exists."
    else
        [ -n "$_probe_id" ] || _probe_id="__doctor_probe__"
        if _bump_write_event_try "$_probe_id" "$_probe_etype" "doctor" >/dev/null 2>&1; then
            _probe_wrote=1
        else
            _probe_wrote=0
        fi
        _probe_count="$(_counter_events_query "$_probe_id" "$_probe_etype")"
        if [ "${_probe_count:-0}" -gt 0 ]; then
            OK "events write/read round trip"
        else
            # Name the expected write path so the operator knows where to look.
            if [ -d "$_probe_doltdb" ]; then
                if command -v dolt >/dev/null 2>&1; then
                    _probe_path="dolt embedded (${_probe_doltdb})"
                else
                    _probe_path="file fallback (${SPIRA_DB}/events.log)"
                fi
            else
                _probe_path="bd sql (server mode at ${SPIRA_DB})"
            fi
            # REFUSED AND DISCARDED ARE DIFFERENT FAULTS. A rejected write names a
            # constraint or a permission; a write that returns success and then cannot be
            # read back names a store that is not persisting. Calling the first a discard
            # sends the reader to check permissions on a path that was never the problem.
            if [ "$_probe_wrote" -eq 0 ]; then
                FAIL "events write/read round trip failed — the write via ${_probe_path} was refused (anchor bead: ${_probe_id})" \
                     "Every write path rejected the event. Run the INSERT by hand to see the error;
        a foreign-key refusal means the anchor bead no longer exists, a permission error means
        ${SPIRA_DB} is not writable by the harness process."
            else
                FAIL "events write/read round trip failed — writes via ${_probe_path} are discarded" \
                     "The write reported success and did not come back. Check that the events path is writable.
        On an embedded store without dolt, ${SPIRA_DB}/events.log must be writable by the harness process."
            fi
        fi
    fi
    unset _probe_id _probe_etype _probe_count _probe_doltdb _probe_path _probe_wrote
else
    WARN "cannot probe events substrate — no database yet"
fi

echo
echo "bd schema"
# BD MIGRATION COUNT vs DATABASE CURSOR. When the installed bd knows more or fewer
# migrations than the database cursor, bd exits 0 with the complaint on stdout — callers
# that check exit status read success and parse the error message as data. This was the
# failure mode on 2026-09-08: a rebuild replaced the installed bd with one that knows only
# v53, the production database was at v61, and the harness summoned nothing for six minutes
# while the panes showed 0 ready rather than a fault.
#
# `bd migrate schema` WITHOUT --ignore-schema-skew is the discriminating check: it exits 0
# and names the matching version on agreement, and exits non-zero naming both counts when
# they differ. The pair (installed bd, database) must stay in lock-step; rebuild one and
# the other may need to move too. Record the installed bd's state right after any rebuild:
#   spira/bd-pin.sh write
if [ -d "$SPIRA_DB/.beads" ]; then
    if _schema_out="$(timeout 30 bd -C "$SPIRA_DB" migrate schema 2>&1)"; then
        _schema_ver="$(printf '%s\n' "$_schema_out" | grep -oE 'already at v[0-9]+' | grep -oE '[0-9]+')"
        OK "bd migration count matches database cursor (v${_schema_ver:-?})"
    else
        _schema_db="$(printf '%s\n' "$_schema_out" | grep -oE 'database is at v[0-9]+' | grep -oE '[0-9]+')"
        _schema_bd="$(printf '%s\n' "$_schema_out" | grep -oE 'binary knows up to v[0-9]+' | grep -oE '[0-9]+')"
        # Report both versions, rendering ? when one cannot be read. Previously this fell
        # to a generic "cannot verify" message when only one version was parseable — losing
        # the partial information that names which side the mismatch is on. (sp-1khst)
        if [ -n "${_schema_db:-}" ] || [ -n "${_schema_bd:-}" ]; then
            FAIL "bd migration count (v${_schema_bd:-?}) does not match database cursor (v${_schema_db:-?})" \
                 "Rebuild bd from the commit recorded in $SPIRA_BD_PIN, then run:
        $SPIRA_HOME/bd-pin.sh write"
        else
            FAIL "bd migrate schema failed — cannot verify migration count" \
                 "$(printf '%s' "$_schema_out" | head -2)"
        fi
    fi
    unset _schema_out _schema_ver _schema_db _schema_bd
    if [ -f "${SPIRA_BD_PIN:-}" ]; then
        _pin_migs="$(grep '^BD_PIN_MIGRATIONS=' "$SPIRA_BD_PIN" 2>/dev/null | cut -d= -f2)"
        OK "bd pin at $SPIRA_BD_PIN (pinned v${_pin_migs:-?})"
        unset _pin_migs
    else
        WARN "no bd pin file at ${SPIRA_BD_PIN:-<unset>}" \
             "After installing a new bd binary, record it: $SPIRA_HOME/bd-pin.sh write"
    fi
else
    WARN "cannot check bd schema — no database yet"
fi
# BD VERSION TAG CHECK. Migration count proves binary and database are in step; tag proves
# the binary is the one the harness decided on. A binary whose schema matches but whose tag
# does not may lack commands or change semantics silently (the v1.2.2/v1.2.1 incident:
# newer number, older code, --all quietly not meaning all).
_bdtag_out="$(timeout 5 "$SPIRA_BD" version 2>/dev/null | head -1 || true)"
_bdtag_ver="$(printf '%s\n' "$_bdtag_out" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)"
_bdtag_pin="${SPIRA_BD_TAG#v}"
if [ -z "${_bdtag_ver:-}" ]; then
    WARN "cannot read version from '$SPIRA_BD version' — tag pin is ${SPIRA_BD_TAG:-<unset>}" \
         "Output: $_bdtag_out"
elif [ "$_bdtag_ver" = "$_bdtag_pin" ]; then
    OK "bd version matches tag pin ($SPIRA_BD_TAG)"
else
    # Distinguish ahead (installed is newer) from behind (installed is older). A version
    # that is AHEAD of the pin may have migrated the store past what other tooling can read
    # — and a migration is one-way. BEHIND means missing commands or silent semantic changes.
    _bdtag_skew="$(awk -v a="$_bdtag_ver" -v b="$_bdtag_pin" 'BEGIN{
        split(a,av,"."); split(b,bv,".")
        for(i=1;i<=3;i++){av[i]+=0;bv[i]+=0
            if(av[i]<bv[i]){print "BEHIND"; exit}
            if(av[i]>bv[i]){print "AHEAD";  exit}
        }
        print "MISMATCH"
    }')"
    if [ "$_bdtag_skew" = AHEAD ]; then
        FAIL "bd version mismatch — installed v$_bdtag_ver is AHEAD of pin $SPIRA_BD_TAG" \
             "A binary ahead of the pin may have migrated the store past what other tooling
        can read; migration is one-way. Install the pinned version:
        $SPIRA_HOME/build-bd.sh --install
        or: $SPIRA_HOME/build-bd.sh --from-release --install"
    else
        FAIL "bd version mismatch — installed v$_bdtag_ver is BEHIND pin $SPIRA_BD_TAG" \
             "The harness expects $SPIRA_BD_TAG; a binary behind the pin may lack commands
        or change semantics silently. Install the pinned version:
        $SPIRA_HOME/build-bd.sh --install
        or: $SPIRA_HOME/build-bd.sh --from-release --install"
    fi
fi
unset _bdtag_out _bdtag_ver _bdtag_pin _bdtag_skew

echo
echo "statutes"
if command -v bd >/dev/null 2>&1 && [ -d "$SPIRA_DB/.beads" ]; then
    missing="$("$SPIRA_HOME/seed.sh" --list 2>/dev/null | grep -c ' -$' || true)"
    if [ "${missing:-0}" -gt 0 ]; then
        WARN "$missing shipped statutes are not in this database" \
             "Agents read their law from the database, not from the repository.
        Write them in with: $SPIRA_HOME/seed.sh"
    else
        OK "every shipped statute is in force"
    fi
else
    WARN "cannot check the statute book without bd and a database"
fi

echo
echo "goal bead"
# A DANGLING SPIRA_GOAL IS A SILENT FAULT. When the configured goal bead does not exist,
# goal_open_children returns nothing, n_open=0, and the sentinel announces "goal reached"
# on every pass regardless of actual state (sp-ejf3). Every correct completion signal is
# indistinguishable from this broken one, so the signal is worthless until the goal is real.
if [ -d "$SPIRA_DB/.beads" ]; then
    if [ -n "$(timeout 10 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$SPIRA_GOAL" --json 2>/dev/null | sed -n '/^[[{]/,$p' | head -c 1)" ]; then
        OK "goal bead $SPIRA_GOAL exists"
    else
        FAIL "SPIRA_GOAL=$SPIRA_GOAL names no bead in $SPIRA_DB — sentinel will announce 'goal reached' on every pass" \
             "Create the goal bead, point SPIRA_GOAL at an existing one in ${CONF:-spira.conf}, or treat an absent goal as an explicit supported state."
    fi
else
    WARN "cannot check goal bead — no database yet"
fi

echo
echo "repositories"
if [ ! -f "$SPIRA_REPO_MAP" ]; then
    FAIL "no repo-map at $SPIRA_REPO_MAP" \
         "Copy $SPIRA_HOME/repo-map.example to repo-map and write your own rows."
else
    case "$SPIRA_REPO_MAP" in
        *.example) WARN "still reading the EXAMPLE map — its rows name repositories that do not exist" \
                        "Copy it to $SPIRA_HOME/repo-map and write your own rows." ;;
        *)         OK "map at $SPIRA_REPO_MAP" ;;
    esac
    . "$SPIRA_HOME/lib.sh"
    home="$(spira_home_repo)"
    # A ROW WITH FEWER THAN SIX COLUMNS MISDIRECTS ITS GATE AS A FORMATTER. The NF-based
    # back-compat in repo_field reads the gate from $5 on a five-field row, $4 on a
    # four-field row, and so on — so a row that drops FORMAT rather than leaving it empty
    # runs the gate expression (e.g., `origin/main`, a test command) as a formatter in the
    # branch's tree after a rebase and commits the result. The fix is not in repo_field,
    # which preserves the historical shape intentionally; the fix is refusing to load a row
    # this narrow in the first place (law-a-documented-control-must-exist).
    while IFS= read -r narrow; do
        [ -n "$narrow" ] || continue
        FAIL "repo:$narrow — fewer than six columns (FORMAT missing or row truncated)" \
             "A short row runs its gate field as a formatter in the branch's tree after a
        rebase and commits the result. Add the missing | delimiters so every field has its
        own column, leaving FORMAT empty where no formatter is needed."
    done < <(awk 'BEGIN{FS="|"} /^[ \t]*#/{next}
                  {n=$1; gsub(/^[ \t]+|[ \t]+$/,"",n)
                   if (n!="" && NF>1 && NF<6) print n}' \
        "$SPIRA_REPO_MAP" 2>/dev/null || true)
    repo_root "$home" >/dev/null 2>&1 \
        && OK "the home repository '$home' has a row" \
        || FAIL "the home repository '$home' has no row in the map" \
                "A bead that names no repository resolves to '$home', and an unmapped name is
        refused rather than guessed. Add a row, or set SPIRA_HOME_REPO in ${CONF:-spira.conf}."
    for n in $(repo_names); do
        p="$(repo_field "$n" path)"
        b="$(repo_field "$n" base)"
        if [ ! -e "$p/.git" ]; then
            WARN "repo:$n — $p is not a checkout" "Another machine's row, or a path that has moved."
            continue
        fi
        if [ -z "$b" ]; then
            WARN "repo:$n — no base declared; it will be resolved, and refused if it cannot be"
        elif ! git -C "$p" rev-parse --verify -q "$b" >/dev/null 2>&1; then
            FAIL "repo:$n — declared base '$b' does not exist in $p" \
                 "Work would be rebased onto a ref nobody chose. Do not assume 'main'."
        else
            OK "repo:$n — $p on $b"
        fi
    done

    # HOW MANY COPIES OF THE HARNESS THIS BOX HAS. One is the answer; anything else means
    # work aimed at the harness can land in a tree nothing executes, pass every check there,
    # and never run. Nothing else here compares the two, which is what makes that failure
    # silent — landed and in effect quietly became different claims.
    #
    # `copies` and not `check`: doctor is read-only, and the full check fetches and
    # escalates. This is the structural half, which costs an ls-files per repository.
    #
    # A repository the map names that this box does not have is skipped by `copies`, so a
    # verdict of "one" here is about this box and not about the map.
    if ! copies="$(bash "$SPIRA_HOME/skew.sh" copies 2>/dev/null)"; then
        WARN "no mapped repository carries a harness this box can find" \
             "Either no row points at a real checkout, or the signature has changed.
        $SPIRA_HOME/skew.sh copies"
    elif [ "$(grep -c ' second$' <<< "$copies")" -gt 0 ]; then
        while read -r n p d k; do
            [ "$k" = second ] || continue
            # WARN and not FAIL, per this file's own line: the loop runs perfectly well with
            # two copies, which is exactly what makes the fault silent. The loud channel is
            # skew.sh's escalation; doctor's job is to name it in the one pass.
            WARN "repo:$n carries a SECOND harness at $p/$d" \
                 "The harness in force is $SPIRA_REPO. Work landing in that other copy passes
        its own gate and its own suites, closes its bead naming a real commit, and never runs.
        Delete the copy, or point this installation at it — but not both.
        $SPIRA_HOME/skew.sh check"
        done <<< "$copies"
    else
        OK "one harness on this box — $SPIRA_REPO is the only copy the map reaches"
    fi
fi

echo
echo "the cockpit"
[ -d "$SPIRA_COCKPIT" ] && OK "cockpit at $SPIRA_COCKPIT" \
    || WARN "no cockpit directory at $SPIRA_COCKPIT" "The loop runs; you have no way to see it."
if [ -x "$SPIRA_PANEL" ]; then OK "attention panel built at $SPIRA_PANEL"
else WARN "attention panel not built at $SPIRA_PANEL" \
          "Build it: cd $SPIRA_COCKPIT/panel && cargo build --release"; fi
# SNAPSHOT FRESHNESS. The collector writes cockpit.env on every tick; absence or a stale
# mtime means the supervisor is not writing — the exact condition that went undetected
# because nothing else checked it (sp-itsy). Uses SPIRA_COCKPIT_STALE_S as the threshold.
_dr_snap="$SPIRA_RUN/cockpit.env"
if [ ! -f "$_dr_snap" ]; then
    WARN "no cockpit snapshot at $_dr_snap — collector may not have run yet" \
         "Check: systemctl --user status $(spira_unit cockpit service)"
else
    _dr_snap_age=$(( $(date +%s) - $(stat --format='%Y' "$_dr_snap" 2>/dev/null || echo 0) ))
    if [ "$_dr_snap_age" -gt "${SPIRA_COCKPIT_STALE_S:-120}" ]; then
        WARN "cockpit snapshot stale — last written ${_dr_snap_age}s ago (limit ${SPIRA_COCKPIT_STALE_S:-120}s)" \
             "The collector is not writing. Check: systemctl --user status $(spira_unit cockpit service)"
    else
        OK "cockpit snapshot fresh — $_dr_snap (${_dr_snap_age}s old)"
    fi
fi
unset _dr_snap _dr_snap_age
[ -x "$SPIRA_HOME/mail.sh" ] && OK "mail delivery path ready at $SPIRA_HOME/mail.sh" \
    || FAIL "mail.sh not found at $SPIRA_HOME/mail.sh" \
            "A mail that reaches nobody is worse than an unanswered question (law-answers-need-a-delivery-path)."
# THE VIEW FOLLOWER is optional — empty means no follower — but WHEN SET, it must exist and
# be executable, or a manifest row silently renders as `off` while the operator believes it is
# configured. The contract: `<prog> watch` loops forever (the unit starts it), `<prog> status`
# prints `want: <session>` (the health assertion reads it).
if [ -n "$SPIRA_VIEW" ]; then
    if [ -x "$SPIRA_VIEW" ]; then
        OK "view follower at $SPIRA_VIEW"
        if out="$("$SPIRA_VIEW" status 2>&1)" && printf '%s\n' "$out" | grep -q '^want:'; then
            OK "view follower answers status — ${out%%$'\n'*}"
        else
            WARN "view follower exists but 'status' does not print 'want:'" \
                 "The health assertion will read it as degraded.
        Run: $SPIRA_VIEW status"
        fi
    else
        WARN "SPIRA_VIEW is set but $SPIRA_VIEW is not executable" \
             "The manifest's ?view row will render as 'off'. Set SPIRA_VIEW in ${CONF:-spira.conf}."
    fi
fi

echo
echo "the status line"
# WHY THIS IS DOCTOR'S BUSINESS AT ALL. The status line is where the context meter and the
# archivist's state machine are read, and it lives in the CLIENT's settings file, outside every
# repository — so nothing the harness ships can set it, and nothing that lands here can fix it.
# What the harness owes instead is to say so, once, in the one pass that names everything else.
#
# AND THE REFRESH INTERVAL IS THE LOAD-BEARING HALF. The client re-runs a status-line command on
# a session starting, a new assistant message, a compaction finishing, a mode change, and a
# timer — and clearing the session is not on that list. So without the timer the meter goes on
# displaying the DISCARDED session's context until something is next said, which means the
# instrument that exists to say whether clearing was worth doing reports that the clear did not
# work. Every other state this feature renders — sweeping, archiving, safe to clear — likewise
# changes while the session is IDLE, which is the definition of background work: with no timer
# the pane can only show what was already true at the last assistant message, and "safe to
# clear" would arrive one turn after it stopped being useful.
SETTINGS="$SPIRA_CLIENT_SETTINGS"
if [ ! -f "$SETTINGS" ]; then
    WARN "no client settings at $SETTINGS — the context meter is not on the status line" \
         "Point statusLine.command at $SPIRA_HOME/ctx-meter.sh, with refreshInterval beside it."
else
    sl="$(python3 "$SPIRA_HOME/statusline-check.py" "$SETTINGS" "$SPIRA_HOME/ctx-meter.sh" "$SPIRA_RUN")"
    case "$sl" in
        unreadable*) WARN "cannot read $SETTINGS — ${sl#unreadable }" ;;
        absent)      WARN "no status line configured — the context meter is not being shown" \
                          "Add a statusLine object to $SETTINGS whose command is
        $SPIRA_HOME/ctx-meter.sh, with \"refreshInterval\": 5 beside it." ;;
        "stale "*)
            _sl_rest="${sl#stale }"
            _sl_path="${_sl_rest% *}"
            case "$_sl_path" in
                "$SPIRA_RUN"/worktree/*)
                    WARN "the status line runs a copy of the context meter from a worktree — $_sl_path" \
                         "That worktree is deleted when the Sending reaps it, so the status line
        is one landing away from rendering nothing. If the copy carries behaviour the
        landed one lacks, land that work first: $SPIRA_HOME/ctx-meter.sh is what survives." ;;
                /tmp/*)
                    WARN "the status line runs a copy of the context meter under /tmp — $_sl_path" \
                         "Lost on reboot. If it carries behaviour the landed one lacks, land that
        work first: $SPIRA_HOME/ctx-meter.sh is what survives." ;;
                *)
                    WARN "the status line runs a different copy of the context meter — $_sl_path" \
                         "If it carries behaviour the landed one lacks, the fix is to land that
        work, not to repoint. The landed meter is $SPIRA_HOME/ctx-meter.sh." ;;
            esac ;;
        "fragile "*)
            _sl_rest="${sl#fragile }"
            _sl_path="${_sl_rest% *}"
            case "$_sl_path" in
                "$SPIRA_RUN"/worktree/*)
                    WARN "the status line command lives in a worktree — $_sl_path" \
                         "That worktree is deleted when the Sending reaps it. The status line
        is one landing away from rendering nothing." ;;
                /tmp/*)
                    WARN "the status line command lives under /tmp — $_sl_path" \
                         "Lost on reboot." ;;
            esac ;;
        "other "*)   WARN "the status line runs a different command — the context meter is not being shown" \
                          "Point statusLine.command at $SPIRA_HOME/ctx-meter.sh." ;;
        "ours -")    WARN "the status line has no refreshInterval — it cannot update between turns" \
                          "It will go on showing a cleared session's context until something is next said, and
        the archivist's state will never change on its own. Add \"refreshInterval\": 5 beside
        \"command\" in the statusLine object in $SETTINGS." ;;
        "ours "*)    OK "status line on the context meter, refreshing every ${sl#ours }s" ;;
        *)           WARN "could not judge the status line configuration in $SETTINGS" ;;
    esac
fi

# AND THE SESSION HOOK, in the same file and for the same reason: nothing that lands in this
# repository can register it, so what the harness owes is to say whether it is registered.
# The failure this catches is not "never installed" — it is a registration still pointing at a
# harness that was decommissioned, which goes on printing its banner into every session on the
# box and looks, from inside that session, exactly like a working one.
hookout="$("$SPIRA_HOME/install-session-hook.sh" status 2>&1)"; hookrc=$?
if [ "$hookrc" = 0 ]; then
    OK "the session hook is registered on every session-start event"
else
    WARN "the session hook is not registered — a new session is told nothing about the watchers" \
         "Run $SPIRA_HOME/install-session-hook.sh install."
fi
# A COMMAND THAT IS NOT OURS IS REPORTED, NEVER REMOVED. Two session hooks both reporting on
# watchers is the state this replaced, and which of them the operator wants is theirs to say.
# A HERE-STRING AND NOT A PIPE, because the loop increments the warning tally and the right
# side of a pipe is a subshell — every warning raised in one would be printed and then
# forgotten by the count that decides what this command reports at the end.
while IFS= read -r l; do
    [ -n "$l" ] || continue
    WARN "another command is registered on that event:${l#*other}" \
         "If it is stale, remove it with $SPIRA_HOME/install-session-hook.sh prune <substring>."
done <<< "$(printf '%s\n' "$hookout" | grep '^  other' || true)"

# ORPHANED GAS TOWN WATCHER PROCESSES. When SPIRA_TOWN is set the operator had a predecessor
# harness. Its watcher processes — daemon-kind rows started with nohup by its watchd.sh — are
# not owned by systemd and survive indefinitely, invisible to every `is-active` check. They
# write to logs nothing here reads, they cannot see new events from the Spira database, and
# they look healthy in every process listing because they ARE running, just watching the wrong
# thing. The discriminating check is /proc: a process whose argv[1] is a script under
# $SPIRA_TOWN/settings/ is one the predecessor started, not this harness.
#
# NEVER pkill -f — the pattern is a substring of the caller's own command line.
if [ -n "${SPIRA_TOWN:-}" ]; then
    _wd_orphans=""
    for _d in /proc/[0-9]*; do
        [ -r "$_d/cmdline" ] || continue
        _cmd="$(tr '\0' '\n' < "$_d/cmdline" 2>/dev/null)" || continue
        # argv[1] is the second NUL-delimited field, which becomes the second line after tr.
        _arg1="$(printf '%s\n' "$_cmd" | sed -n '2p')"
        case "$_arg1" in
            "$SPIRA_TOWN/settings/watch-"*)
                _wd_orphans="${_wd_orphans}${_wd_orphans:+ }${_d##*/}" ;;
        esac
    done
    if [ -n "$_wd_orphans" ]; then
        WARN "$(printf '%s' "$_wd_orphans" | wc -w | tr -d ' ') Gas Town watcher process(es) still running under $SPIRA_TOWN" \
             "Kill each by PID after confirming /proc/<pid>/cmdline — never pkill -f, whose pattern
        matches the caller's own argv. PIDs: $_wd_orphans"
    else
        OK "no orphaned Gas Town watcher processes under $SPIRA_TOWN"
    fi
fi
unset _wd_orphans _d _cmd _arg1

echo
echo "loom"
# THE BINARY IS NOT BUILT BY INSTALL. It is a Rust binary that must be compiled separately:
# `cd loom && cargo build --release`. Doctor says so rather than guessing the binary is fine.
if [ -x "${SPIRA_LOOM_BIN:-}" ]; then
    OK "loom binary built at $SPIRA_LOOM_BIN"
    # A SERVICE THAT IS NOT ACTIVE IS NOT SERVING. `is-active` returns a non-zero exit code
    # and prints `inactive` (or `unknown`) when the unit is not running. Checking it here
    # rather than in the loop above puts it beside the binary check that is its prerequisite.
    _loom_unit="$(spira_unit loom service)"
    if [ "$_loom_unit" = '?' ]; then
        WARN "Loom service unit not found — install.sh may not have run yet" \
             "Run install.sh to enable and start Loom on every boot."
    elif systemctl --user is-active --quiet "$_loom_unit" 2>/dev/null; then
        OK "$_loom_unit is active — Loom is reachable at $SPIRA_LOOM_ADDR"
    else
        WARN "$_loom_unit is not active — Loom is not reachable" \
             "Run: systemctl --user start $_loom_unit
        Or run install.sh to enable and start it on every boot."
    fi
else
    WARN "loom binary not built at ${SPIRA_LOOM_BIN:-<unset>}" \
         "Build it: cd $SPIRA_REPO/loom && cargo build --release"
fi

echo
echo "installed units"
# DUPLICATE UNIT DETECTION. Before per-instance naming, spira-* units were installed under
# their plain names (e.g., spira-sentinel.service). install.sh migrates them: it disables the
# old name and removes the file. If a plain-named unit file coexists with its instance-named
# successor, the migration's rm step was absent or the file was placed by hand — and
# `systemctl list-unit-files` then shows two units claiming the same service.
#
# The discriminating check: for each plain spira-<name>.<ext> in the user unit directory,
# check whether spira-<name>-<instance>.<ext> also exists. That coexistence is the defect.
# The shared-unit exceptions (cockpit-ensure, concierge, beads-push, dolt-beads) install
# under their plain names permanently and are excluded by the `spira-` prefix requirement.
#
# SPIRA_SYSTEMCTL is the override used by tests to substitute a fake systemctl.
_dr_sc="${SPIRA_SYSTEMCTL:-systemctl}"
_dr_unit_dir="${HOME}/.config/systemd/user"
_dr_dup_found=0
if [ -d "$_dr_unit_dir" ] && [ -n "${SPIRA_INSTANCE:-}" ]; then
    for _dr_f in "$_dr_unit_dir"/spira-*.service "$_dr_unit_dir"/spira-*.timer; do
        [ -e "$_dr_f" ] || continue
        _dr_base="$(basename "$_dr_f")"
        # Skip the watcher template (spira-watch@.service) — it is never a duplicate.
        case "$_dr_base" in spira-watch@*) continue ;; esac
        # Skip units that already carry an instance suffix: we are looking for plain names.
        # A plain name ends in .service or .timer with no preceding -<instance> segment.
        # The instance suffix is SPIRA_INSTANCE, which is [A-Za-z0-9_-] by construction.
        _dr_ext="${_dr_base##*.}"       # service | timer
        _dr_stem="${_dr_base%.*}"       # spira-sentinel
        _dr_inst_name="${_dr_stem}-${SPIRA_INSTANCE}.${_dr_ext}"
        # If this file IS already the instance-named variant, skip it.
        [ "$_dr_base" = "$_dr_inst_name" ] && continue
        # If it ends with -<instance>.<ext> for any instance, it is already suffixed; skip.
        # (Handles the case where SPIRA_INSTANCE differs from what was installed.)
        case "$_dr_stem" in *"-${SPIRA_INSTANCE}") continue ;; esac
        # Check for a sibling with the instance suffix.
        if [ -e "$_dr_unit_dir/$_dr_inst_name" ]; then
            _dr_dup_found=$((_dr_dup_found + 1))
            # The sentinel uses the unit name as its ONLY concurrency control — sentinel.sh
            # says so at the lock comment. Two enabled units with the same ExecStart means
            # the mutex is gone: every pass runs twice, against the same free-slot count,
            # and strand.sh produces duplicate escalations for the same episode. This is a
            # fatal defect, not a warning. Other duplicate pairs are WARN because no other
            # unit here carries that invariant.
            case "$_dr_stem" in
                spira-sentinel)
                    FAIL "duplicate unit pair: $_dr_base and $_dr_inst_name both exist in $_dr_unit_dir" \
                         "The sentinel uses the unit name as its mutex; two copies running simultaneously is a defect.
        Delete the stale plain-named file and daemon-reload:
        rm $_dr_unit_dir/$_dr_base && systemctl --user daemon-reload" ;;
                *)
                    WARN "duplicate unit pair: $_dr_base and $_dr_inst_name both exist in $_dr_unit_dir" \
                         "The plain-named file is a stale legacy copy. Re-run install.sh to remove it,
        or delete it by hand: rm $_dr_unit_dir/$_dr_base && systemctl --user daemon-reload" ;;
            esac
        fi
    done
fi
[ "$_dr_dup_found" -eq 0 ] && OK "no duplicate plain/instance unit pairs"
unset _dr_sc _dr_unit_dir _dr_dup_found _dr_f _dr_base _dr_ext _dr_stem _dr_inst_name

echo
echo "enabled units"
# ENABLE-SET DRIFT. Every unit in the ENABLE set must be enabled in systemd unless the
# control plane records a deliberate suspension. A disabled unit with no control-plane
# entry is silent drift: nothing alerts, nothing fails, the feature it drives simply stops.
#
# Fail closed: if systemctl returns no output for a unit, the result is a FAIL rather than
# a clean pass — a probe that says clean when it cannot see is the silence that hides the
# outage (law-alerts-must-be-actionable).
#
# The ENABLE set is sourced from units.sh directly, not restated here. A second hand-
# written list rots the way the gate's suite list did: units.sh grows, the copy here does
# not, and the check passes on units it never knew to test.
_dr_en_sc="${SPIRA_SYSTEMCTL:-systemctl}"
_dr_en_bad=0
_dr_en_units_sh="$SPIRA_REPO/systemd/units.sh"
if [ -z "${_SPIRA_UNITS_LOADED:-}" ]; then
    { . "$_dr_en_units_sh"; } 2>/dev/null \
        || { FAIL "cannot source $_dr_en_units_sh — enabled-units check skipped" \
                  "Check that $_dr_en_units_sh exists and SPIRA_REPO is set correctly."
             _dr_en_bad=1; }
fi
if [ "${_dr_en_bad}" -eq 0 ]; then
    for _dr_en_unit in "${ENABLE[@]}"; do
        [ -n "$_dr_en_unit" ] || continue
        _dr_en_state="$("$_dr_en_sc" --user is-enabled "$_dr_en_unit" 2>/dev/null || true)"
        if [ -z "$_dr_en_state" ]; then
            FAIL "$_dr_en_unit — systemctl returned no output; cannot verify enablement state" \
                 "Check that the systemd user session is active: $_dr_en_sc --user status"
            _dr_en_bad=$((_dr_en_bad + 1))
            continue
        fi
        [ "$_dr_en_state" = "enabled" ] && continue
        # not-found means the unit file does not exist yet — a fresh install has none.
        # That is not drift; drift is a unit whose file IS installed but not enabled.
        [ "$_dr_en_state" = "not-found" ] && continue
        # Not enabled. Check the control plane before classifying as drift. The subject key
        # is derived the same way ctrl.sh divergence derives it: strip the file extension,
        # then strip the per-instance suffix.
        _dr_en_base="${_dr_en_unit%.*}"
        _dr_en_subj="${_dr_en_base%-${SPIRA_INSTANCE:-prod}}"
        if "$SPIRA_HOME/ctrl.sh" check "$_dr_en_subj" 2>/dev/null; then
            : # deliberately suspended — control-plane decision, not drift
        else
            FAIL "$_dr_en_unit is $_dr_en_state but has no control-plane suspension" \
                 "Enable it:   $_dr_en_sc --user enable $_dr_en_unit
        Suspend it:  $SPIRA_HOME/ctrl.sh suspend $_dr_en_subj --reason <why> --owner <bead>"
            _dr_en_bad=$((_dr_en_bad + 1))
        fi
    done
    [ "$_dr_en_bad" -eq 0 ] \
        && OK "all ${#ENABLE[@]} ENABLE units are enabled or suspended via ctrl.sh"
fi
unset _dr_en_sc _dr_en_bad _dr_en_units_sh _dr_en_unit _dr_en_state _dr_en_base _dr_en_subj

echo
echo "writable state"
if mkdir -p "$SPIRA_RUN" 2>/dev/null && [ -w "$SPIRA_RUN" ]; then OK "runtime directory $SPIRA_RUN"
else FAIL "cannot write $SPIRA_RUN" "Leases, logs and worktrees live here. Set SPIRA_RUN in ${CONF:-spira.conf}."; fi

echo
echo "promote"
# TWO SUPPORTED MODELS, AND ONE ERROR. The mode comes from spira_single_checkout in
# conf.sh so that this check and the unit manifest cannot reach different answers about
# one box; they did, and the box reported an un-suspended fatal for a promote unit that
# was correctly absent.
#
# SPLIT-CHECKOUT. SPIRA_PROD outside SPIRA_REPO. promote.sh carries commits from the dev
# checkout to a separate production clone on a timer; a dirty or mid-landing SPIRA_REPO
# does not reach the executing copy.
#
# SINGLE-CHECKOUT. SPIRA_PROD inside SPIRA_REPO — one tree, developed and executed. This
# was a FAIL, which made the mode unusable while spira-skew.service's own comment called
# it supported. Two of the three reasons that FAIL gave have since dissolved: promote.sh
# has no caller here at all (the manifest omits its units in this mode), and skew's
# two-checkout question is a separate defect. The third is real and survives as the body
# of this warning — an edit is live the moment it is saved, with no gate in between. That
# is a cost to state plainly, not a reason to refuse: a box whose production lives on
# another machine, reached through a tagged release, has no second checkout to promote
# into and cannot satisfy a rule demanding one.
if spira_single_checkout; then
    WARN "single-checkout mode: SPIRA_PROD ($SPIRA_PROD) is inside SPIRA_REPO" \
         "Edits here are live immediately — no promote step stands between saving and running. Commit or restore before you stop."
elif [ -z "${SPIRA_PROD:-}" ]; then
    FAIL "SPIRA_PROD is not set — nothing says what systemd should execute" \
         "Set SPIRA_PROD in ${CONF:-spira.conf} to the harness subdir inside the production clone, or to \$SPIRA_HOME for single-checkout mode."
elif [ -d "$SPIRA_PROD" ]; then
    OK "split-checkout mode: production at $SPIRA_PROD"
else
    WARN "SPIRA_PROD ($SPIRA_PROD) does not exist yet" \
         "The first call to promote.sh will clone from $SPIRA_REPO."
fi

echo
if [ "$fatal" -gt 0 ]; then
    printf '%d fatal, %d warnings — the harness will not run until the fatals are fixed.\n' "$fatal" "$warn"
    exit 1
fi
printf '0 fatal, %d warnings — the harness can run.\n' "$warn"
exit 0
