#!/usr/bin/env bash
#
# escape-classify.sh — classify one (member, red suite) pair the round settled attribution
# on, per sp-6vd2s: every escape is a MAPPING GAP, a GATE GAP, an ENVIRONMENT GAP, or a
# FLAKE. Feeds MAPPING GAPs back into the test plan so the round converges coverage instead
# of just naming a culprit.
#
#   escape-classify.sh classify --repo <path> --base <sha> --tip <sha> --suite-file <path>
#                                [--gate-log <path>] [--branch <name>]
#       -> prints one of: mapping_gap gate_gap environment_gap
#
#   escape-classify.sh record --member <id> --suite <name> --class <class>
#                              [--batch-id <id>] [--repo <name>] [--paths <csv>] [--evidence <text>]
#       -> appends one escape row (run/tsd/escape) and, for class=mapping_gap, files (or
#          bumps the recurrence on) one test-plan-correction bead via incident.sh's own
#          dedupe — a rerun of the same suite never double-files.
#
# CLASSES 1-3 ARE DECIDABLE FROM STATIC EVIDENCE: whether the suite's own # covers: globs
# match a path the member changed, and whether the member's gate ever produced a verdict
# for it (gate.log, gate_meter's own format — rc=$SPIRA_GATE_NOVERDICT is a fault, not a
# fail). FLAKE is not decided here: "red on the round without the member as well" or
# "flips on rerun" both require evidence only the caller already holds (attribute.sh's own
# base-red pass, or a rerun) — a class this script would otherwise have to fabricate by
# re-running the suite itself, which is the caller's job, not the classifier's.
#
# covers: spira/escape-classify.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/suite-covers.sh"

# escape_covers_touched <repo> <base> <tip> <suite-file> -> 0 iff the suite's own
# # covers: globs match at least one path changed between <base> and <tip>. An
# undeclared covers: line (suite_covers_of returns empty) means "covers everything" per
# suite-covers.sh's own rule, so an undeclared suite is never a mapping gap by omission.
escape_covers_touched() {
    local repo="$1" base="$2" tip="$3" suite_file="$4"
    local globs; globs="$(suite_covers_of "$suite_file")"
    [ -n "$globs" ] || return 0
    local changed; changed="$(git -C "$repo" diff --name-only "$base...$tip" 2>/dev/null)"
    [ -n "$changed" ] || return 1
    local g p
    for g in $globs; do
        case "$g" in UC-*) continue ;; esac
        for p in $changed; do
            # shellcheck disable=SC2254
            case "$p" in $g) return 0 ;; esac
        done
    done
    return 1
}

# escape_gate_verdict <gate-log> <branch> -> pass | noverdict | fail | none, read from the
# LAST line <gate-log> carries for <branch> (gate_meter's format: "<ts> <repo> <branch>
# waited=Ns ran=Ns rc=<n>[ note]"). "none" (no line at all) is itself GATE GAP evidence —
# a suite this gate never even attempted is exactly what class 2 names.
escape_gate_verdict() {
    local gate_log="$1" branch="$2"
    [ -n "$branch" ] && [ -r "$gate_log" ] || { printf 'none\n'; return 0; }
    local line; line="$(awk -v b="$branch" '$3 == b { l = $0 } END { print l }' "$gate_log" 2>/dev/null)"
    [ -n "$line" ] || { printf 'none\n'; return 0; }
    local rc; rc="$(printf '%s\n' "$line" | grep -oE 'rc=[0-9]+' | head -1 | cut -d= -f2)"
    case "$rc" in
        0) printf 'pass\n' ;;
        "${SPIRA_GATE_NOVERDICT:-75}") printf 'noverdict\n' ;;
        *) printf 'fail\n' ;;
    esac
}

# escape_classify <repo> <base> <tip> <suite-file> <gate-log> <branch> -> mapping_gap |
# gate_gap | environment_gap. See the file header for why flake is never returned here.
escape_classify() {
    local repo="$1" base="$2" tip="$3" suite_file="$4" gate_log="$5" branch="$6"
    if ! escape_covers_touched "$repo" "$base" "$tip" "$suite_file"; then
        printf 'mapping_gap\n'; return 0
    fi
    case "$(escape_gate_verdict "$gate_log" "$branch")" in
        pass) printf 'environment_gap\n' ;;
        *)    printf 'gate_gap\n' ;;
    esac
}

