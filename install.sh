#!/usr/bin/env bash
# install.sh — the one entry point for installing Spira on a new machine.
#
# This is the installer. For the unit template renderer alone, see systemd/install.sh —
# it names this file in its header so a reader who found the wrong one is directed here.
#
# USAGE
#   install.sh [<instance>] [--dry-run] [--ephemeral] [--laptop] [--skip-build]
#              [--no-session-hook]
#
#   <instance>            Spira instance name (default: from config, usually 'prod')
#   --dry-run             print each phase's intended action; change nothing
#   --ephemeral           isolated instance for CI/test: own database, own SPIRA_RUN,
#                         statutes seeded, no session hook, no alert drop-ins,
#                         no concierge, agent pointed at a stub
#   --laptop              passed through to systemd/install.sh
#   --skip-build          skip build.sh (use when binaries are pre-built)
#   --no-session-hook     skip the session-hook registration (phase 5)
#
# NON-INTERACTIVE: every prompt has an env-var path (see configure.sh --help).
#
# PHASES
#   0.   preflight  — doctor.sh; any fatal stops the install             exits 1
#   0.5  conflicts  — five checks; each exits 5 with its remedy;
#                     override: SPIRA_INSTALL_CONFLICT_CONSIDERED=1
#   1.   config     — configure.sh; never overwrites an existing file
#   2.   build      — build.sh (skipped under --skip-build)
#   3.   database   — ensure Dolt data dir, bd init, seed.sh
#   4.   units      — systemd/install.sh <instance>; loginctl enable-linger
#   5.   hooks      — session hook (skipped under --ephemeral / --no-session-hook)
#                     and alert intake when SPIRA_ALERT_GLOB names units
#   6.   cockpit    — ~/.local/bin symlinks; build panes if tmux is reachable
#   7.   verify     — ready.sh; exits 3 when installed-but-not-ready
#
# EXIT CODES
#   0  ready — ready.sh passes (warns are allowed)
#   1  preflight refused — the box is missing something doctor.sh can name
#   2  a phase failed
#   3  installed but NOT ready — ready.sh exited non-zero
#   5  conflict — the box needs attention before a fresh install:
#        a different harness copy owns these unit names
#        a live aeon is running under this installation
#        a landing pass is in flight (gate flock held)
#        the instance argument disagrees with the existing config
#        a Dolt server is listening on the configured port with a different data dir
#
# CONFLICT OVERRIDE
#   Set SPIRA_INSTALL_CONFLICT_CONSIDERED=1 to bypass the conflict phase.
#   Every conflict guard names its own override so the fence is crossable.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ---------------------------------------------------------------------------
# ARGUMENT PARSING — before sourcing conf.sh so SPIRA_INSTANCE is in the
# environment when conf.sh derives instance-qualified paths.
# ---------------------------------------------------------------------------
_inst_arg=""
_dry=0
_ephemeral=0
_laptop=0
_skip_build=0
_no_hook=0

for _a in "$@"; do
    case "$_a" in
        --dry-run)        _dry=1 ;;
        --ephemeral)      _ephemeral=1 ;;
        --laptop)         _laptop=1 ;;
        --skip-build)     _skip_build=1 ;;
        --no-session-hook) _no_hook=1 ;;
        --*)              printf 'install: unknown flag: %s\n' "$_a" >&2; exit 2 ;;
        *) [ -z "$_inst_arg" ] && _inst_arg="$_a" \
           || { printf 'install: extra argument: %s\n' "$_a" >&2; exit 2; }
           ;;
    esac
done
unset _a
[ -n "$_inst_arg" ] && export SPIRA_INSTANCE="$_inst_arg"
unset _inst_arg

# Ephemeral: generate an isolated instance name so it cannot collide with prod.
if [ "$_ephemeral" = 1 ] && [ -z "${SPIRA_INSTANCE:-}" ]; then
    export SPIRA_INSTANCE="eph-$$"
fi

. "$HERE/spira/conf.sh"

SPIRA_LOGINCTL="${SPIRA_LOGINCTL:-loginctl}"

# ---------------------------------------------------------------------------
# DRY-RUN INFRASTRUCTURE
# Print intended action but do nothing.  phase_do wraps every mutating call.
# ---------------------------------------------------------------------------
_changes=0
phase_start() { printf '\n[%s]\n' "$1"; }
phase_info()  { printf '  %s\n' "$1"; }
phase_act() {
    # Usage: phase_act "<description>" <cmd> [args...]
    local desc="$1"; shift
    if [ "$_dry" = 1 ]; then
        printf '  would: %s\n' "$desc"
    else
        printf '  %s\n' "$desc"
        "$@"
        _changes=$((_changes+1))
    fi
}
phase_skip() { printf '  already done: %s\n' "$1"; }

