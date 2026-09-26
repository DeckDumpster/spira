#!/usr/bin/env bash
#
# overrides.sh — operator overrides held against a production checkout, as a harness concept.
#
#   overrides.sh apply [repo]     (re)apply every declared override that still needs it,
#                                 retiring any whose bead has landed
#   overrides.sh list             one line per override: name, bead, state
#   overrides.sh doctor [repo]    problems doctor.sh should surface (failed / stale-closed)
#
# WHAT THIS REPLACES. A hand edit to the production checkout — a brief paragraph, a config
# cap — used to be held by a user systemd timer running specs outside the harness, invisible
# to doctor and the cockpit, and reverted for up to a minute by every `skew.sh refresh` before
# the timer caught up (sp-qdh0x). `refresh` now calls this script immediately after it resets
# the checkout, so there is no window where the reset brief is live.
#
# THE SPEC FORMAT, unchanged from the specs it replaces, so an operator's existing
# `*.override` files move over as-is. A spec is a shell file defining:
#
#   BEAD="sp-xxxx"        the bead that makes this edit permanent
#   needed() { ... }      $1 = repo path; exit 0 if the override is not currently in effect
#   apply()  { ... }      $1 = repo path; make the edit
#   on_landed() { ... }   optional, $1 = repo path; runs once when BEAD lands
#
# RETIREMENT. Once HEAD carries a commit titled `spira: land <BEAD>`, the override is done:
# on_landed runs (if declared), then the spec moves to retired/ so it is never applied again
# but stays on disk for history. A spec whose on_landed() fails is left in place and reported
# — the failure that motivated this file was exactly a failure nobody saw
# (sp-qdh0x notes: the timer's own PATH lacked `bd`, so a restore silently did nothing).
#
# THE PATH A HOOK CAN TRUST. A systemd timer's minimal PATH is the reason the earlier version
# of this broke: `bd` was not on it, because the timer ran apply.sh without ever sourcing
# conf.sh. Sourcing lib.sh here — which sources conf.sh, the one place PATH is assembled and
# SPIRA_BD resolved deterministically, and defines `bdq` for the doctor check below — is what
# fixes that: every spec runs with the same PATH and the same bd every other harness script
# uses.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/lib.sh"

_ov_state_dir() { printf '%s/.state' "$SPIRA_OVERRIDES"; }

# _ov_landed <repo> <bead> — has HEAD's history got a commit titled "spira: land <bead>"?
_ov_landed() {
    local repo="$1" bead="$2"
    git -C "$repo" log --format='%s' -F --grep="spira: land $bead" -n 1 2>/dev/null \
        | grep -qFx "spira: land $bead"
}

# _ov_load <spec> — source one spec with a clean slate. Resets BEAD and the three functions
# first so a spec that declares none of them cannot inherit the previous spec's.
_ov_load() {
    local spec="$1"
    unset -f needed apply on_landed 2>/dev/null
    BEAD=""
    # shellcheck disable=SC1090
    . "$spec"
}

