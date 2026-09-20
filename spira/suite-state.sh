#!/usr/bin/env bash
# suite-state.sh — read and lint the per-repository suite lifecycle state file.
# Sourced, never executed directly.
#
# File format (one line per suite not in the default active state):
#   <suite> | <state> | <since UTC> | <bead> | <reason>
#
# States: active (default, not stored), quarantined, disabled.
# Fail-closed: an unparseable line or unknown state is treated as active.

# _sts_trim <str> — trim leading and trailing whitespace
_sts_trim() {
    local s="$1"
    s="${s#"${s%%[![:space:]]*}"}"
    s="${s%"${s##*[![:space:]]}"}"
    printf '%s' "$s"
}

# suite_state_file <repo> -> absolute path of the lifecycle file for this repo
suite_state_file() {
    local repo="${1:?suite_state_file: requires a repo path}"
    printf '%s/%s' "${repo%/}" "${SPIRA_SUITE_STATE_FILE:-spira/suite-state}"
}

# suite_state_parse <file> -> emit tab-separated "suite state since bead reason" per non-default entry.
# Silently skips unparseable lines and unknown states (fail-closed: treated as active = blocking).
suite_state_parse() {
    local file="$1" line suite state since bead reason rest
    [ -r "$file" ] || return 0
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%%#*}"
        line="$(_sts_trim "$line")"
        [ -n "$line" ] || continue
        # Must have at least 4 pipes (5 fields)
        case "$line" in *'|'*'|'*'|'*'|'*) ;; *) continue ;; esac
        suite="$(_sts_trim "${line%%|*}")"
        rest="${line#*|}"
        state="$(_sts_trim "${rest%%|*}")"
        rest="${rest#*|}"
        since="$(_sts_trim "${rest%%|*}")"
        rest="${rest#*|}"
        bead="$(_sts_trim "${rest%%|*}")"
        rest="${rest#*|}"
        reason="$(_sts_trim "$rest")"
        case "$state" in
            active|quarantined|disabled) ;;
            *) continue ;;  # unknown state: skip (treated as active)
        esac
        [ -n "$suite" ] || continue
        printf '%s\t%s\t%s\t%s\t%s\n' "$suite" "$state" "$since" "$bead" "$reason"
    done < "$file"
}

# suite_state_of <file> <suite> -> active|quarantined|disabled
suite_state_of() {
    local file="$1" suite="$2" s st
    while IFS=$'\t' read -r s st _since _bead _reason; do
        [ "$s" = "$suite" ] && { printf '%s' "$st"; return 0; }
    done < <(suite_state_parse "$file" 2>/dev/null)
    printf 'active'
}

# suite_state_write <file> <suite> <state> <bead> <reason>
# Removes any existing entry for the suite and appends the new entry (unless active).
suite_state_write() {
    local file="$1" suite="$2" state="$3" bead="$4" reason="$5"
    local since; since="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    local tmp; tmp="$(mktemp)" || return 1
    if [ -r "$file" ]; then
        local line stripped s
        while IFS= read -r line || [ -n "$line" ]; do
            stripped="${line%%#*}"
            stripped="$(_sts_trim "$stripped")"
            if [ -z "$stripped" ]; then
                printf '%s\n' "$line" >> "$tmp"
                continue
            fi
            s="$(_sts_trim "${stripped%%|*}")"
            [ "$s" = "$suite" ] && continue  # remove old entry
            printf '%s\n' "$line" >> "$tmp"
        done < "$file"
    fi
    # active means "remove entry" — nothing to append
    if [ "$state" != "active" ]; then
        printf '%s | %s | %s | %s | %s\n' "$suite" "$state" "$since" "$bead" "$reason" >> "$tmp"
    fi
    mv "$tmp" "$file"
}

# suite_state_clear <file> <suite> — remove this suite's entry (equivalent to activate)
suite_state_clear() { suite_state_write "$1" "$2" active "" ""; }

# suite_state_lint <file> <suite-dir>
# Fails on: missing suite, missing reason, quarantine with no bead.
# suite-dir is optional; when given, each named suite is checked to exist there.
suite_state_lint() {
    local file="$1" suite_dir="${2:-}" errors=0 lineno=0
    if [ ! -r "$file" ]; then
        printf 'suite-state: %s is not readable\n' "$file" >&2
        return 1
    fi
    local line stripped suite state bead reason rest
    while IFS= read -r line || [ -n "$line" ]; do
        lineno=$((lineno+1))
        stripped="${line%%#*}"
        stripped="$(_sts_trim "$stripped")"
        [ -n "$stripped" ] || continue
        case "$stripped" in *'|'*'|'*'|'*'|'*) ;; *)
            printf 'suite-state:%d: not parseable (expected suite|state|since|bead|reason): %s\n' \
                "$lineno" "$stripped" >&2
            errors=$((errors+1))
            continue ;;
        esac
        suite="$(_sts_trim "${stripped%%|*}")"
        rest="${stripped#*|}"
        state="$(_sts_trim "${rest%%|*}")"
        rest="${rest#*|}"
        rest="${rest#*|}"  # skip since
        bead="$(_sts_trim "${rest%%|*}")"
        rest="${rest#*|}"
        reason="$(_sts_trim "$rest")"
        if [ -n "$suite_dir" ] && [ ! -f "$suite_dir/$suite" ]; then
            printf 'suite-state:%d: suite does not exist: %s\n' "$lineno" "$suite" >&2
            errors=$((errors+1))
        fi
        case "$state" in
            active|quarantined|disabled) ;;
            *)
                printf 'suite-state:%d: unknown state %s (valid: active quarantined disabled)\n' \
                    "$lineno" "$state" >&2
                errors=$((errors+1)) ;;
        esac
        if [ -z "$reason" ]; then
            printf 'suite-state:%d: missing reason for %s\n' "$lineno" "$suite" >&2
            errors=$((errors+1))
        fi
        if [ "$state" = "quarantined" ] && [ -z "$bead" ]; then
            printf 'suite-state:%d: quarantined suite %s has no bead id\n' "$lineno" "$suite" >&2
            errors=$((errors+1))
        fi
    done < "$file"
    [ "$errors" -eq 0 ]
}
