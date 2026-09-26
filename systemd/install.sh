#!/usr/bin/env bash
# install.sh — render the unit TEMPLATES for this box, install them, and start their timers.
#
# THIS IS THE UNIT RENDERER. For the full install (config, build, database, units,
# hooks, cockpit, verify), use install.sh at the REPOSITORY ROOT instead. If you ran
# this file first and wanted the full installer, go one directory up: ../install.sh
#
#   ./install.sh [<instance>]                      render, install, enable and start for the
#                                                  named instance (default: $SPIRA_INSTANCE
#                                                  from conf.sh, which is 'prod' on a clean
#                                                  install)
#   ./install.sh [<instance>] --diff               show how the installed units differ from
#                                                  what this instance would render, and change
#                                                  nothing
#   ./install.sh [<instance>] --render             show the rendered units on stdout and change
#                                                  nothing
#   ./install.sh [<instance>] --no-migrate-watchers  install and enable/restart units; migrate
#                                                  non-watcher legacy units normally but skip
#                                                  only the watcher migration loops, leaving any
#                                                  running watcher units untouched. The sentinel
#                                                  and all other non-watcher spira units ARE still
#                                                  migrated. Spared: spira-watch-* and
#                                                  spira-watch@* legacy forms only.
#
# THE FILES HERE ARE TEMPLATES, NOT UNITS. Every path in them is a placeholder — @SPIRA_HOME@,
# @SPIRA_DB@ and so on — filled from spira.conf. A unit file with a path baked into it runs on
# exactly one box, and systemd gives no clue when the path is wrong: a Documentation= line
# nobody reads and an ExecStart= that fails with a bare "No such file or directory" into a
# journal nobody is watching.
#
# NEVER EDIT AN INSTALLED UNIT. Edit the template and re-run this; `--diff` is how you find
# out that somebody did. The copies here are the source of truth, because
# ~/.config/systemd/user is one directory on one disk that nothing backs up.
#
# UNITS ARE CATTLE: unique plain names per instance — spira-sentinel-prod.service,
# spira-sentinel-test.service — no systemd templates, no %i, each unit carrying its
# instance written out in full. Units whose names start with 'spira-' get the instance
# suffix; units whose names do not (cockpit-ensure, concierge, beads-push, dolt-beads)
# are shared across instances and installed under their plain names. The watcher template
# (spira-watch@.service) is rendered once per manifest row, with %i substituted, and
# installed as spira-watch-<name>-<instance>.service — no systemd @-instantiation.
set -uo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# _seed_instance_conf <conf-file> <instance> -> append SPIRA_INSTANCE=<instance> unless
# that exact line is already there. APPEND, NOT OVERWRITE: the file may carry operator
# settings, and appending overrides any earlier value (spira_conf_read: last write wins)
# without disturbing lines placed before it. Defined before conf.sh is sourced so a test
# can source this file (BASH_SOURCE[0] != $0) and call it directly.
_seed_instance_conf() {
    local file="$1" inst="$2"
    grep -qxF "SPIRA_INSTANCE=$inst" "$file" 2>/dev/null && return 0
    printf 'SPIRA_INSTANCE=%s\n' "$inst" >> "$file"
    printf 'install: seeded %s with SPIRA_INSTANCE=%s\n' "$file" "$inst"
}

# _unit_action <changed> <masked> <suspended> <disabled> <halted> <active> -> action word.
# THE PER-UNIT APPLY DECISION, extracted so a T1 test can drive it directly — no rendered
# DEST tree, no recording systemctl (spira/test-install-decide.sh). Every argument is 0/1:
#   changed    rendered content differs from what is installed, or the unit is new
#   masked     symlinked to /dev/null by the operator
#   suspended  ctrl.sh reports this unit's subject suspended
#   disabled   systemctl is-enabled=disabled, and the unit is not newly installed this run
#   halted     $SPIRA_RUN/world.halted is present
#   active     systemctl is-active=active
# -> masked | suspended | operator-disabled | enable | skip | restart | enable-now
# ORDER IS THE CONTRACT: a masked unit stays masked even if also suspended or disabled: an
# operator's explicit mask must never be second-guessed by a control-plane state that could
# be stale. Suspended outranks disabled and halted for the same reason — ctrl.sh is the one
# surface that answers "why is this not running" and must stay authoritative over it.
# DEFINED BEFORE BOTH SOURCING GUARDS BELOW, alongside _seed_instance_conf, for the same
# reason: a test sources this file for the function alone and never reaches past here.
_unit_action() {
    local changed="$1" masked="$2" suspended="$3" disabled="$4" halted="$5" active="$6"
    if [ "$masked" = 1 ]; then echo masked; return 0; fi
    if [ "$suspended" = 1 ]; then echo suspended; return 0; fi
    if [ "$disabled" = 1 ]; then echo operator-disabled; return 0; fi
    if [ "$halted" = 1 ]; then echo enable; return 0; fi
    if [ "$changed" != 1 ] && [ "$active" = 1 ]; then echo skip; return 0; fi
    if [ "$changed" = 1 ] && [ "$active" = 1 ]; then echo restart; return 0; fi
    echo enable-now
}
# NAMED GUARD FOR THE T1 SEAM, independent of the generic sourcing guard just below: a
# later edit to that guard (e.g. if _seed_instance_conf's own sourcing use goes away)
# must not silently let a SPIRA_INSTALL_LIB=1 source run the whole installer.
if [ "${SPIRA_INSTALL_LIB:-0}" = 1 ]; then return 0 2>/dev/null || exit 0; fi

if [ "${BASH_SOURCE[0]}" != "$0" ]; then return 0 2>/dev/null || true; fi

# PARSE THE INSTANCE ARGUMENT AND THE MODE FLAG BEFORE SOURCING conf.sh SO THAT conf.sh
# DERIVES SPIRA_RUN, SPIRA_DB, ETC. FOR THE CORRECT INSTANCE. conf.sh reads SPIRA_INSTANCE
# from the environment before the config file, so setting it here in the environment wins.
_install_mode=""             # --diff | --render | --laptop | empty (install)
_install_instance=""         # explicit instance arg, empty means use conf.sh default
_skip_migrate_watchers=0     # 1 when --no-migrate-watchers is passed
for _a in "$@"; do
    case "$_a" in
        --diff|--render|--laptop) _install_mode="$_a" ;;
        --no-migrate-watchers)    _skip_migrate_watchers=1 ;;
        --*)                      ;;
        *) [ -z "$_install_instance" ] && _install_instance="$_a" ;;
    esac
done
unset _a
[ -n "$_install_instance" ] && export SPIRA_INSTANCE="$_install_instance"
unset _install_instance