# escape_file_mapping_gap <member> <suite> <paths-csv> <evidence> -> files, or bumps the
# recurrence on, ONE open test-plan-correction bead per suite. SPIRA_INCIDENT_REF is the
# suite's own name: incident.sh's existing dedupe (law-alerts-must-be-actionable's own
# mechanism, reused rather than reimplemented) is what makes a rerun never double-file.
escape_file_mapping_gap() {
    local member="$1" suite="$2" paths_csv="$3" evidence="$4"
    local body; body="$(mktemp)"
    {
        printf 'Suite: %s\nMember: %s\nPath(s) changed: %s\n\n' "$suite" "$member" "$paths_csv"
        printf '%s does not declare a # covers: glob (or use-case) matching the path(s)\n' "$suite"
        printf 'above, so the touched-suites gate could never have selected it. Add a covers:\n'
        printf 'entry or use-case mapping so this suite is reachable from what %s touched.\n\n' "$member"
        [ -n "$evidence" ] && printf 'Round evidence:\n%s\n' "$evidence"
    } > "$body"
    SPIRA_INCIDENT_REF="test-plan-gap:$suite" \
    SPIRA_INCIDENT_LABELS="${SPIRA_INCIDENT_LABELS:-${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},test-plan-gap}" \
    SPIRA_INCIDENT_CAUSE="test-plan-gap" \
    SPIRA_INCIDENT_REPO="${SPIRA_INCIDENT_REPO:-}" \
        bash "$HERE/incident.sh" file "test-plan gap: $suite does not cover $member's change" "$body"
    local rc=$?
    rm -f "$body"
    return "$rc"
}

escape_record() {
    local member="" suite="" class="" batch_id="" paths="" evidence=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --member)   member="$2"; shift 2 ;;
            --suite)    suite="$2"; shift 2 ;;
            --class)    class="$2"; shift 2 ;;
            --batch-id) batch_id="$2"; shift 2 ;;
            --repo)     SPIRA_INCIDENT_REPO="$2"; shift 2 ;;
            --paths)    paths="$2"; shift 2 ;;
            --evidence) evidence="$2"; shift 2 ;;
            *) printf 'escape-classify.sh record: unknown option: %s\n' "$1" >&2; return 2 ;;
        esac
    done
    [ -n "$member" ] && [ -n "$suite" ] && [ -n "$class" ] || {
        printf 'usage: escape-classify.sh record --member <id> --suite <name> --class <class> [--batch-id <id>] [--repo <name>] [--paths <csv>] [--evidence <text>]\n' >&2
        return 2
    }
    printf 'ESCAPE member=%s suite=%s class=%s\n' "$member" "$suite" "$class"
    _tsd_escape "$member" "$suite" "$class" "$batch_id"
    [ "$class" = mapping_gap ] && escape_file_mapping_gap "$member" "$suite" "$paths" "$evidence"
    return 0
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    SUB="${1:-}"; shift || true
    case "$SUB" in
        classify)
            REPO="" BASE="" TIP="" SUITE_FILE="" GATE_LOG="/nonexistent" BRANCH=""
            while [ $# -gt 0 ]; do
                case "$1" in
                    --repo)       REPO="$2"; shift 2 ;;
                    --base)       BASE="$2"; shift 2 ;;
                    --tip)        TIP="$2"; shift 2 ;;
                    --suite-file) SUITE_FILE="$2"; shift 2 ;;
                    --gate-log)   GATE_LOG="$2"; shift 2 ;;
                    --branch)     BRANCH="$2"; shift 2 ;;
                    *) printf 'escape-classify.sh classify: unknown option: %s\n' "$1" >&2; exit 2 ;;
                esac
            done
            [ -n "$REPO" ] && [ -n "$BASE" ] && [ -n "$TIP" ] && [ -n "$SUITE_FILE" ] || {
                printf 'usage: escape-classify.sh classify --repo <path> --base <sha> --tip <sha> --suite-file <path> [--gate-log <path>] [--branch <name>]\n' >&2
                exit 2
            }
            escape_classify "$REPO" "$BASE" "$TIP" "$SUITE_FILE" "$GATE_LOG" "$BRANCH"
            ;;
        record) escape_record "$@" ;;
        *) printf 'usage: escape-classify.sh classify|record ...\n' >&2; exit 2 ;;
    esac
fi
