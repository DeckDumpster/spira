#!/usr/bin/env bash
#
# schema.sh — the ONE declaration of the bead model, and the only file allowed to contain a
# label, status or type literal.
#
#   schema.sh contract              the model, declared beside what the store actually reports
#   schema.sh check                 exit non-zero when the store has drifted from the declaration
#   schema.sh name <key>            one name, resolved; fails closed on an undeclared key
#   schema.sh kinds|statuses|dims   the declared vocabularies, one per line
#
# WHY THIS EXISTS
# ---------------
# A name that appears as a literal in a reader is a name that can disagree with the writer.
# spira/lib.sh:221 reads "${SPIRA_ASK_LABEL:-needs-ryan}" while spira/lib.sh:117 greps for a
# hardcoded "needs-ryan" — and the code default is `needs-operator`, so on a default install
# the destructive-procedure fence has NO bypass at all and every legitimate halting bead is
# refused. The same split exists for awaiting-ci (6 files), spike (10), groom (6),
# maechen-sweep (3). One accessor per name makes that class unwritable, because there is no
# literal left to write.
#
# WHAT IS DECLARED HERE vs WHAT IS ASKED
# --------------------------------------
# Declared here: the KINDS, the custom STATUSES, and the state DIMENSIONS. These are our
# model; nothing else in the world knows them, so this file is their source.
#
# Asked, never written down: personas and their partitions (the .fayth files), the repository
# map, and the type/status vocabularies the bd binary will actually accept. `bead.sh contract`
# already works this way and the reason generalises — a list written down is a list that goes
# stale silently, which is how world.sh came to miss nine timers.
#
# `contract` prints DECLARED beside REPORTED precisely so a drift is visible rather than
# inferred, and `check` is the same comparison with an exit code (law-schema-over-code: the
# store enforces what it can; this file covers what SQL cannot express).
set -uo pipefail
SCHEMA_HOME="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=/dev/null
. "$SCHEMA_HOME/conf.sh" || true
# shellcheck source=/dev/null
. "$SCHEMA_HOME/lib.sh" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# THE DECLARATION
# --------------------------------------------------------------------------------------
# KIND — what a record IS. It never changes over the record's life, which is why it is a
# type rather than a status. Every work query excludes the non-work kinds BY TYPE, which is
# validated, rather than by a label predicate each reader has to remember.
#
# `insight` is deliberately NOT a type: an insight is created closed, so every work query
# already excludes it on status, and a type would cost something in each reader that
# enumerates types while buying nothing. It is a closed `chore`
# carrying the `insight` label at P4.
#
# `gate` is bd's own type, backing await_type/await_id/timeout/waiters. It is listed here so
# that exhaustive registration does not retire it by omission.
SCHEMA_KINDS="work:task event:event escalation:escalation proposal:proposal insight:chore gate:gate"

# The kinds a work query returns. Anything not here is excluded by type.
SCHEMA_WORK_TYPES="task bug feature epic chore spike"

# STATUS — where a record is UP TO. Mutually exclusive by construction, which is the point:
# a bead cannot be both approved and premise_rejected, and today's label bag permits it.
#
# awaiting_ci is NOT here. It is a gh:run GATE, because a status still requires every reader
# to remember to exclude it while a gate makes the bead not ready — so a reader that knows
# nothing about CI is still correct.
SCHEMA_STATUSES="approved premise_rejected retired archived"

# DIMENSION — a single-valued attribute, written with `bd set-state <id> <dim>=<val>` and read
# with `bd state <id> <dim>`. set-state removes the previous dim: label atomically, so
# single-valuedness is enforced rather than merely observed, and it writes an event bead as
# the source of truth with the label as a lookup cache.
#
# A dimension with no enumerable value set declares `*`.
SCHEMA_DIMS="repo:* severity:* branch:* fayth:* lane:* gate:none,awaiting,red,green"