_phase_fail() {
    local phase="$1" reason="$2"
    printf 'install: phase %s failed — %s\n' "$phase" "$reason" >&2
    exit 2
}

_conflict() {
    # Usage: _conflict <exit-code> <message> <remedy>
    # The one place all five conflict reports go through.
    local code="$1" msg="$2" remedy="$3"
    printf 'install: CONFLICT — %s\n' "$msg" >&2
    printf 'install:   remedy: %s\n' "$remedy" >&2
    printf 'install:   override: SPIRA_INSTALL_CONFLICT_CONSIDERED=1\n' >&2
    exit "$code"
}

# ---------------------------------------------------------------------------
# PHASE 0 — PREFLIGHT (doctor.sh)
# ---------------------------------------------------------------------------
phase_start "phase 0: preflight"
_doctor_out="$(SPIRA_DOCTOR_INSTALLING=1 "$SPIRA_HOME/doctor.sh" 2>&1)"
_doctor_rc=$?
printf '%s\n' "$_doctor_out" | sed 's/^/  /'
if [ "$_doctor_rc" != 0 ]; then
    printf '\ninstall: preflight failed — see doctor output above\n' >&2
    exit 1
fi
phase_info "preflight passed"

# ---------------------------------------------------------------------------
# PHASE 0.5 — CONFLICTS
# Skip the whole phase when the override is set.
# ---------------------------------------------------------------------------
if [ -z "${SPIRA_INSTALL_CONFLICT_CONSIDERED:-}" ]; then
    phase_start "phase 0.5: conflict checks"

    UNITDIR="$HOME/.config/systemd/user"

    # -------------------------------------------------------------------------
    # CONFLICT 1: a different harness copy already owns these unit names.
    #
    # Check the sentinel unit for this instance. Its ExecStart resolves to
    # $SPIRA_PROD/sentinel.sh, which lives under a SPIRA_HOME directory.
    # If that directory is not our SPIRA_HOME, the wrong harness is installed.
    # The confusion this prevents: installing from a second clone silently repoints
    # every unit to the new path — which is the skew skew.sh reports hourly.
    # -------------------------------------------------------------------------
    _inst_sentinel_unit="$(
        . "$HERE/systemd/units.sh" 2>/dev/null
        inst_name "spira-sentinel.service" 2>/dev/null || true
    )"
    _inst_unit_file="$UNITDIR/${_inst_sentinel_unit:-spira-sentinel-${SPIRA_INSTANCE:-prod}.service}"
    if [ -f "$_inst_unit_file" ]; then
        _inst_exec="$(grep -E '^ExecStart=' "$_inst_unit_file" 2>/dev/null | head -1 | cut -d= -f2-)"
        _inst_exec_dir="$(dirname "${_inst_exec%% *}" 2>/dev/null)"
        # The ExecStart points to $SPIRA_PROD/sentinel.sh. conf.sh sets SPIRA_PROD to SPIRA_HOME
        # when unset, so the parent's parent is SPIRA_HOME. More precisely: sentinel.sh lives in
        # SPIRA_HOME (the spira/ subdir), and SPIRA_PROD either equals SPIRA_HOME or is a
        # separate checkout that also contains a spira/. Comparing the realpath of the installed
        # dir against our SPIRA_HOME catches both cases.
        if [ -n "$_inst_exec_dir" ] && [ "$_inst_exec_dir" != "." ]; then
            _inst_real="$(realpath "$_inst_exec_dir" 2>/dev/null || printf '%s' "$_inst_exec_dir")"
            _our_real="$(realpath "$SPIRA_HOME" 2>/dev/null || printf '%s' "$SPIRA_HOME")"
            if [ -n "$_inst_real" ] && [ "$_inst_real" != "$_our_real" ]; then
                _conflict 5 \
                    "installed units for instance '${SPIRA_INSTANCE:-prod}' exec from $_inst_real (not $SPIRA_HOME)" \
                    "uninstall the other copy first, or re-run with SPIRA_INSTALL_CONFLICT_CONSIDERED=1 to repoint the units"
            fi
        fi
    fi
    phase_info "conflict 1 clear: no foreign harness owns these unit names"

    # -------------------------------------------------------------------------
    # CONFLICT 2: a live aeon is running under this installation.
    #
    # Resolve from /proc argv — never pgrep -f, which matches the caller's own
    # command line (law-a-pattern-match-is-not-an-identity-check).
    # -------------------------------------------------------------------------
    # grep -alFf reads all cmdlines in one pass; pattern via /dev/stdin so grep's own cmdline
    # does not contain it — the self-match 'grep -alF PATTERN' would cause
    # (law-a-pattern-match-is-not-an-identity-check). 30k+ threads: fork-per-PID ~2s, this ~0.02s.
    _live_aeon_pid=""
    _aeon_match="$(printf '%s\n' "$SPIRA_HOME/aeon.sh" \
        | grep -alFf /dev/stdin /proc/[0-9]*/cmdline 2>/dev/null | head -1)"
    if [ -n "$_aeon_match" ]; then
        _live_aeon_pid="${_aeon_match%/cmdline}"; _live_aeon_pid="${_live_aeon_pid##*/}"
    fi
    unset _aeon_match
    if [ -n "$_live_aeon_pid" ]; then
        _conflict 5 \
            "live aeon running under this installation (pid $_live_aeon_pid)" \
            "wait for it to finish, or run: $SPIRA_HOME/world.sh stop; then re-run install"
    fi
    phase_info "conflict 2 clear: no live aeons"

    # -------------------------------------------------------------------------
    # CONFLICT 3: a landing pass is in flight (gate flock held).
    #
    # The gate serialises on $SPIRA_RUN/worktree/.gate.<repo-basename>.lock.
    # flock -n exits 1 immediately when the lock is held.
    # -------------------------------------------------------------------------
    _gate_lock="$SPIRA_RUN/worktree/.gate.$(basename "$SPIRA_REPO").lock"
    if [ -f "$_gate_lock" ] && command -v flock >/dev/null 2>&1; then
        if ! flock -n "$_gate_lock" true 2>/dev/null; then
            _conflict 5 \
                "landing pass in flight — gate tree lock is held at $_gate_lock" \
                "wait for the landing pass to complete, then re-run install"
        fi
    fi
    phase_info "conflict 3 clear: no landing in flight"

    # -------------------------------------------------------------------------
    # CONFLICT 4: instance mismatch.
    #
    # Check two things:
    #   a) the argument disagrees with SPIRA_INSTANCE from the config file
    #   b) another installed instance's units point at this database
    # -------------------------------------------------------------------------
    # (a) argument vs. config SPIRA_INSTANCE
    _conf_instance="${SPIRA_CONF_FILE:+}"
    if [ -n "${SPIRA_CONF_FILE:-}" ] && [ -f "$SPIRA_CONF_FILE" ]; then
        _conf_inst_val="$(grep -E '^\s*SPIRA_INSTANCE\s*=' "$SPIRA_CONF_FILE" 2>/dev/null \
            | head -1 | sed 's/.*=\s*//' | tr -d ' ')"
        if [ -n "$_conf_inst_val" ] && [ "${SPIRA_INSTANCE:-prod}" != "$_conf_inst_val" ]; then
            _conflict 5 \
                "instance argument '${SPIRA_INSTANCE:-prod}' disagrees with config SPIRA_INSTANCE='$_conf_inst_val'" \
                "re-run without an instance argument, or edit SPIRA_INSTANCE in $SPIRA_CONF_FILE"
        fi
    fi
    # (b) another instance's units pointing at our database
    for _other_sent in "$UNITDIR"/spira-sentinel-*.service; do
        [ -f "$_other_sent" ] || continue
        _other_bn="$(basename "$_other_sent")"
        # Skip the unit for our own instance.
        _our_unit="${_inst_sentinel_unit:-spira-sentinel-${SPIRA_INSTANCE:-prod}.service}"
        [ "$_other_bn" = "$_our_unit" ] && continue
        # Extract SPIRA_DB path from StandardOutput or Environment lines that mention SPIRA_DB.
        # Most units embed SPIRA_RUN in StandardOutput; extract the run dir and compare.
        _other_run="$(grep -E '^(StandardOutput|StandardError)=append:' "$_other_sent" 2>/dev/null \
            | head -1 | sed 's/.*=append:\(.*\)\/[^/]*/\1/')"
        if [ -n "$_other_run" ]; then
            _other_real="$(realpath "$_other_run" 2>/dev/null || printf '%s' "$_other_run")"
            _our_run_real="$(realpath "$SPIRA_RUN" 2>/dev/null || printf '%s' "$SPIRA_RUN")"
            if [ "$_other_real" = "$_our_run_real" ]; then
                _other_inst="${_other_bn%.service}"; _other_inst="${_other_inst##*-}"
                _conflict 5 \
                    "instance '$_other_inst' units ($(basename "$_other_sent")) already point at SPIRA_RUN=$SPIRA_RUN" \
                    "use a different SPIRA_RUN, or uninstall the other instance first"
            fi
        fi
    done
    unset _other_sent _other_bn _other_run _other_real _other_inst _our_run_real
    phase_info "conflict 4 clear: no instance mismatch"

    # -------------------------------------------------------------------------
    # CONFLICT 5: a Dolt server is listening on the configured port with a
    # different data directory.
    #
    # Read the port from $SPIRA_DOLT_DATA/dolt-server.yaml (default 3307).
    # Probe with /dev/tcp. If listening, find the process via /proc and compare
    # its data_dir to our SPIRA_DOLT_DATA.
    # -------------------------------------------------------------------------
    if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
        _dolt_port=3307
        _yaml="$SPIRA_DOLT_DATA/dolt-server.yaml"
        if [ -f "$_yaml" ]; then
            _yaml_port="$(grep -E '^\s*port\s*:' "$_yaml" 2>/dev/null | head -1 \
                | sed 's/.*:\s*//' | tr -d ' ')"
            [ -n "$_yaml_port" ] && [ "$_yaml_port" -gt 0 ] 2>/dev/null && _dolt_port="$_yaml_port"
        fi
        # TCP probe — using /dev/tcp to avoid depending on netstat/ss.
        if (echo -n "" >/dev/tcp/127.0.0.1/"$_dolt_port") 2>/dev/null; then
            # Port is listening. Find the process and its data directory from /proc cmdline.
            # Pattern via /dev/stdin so grep's cmdline lacks it, avoiding self-match
            # (law-a-pattern-match-is-not-an-identity-check); tr is only called for matches.
            _found_other_dolt=0
            for _p in $(printf 'sql-server\n' \
                        | grep -alFf /dev/stdin /proc/[0-9]*/cmdline 2>/dev/null \
                        | sed 's|/cmdline$||'); do
                [ -r "$_p/cmdline" ] || continue
                _cmd="$(tr '\0' ' ' < "$_p/cmdline" 2>/dev/null)" || continue
                case "$_cmd" in *"sql-server"*) ;; *) continue ;; esac
                # Extract the data_dir from the --config yaml or from --data-dir flag.
                _cmd_datadir=""
                # Try --data-dir flag first.
                case "$_cmd" in *"--data-dir"*)
                    _cmd_datadir="$(printf '%s' "$_cmd" | grep -oE -- '--data-dir\s+\S+' | awk '{print $2}')"
                    ;;
                esac
                # Fall back to --config yaml.
                if [ -z "$_cmd_datadir" ]; then
                    _conf_arg="$(printf '%s' "$_cmd" | grep -oE -- '--config\s+\S+' | awk '{print $2}')"
                    if [ -n "$_conf_arg" ] && [ -f "$_conf_arg" ]; then
                        _cmd_datadir="$(grep -E '^\s*data_dir\s*:' "$_conf_arg" 2>/dev/null \
                            | head -1 | sed 's/.*:\s*//' | tr -d '"'"'"' ')"
                    fi
                fi
                if [ -n "$_cmd_datadir" ]; then
                    _cmd_real="$(realpath "$_cmd_datadir" 2>/dev/null || printf '%s' "$_cmd_datadir")"
                    _our_dolt_real="$(realpath "$SPIRA_DOLT_DATA" 2>/dev/null || printf '%s' "$SPIRA_DOLT_DATA")"
                    if [ "$_cmd_real" != "$_our_dolt_real" ]; then
                        _found_other_dolt=1
                        _conflict 5 \
                            "Dolt server listening on port $_dolt_port is serving '$_cmd_datadir' (not $SPIRA_DOLT_DATA)" \
                            "stop the other Dolt server or set SPIRA_DOLT_DATA to match its data directory"
                    fi
                fi
            done
            unset _p _cmd _conf_arg _cmd_datadir _cmd_real _our_dolt_real _found_other_dolt
        fi
        unset _dolt_port _yaml _yaml_port
    fi
    phase_info "conflict 5 clear: no Dolt port collision"

    unset _inst_sentinel_unit _inst_unit_file _inst_exec _inst_exec_dir _inst_real _our_real
    unset _live_aeon_pid _gate_lock _conf_instance _conf_inst_val _our_unit