. "$(cd "$SRC/../spira" && pwd -P)/conf.sh"
. "$(cd "$SRC/../spira" && pwd -P)/lib.sh"
DEST="$HOME/.config/systemd/user"

# SOURCE THE UNIT NAMING LIBRARY. inst_name, inst_watch_name, UNITS, ENABLE, OPTIONAL,
# _watch_names and watch_units all live there; sourcing once is enough.
# FOUND VIA THE REAL PATH OF THIS FILE, NOT VIA $SRC. $SRC resolves to the directory of
# the path used to invoke install.sh; when tests invoke it through a symlink in a fixture
# directory, $SRC resolves to that fixture directory, which has no units.sh. readlink -f
# follows the symlink to the real file and yields the real directory, where units.sh always
# lives alongside install.sh.
_install_real_dir="$(dirname "$(readlink -f "${BASH_SOURCE[0]}" 2>/dev/null || printf '%s' "${BASH_SOURCE[0]}")")"
. "$_install_real_dir/units.sh" || exit 1

# `dolt` is resolved once, absolutely, because a systemd unit has no PATH worth the name.
DOLT="$(command -v dolt 2>/dev/null || true)"

# render <template> [<watcher-name>] -> the unit for this instance on stdout.
#
# THE RENDERER ITSELF LIVES IN render.py, NOT HERE, and is shared with unit-ensure.sh:
# two independent copies of this substitution drifted apart once (unit-ensure.sh's
# never learned three keys added later to this one), so it silently failed to render
# any unit using them. Every value is HANDED IN, NOT INHERITED — conf.sh deliberately
# does not export anything derived from where it sits, so this passes them as named
# flags rather than reading an environment that will not have them, or relying on
# positional argv order (which breaks silently when a caller omits one).
render() {
    python3 "$_install_real_dir/render.py" "$1" \
        --home "$SPIRA_HOME" --repo "$SPIRA_REPO" --run "$SPIRA_RUN" --db "$SPIRA_DB" \
        --cockpit "$SPIRA_COCKPIT" --dolt-data "$SPIRA_DOLT_DATA" \
        --testdb-data "$SPIRA_TESTDB_DATA" --dolt "$DOLT" --prod "$SPIRA_PROD" \
        --instance "$SPIRA_INSTANCE" --testdb-port "$SPIRA_TESTDB_PORT" \
        --supervise-bin "$SPIRA_SUPERVISE_BIN" --snap-stale-s "$SPIRA_SNAP_STALE_S" \
        --landing-pass-bin "$SPIRA_LANDING_PASS_BIN" --watcher-name "${2:-}"
}

# --laptop: link only the cockpit dialer on this machine and exit. The two halves of the
# cockpit install on different machines; the server half uses the full install, the laptop
# half only needs the dialer. "Symlink this file yourself" rots — one command should do it.
if [ "$_install_mode" = "--laptop" ]; then
    _dialer="$(dirname "$SPIRA_HOME")/cockpit/remote/cockpit"
    if [ ! -x "$_dialer" ]; then
        printf 'install: --laptop: cockpit dialer not found at %s\n' "$_dialer" >&2
        printf 'install: --laptop: run the full install on the host; only the server needs systemd units\n' >&2
        exit 1
    fi
    mkdir -p "$HOME/.local/bin"
    ln -sf "$_dialer" "$HOME/.local/bin/cockpit"
    printf 'install: linked cockpit dialer: %s -> %s\n' "$HOME/.local/bin/cockpit" "$_dialer"
    exit 0
fi

if [ "$_install_mode" = "--render" ]; then
    for u in "${UNITS[@]}"; do
        [ "$u" = "spira-watch@.service" ] && continue
        printf '===== %s =====\n' "$(inst_name "$u")"
        render "$SRC/$u" || exit 1
    done
    for _wname in "${_watch_names[@]}"; do
        printf '===== %s =====\n' "$(inst_watch_name "$_wname")"
        render "$SRC/spira-watch@.service" "$_wname" || exit 1
    done
    exit 0
fi