# NAMES — every configurable label name, with its accessor. The value comes from conf.sh so
# an operator's override is honoured; the DEFAULT lives here and nowhere else.
schema_name() {          # schema_name <key> -> the name; exit 2 on an undeclared key
    local k="${1:-}"
    case "$k" in
        ask)           printf '%s' "${SPIRA_ASK_LABEL:-needs-operator}" ;;
        ci)            printf '%s' "${SPIRA_CI_LABEL:-awaiting-ci}" ;;
        scope)         printf '%s' "${SPIRA_SCOPE_LABEL-spira}" ;;
        # THE TWO CORE PARTITION LABELS — one per persona that works the plan. Declared here
        # so a caller that needs to say "plan bead" or "incident bead" reads the configured
        # value through a validated name rather than through a literal it has to remember and
        # keep consistent with the fayth file. An undeclared key returns 2 (below); if a
        # caller asks for a name not on this list, the error is immediate rather than a silent
        # empty query that returns [] from bd and reads as "no work".
        plan)          printf '%s' "${SPIRA_PLAN_LABEL:-plan}" ;;
        incident)      printf '%s' "${SPIRA_INCIDENT_LABEL:-incident}" ;;
        spike)         printf '%s' "${SPIRA_SPIKE_LABEL:-spike}" ;;
        groomer)       printf '%s' "${SPIRA_GROOMER_LABEL:-groom}" ;;
        maechen)       printf '%s' "${SPIRA_MAECHEN_LABEL:-maechen-sweep}" ;;
        maechen_remedy) printf '%s' "${SPIRA_MAECHEN_REMEDY_LABEL:-maechen-remedy}" ;;
        review)        printf '%s' "${SPIRA_REVIEW_LABEL:-review-finding}" ;;
        reclaim_skip)  printf '%s' "${SPIRA_RECLAIM_SKIP_LABEL:-spira-waiting-operator}" ;;
        world_stop)    printf '%s' "${SPIRA_WORLD_STOP_LABEL:-world-stop}" ;;
        insight)       printf '%s' 'insight' ;;
        # FAIL CLOSED. An undeclared key must not resolve to the empty string: an empty label
        # in a query is a well-formed question about nothing, which returns [] truthfully and
        # reads exactly like "no work" — that is sp-xrkuu, where the pane printed a confident
        # 0 against a true 32.
        *)  printf 'schema: no such name: %s\n' "${k:-<empty>}" >&2
            printf 'schema: declared names: %s\n' "$(schema_names | tr '\n' ' ')" >&2
            return 2 ;;
    esac
}
schema_names() { printf '%s\n' ask ci scope plan incident spike groomer maechen maechen_remedy review reclaim_skip world_stop insight; }

# schema_default <key> — the declared default value, ignoring env and conf.
# Used by literal-lint.sh to build its pattern list without the ambient environment
# masking the shipped default (law-schema-over-code).
schema_default() {
    local k="${1:-}"
    case "$k" in
        ask)            printf '%s' "needs-operator" ;;
        ci)             printf '%s' "awaiting-ci" ;;
        scope)          printf '%s' "spira" ;;
        plan)           printf '%s' "plan" ;;
        incident)       printf '%s' "incident" ;;
        spike)          printf '%s' "spike" ;;
        groomer)        printf '%s' "groom" ;;
        maechen)        printf '%s' "maechen-sweep" ;;
        maechen_remedy) printf '%s' "maechen-remedy" ;;
        review)         printf '%s' "review-finding" ;;
        reclaim_skip)   printf '%s' "spira-waiting-operator" ;;
        world_stop)     printf '%s' "world-stop" ;;
        insight)        printf '%s' "insight" ;;
        *)  printf 'schema: no such name: %s\n' "${k:-<empty>}" >&2
            printf 'schema: declared names: %s\n' "$(schema_names | tr '\n' ' ')" >&2
            return 2 ;;
    esac
}

schema_kinds()    { local p; for p in $SCHEMA_KINDS; do printf '%s\n' "${p%%:*}"; done; }
schema_type_of()  { local p; for p in $SCHEMA_KINDS; do [ "${p%%:*}" = "${1:-}" ] && { printf '%s' "${p#*:}"; return 0; }; done
                    printf 'schema: no such kind: %s\n' "${1:-<empty>}" >&2; return 2; }
schema_statuses() { printf '%s\n' $SCHEMA_STATUSES; }
schema_dims()     { local p; for p in $SCHEMA_DIMS; do printf '%s\n' "${p%%:*}"; done; }
schema_dim_values(){ local p; for p in $SCHEMA_DIMS; do [ "${p%%:*}" = "${1:-}" ] && { printf '%s' "${p#*:}"; return 0; }; done
                    printf 'schema: no such dimension: %s\n' "${1:-<empty>}" >&2; return 2; }

# The custom types the store must carry: every kind's type that is not a bd built-in.
schema_custom_types() {
    local k t builtin=" task bug feature epic chore decision spike story milestone "
    for k in $(schema_kinds); do
        t="$(schema_type_of "$k")"
        case "$builtin" in *" $t "*) continue ;; esac
        printf '%s\n' "$t"
    done | sort -u
}