fi  # end conflict checks

# Refuse a CONFIGURE_PROD that does not contain conf.sh — must be the harness
# subdir, not the clone root.
if [ -n "${CONFIGURE_PROD:-}" ] && [ ! -f "${CONFIGURE_PROD}/conf.sh" ]; then
    printf 'install: CONFIGURE_PROD (%s) does not contain conf.sh\n' "$CONFIGURE_PROD" >&2
    printf 'install:   set CONFIGURE_PROD to the harness subdir: %s/spira\n' "$CONFIGURE_PROD" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# PHASE 1 — CONFIG (configure.sh)
# Never overwrites; idempotent.
# ---------------------------------------------------------------------------
phase_start "phase 1: config"
_conf_dest="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
if [ -f "$_conf_dest" ]; then
    phase_skip "config exists at $_conf_dest"
else
    if [ "$_dry" = 1 ]; then
        phase_info "would run: spira/configure.sh --out $_conf_dest"
    else
        phase_info "running configure.sh"
        "$SPIRA_HOME/configure.sh" || _phase_fail "config" "configure.sh exited non-zero"
    fi
    _changes=$((_changes+1))
fi

# ---------------------------------------------------------------------------
# PHASE 2 — BUILD (build.sh)
# Skipped under --skip-build or --ephemeral.
# cargo build is incremental, so a re-run on an unchanged tree is cheap.
# ---------------------------------------------------------------------------
phase_start "phase 2: build"
if [ "$_skip_build" = 1 ] || [ "$_ephemeral" = 1 ]; then
    phase_skip "build skipped (--skip-build or --ephemeral)"
