#!/usr/bin/env bash
#
# host-check.sh — require every suite to run in a container, or declare why it cannot.
#
#   host-check.sh                    check all suites; exit 1 naming each undeclared host suite
#   host-check.sh --host-check       same as no-arg (the default)
#   host-check.sh --host-check <f>   check one file
#   host-check.sh --count-undeclared print the count of suites with neither declaration nor container
#   host-check.sh --count-copying    print the count of suites still copying harness files or stubs
#
# WHAT THIS IS FOR. The container is the default execution context for a suite.
# A suite added with no annotation is expected to run inside testenv.sh; one that
# cannot says why at the top of its file:
#
#   # host-reason: drives the operator's own tmux server, which the container has no equivalent of
#
# The reason must be non-empty — it is the point of the declaration. This fence
# refuses a suite that has neither a container call nor a reason, and counts all
# undeclared suites so migration progress is visible on the sweep.
#
# HOW THIS FITS THE GATE. hermetic.sh carried this check as --host-check alongside
# its main scan (which detected suites reaching the running box). The main scan became
# moot once every suite runs in a container — commands that would have been unroutable
# on the host are legitimate inside a real install, and the container provides isolation
# the scan was written to compensate for. This script is what remains: the declaration
# requirement that ensures host-running exceptions are visible and intentional.
#
# ITS ESCAPE. Add `# host-reason: <why>` anywhere in the suite file. The reason
# must be non-empty text. Do not leave the marker without a reason; that is refused
# as well, because the reason is what makes the declaration meaningful.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

# host_reason_of <file> -> prints the reason text from `# host-reason:`.
# Returns 0 and prints the reason (possibly empty if no text follows the colon)
# when the annotation is present; returns 1 if it is absent.
host_reason_of() {
    local line r
    while IFS= read -r line; do
        case "$line" in '# host-reason:'*)
            r="${line#'# host-reason:'}"
            # Strip leading whitespace between the colon and the reason text.
            while [ "${r:0:1}" = " " ] || [ "${r:0:1}" = "	" ]; do r="${r:1}"; done
            # Strip trailing whitespace so `# host-reason:   ` is treated as empty.
            while [ -n "$r" ] && { [ "${r: -1}" = " " ] || [ "${r: -1}" = "	" ]; }; do
                r="${r%?}"
            done
            printf '%s' "$r"
            return 0
        ;; esac
    done < "$1"
    return 1
}

# has_container <file> -> 0 if any non-comment code line calls testenv.sh with up or exec.
# `up` and `exec` are the subcommands that start or send work into a container; the others
# (scratch, shell, tag, probe) run on the host. Skips comment-only and blank lines.
has_container() {
    local line stripped
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        # Strip leading whitespace, then skip lines whose first non-blank character is #.
        stripped="$line"
        while [ "${stripped:0:1}" = " " ] || [ "${stripped:0:1}" = "	" ]; do
            stripped="${stripped:1}"
        done
        case "$stripped" in '#'*|'') continue ;; esac
        # Non-comment code line: match testenv.sh (or a variable that contains it) being
        # called with `up` or `exec`. The `up` subcommand starts the container; `exec` sends
        # a command into it. Both are the signals that assertions run inside a container.
        case "$line" in
            *testenv*" up "*|*testenv*" up"|\
            *testenv*" exec "*|*testenv*" exec"|\
            *TESTENV*" up "*|*TESTENV*" up"|\
            *TESTENV*" exec "*|*TESTENV*" exec") return 0 ;;
        esac
    done < "$1"
    return 1
}

# host_check_one <file> <relpath> -> 0 if ok, 1 if refused (prints the reason)
host_check_one() {
    local f="$1" rel="$2" r
    if r="$(host_reason_of "$f")"; then
        if [ -n "$r" ]; then return 0; fi
        # Declared but empty: the reason is the point of the declaration.
        printf '%s: # host-reason: with no reason text\n' "$rel"
        return 1
    fi
    if has_container "$f"; then return 0; fi
    printf '%s: host suite without a declaration\n' "$rel"
    return 1
}