# --------------------------------------------------------------------------------------
# WHAT THE STORE REPORTS — asked, never assumed.
# --------------------------------------------------------------------------------------
# THE SEAM IS `bd sql`, not a direct dolt call. bd already knows which database this store
# is, so routing through it needs no path of our own — and a path of our own is one
# operator's box baked into a repository meant to be cloned, which inventory.sh refuses and
# test-conf.sh fails the gate on.
_sql_scalar()     { bd -C "$SPIRA_DB" sql "$1" 2>/dev/null | sed -n '3p' | tr -d ' '; }
_store_types()    { bd -C "$SPIRA_DB" types 2>/dev/null | sed -n '/Configured custom types/,$p' | tail -n +2 | tr -d ' ' | grep -v '^$'; }
_store_statuses() { bd -C "$SPIRA_DB" config get status.custom 2>/dev/null | tail -1 | tr ',' '\n' | tr -d ' ' | grep -v '^$'; }

schema_contract() {
    echo "KINDS — declared here; a kind is what a record IS and never changes"
    local k; for k in $(schema_kinds); do printf '  %-11s -> issue_type %s\n' "$k" "$(schema_type_of "$k")"; done
    echo
    echo "WORK TYPES — what a work query returns; everything else is excluded BY TYPE"
    printf '  %s\n' "$SCHEMA_WORK_TYPES"
    echo
    echo "STATUSES — declared here; where a record is UP TO. Mutually exclusive."
    printf '  %s\n' "$SCHEMA_STATUSES"
    echo
    echo "DIMENSIONS — single-valued; write with 'bd set-state', read with 'bd state'"
    local d; for d in $(schema_dims); do printf '  %-9s %s\n' "$d" "$(schema_dim_values "$d")"; done
    echo
    echo "NAMES — one accessor each; the literal appears in no other file"
    for k in $(schema_names); do printf '  %-15s %s\n' "$k" "$(schema_name "$k")"; done
    echo
    echo "PERSONAS — asked of the chamber, never written down here"
    if command -v fayth_names >/dev/null 2>&1; then
        for k in $(fayth_names 2>/dev/null); do printf '  %-9s %s\n' "$k" "$(fayth_get "$k" FAYTH_LABELS '' 2>/dev/null)"; done
    else echo "  (lib.sh unavailable)"; fi
    echo
    echo "STORE — what bd actually carries right now"
    printf '  custom types:    %s\n' "$(_store_types | tr '\n' ' ')"
    printf '  custom statuses: %s\n' "$(_store_statuses | tr '\n' ' ')"
}

# check — DECLARED vs REPORTED, with an exit code. This is the drift detector for everything
# law-schema-over-code pushed into the store: a constraint or a registration that silently
# disappeared is worse than one never added, because every tool above it stopped guarding
# what it no longer does.
schema_check() {
    local rc=0 want have x
    want="$(schema_custom_types)"; have="$(_store_types)"
    for x in $want; do
        printf '%s\n' "$have" | grep -qx "$x" || { printf 'schema: MISSING custom type: %s\n' "$x" >&2; rc=1; }
    done
    for x in $have; do
        printf '%s\n' "$want" | grep -qx "$x" || { printf 'schema: UNDECLARED custom type still registered: %s\n' "$x" >&2; rc=1; }
    done
    want="$(schema_statuses)"; have="$(_store_statuses)"
    for x in $want; do
        printf '%s\n' "$have" | grep -qx "$x" || { printf 'schema: MISSING custom status: %s\n' "$x" >&2; rc=1; }
    done
    # THE SUBSTRATE HALF. These live in Dolt, below bd, and a bd upgrade can carry a table
    # rewrite that does not bring them across. A constraint that silently disappeared is
    # worse than one never added, because every tool above it stopped guarding what it no
    # longer does — so its absence must be an ERROR here, not an omission.
    local q
    q="select count(*) from information_schema.columns where table_name='issues' and column_name='_is_work';"
    [ "$(_sql_scalar "$q")" = "1" ] || { printf 'schema: MISSING generated column: _is_work\n' >&2; rc=1; }
    q="select count(*) from information_schema.table_constraints where table_name='issues' and constraint_type='CHECK' and constraint_name='spira_priority_range';"
    [ "$(_sql_scalar "$q")" = "1" ] || { printf 'schema: MISSING constraint: spira_priority_range\n' >&2; rc=1; }
    [ "$rc" = 0 ] && echo "schema: store matches the declaration"
    return "$rc"
}

case "${1:-contract}" in
    contract) schema_contract ;;
    check)    schema_check ;;
    name)     shift; schema_name "${1:-}" ;;
    default)  shift; schema_default "${1:-}" ;;
    type-of)  shift; schema_type_of "${1:-}" ;;
    kinds)    schema_kinds ;;
    statuses) schema_statuses ;;
    dims)     schema_dims ;;
    names)    schema_names ;;
    custom-types) schema_custom_types ;;
    *) printf 'usage: schema.sh [contract|check|name <key>|default <key>|type-of <kind>|kinds|statuses|dims|names|custom-types]\n' >&2; exit 2 ;;
esac