else
    if [ "$_dry" = 1 ]; then
        phase_info "would run: spira/build.sh"
    else
        phase_info "running build.sh"
        "$SPIRA_HOME/build.sh" || _phase_fail "build" "build.sh exited non-zero"
    fi
fi

# ---------------------------------------------------------------------------
# PHASE 3 — DATABASE
# Ensure Dolt data dir, init bd database, seed statutes.
# REFUSES a database path inside a git checkout: it accumulates internal working
# notes and is one 'git add -A' from publishing every bead body.
# ---------------------------------------------------------------------------
phase_start "phase 3: database"

# Refuse a DB inside a git checkout.
if [ -n "${SPIRA_DB:-}" ]; then
    _db_candidate="$SPIRA_DB"
    _walk="$_db_candidate"
    _inside_git=0
    while [ "$_walk" != "/" ] && [ -n "$_walk" ]; do
        if [ -d "$_walk/.git" ]; then
            _inside_git=1; break
        fi
        _walk="$(dirname "$_walk")"
    done
    if [ "$_inside_git" = 1 ]; then
        printf 'install: REFUSING database at %s — it is inside a git checkout (%s)\n' \
            "$_db_candidate" "$_walk" >&2
        printf 'install:   a git-tracked database accumulates internal notes and is one\n' >&2
        printf 'install:   "git add -A" from publishing every bead body.\n' >&2
        printf 'install:   Set SPIRA_DB outside any git checkout in spira.conf.\n' >&2
        exit 2
    fi
    unset _db_candidate _walk _inside_git