# A UNIT FILE IN THIS DIRECTORY THAT IS NOT IN UNITS IS INVISIBLE TO EVERYTHING.
# --diff compares the units it already knows about, so a unit added here but never listed
# is never installed, never enabled, and never reported as missing — it simply does not
# exist as far as this script is concerned. spira-cockpit.service sat in exactly that
# state: committed, and enabled by hand on the one box that had it, so it looked healthy
# while the next install.sh would have installed every unit except that one and re-enabled
# the Gas Town collector it replaced. The check is here rather than in a comment because a
# list that must be remembered is the thing that failed.
unlisted() {
    local f b
    for f in "$SRC"/*.service "$SRC"/*.timer; do
        [ -e "$f" ] || continue
        b="$(basename "$f")"
        case " ${UNITS[*]} " in *" $b "*) continue ;; esac
        case " ${OPTIONAL[*]} " in *" $b "*) continue ;; esac
        echo "$b"
    done
}

if [ "$_install_mode" = "--diff" ]; then
    rc=0
    TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
    for u in $(unlisted); do
        echo "UNLISTED $u (in this directory but absent from UNITS — it will never be installed)"
        rc=1
    done
    for u in "${UNITS[@]}"; do
        [ "$u" = "spira-watch@.service" ] && continue
        inst="$(inst_name "$u")"
        render "$SRC/$u" > "$TMP/$inst" || { rc=1; continue; }
        if [ ! -f "$DEST/$inst" ]; then echo "MISSING  $inst (not installed)"; rc=1; continue; fi
        if ! diff -q "$TMP/$inst" "$DEST/$inst" >/dev/null; then
            echo "DIFFERS  $inst"; diff -u "$TMP/$inst" "$DEST/$inst" | sed 's/^/    /'; rc=1
        fi
    done
    for _wname in "${_watch_names[@]}"; do
        inst="$(inst_watch_name "$_wname")"
        render "$SRC/spira-watch@.service" "$_wname" > "$TMP/$inst" || { rc=1; continue; }
        if [ ! -f "$DEST/$inst" ]; then echo "MISSING  $inst (not installed)"; rc=1; continue; fi
        if ! diff -q "$TMP/$inst" "$DEST/$inst" >/dev/null; then
            echo "DIFFERS  $inst"; diff -u "$TMP/$inst" "$DEST/$inst" | sed 's/^/    /'; rc=1
        fi
    done
    [ "$rc" = 0 ] && echo "installed units match what this box renders"
    exit "$rc"
fi

# REFUSE IF THE CHECKOUT IS NOT ON ITS LANDREF OR IS BEHIND IT. Running install.sh from an
# aeon's feature branch silently ships units whose ExecStart paths are inside that branch's
# worktree, not the operator checkout; running it behind the landref means the templates it
# renders predate work that has already landed. skew.sh sees and reports BEHIND, but its own
# escalation used to recommend "Re-run install.sh" — so a behind checkout was recommending
# itself as the remedy. This fence is Rung 4 closing that loop. (sp-mlcd)
#
# THREE OF SEVEN REPOSITORIES HERE USE master, NOT main. The repo map carries a declared
# base column for exactly this; the git fallback (origin/HEAD) handles everything else.
# lib.sh carries spira_landref but also runs spira_containment_check at source time, which
# rejects synthetic paths used in tests. The landref is resolved here inline — the same
# logic spira_landref uses, without that side effect.
# SPIRA_INSTALL_FORCE is the named override — the same variable the live-aeons fence below
# uses; one override covers both pre-install refusals.
_install_landref() {    # -> the landref for SPIRA_REPO, or non-zero if it cannot be found
    local name base ref remotes remote
    # 1 — Declared in the repo map. The `base` column exists only in the six-field row shape
    #     (added after the five-field shape that predates it). A missing map or a missing
    #     entry falls through to the git path below.
    name="$(basename "${SPIRA_REPO:-}")"
    if [ -f "${SPIRA_REPO_MAP:-}" ] && [ -n "$name" ]; then
        base="$(awk -v want="$name" 'BEGIN{FS="|"} /^[ \t]*#/{next}
            { n=$1; gsub(/^[ \t]+|[ \t]+$/,"",n)
              if (n != want || NF < 6) next
              v=$4; gsub(/^[ \t]+|[ \t]+$/,"",v); if (v != "") {print v; exit} }
            ' "$SPIRA_REPO_MAP" 2>/dev/null)"
        if [ -n "$base" ] && git -C "$SPIRA_REPO" rev-parse --verify -q "$base" >/dev/null 2>&1; then
            printf '%s' "$base"; return 0
        fi
    fi
    # 2 — The remote's cached default (set once by 'git remote set-head origin -a').
    ref="$(git -C "$SPIRA_REPO" symbolic-ref -q --short refs/remotes/origin/HEAD 2>/dev/null)"
    if [ -n "$ref" ] && git -C "$SPIRA_REPO" rev-parse --verify -q "$ref" >/dev/null 2>&1; then
        printf '%s' "$ref"; return 0
    fi
    # 3 — Ask the remote; caches the result so the check runs once per repo.
    remotes="$(git -C "$SPIRA_REPO" remote 2>/dev/null)"
    if grep -qx "origin" <<< "$remotes" 2>/dev/null; then remote="origin"
    elif [ "$(printf '%s\n' "$remotes" | wc -l)" = "1" ] && [ -n "$remotes" ]; then remote="$remotes"
    else remote=""; fi
    if [ -n "$remote" ]; then
        git -C "$SPIRA_REPO" remote set-head "$remote" -a >/dev/null 2>&1 || true
        ref="$(git -C "$SPIRA_REPO" symbolic-ref -q --short "refs/remotes/$remote/HEAD" 2>/dev/null)"
        if [ -n "$ref" ] && git -C "$SPIRA_REPO" rev-parse --verify -q "$ref" >/dev/null 2>&1; then
            printf '%s' "$ref"; return 0
        fi
    fi
    return 1
}

_check_landref_current() {
    local cur base behind base_short tag
    # Not a git checkout — landref is not applicable (artifact deploy); skip rather than fail.
    git -C "${SPIRA_REPO:-}" rev-parse --git-dir >/dev/null 2>&1 || return 0
    cur="$(git -C "$SPIRA_REPO" rev-parse --abbrev-ref HEAD 2>/dev/null)" || cur=""
    # Detached HEAD at an exact tag — release install; no base to be behind.
    if [ "$cur" = "HEAD" ]; then
        tag="$(git -C "$SPIRA_REPO" describe --exact-match --tags HEAD 2>/dev/null)" || tag=""
        if [ -n "$tag" ]; then
            printf 'install: release install from tag %s — landref currency not applicable\n' "$tag"
            return 0
        fi
    fi
    base="$(_install_landref)" || {
        printf 'install: cannot resolve landref for %s\n' "$SPIRA_REPO" >&2
        printf 'install: update origin/HEAD (git remote set-head origin -a) or set SPIRA_INSTALL_FORCE=1 to skip.\n' >&2
        return 1
    }
    # Strip the remote prefix (origin/main → main) for the branch-name comparison.
    base_short="${base##*/}"
    if [ "$cur" != "$base_short" ]; then
        printf 'install: refusing — checkout is on branch %s, not the landref (%s)\n' \
            "$cur" "$base_short" >&2
        printf 'install: switch to %s first, or set SPIRA_INSTALL_FORCE=1 to override.\n' \
            "$base_short" >&2
        return 1
    fi
    behind="$(git -C "$SPIRA_REPO" rev-list --count "HEAD..$base" 2>/dev/null || echo 0)"
    if [ "${behind:-0}" -gt 0 ]; then
        printf 'install: refusing — checkout is %d commit(s) behind %s\n' "$behind" "$base" >&2
        printf 'install:   git -C %s pull --rebase\n' "$SPIRA_REPO" >&2
        printf 'install: or set SPIRA_INSTALL_FORCE=1 to override.\n' >&2
        return 1
    fi
}