case "${1:-}" in
# --host-check [<file>]: check one file, or all suites, for host-reason compliance.
# A suite passes if it has a non-empty `# host-reason:` declaration, or if any
# non-comment code line calls testenv.sh with `up` or `exec`.
""|--host-check)
    if [ -n "${2:-}" ]; then
        host_check_one "$2" "${2##*/}"
        rc=$?
        if [ "$rc" -ne 0 ]; then
            cat >&2 <<'WHY'

REFUSED by host-check.sh — the suite above runs on the host without declaring why.

  The default is the container. A suite that cannot run in a container names the reason:

    # host-reason: drives the operator's own tmux server, which the container has no equivalent of

  The reason must be non-empty. A suite that runs in a container instead carries no annotation
  and calls testenv.sh (up or exec) so its assertions execute inside the container.
WHY
        fi
        exit "$rc"
    fi
    # Walk all suites in the tree, same glob as the main check.
    shopt -s nullglob
    suites=("$ROOT"/spira/test-*.sh)
    if [ "${#suites[@]}" -eq 0 ]; then
        printf 'host-check: no suites matched %s/spira/test-*.sh — refusing to report clean\n' "$ROOT" >&2
        exit 3
    fi
    bad=0
    for f in "${suites[@]}"; do
        case "${f##*/}" in test-host-check.sh|test-host-reason.sh) continue ;; esac
        rel="${f#"$ROOT"/}"
        host_check_one "$f" "$rel" || bad=1
    done
    if [ "$bad" = 0 ]; then
        printf 'host-check: clean — all %d suite(s) have a container or # host-reason:\n' "${#suites[@]}"
        exit 0
    fi
    cat >&2 <<'WHY'

REFUSED by host-check.sh — each suite above runs on the host without declaring why.

  The default is the container. A suite that cannot run in a container names the reason:

    # host-reason: drives the operator's own tmux server, which the container has no equivalent of

  The reason must be non-empty. A suite that runs in a container instead carries no annotation
  and calls testenv.sh (up or exec) so its assertions execute inside the container.
WHY
    exit 1
    ;;

# --count-undeclared: print the count of suites with neither # host-reason: nor container
# calls. Renders ? if the suite glob matches nothing (empty tree is indistinguishable from a
# wrong path). Exits 0 either way — this is an informational count, not a gate.
--count-undeclared)
    shopt -s nullglob
    suites=("$ROOT"/spira/test-*.sh)
    if [ "${#suites[@]}" -eq 0 ]; then printf '?\n'; exit 0; fi
    count=0
    for f in "${suites[@]}"; do
        host_reason_of "$f" > /dev/null && continue   # declared
        has_container "$f" && continue                # container suite
        count=$(( count + 1 ))
    done
    printf '%d\n' "$count"
    exit 0
    ;;

# --count-copying: print the count of suites that still copy harness files or create inline
# stubs — the wave 2 migration backlog. Falls monotonically as child beads of sp-841s close.
# Renders ? if the glob matches nothing. Host-reason suites are excluded: they are declared
# and intentional, not wave-2 candidates. Exits 0 either way — informational, not a gate.
--count-copying)
    shopt -s nullglob
    suites=("$ROOT"/spira/test-*.sh)
    if [ "${#suites[@]}" -eq 0 ]; then printf '?\n'; exit 0; fi
    count=0
    for f in "${suites[@]}"; do
        host_reason_of "$f" > /dev/null && continue   # declared host suite, not wave-2 work
        # Detect cp of harness files or inline stub creation. Three patterns cover the corpus:
        #   cp "$HERE/  — copies a named harness script to a scratch dir
        #   FAKE_SPIRA_HOME — creates a fake installation tree
        #   cat > "$...sh  — writes an inline stub script to a variable path
        if grep -qE 'cp "\$HERE/|FAKE_SPIRA_HOME=|cat > "\$[A-Za-z_]+/[^ ].*\.sh' "$f" 2>/dev/null; then
            count=$(( count + 1 ))
        fi
    done
    printf '%d\n' "$count"
    exit 0
    ;;

*)
    printf 'usage: host-check.sh [--host-check [<file>]|--count-undeclared|--count-copying]\n' >&2
    exit 2
    ;;
esac