fi

# Dolt data directory — write dolt-server.yaml from the template if SPIRA_DOLT_DATA is set.
if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
    if [ ! -d "$SPIRA_DOLT_DATA" ]; then
        phase_act "create Dolt data directory: $SPIRA_DOLT_DATA" \
            mkdir -p "$SPIRA_DOLT_DATA"
    else
        phase_skip "Dolt data directory exists: $SPIRA_DOLT_DATA"
    fi
    _yaml_dest="$SPIRA_DOLT_DATA/dolt-server.yaml"
    _yaml_tpl="$HERE/systemd/dolt-server.yaml"
    if [ ! -f "$_yaml_dest" ] && [ -f "$_yaml_tpl" ]; then
        if [ "$_dry" = 1 ]; then
            phase_info "would write dolt-server.yaml to $_yaml_dest (from $_yaml_tpl)"
        else
            sed "s|@SPIRA_DOLT_DATA@|$SPIRA_DOLT_DATA|g" "$_yaml_tpl" > "$_yaml_dest"
            phase_info "wrote dolt-server.yaml to $_yaml_dest"
            _changes=$((_changes+1))
        fi
    else
        phase_skip "dolt-server.yaml already at $_yaml_dest"
    fi
    unset _yaml_dest _yaml_tpl
fi

_dolt_bg_pid=""
_db_fresh=0

# Database init — run bd init if no .beads yet.
if [ -d "${SPIRA_DB:-}/.beads" ]; then
    _meta="${SPIRA_DB}/.beads/metadata.json"
    if [ -f "$_meta" ]; then
        _stored_mode="$(python3 -c \
            'import json,sys; print(json.load(sys.stdin).get("dolt_mode",""))' \
            < "$_meta" 2>/dev/null || true)"
        if [ "${_stored_mode:-}" = "embedded" ]; then
            _phase_fail "database" \
                "existing store at $SPIRA_DB is embedded (one lock, all clients queue); set SPIRA_DOLT_DATA in spira.conf and re-run install.sh"
        fi
        unset _stored_mode
    fi
    unset _meta
    _bead_count="$(timeout 10 "$SPIRA_BD" -C "$SPIRA_DB" list --limit 0 --json 2>/dev/null \
        | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null || echo '?')"
    phase_skip "database exists at $SPIRA_DB ($_bead_count bead(s))"