apply_all() {
    local repo="${1:-$SPIRA_REPO}" dir="$SPIRA_OVERRIDES" state_dir spec name failed=0 found=0
    [ -d "$dir" ] || { echo "overrides: none declared (no directory at $dir)"; return 0; }
    state_dir="$(_ov_state_dir)"; mkdir -p "$state_dir" 2>/dev/null || true

    for spec in "$dir"/*.override; do
        [ -e "$spec" ] || continue
        found=1
        name="$(basename "$spec" .override)"
        if ! _ov_load "$spec"; then
            echo "overrides: $name: failed to load spec" >&2
            printf 'load-error\n' > "$state_dir/$name.failed"
            failed=$((failed+1))
            continue
        fi
        if [ -z "$BEAD" ]; then
            echo "overrides: $name: no BEAD declared — skipping" >&2
            continue
        fi
        if ! declare -f needed >/dev/null || ! declare -f apply >/dev/null; then
            echo "overrides: $name: missing needed() or apply() — skipping" >&2
            continue
        fi

        if _ov_landed "$repo" "$BEAD"; then
            local _ov_landed_rc=0
            if declare -f on_landed >/dev/null; then
                on_landed "$repo"; _ov_landed_rc=$?
            fi
            if [ "$_ov_landed_rc" != 0 ]; then
                echo "overrides: $name: on_landed failed for $BEAD (rc=$_ov_landed_rc) — leaving spec active" >&2
                printf 'on_landed-failed\n' > "$state_dir/$name.failed"
                failed=$((failed+1))
                continue
            fi
            mkdir -p "$dir/retired" 2>/dev/null || true
            mv -f "$spec" "$dir/retired/$name.override"
            rm -f "$state_dir/$name.failed"
            echo "overrides: $name: retired — $BEAD landed"
            continue
        fi

        if needed "$repo"; then
            if apply "$repo"; then
                rm -f "$state_dir/$name.failed"
                echo "overrides: $name: applied (awaiting $BEAD)"
            else
                echo "overrides: $name: apply failed for $BEAD" >&2
                printf 'apply-failed\n' > "$state_dir/$name.failed"
                failed=$((failed+1))
            fi
        fi
    done
    [ "$found" = 1 ] || echo "overrides: none declared in $dir"
    [ "$failed" -eq 0 ]
}

# list — name, bead, state (active|failed|retired). Read by the cockpit ops pane and doctor.
list_all() {
    local dir="$SPIRA_OVERRIDES" state_dir spec name state found=0
    [ -d "$dir" ] || { echo "overrides: none declared (no directory at $dir)"; return 0; }
    state_dir="$(_ov_state_dir)"

    for spec in "$dir"/*.override; do
        [ -e "$spec" ] || continue
        found=1
        name="$(basename "$spec" .override)"
        _ov_load "$spec" 2>/dev/null
        state=active
        [ -f "$state_dir/$name.failed" ] && state="failed:$(cat "$state_dir/$name.failed" 2>/dev/null)"
        printf '%s %s %s\n' "$name" "${BEAD:-?}" "$state"
    done
    for spec in "$dir"/retired/*.override; do
        [ -e "$spec" ] || continue
        found=1
        name="$(basename "$spec" .override)"
        _ov_load "$spec" 2>/dev/null
        printf '%s %s retired\n' "$name" "${BEAD:-?}"
    done
    [ "$found" = 1 ] || echo "overrides: none declared in $dir"
}

# doctor [repo] — problems doctor.sh should surface: an override that failed to apply or
# retire, or whose bead has been closed for over a day without landing (spec still active
# means refresh has not seen a `spira: land <BEAD>` commit yet — worth a human look, not a
# silent wait). One line per problem on stdout; exit 0 with none, 1 otherwise.
doctor_report() {
    local repo="${1:-$SPIRA_REPO}" dir="$SPIRA_OVERRIDES" state_dir spec name problems=0
    [ -d "$dir" ] || return 0
    state_dir="$(_ov_state_dir)"

    for spec in "$dir"/*.override; do
        [ -e "$spec" ] || continue
        name="$(basename "$spec" .override)"
        _ov_load "$spec" 2>/dev/null
        [ -n "$BEAD" ] || continue

        if [ -f "$state_dir/$name.failed" ]; then
            printf '%s: %s failed — %s\n' "$name" "$BEAD" "$(cat "$state_dir/$name.failed" 2>/dev/null)"
            problems=1
            continue
        fi

        if _ov_landed "$repo" "$BEAD"; then
            printf '%s: %s landed but the override is still active — apply has not run since\n' \
                "$name" "$BEAD"
            problems=1
            continue
        fi

        local _ov_closed_age
        _ov_closed_age="$(bdq show "$BEAD" --json 2>/dev/null | python3 -c '
import json, sys, datetime
try:
    d = json.load(sys.stdin)
    d = d[0] if isinstance(d, list) else d
    if d.get("status") != "closed":
        sys.exit(0)
    closed = d.get("closed_at") or ""
    c = datetime.datetime.fromisoformat(closed.replace("Z", "+00:00"))
    now = datetime.datetime.now(datetime.timezone.utc)
    print(int((now - c).total_seconds()))
except Exception:
    pass
' 2>/dev/null)"
        if [ -n "$_ov_closed_age" ] && [ "$_ov_closed_age" -gt 86400 ] 2>/dev/null; then
            printf '%s: %s closed %dh ago but not landed\n' "$name" "$BEAD" "$((_ov_closed_age/3600))"
            problems=1
        fi
    done
    [ "$problems" = 0 ]
}

case "${1:-list}" in
    apply)  shift; apply_all "$@" ;;
    list)   list_all ;;
    doctor) shift; doctor_report "$@" ;;
    *)      sed -n '3,8p' "$0" >&2; exit 2 ;;
esac