# REFUSE IF ANOTHER INSTALLED INSTANCE SHARES A CRITICAL PATH. A shared SPIRA_RUN
# lets a test reaper delete production worktrees; a shared SPIRA_DB makes test aeons
# file real beads. Checked before any directory is created or unit written, so a
# refusal leaves the box as it was.
#
# OTHER INSTANCES ARE FOUND BY SCANNING *.conf FILES ALONGSIDE THE CURRENT CONFIG.
# Only EXPLICIT settings in each file are compared against this instance's resolved
# values — defaults are instance-qualified by construction and cannot collide without
# an operator's help. A missing or non-existent config file skips the check.
_check_path_collisions() {
    local conf_dir other_conf key val other_instance collision=0
    [ -n "${SPIRA_CONF_FILE:-}" ] || return 0
    conf_dir="$(dirname "$SPIRA_CONF_FILE")"
    [ -d "$conf_dir" ] || return 0

    local -A _other
    for other_conf in "$conf_dir"/*.conf; do
        [ -f "$other_conf" ] || continue
        [ "$other_conf" = "$SPIRA_CONF_FILE" ] && continue
        _other=()
        other_instance="prod"
        while IFS= read -r line || [ -n "$line" ]; do
            line="${line#"${line%%[![:space:]]*}"}"          # ltrim
            case "$line" in ''|'#'*) continue ;; esac
            case "$line" in *=*) ;; *) continue ;; esac
            key="${line%%=*}"; val="${line#*=}"
            key="${key%"${key##*[![:space:]]}"}"             # rtrim key
            val="${val#"${val%%[![:space:]]*}"}"             # ltrim val
            val="${val%"${val##*[![:space:]]}"}"             # rtrim val
            case "$val" in
                \"*\") val="${val#\"}"; val="${val%\"}" ;;
                \'*\') val="${val#\'}"; val="${val%\'}" ;;
            esac
            case "$val" in '~'|'~/'*) val="$HOME${val#\~}" ;; esac
            val="${val//\$HOME/$HOME}"; val="${val//\$\{HOME\}/$HOME}"
            case "$key" in
                SPIRA_INSTANCE) other_instance="$val" ;;
                SPIRA_RUN|SPIRA_DB|SPIRA_PROD|SPIRA_DOLT_DATA|SPIRA_TESTDB_PORT)
                    _other[$key]="$val" ;;
            esac
        done < "$other_conf"
        [ "$other_instance" = "$SPIRA_INSTANCE" ] && continue
        for key in SPIRA_RUN SPIRA_DB SPIRA_PROD SPIRA_DOLT_DATA SPIRA_TESTDB_PORT; do
            val="${_other[$key]:-}"
            [ -n "$val" ] || continue
            local this_val="${!key}"
            [ -n "$this_val" ] || continue
            if [ "$val" = "$this_val" ]; then
                printf 'install: refusing — %s collides with instance %s (%s)\n' \
                    "$key" "$other_instance" "$(basename "$other_conf")" >&2
                printf 'install:   both set to: %s\n' "$this_val" >&2
                collision=1
            fi
        done
    done
    return "$collision"
}
_check_path_collisions || exit 1

if [ -z "${SPIRA_INSTALL_FORCE:-}" ]; then
    _check_landref_current || exit 1
fi

mkdir -p "$DEST"

# THE RUNTIME DIRECTORY, BEFORE ANY UNIT STARTS. `.runtime/` is gitignored — it holds logs,
# leases, worktrees and the cockpit snapshot, none of which is content — so a fresh clone does
# not have one. Most of the harness gets it from lib.sh, but the units start things that source
# only conf.sh, and those fail on a path that does not exist yet. Install is the one act that
# turns a clone into an installation, so it is where the directory is made.
mkdir -p "$SPIRA_RUN"
# AND THE WATCHERS' DIRECTORY. `spira-watch-<name>-<instance>.service` appends its stdout to
# a file in here, and systemd opens that file BEFORE ExecStart — so a missing directory is not
# a watcher that starts and complains, it is a unit that fails instantly with a message about
# a path.
mkdir -p "$SPIRA_RUN/watchd"

# PLACE THE DOLT SERVER CONFIGS if the data directories exist and the files do not. The
# dolt-beads* units reference a config file inside the data directory; a service whose config
# is absent fails at startup with a plain "No such file" that is opaque without context.
# Never overwrite: the operator may have tuned the config after the first install.
# If the live file differs from what the template would render, print a note but keep theirs.
_place_dolt_yaml() {  # args: <template-name> <data-dir>
    local tmpl="$SRC/$1" dir="$2" dest text
    [ -n "$dir" ] || return 0
    mkdir -p "$dir"
    dest="$dir/dolt-server.yaml"
    text="$(render "$tmpl")" || return 1
    if [ -f "$dest" ]; then
        if ! cmp -s <(printf '%s\n' "$text") "$dest"; then
            printf 'install: note: %s differs from template — keeping existing\n' "$dest"
        fi
        return 0
    fi
    printf '%s\n' "$text" > "$dest.new" && mv "$dest.new" "$dest" \
        && printf 'install: placed %s\n' "$dest"
}
[ -n "${SPIRA_DOLT_DATA:-}" ]   && _place_dolt_yaml dolt-server.yaml      "$SPIRA_DOLT_DATA"
[ -n "${SPIRA_TESTDB_DATA:-}" ] && _place_dolt_yaml dolt-server-test.yaml "$SPIRA_TESTDB_DATA"

# SEED THE TEST PROD CHECKOUT'S CONFIG. A non-prod sentinel runs from $SPIRA_PROD/sentinel.sh.
# That conf.sh resolves SPIRA_REPO as the git root of $SPIRA_PROD, then looks for
# $SPIRA_REPO/spira.conf BEFORE ~/.config/spira/spira.conf. Without that file the sentinel
# falls through to the prod config — using the prod database, prod runtime tree, and
# SPIRA_INSTANCE=prod — so the containment fence never fires (it is a no-op for prod).
# Writing SPIRA_INSTANCE=<instance> to dirname($SPIRA_PROD)/spira.conf closes the gap: every
# path the sentinel derives from it (SPIRA_DB, SPIRA_RUN, etc.) inherits the instance
# qualifier automatically, and the containment check fires as intended.
if [ "$SPIRA_INSTANCE" != "prod" ] && [ -n "${SPIRA_PROD:-}" ]; then
    _prod_root="$(dirname "$SPIRA_PROD")"
    _home_root="$(dirname "$SPIRA_HOME")"
    # GUARD: skip when the prod checkout shares the same parent as the dev checkout.
    # That only happens when SPIRA_PROD was not set to a separate checkout — a test
    # fixture or a no-split install. In real usage the prod tree lives in a sibling
    # directory (e.g. spira-harness-test/) and the two parents differ.
    if [ "$_prod_root" != "$_home_root" ]; then
        _seed_instance_conf "$_prod_root/spira.conf" "$SPIRA_INSTANCE"
    fi
    unset _prod_root _home_root
fi

# REFUSE IF THIS INSTANCE'S AEONS ARE LIVE. Scoped to the named instance so that installing
# 'test' does not refuse because 'prod' aeons are running — isolation between instances is the
# whole point of per-instance naming. The check is shared with promote.sh via spira_live_aeons()
# in lib.sh so the two cannot drift.
if [ -z "${SPIRA_INSTALL_FORCE:-}" ]; then
    _install_live_aeons="$(spira_live_aeons)"
    if [ -n "$_install_live_aeons" ]; then
        printf 'install: refusing — live aeons for instance %s would be disrupted:\n' "$SPIRA_INSTANCE" >&2
        printf '%s\n' "$_install_live_aeons" | sed 's/^/    /' >&2
        printf 'install: wait for them to finish, or set SPIRA_INSTALL_FORCE=1 to override.\n' >&2
        exit 1
    fi
    unset _install_live_aeons
fi

declare -A _CHANGED=()  # units whose rendered content differs from what is installed
declare -A _MASKED=()   # units masked by the operator — skipped, never overwritten
declare -A _NEW=()      # units that did not exist before this run — safe to enable fresh
_n_unchanged=0

# CONTROL PLANE, READ ONCE. ctrl.sh check sources conf.sh and spawns python3 per call; asking
# it once per unit (4 call sites below, over dozens of units) made that dozens of process
# starts per install. CTRL_LIB=1 sources ctrl.sh for its functions only (no CLI dispatch);
# ctrl_load_suspended does the one python3 read and ctrl_is_suspended decides per unit in pure
# bash — the same predicate world.sh start now uses (cluster 6, docs/test-plan/instance-lifecycle.md).
# A fallback definition covers the pre-existing "ctrl.sh absent or fails -> enabled normally"
# contract when the file is not executable.
ctrl_is_suspended() { return 1; }
declare -A _CTRL_SUSPENDED=()
if [ -x "$SPIRA_HOME/ctrl.sh" ]; then
    CTRL_LIB=1 . "$SPIRA_HOME/ctrl.sh"
    ctrl_load_suspended _CTRL_SUSPENDED
fi
for u in "${UNITS[@]}"; do
    [ "$u" = "spira-watch@.service" ] && continue
    inst="$(inst_name "$u")"
    unit_text="$(render "$SRC/$u")" || { echo "install: $u FAILED" >&2; exit 1; }
    # A MASK IS A SYMLINK TO /dev/null — an explicit operator decision that this unit
    # must not run. [ -f ] follows symlinks: a character device fails -f, so a masked
    # unit looks MISSING to the comparison below and would be classified as a change
    # and written over, silently undoing the operator's decision. Detect it first.
    if [ -L "$DEST/$inst" ] && [ "$(readlink "$DEST/$inst" 2>/dev/null)" = "/dev/null" ]; then
        printf 'install: %s is masked — skipping\n' "$inst" >&2
        _MASKED[$inst]=1
        continue
    fi
    _cs_check="${inst%"-${SPIRA_INSTANCE}.service"}"; _cs_check="${_cs_check%"-${SPIRA_INSTANCE}.timer"}"
    _cs_check="${_cs_check%.service}"; _cs_check="${_cs_check%.timer}"
    _unit_suspended=0
    ctrl_is_suspended _CTRL_SUSPENDED "$_cs_check" >/dev/null && _unit_suspended=1
    # REFUSE AN UNEXECUTABLE ExecStart TARGET before writing a single byte. The failure mode
    # this prevents is 203/EXEC: systemd accepts the unit, a timer reports 'active', and the
    # service never runs. Every path is in hand at render time; an unresolved @KEY@ raises an
    # error above, so what reaches this check is a fully-substituted path.
    # System binaries (/usr/*, /bin/*, /sbin/*) are the OS's responsibility, not ours.
    # Suspended units are exempt: their dependency may be absent in the current environment.
    if [ "$_unit_suspended" = 0 ]; then
    while IFS= read -r line; do
        case "$line" in
            ExecStart=*|ExecStartPre=*)
                exec_path="${line#*=}"; exec_path="${exec_path%% *}"
                case "$exec_path" in ''|-*|/usr/*|/bin/*|/sbin/*) continue ;; esac
                if [ ! -x "$exec_path" ]; then
                    printf 'install: %s: ExecStart target is not executable: %s\n' \
                        "$inst" "$exec_path" >&2
                    exit 1
                fi
                ;;
        esac
    done <<< "$unit_text"
    fi
    # WRITE ONLY WHEN THE CONTENT DIFFERS. An unconditional write triggers daemon-reload
    # and restarts units on every install, even when nothing changed — including reloading
    # unit state for running aeons that were not touched.
    if [ ! -f "$DEST/$inst" ]; then
        _NEW[$inst]=1; _CHANGED[$inst]=1
        printf '%s\n' "$unit_text" > "$DEST/$inst.new"
        mv "$DEST/$inst.new" "$DEST/$inst" && chmod 0644 "$DEST/$inst" && echo "installed $inst"
    elif ! cmp -s <(printf '%s\n' "$unit_text") "$DEST/$inst"; then
        _CHANGED[$inst]=1
        printf '%s\n' "$unit_text" > "$DEST/$inst.new"
        mv "$DEST/$inst.new" "$DEST/$inst" && chmod 0644 "$DEST/$inst" && echo "installed $inst"
    else
        echo "unchanged $inst"
        _n_unchanged=$(( _n_unchanged + 1 ))
    fi
done

# WATCHER UNITS — one plain file per manifest row per instance. The template
# (spira-watch@.service) is rendered with %i substituted for the watcher name, so the
# installed file has no @-template syntax and systemd never instantiates it.
for _wname in "${_watch_names[@]}"; do
    inst="$(inst_watch_name "$_wname")"
    unit_text="$(render "$SRC/spira-watch@.service" "$_wname")" \
        || { echo "install: watcher $_wname FAILED" >&2; exit 1; }
    if [ -L "$DEST/$inst" ] && [ "$(readlink "$DEST/$inst" 2>/dev/null)" = "/dev/null" ]; then
        printf 'install: %s is masked — skipping\n' "$inst" >&2
        _MASKED[$inst]=1
        continue
    fi
    while IFS= read -r line; do
        case "$line" in
            ExecStart=*)
                exec_path="${line#ExecStart=}"; exec_path="${exec_path%% *}"
                case "$exec_path" in ''|-*|/usr/*|/bin/*|/sbin/*) continue ;; esac
                if [ ! -x "$exec_path" ]; then
                    printf 'install: %s: ExecStart target is not executable: %s\n' \
                        "$inst" "$exec_path" >&2
                    exit 1
                fi
                ;;
        esac
    done <<< "$unit_text"
    if [ ! -f "$DEST/$inst" ]; then
        _NEW[$inst]=1; _CHANGED[$inst]=1
        printf '%s\n' "$unit_text" > "$DEST/$inst.new"
        mv "$DEST/$inst.new" "$DEST/$inst" && chmod 0644 "$DEST/$inst" && echo "installed $inst"
    elif ! cmp -s <(printf '%s\n' "$unit_text") "$DEST/$inst"; then
        _CHANGED[$inst]=1
        printf '%s\n' "$unit_text" > "$DEST/$inst.new"
        mv "$DEST/$inst.new" "$DEST/$inst" && chmod 0644 "$DEST/$inst" && echo "installed $inst"
    else
        echo "unchanged $inst"
        _n_unchanged=$(( _n_unchanged + 1 ))
    fi
done

# SUMMARY before any enable/restart. An idempotent installer that prints nothing is
# indistinguishable from one that silently skipped everything.
_n_changed="${#_CHANGED[@]}"
_n_masked="${#_MASKED[@]}"
printf 'install: %d unit file(s) changed, %d unchanged' "$_n_changed" "$_n_unchanged"
[ "$_n_masked" -gt 0 ] && printf ', %d masked (skipped)' "$_n_masked"
printf '\n'

# DAEMON-RELOAD ONLY WHEN UNIT FILES ACTUALLY CHANGED. An unconditional reload invalidates
# systemd's state for every unit on the machine — including running aeons — on every install,
# even when no file was written.
if [ "${#_CHANGED[@]}" -gt 0 ]; then
    systemctl --user daemon-reload
else
    printf 'install: no unit files changed — skipping daemon-reload\n'
fi

# Without lingering, user units stop when the last session closes — which is precisely the
# case these exist to survive.
# `id -un` rather than $USER alone: this file runs under `set -u`, and a minimal environment
# — a gate, a timer, a test harness — carries no USER, so the install died on an unbound
# variable after having written every unit and before enabling any of them.
loginctl enable-linger "${USER:-$(id -un)}" 2>/dev/null || true

# MIGRATE LEGACY UN-SUFFIXED UNITS. Before per-instance naming, spira-* units were
# installed without an instance suffix (e.g., spira-sentinel.service). If any of those
# old names survive, disable and stop them NOW — before the per-instance units are
# enabled — so the two sets never run simultaneously against the same database and
# worktrees. Enumerate from the template list rather than from a glob of what happens
# to be installed, so a unit added to UNITS is automatically covered without a second
# edit here. On a fresh box where no legacy units exist, every disable call returns
# non-zero and is silently ignored; no spurious output is produced.
_migrate_legacy() {
    local u old _wn disabled=0
    for u in "${UNITS[@]}"; do
        # The watcher template is never installed directly; its per-watcher instances
        # are handled by the loop below.
        [ "$u" = "spira-watch@.service" ] && continue
        case "$u" in
            spira-*.service|spira-*.timer) ;;
            *) continue ;;
        esac
        old="$u"
        # inst_name returns the same string for shared units (cockpit-ensure, concierge,
        # beads-push): those plain names ARE their installed names and must not be treated
        # as legacy names here.
        [ "$(inst_name "$u")" = "$u" ] && continue
        # DISABLE AND DELETE. `disable --now` stops and unlinks the unit; `rm` removes the
        # file from the installed unit directory so the plain name does not survive as a
        # stale copy alongside its instance-named successor. A disable that fails because the
        # unit was never installed is silent; a file delete is safe to run even on a missing
        # file. Both must happen: disable without rm leaves the file visible in
        # `list-unit-files`, making two units appear to claim the same service.
        systemctl --user disable --now "$old" 2>/dev/null && \
            { rm -f "$DEST/$old"
              printf 'migrated  %s (disabled and removed; superseded by %s)\n' \
                     "$old" "$(inst_name "$old")"; disabled=$((disabled+1)); }
    done
    # --no-migrate-watchers spares the per-watcher unit pairs only. Non-watcher units
    # (sentinel, ops, etc.) are always migrated above because the unit name IS their only
    # concurrency control: two units with the same ExecStart running simultaneously breaks
    # the design. Watcher units are spared because a running watcher holds stateful
    # positions in the output stream; tearing one down mid-operation can lose lines.
    if [ "$_skip_migrate_watchers" = 1 ]; then
        printf 'install: --no-migrate-watchers: sparing watcher legacy units (spira-watch-* and spira-watch@*)\n'
    else
        # Old per-watcher units also lacked the instance suffix.
        for _wn in "${_watch_names[@]}"; do
            old="spira-watch-${_wn}.service"
            systemctl --user disable --now "$old" 2>/dev/null && \
                { rm -f "$DEST/$old"
                  printf 'migrated  %s (disabled and removed; superseded by %s)\n' \
                         "$old" "$(inst_watch_name "$_wn")"; disabled=$((disabled+1)); }
            # Before per-instance naming, watchers were started via the spira-watch@.service
            # systemd template, so running units were named spira-watch@<name>.service (with @),
            # not spira-watch-<name>.service (with hyphen). The hyphen form was never what ran;
            # both forms must be retired to avoid a surviving instance that holds the work the
            # new per-instance unit tries to start — which produces a crash-looping new unit
            # racing a still-running old one.
            old_at="spira-watch@${_wn}.service"
            systemctl --user disable --now "$old_at" 2>/dev/null && \
                { rm -f "$DEST/$old_at"
                  printf 'migrated  %s (disabled and removed; superseded by %s)\n' \
                         "$old_at" "$(inst_watch_name "$_wn")"; disabled=$((disabled+1)); }
        done
    fi
    [ "$disabled" -gt 0 ] && \
        printf 'install: migrated %d legacy unit(s) to per-instance naming\n' "$disabled"
}
_migrate_legacy

# MIGRATE COCKPIT STATE FROM THE OLD .runtime LOCATION TO SPIRA_RUN.
# Before sp-ie1n the cockpit scripts wrote answered-seen.json and self-closed inside the
# harness tree under cockpit/.runtime. That blocks making the harness read-only. The files
# move to SPIRA_RUN; this migration carries them on upgrade so the operator is not re-shown
# answers already acknowledged and the self-closed filter does not forget what it closed.
# Only moves when the old file exists and the new location is empty — never overwrites.
_migrate_cockpit_state() {
    local old_dir new_dir f old new moved=0
    old_dir="${SPIRA_COCKPIT}/.runtime"
    new_dir="${SPIRA_RUN}"
    [ -d "$old_dir" ] || return 0
    mkdir -p "$new_dir"
    for f in answered-seen.json self-closed; do
        old="$old_dir/$f"
        new="$new_dir/$f"
        [ -f "$old" ] || continue
        if [ -f "$new" ]; then
            printf 'install: cockpit state %s already at new location — skipping\n' "$f"
            continue
        fi
        if cp "$old" "$new"; then
            printf 'install: migrated cockpit state %s → %s\n' "$old" "$new"
            moved=$((moved+1))
        else
            printf 'install: warning — could not migrate cockpit state %s\n' "$f" >&2
        fi
    done
    [ "$moved" -gt 0 ] && \
        printf 'install: migrated %d cockpit state file(s) to %s\n' "$moved" "$new_dir"
}
_migrate_cockpit_state

# Wait for a running oneshot service to finish before restarting it.
# A long-running service is not drained — we restart it directly.
# SPIRA_DRAIN_INTERVAL overrides the 2-second poll interval (set to 0 in tests).
_drain_oneshot() {
    local svc="$1" waited=0 max=300
    local state type interval="${SPIRA_DRAIN_INTERVAL:-2}"
    state="$(systemctl --user is-active "$svc" 2>/dev/null || true)"
    [ "$state" = "active" ] || return 0
    type="$(systemctl --user show -p Type --value "$svc" 2>/dev/null || true)"
    [ "$type" = "oneshot" ] || return 0
    printf 'install: %s is mid-pass — waiting for it to finish\n' "$svc"
    while [ "$waited" -lt "$max" ]; do
        state="$(systemctl --user is-active "$svc" 2>/dev/null || true)"
        case "$state" in active|activating) ;; *) return 0 ;; esac
        sleep "$interval"; waited=$(( waited + interval ))
    done
    printf 'install: warning — %s did not finish within %ss; proceeding\n' "$svc" "$max" >&2
}

# IF THE WORLD IS HALTED, install the units but leave them stopped. A routine install
# restarting the loop is the worst shape: the operator believes the world is down, every
# surface agrees, and it is running. An explicit world.sh start is how a halt is lifted.
_world_halted=0
if [ -f "$SPIRA_RUN/world.halted" ]; then
    _world_halted=1
    printf '\ninstall: world is HALTED (%s)\n' "$(head -1 "$SPIRA_RUN/world.halted")"
    printf 'install: reason: %s\n' "$(sed -n 2p "$SPIRA_RUN/world.halted")"
    printf 'install: units installed but NOT started — run world.sh start to lift the halt\n\n'
fi
# RESTART ONLY WHAT CHANGED. A no-op install touches nothing. An unchanged unit that is
# already active is skipped; a changed unit is drained (if it is a running oneshot) and
# then restarted. Transient spira-aeon-* units are never in UNITS or ENABLE, so they are
# structurally unreachable here — no explicit guard is needed.
#
# Template units (spira-watch@.service) cover all their instances: if the template
# changed, every instance derived from it is restarted.
#
# THE DECISION ITSELF IS _unit_action (defined above the SPIRA_INSTALL_LIB guard). What
# follows here is only gathering its six inputs and dispatching the systemctl calls its
# answer implies.
for u in "${ENABLE[@]}"; do
    _masked=0; [ "${_MASKED[$u]:-}" = "1" ] && _masked=1
    # CONTROL PLANE: a unit declared suspended in $SPIRA_CTRL is not enabled, halted world
    # or not. The subject is the base unit name without instance suffix or extension (e.g.
    # "spira-suites" from "spira-suites-prod.timer"). ctrl_is_suspended reads the single
    # load done above the UNITS loop. If ctrl.sh was absent, the fallback defined there
    # always says "not suspended" — a missing control tool is not a reason to refuse
    # enabling everything.
    _cs="${u%"-${SPIRA_INSTANCE}.service"}"; _cs="${_cs%"-${SPIRA_INSTANCE}.timer"}"
    _cs="${_cs%.service}"; _cs="${_cs%.timer}"
    _suspended=0
    ctrl_is_suspended _CTRL_SUSPENDED "$_cs" >/dev/null && _suspended=1
    # AN OPERATOR-DISABLED UNIT IS LEFT AT ITS CURRENT STATE. A freshly installed unit
    # (_NEW) also reports 'disabled' from is-enabled because it has never been enabled,
    # but that is not a decision the operator made, so it is enabled as normal.
    _en="$(systemctl --user is-enabled "$u" 2>/dev/null || true)"
    _disabled=0; [ "$_en" = "disabled" ] && [ -z "${_NEW[$u]:-}" ] && _disabled=1
    # A changed template (spira-watch@.service) counts as a change to every instance of it.
    tmpl="${u%%@*}@.service"
    _changed=0; [ -n "${_CHANGED[$u]:-}${_CHANGED[$tmpl]:-}" ] && _changed=1
    _active=0; [ "$(systemctl --user is-active "$u" 2>/dev/null || true)" = "active" ] && _active=1

    case "$(_unit_action "$_changed" "$_masked" "$_suspended" "$_disabled" "$_world_halted" "$_active")" in
        masked)
            printf 'install: %s is masked — skipping\n' "$u" ;;
        suspended)
            printf 'install: %s is suspended (ctrl: %s) — skipping\n' "$u" "$_cs" ;;
        operator-disabled)
            printf 'install: %s is disabled by operator — leaving unchanged\n' "$u" ;;
        enable)
            systemctl --user enable "$u" 2>/dev/null && printf 'enabled   %s (stopped — world is halted)\n' "$u" ;;
        skip)
            printf 'unchanged %s (active, skipping)\n' "$u" ;;
        restart)
            case "$u" in *.timer) _drain_oneshot "${u%.timer}.service" ;; *) _drain_oneshot "$u" ;; esac
            systemctl --user enable "$u" >/dev/null 2>&1 || true
            systemctl --user restart "$u" && echo "restarted $u" ;;
        enable-now)
            case "$u" in *.timer) _drain_oneshot "${u%.timer}.service" ;; *) _drain_oneshot "$u" ;; esac
            systemctl --user enable --now "$u" && echo "enabled   $u" ;;
    esac
done

# AND THE ONE PIECE OF WIRING THAT IS NOT A UNIT. The session hook is registered in the coding
# agent client's own settings file, outside every checkout, so installing the harness is the
# moment to put it there — a fresh clone that had to be told to run a second command would go
# without it. `cockpit-ensure` repairs the same registration on a timer, so this is the first
# write rather than the only one, and both are silent when there is nothing to change.
"$SPIRA_HOME/install-session-hook.sh" install || \
    echo "note: the session hook was not registered — run $SPIRA_HOME/install-session-hook.sh install" >&2

# LINK THE COCKPIT VIEW FOLLOWER. The server-side half of the cockpit is now shipped in the
# harness at cockpit/remote/cockpit-remote; link it to ~/.local/bin/cockpit-remote so that
# SPIRA_VIEW, rebuild.sh, and the design-review skill all find it at its canonical name.
# Silent when the target is already correct; updates the link when the harness moved.
_cr_src="$(dirname "$SPIRA_HOME")/cockpit/remote/cockpit-remote"
_cr_dst="$HOME/.local/bin/cockpit-remote"
if [ -x "$_cr_src" ]; then
    mkdir -p "$HOME/.local/bin"
    _cr_current="$(readlink "$_cr_dst" 2>/dev/null || true)"
    if [ "$_cr_current" != "$_cr_src" ]; then
        ln -sf "$_cr_src" "$_cr_dst"
        printf 'install: linked cockpit-remote: %s -> %s\n' "$_cr_dst" "$_cr_src"
    fi
fi
unset _cr_src _cr_dst _cr_current

# A ROW THAT HAS GONE MUST STOP RUNNING. Otherwise the manifest is the source of truth only
# for what starts, and a watcher deleted from it goes on polling — and goes on being believed
# — until somebody reads `systemctl` output they had no reason to read.
#
# Keyed on the per-instance watcher pattern: spira-watch-*-<instance>.service. This means
# a prune run for 'test' only disables 'test' watchers, not 'prod' watchers, which is the
# isolation guarantee the per-instance naming provides. The old template pattern
# (spira-watch@*.service) matches nothing under per-instance names and is gone.
#
# The instance name in the pattern is matched literally in the grep; no regex escaping is
# needed as long as the instance name is [A-Za-z0-9_-] — which conf.sh enforces via the
# SPIRA_INSTANCE default 'prod'.
#
# SPACE-JOINED ABOVE, and that is the whole reason: matched against `units`' raw
# newline-separated output, every instance failed to find its own row and a second
# install disabled every watcher it had just enabled.
for u in $({ systemctl --user list-unit-files --no-legend \
                 "spira-watch-*-${SPIRA_INSTANCE}.service" 2>/dev/null
             systemctl --user list-units --all --no-legend \
                 "spira-watch-*-${SPIRA_INSTANCE}.service" 2>/dev/null
             # DEST scan catches orphans that systemd has not yet indexed — before
             # daemon-reload, or when the systemd daemon's HOME differs from install.sh's
             # HOME (per-suite test isolation gives each suite its own HOME, but the daemon
             # was started with the real user home). DEST is where install.sh writes unit
             # files, so a watcher file here that is absent from the manifest is an orphan
             # regardless of what the daemon's search path currently includes. In production
             # DEST == the real systemd unit dir, so both sources agree and sort -u deduplicates.
             for _df in "$DEST"/spira-watch-*-"${SPIRA_INSTANCE}".service; do
                 [ -e "$_df" ] && basename "$_df"
             done
           } | tr -s ' \t' '\n\n' \
             | grep -E "^spira-watch-[A-Za-z0-9_-]+-${SPIRA_INSTANCE}\.service$" | sort -u); do
    case "$watch_units" in *" $u "*) continue ;; esac
    systemctl --user disable --now "$u" >/dev/null 2>&1 && echo "disabled  $u (no row in the manifest)"
done

# PRUNE NON-WATCHER SPIRA-* UNITS FOR THIS INSTANCE that are installed but absent from the
# manifest. Mirrors the watcher prune above. Without this, a unit added in a newer release
# and then rolled back stays installed against prior-release scripts and crash-loops.
_expected_units=" "
for _eu in "${UNITS[@]}"; do
    [ "$_eu" = "spira-watch@.service" ] && continue
    _expected_units="$_expected_units$(inst_name "$_eu") "
done
for u in $({ systemctl --user list-unit-files --no-legend \
                 "spira-*-${SPIRA_INSTANCE}.service" \
                 "spira-*-${SPIRA_INSTANCE}.timer" 2>/dev/null
             systemctl --user list-units --all --no-legend \
                 "spira-*-${SPIRA_INSTANCE}.service" \
                 "spira-*-${SPIRA_INSTANCE}.timer" 2>/dev/null
             for _df in "$DEST"/spira-*-"${SPIRA_INSTANCE}".service \
                        "$DEST"/spira-*-"${SPIRA_INSTANCE}".timer; do
                 [ -e "$_df" ] && basename "$_df"
             done
           } | tr -s ' \t' '\n\n' \
             | grep -E "^spira-[A-Za-z0-9_-]+-${SPIRA_INSTANCE}\.(service|timer)$" | sort -u); do
    case "$u" in spira-watch-*) continue ;; esac   # handled by the watcher prune above
    case "$u" in spira-aeon-*) continue ;; esac    # transient; not in UNITS
    case "$_expected_units" in *" $u "*) continue ;; esac
    systemctl --user disable --now "$u" >/dev/null 2>&1 || true
    rm -f "$DEST/$u"
    echo "pruned    $u (no longer in the manifest)"
done
unset _eu _expected_units

systemctl --user list-timers --all 2>/dev/null | grep -E 'cockpit|concierge|beads-push|spira' || true
# Long-running services never appear above. Everything else on this list is worthless if
# they are down.
for u in "spira-cockpit-${SPIRA_INSTANCE}.service" \
         "spira-loom-${SPIRA_INSTANCE}.service" \
         ${SPIRA_DOLT_DATA:+dolt-beads.service}; do
    printf '%-36s %s\n' "$u" "$(systemctl --user is-active "$u")"
done

# REPORT THE END STATE. A silent partial install is the defect: a unit enabled but not
# running is indistinguishable from one that was never started, and daemon-reload is the
# specific mechanism that produces this state for any unit whose file has gone (aeons aside,
# a template change can cause a reload to leave a previously-active unit in failed state).
# Skip this check when the world is halted — units are intentionally not running.
if [ ! -f "$SPIRA_RUN/world.halted" ]; then
    not_active=""
    for u in "${ENABLE[@]}"; do
        # Masked, operator-disabled, and control-plane-suspended units were intentionally
        # left alone; do not fault them as "enabled but not active".
        [ "${_MASKED[$u]:-}" = "1" ] && continue
        _cs="${u%"-${SPIRA_INSTANCE}.service"}"; _cs="${_cs%"-${SPIRA_INSTANCE}.timer"}"
        _cs="${_cs%.service}"; _cs="${_cs%.timer}"
        if ctrl_is_suspended _CTRL_SUSPENDED "$_cs" >/dev/null; then
            continue
        fi
        _en="$(systemctl --user is-enabled "$u" 2>/dev/null || true)"
        [ "$_en" = "disabled" ] && [ -z "${_NEW[$u]:-}" ] && continue
        state="$(systemctl --user is-active "$u" 2>/dev/null || true)"
        [ "$state" = "active" ] || not_active="${not_active}    $u ($state)"$'\n'
    done
    if [ -n "$not_active" ]; then
        printf '\ninstall: ERROR — these units are enabled but not active:\n' >&2
        printf '%s' "$not_active" >&2
        printf 'install: check journalctl --user -xe for details.\n' >&2
        exit 1
    fi
fi