else
    _db_fresh=1
    if [ "$_dry" = 1 ]; then
        if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
            phase_info "would start dolt server and run: (cd $SPIRA_DB && bd init --server --external)"
        else
            phase_info "would run: (cd $SPIRA_DB && bd init)"
        fi
    else
        phase_info "initialising database at $SPIRA_DB"
        mkdir -p "$SPIRA_DB"
        if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
            # SERVER MODE: start a temporary dolt, init in server mode, stop it.
            # Phase 4 installs and starts dolt-beads.service to manage it going forward.
            _dolt_port=3307
            _dolt_yaml="$SPIRA_DOLT_DATA/dolt-server.yaml"
            if [ -f "$_dolt_yaml" ]; then
                _p="$(grep -E '^\s*port\s*:' "$_dolt_yaml" 2>/dev/null | head -1 \
                    | sed 's/.*:\s*//' | tr -d ' ')"
                [ -n "$_p" ] && [ "$_p" -gt 0 ] 2>/dev/null && _dolt_port="$_p"
                unset _p
            fi
            _dolt_dbname="$(basename "$SPIRA_DB")"
            _dolt_bg_pid=""
            if ! (echo -n "" >/dev/tcp/127.0.0.1/"$_dolt_port") 2>/dev/null; then
                command -v dolt >/dev/null 2>&1 \
                    || _phase_fail "database" "dolt is not on PATH — required to start the server for bd init"
                phase_info "starting dolt server on port $_dolt_port for database init"
                dolt sql-server --config "$_dolt_yaml" </dev/null >/dev/null 2>&1 &
                _dolt_bg_pid=$!
                _dolt_wait=0
                while [ "$_dolt_wait" -lt 30 ]; do
                    (echo -n "" >/dev/tcp/127.0.0.1/"$_dolt_port") 2>/dev/null && break
                    sleep 1
                    _dolt_wait=$((_dolt_wait + 1))
                done
                if ! (echo -n "" >/dev/tcp/127.0.0.1/"$_dolt_port") 2>/dev/null; then
                    [ -n "$_dolt_bg_pid" ] && kill "$_dolt_bg_pid" 2>/dev/null || true
                    _phase_fail "database" "dolt server did not start on port $_dolt_port within 30s"
                fi
            fi
            ( cd "$SPIRA_DB" && BD_NON_INTERACTIVE=1 \
                "$SPIRA_BD" init --non-interactive --prefix sp \
                --skip-agents --skip-hooks \
                --server --server-host 127.0.0.1 --server-port "$_dolt_port" \
                --database "$_dolt_dbname" --external -q ) \
                || { [ -n "$_dolt_bg_pid" ] && kill "$_dolt_bg_pid" 2>/dev/null || true
                     _phase_fail "database" "bd init (server mode) failed"; }
            unset _dolt_port _dolt_yaml _dolt_dbname _dolt_wait
        else
            ( cd "$SPIRA_DB" && "$SPIRA_BD" init ) || _phase_fail "database" "bd init failed"
        fi
        _changes=$((_changes+1))
    fi
fi

# Seed statutes — run seed.sh if the database is present.
if [ -d "${SPIRA_DB:-}/.beads" ] || [ "$_dry" = 0 ]; then
    if [ "$_dry" = 1 ]; then
        phase_info "would run: spira/seed.sh (skips statutes already in force)"
    else
        phase_info "seeding statutes"
        "$SPIRA_HOME/seed.sh" 2>&1 | sed 's/^/  /'
        _seed_rc=${PIPESTATUS[0]}
        [ -n "$_dolt_bg_pid" ] && { kill "$_dolt_bg_pid" 2>/dev/null || true; }
        unset _dolt_bg_pid
        [ "$_seed_rc" != 0 ] && [ "$_db_fresh" = 1 ] && \
            _phase_fail "database" "seed.sh failed — statutes not seeded on fresh database"
        unset _seed_rc
    fi
fi

# ---------------------------------------------------------------------------
# PHASE 4 — UNITS (systemd/install.sh)
# Pass --laptop when requested.
# Enable loginctl linger, recording whether WE enabled it.
# ---------------------------------------------------------------------------
phase_start "phase 4: units"

# REFUSE a SPIRA_PROD that is inside a git checkout — a git pull would be a silent
# deploy with no audit trail. The release model requires SPIRA_PROD to resolve through
# SPIRA_RELEASES/current, the symlink activate.sh swaps atomically on each deployment.
# Override: SPIRA_INSTALL_PROD_GIT_CONSIDERED=1
if [ -z "${SPIRA_INSTALL_PROD_GIT_CONSIDERED:-}" ] && [ -n "${SPIRA_PROD:-}" ] && [ -e "$SPIRA_PROD" ]; then
    _prod_walk="$SPIRA_PROD"
    _prod_in_git=0
    while [ "$_prod_walk" != "/" ] && [ -n "$_prod_walk" ]; do
        if [ -d "$_prod_walk/.git" ] || [ -f "$_prod_walk/.git" ]; then
            _prod_in_git=1; break
        fi
        _prod_walk="$(dirname "$_prod_walk")"
    done
    if [ "$_prod_in_git" = 1 ]; then
        printf 'install: REFUSING — SPIRA_PROD (%s) is a git checkout\n' \
            "$SPIRA_PROD" >&2
        printf 'install:   The release model requires SPIRA_PROD to resolve through\n' >&2
        printf 'install:   %s/current (the symlink activate.sh swaps on each deploy).\n' \
            "$SPIRA_RELEASES" >&2
        printf 'install:   Activate a release tarball first: bash spira/activate.sh <tarball>\n' >&2
        printf 'install:   Override: SPIRA_INSTALL_PROD_GIT_CONSIDERED=1\n' >&2
        exit 2
    fi
    unset _prod_walk _prod_in_git
fi

# SPIRA_PROD must exist before systemd/install.sh renders units: it checks that every
# ExecStart target is executable. When SPIRA_PROD is under SPIRA_RELEASES (the release
# path), a missing directory means no release has been activated yet — promote.sh cannot
# create it. For a non-release SPIRA_PROD, promote.sh creates the checkout.
if ! spira_single_checkout && [ -n "${SPIRA_PROD:-}" ] && [ ! -d "$SPIRA_PROD" ]; then
    _prod_parent="$(dirname "$SPIRA_PROD" 2>/dev/null)"
    if [ "$_prod_parent" = "$SPIRA_RELEASES" ]; then
        _phase_fail "units" \
            "SPIRA_PROD ($SPIRA_PROD) does not exist — no release activated; run: bash spira/activate.sh <tarball>"
    fi
    unset _prod_parent
    if [ "$_dry" = 1 ]; then
        phase_info "would run: spira/promote.sh HEAD (SPIRA_PROD does not exist yet)"
    else
        phase_info "creating production checkout at $SPIRA_PROD"
        "$SPIRA_HOME/promote.sh" HEAD \
            || _phase_fail "units" "promote.sh failed to create $SPIRA_PROD"
        _changes=$((_changes+1))
    fi
fi

_unit_args=("${SPIRA_INSTANCE:-prod}")
[ "$_laptop" = 1 ] && _unit_args+=("--laptop")

if [ "$_dry" = 1 ]; then
    # Use --diff to show what would change without touching anything.
    _diff_out="$(SPIRA_INSTALL_FORCE=1 bash "$HERE/systemd/install.sh" \
        "${_unit_args[@]}" --diff 2>/dev/null)" || true
    if printf '%s\n' "$_diff_out" | grep -qE '^(MISSING|DIFFERS)'; then
        printf '%s\n' "$_diff_out" | sed 's/^/  /'
    else
        phase_skip "all units match what would be rendered"
    fi
else
    phase_info "installing units for instance '${SPIRA_INSTANCE:-prod}'"
    bash "$HERE/systemd/install.sh" "${_unit_args[@]}" \
        || _phase_fail "units" "systemd/install.sh exited non-zero"
    _changes=$((_changes+1))
fi

# Linger — enable so user units survive session logout.
# Record the stamp file only when we actually enable it, so uninstall knows we did it.
_linger_stamp="$SPIRA_RUN/install-linger-enabled"
_linger_user="${USER:-$(id -un 2>/dev/null || true)}"
_cur_linger="$("$SPIRA_LOGINCTL" show-user "$_linger_user" -p Linger 2>/dev/null || true)"
if [ "$_cur_linger" = "Linger=yes" ]; then
    phase_skip "linger already enabled for $_linger_user"
else
    if [ "$_dry" = 1 ]; then
        phase_info "would run: loginctl enable-linger $_linger_user"
    else
        "$SPIRA_LOGINCTL" enable-linger "$_linger_user" 2>/dev/null || true
        mkdir -p "$SPIRA_RUN"
        touch "$_linger_stamp"
        phase_info "enabled linger for $_linger_user (stamp: $_linger_stamp)"
        _changes=$((_changes+1))
    fi
fi
unset _linger_user _cur_linger _linger_stamp

# ---------------------------------------------------------------------------
# PHASE 5 — HOOKS
# Git hooks armed unconditionally (idempotent); session hook skipped under
# --ephemeral / --no-session-hook.
# ---------------------------------------------------------------------------
phase_start "phase 5: hooks"

if [ "$_dry" = 1 ]; then
    phase_info "would run: spira/exclude.sh install $SPIRA_REPO"
else
    "$SPIRA_HOME/exclude.sh" install "$SPIRA_REPO" 2>&1 | sed 's/^/  /' \
        || _phase_fail "hooks" "exclude.sh install failed — core.hooksPath not set"
fi

if [ "$_ephemeral" = 1 ] || [ "$_no_hook" = 1 ]; then
    phase_skip "session hook skipped (--ephemeral or --no-session-hook)"
else
    if [ "$_dry" = 1 ]; then
        _hook_status="$("$SPIRA_HOME/install-session-hook.sh" status 2>/dev/null)" || true
        if printf '%s\n' "$_hook_status" | grep -qE '^ok\s+SessionStart'; then
            phase_skip "session hook already installed"
        else
            phase_info "would run: spira/install-session-hook.sh install"
        fi
    else
        phase_info "installing session hook"
        "$SPIRA_HOME/install-session-hook.sh" install \
            || _phase_fail "hooks" "install-session-hook.sh failed"
        _changes=$((_changes+1))
    fi
fi

# Alert intake — wire SPIRA_ALERT_GLOB units if set. Skipped under --ephemeral.
if [ -n "${SPIRA_ALERT_GLOB:-}" ] && [ "$_ephemeral" = 0 ]; then
    if [ "$_dry" = 1 ]; then
        phase_info "would run: spira/install-intake.sh install (SPIRA_ALERT_GLOB=$SPIRA_ALERT_GLOB)"
    else
        phase_info "wiring alert intake for SPIRA_ALERT_GLOB=$SPIRA_ALERT_GLOB"
        "$SPIRA_HOME/install-intake.sh" install 2>&1 | sed 's/^/  /' || true
    fi
else
    phase_skip "alert intake skipped (SPIRA_ALERT_GLOB not set or --ephemeral)"
fi

# ---------------------------------------------------------------------------
# PHASE 6 — COCKPIT
# Link the two cockpit programs into ~/.local/bin.
# Build panes if a tmux server is reachable; print the one command otherwise.
# ---------------------------------------------------------------------------
phase_start "phase 6: cockpit"

_bin="$HOME/.local/bin"
_cockpit_dir="$(dirname "$SPIRA_HOME")/cockpit"
for _prog in cockpit cockpit-remote; do
    _src="$_cockpit_dir/$_prog"
    _link="$_bin/$_prog"
    if [ ! -f "$_src" ]; then
        phase_skip "$_prog not found at $_src — skipping"
        continue
    fi
    if [ -L "$_link" ]; then
        _cur_tgt="$(readlink "$_link" 2>/dev/null || true)"
        if [ "$_cur_tgt" = "$_src" ]; then
            phase_skip "~/.local/bin/$_prog -> $_src"
            continue
        fi
    fi
    if [ "$_dry" = 1 ]; then
        phase_info "would link: $_link -> $_src"
    else
        mkdir -p "$_bin"
        ln -sf "$_src" "$_link"
        phase_info "linked $_link -> $_src"
        _changes=$((_changes+1))
    fi
done
unset _prog _src _link _cur_tgt

# Build panes if tmux is reachable.
if command -v tmux >/dev/null 2>&1 && tmux list-panes -a >/dev/null 2>&1; then
    _panel="$(tmux list-panes -a -F '#{@cockpit}' 2>/dev/null | grep -c 'panel' || true)"
    if [ "${_panel:-0}" -gt 0 ]; then
        phase_skip "cockpit panes already present"
    else
        if [ "$_dry" = 1 ]; then
            phase_info "would run: cockpit/layout.sh up"
        else
            phase_info "building cockpit panes"
            "$_cockpit_dir/layout.sh" up 2>/dev/null || true
            _changes=$((_changes+1))
        fi
    fi
else
    phase_info "tmux server not reachable — build the cockpit when ready:"
    phase_info "  $SPIRA_REPO/cockpit/layout.sh up"
fi
unset _bin _cockpit_dir _panel

# ---------------------------------------------------------------------------
# PHASE 7 — VERIFY (ready.sh)
# Phase 7 is the point: the installer ends by proving what it asserts.
# ---------------------------------------------------------------------------
phase_start "phase 7: verify"

if [ "$_dry" = 1 ]; then
    if [ "$_changes" -gt 0 ]; then
        phase_info "dry-run complete — $_changes change(s) would be made"
    else
        phase_info "dry-run complete — no changes needed"
    fi
    phase_info "running ready.sh in read-only mode to show current state"
    "$SPIRA_HOME/ready.sh" 2>&1 | sed 's/^/  /' || true
    exit 0
fi

printf '\n'
"$SPIRA_HOME/ready.sh"
_ready_rc=$?
if [ "$_ready_rc" = 0 ]; then
    printf '\ninstall: done — Spira is ready.\n'
    exit 0
else
    printf '\ninstall: installation complete but ready.sh exited %s — installed but NOT ready\n' \
        "$_ready_rc" >&2
    printf 'install: see the output above for what is preventing the loop from receiving work\n' >&2
    exit 3
fi
