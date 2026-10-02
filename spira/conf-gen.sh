#!/usr/bin/env bash
# conf-gen.sh (sp-g3uwp) — regenerates conf.sh's two generated fragments from the config key
# registry at spira/conf.d/: one file per key, ONE SOURCE from which everything else is
# derived (law-schema-over-code). Never run by hand on a live box as the only way to see new
# config take effect — conf.sh's own _spira_conf_gen_ensure calls this automatically whenever
# the output is missing or older than the registry. Run it by hand only to regenerate ahead of
# a commit, or to debug the generator itself.
#
# WHY A REGISTRY. Before this bead, SPIRA_CONF_KEYS (conf.sh's allowlist) and the ~250
# `: "${KEY:=default}"` statements that fill it in were free text, hand-maintained in one
# giant file. Two branches that each added an unrelated key to the same grouped line
# conflicted on text that had nothing to do with either change (conf.sh: 124 commits in 3
# days, the queue's conflict hot spot). Worse, the allowlist and the defaults could silently
# disagree: by the time this bead started, SPIRA_EXPRESS_LABEL/SPIRA_GH_APP_CONFIG/
# SPIRA_GROOM_THRESHOLD each had TWO default statements, and eleven keys (SPIRA_LC_SOCKET,
# SPIRA_REBASE_DECOMPOSE_FILES, SPIRA_CZAR_OUTCOME_MINS, ...) had a real default but were
# missing from the allowlist entirely, so the operator's config file could never legally set them. A registry
# with one file per key makes both failure modes structural: two branches that each add a
# key add two different files, which git can only merge cleanly, and the allowlist IS the
# directory listing, so it cannot omit a key that has a file.
#
# SPIRA-CONFIG READS THE SAME FILES. Its build script generates SpiraSection, the KEY=>field
# mapping and the key history from conf.d (plus spira/conf.toml.d, keys only the typed
# config carries, which this script never lists). TYPE, MAX and SCHEMA below are read by it, not here.
#
# WHAT A conf.d/<KEY> FILE LOOKS LIKE — these fields, in this order:
#   TYPE=string|u32|u64|bool|list|onoff|czar_stage   the type of the spira-config field
#   MAX=<n>                             optional: schema ceiling for a numeric field (spira-config)
#   SCHEMA=<text>                       optional: the field's schema description (spira-config)
#   GROUP=<lowercase word>              for humans browsing the directory; not consumed here
#   DOC=<one line>                      not consumed here; for humans and future tooling
#   DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'
#       <verbatim bash — comments and exactly one top-level ": "${KEY:=default}"" statement,
#        OR a comment-only stub for a key this generator does not default (see below)>
#   SPIRA_CONF_DEFAULT_EOF
#
# A key's DEFAULT body is OPTIONAL CONTENT, not a required statement. Two kinds of key
# carry no generated default on purpose, and this script must not invent one:
#   PROCEDURAL — the key's default has an ordering constraint (a guard, a dependency on a
#     function-local shell variable, a reader elsewhere in spira_conf_defaults() that runs
#     before any single safe insertion point) that makes hoisting it into a generated,
#     separately-sourced block unsafe. conf.sh keeps defaulting it inline, in its original
#     position; this script still contributes its name to the allowlist.
#   NO-DEFAULT — the key carries no default anywhere today; it resolves empty unless the
#     environment or the operator's config file sets it. Also allowlist-only.
# Either way, a DEFAULT body with no `: "${KEY:=...}"` or `: "${KEY=...}"` statement in it is
# read as "allowlist membership only" — not an error.
#
# ORDERING. A migrated key's default may reference another migrated key (SPIRA_WIKI feeds
# COCKPIT_CWD's default, for one) and conf.d/ is a directory — `ls` gives alphabetical order,
# not dependency order. This script topologically sorts the migrated keys by the OTHER
# registry keys each one's DEFAULT text references, so the generated file sets a dependency
# before anything that reads it, regardless of filename order. A cycle is refused, loudly —
# generation fails rather than emitting a block that would resolve one of the two keys wrong
# depending on the sourcing shell's own hash-table iteration order.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
CONFD="$HERE/conf.d"
OUT_KEYS="$HERE/conf.d.keys.generated.sh"
OUT_DEFAULTS="$HERE/conf.d.defaults.generated.sh"
# PER-PROCESS TEMP NAMES (sp-wm2a3), not a fixed ".tmp" suffix: two processes that each
# source conf.sh against the same, just-created-or-updated spira/ (the generated cache is
# stale for both) used to race on ONE shared "$OUT_KEYS.tmp" — one process's `mv -f` renamed
# it away a heartbeat before the other's own `mv -f` of that now-vanished path, which failed
# ("cannot stat ...: No such file or directory") under this script's `set -e` and left the
# loser's caller refusing to run at all ("refusing to run with a stale or missing generated
# file"). Both writers compute byte-identical output from the same registry, so which one's
# rename wins the final destination does not matter — only that neither clobbers the OTHER's
# still-being-written temp file in between. Cleaned up on any exit so a killed run never
# leaves a stray "*.tmp.<pid>" behind.
TMP_SUFFIX=".tmp.$$"
trap 'rm -f "$OUT_KEYS$TMP_SUFFIX" "$OUT_DEFAULTS$TMP_SUFFIX"' EXIT

if [ ! -d "$CONFD" ]; then
    echo "conf-gen.sh: $CONFD does not exist — refusing to generate an empty allowlist" >&2
    exit 1
fi

# One associative array per field, keyed by registry key name.
declare -A TYPE GROUP DOC DEFAULT_BODY

parse_one() {
    local file="$1" key
    key="$(basename "$file")"
    case "$key" in
        [A-Z_]*) ;;
        *) echo "conf-gen.sh: $file — filename is not an uppercase KEY; skipping" >&2; return 0 ;;
    esac
    local in_default=0 body="" line
    while IFS= read -r line || [ -n "$line" ]; do
        if [ "$in_default" -eq 1 ]; then
            if [ "$line" = "SPIRA_CONF_DEFAULT_EOF" ]; then
                in_default=0
                continue
            fi
            body="$body$line"$'\n'
            continue
        fi
        case "$line" in
            TYPE=*)  TYPE[$key]="${line#TYPE=}" ;;
            GROUP=*) GROUP[$key]="${line#GROUP=}" ;;
            DOC=*)   DOC[$key]="${line#DOC=}" ;;
            MAX=*|SCHEMA=*) ;;
            "DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'") in_default=1 ;;
            "") ;;
            *) echo "conf-gen.sh: $file: unrecognised line, ignoring: $line" >&2 ;;
        esac
    done < "$file"
    if [ "$in_default" -eq 1 ]; then
        echo "conf-gen.sh: $file: DEFAULT heredoc never closed (missing SPIRA_CONF_DEFAULT_EOF)" >&2
        exit 1
    fi
    DEFAULT_BODY[$key]="$body"
}

keys=()
for f in "$CONFD"/*; do
    [ -f "$f" ] || continue
    keys+=("$(basename "$f")")
    parse_one "$f"
done

if [ "${#keys[@]}" -eq 0 ]; then
    echo "conf-gen.sh: $CONFD holds no key files — refusing to generate an empty allowlist" >&2
    exit 1
fi

# -------- OUT_KEYS: the allowlist, one key per line (git-merge-friendly; collapsed to a
# single space-padded line by conf.sh itself, same as it always has been). --------
{
    echo "# GENERATED by spira/conf-gen.sh from spira/conf.d/ — DO NOT EDIT BY HAND."
    echo "# Add, rename or remove a key by touching its file under spira/conf.d/, then either"
    echo "# re-run conf-gen.sh or just source conf.sh again — _spira_conf_gen_ensure notices"
    echo "# this file is stale and regenerates it for you."
    echo "SPIRA_CONF_KEYS=\""
    printf '%s\n' "${keys[@]}" | LC_ALL=C sort
    echo "\""
} > "$OUT_KEYS$TMP_SUFFIX"

# -------- OUT_DEFAULTS: topologically sorted ": "${KEY:=...}"" / ": "${KEY=...}"" statements
# for every key whose DEFAULT body actually contains one. --------

# Which keys carry a real default statement (vs. an allowlist-only procedural/no-default stub)?
has_default=()
for k in "${keys[@]}"; do
    case "${DEFAULT_BODY[$k]:-}" in
        *': "${'"$k"':='*'}"'*|*': "${'"$k"'='*'}"'*) has_default+=("$k") ;;
    esac
done

# Dependency edges: for each defaulted key, which OTHER defaulted keys does its body mention?
declare -A DEPS
for k in "${has_default[@]}"; do
    deps=""
    for other in "${has_default[@]}"; do
        [ "$other" = "$k" ] && continue
        case "${DEFAULT_BODY[$k]}" in
            *"\$$other"*|*'${'"$other"*) deps="$deps $other" ;;
        esac
    done
    DEPS[$k]="$deps"
done

# Kahn's algorithm, deterministic (alphabetical) tie-break so the generated file's statement
# order does not reshuffle on every run.
declare -A INDEG
for k in "${has_default[@]}"; do INDEG[$k]=0; done
for k in "${has_default[@]}"; do
    for d in ${DEPS[$k]}; do
        INDEG[$k]=$(( INDEG[$k] + 1 ))
    done
done

remaining=("${has_default[@]}")
order=()
progress=1
while [ "${#remaining[@]}" -gt 0 ] && [ "$progress" -eq 1 ]; do
    progress=0
    next_remaining=()
    avail=()
    for k in "${remaining[@]}"; do
        if [ "${INDEG[$k]}" -eq 0 ]; then
            avail+=("$k")
        else
            next_remaining+=("$k")
        fi
    done
    if [ "${#avail[@]}" -gt 0 ]; then
        progress=1
        # deterministic order among this round's available keys
        while IFS= read -r k; do
            [ -n "$k" ] || continue
            order+=("$k")
            for other in "${remaining[@]}"; do
                case " ${DEPS[$other]} " in
                    *" $k "*) INDEG[$other]=$(( INDEG[$other] - 1 )) ;;
                esac
            done
        done < <(printf '%s\n' "${avail[@]}" | LC_ALL=C sort)
    fi
    remaining=("${next_remaining[@]}")
done

if [ "${#remaining[@]}" -gt 0 ]; then
    echo "conf-gen.sh: dependency cycle among registry defaults — refusing to generate:" >&2
    printf '  %s\n' "${remaining[@]}" >&2
    exit 1
fi

{
    echo "# GENERATED by spira/conf-gen.sh from spira/conf.d/ — DO NOT EDIT BY HAND."
    echo "# Topologically sorted: a key's default here always runs after any other registry"
    echo "# key its own default text references, regardless of this directory's own (merely"
    echo "# alphabetical) file order."
    for k in "${order[@]}"; do
        printf '%s' "${DEFAULT_BODY[$k]}"
    done
} > "$OUT_DEFAULTS$TMP_SUFFIX"

mv -f "$OUT_KEYS$TMP_SUFFIX" "$OUT_KEYS"
mv -f "$OUT_DEFAULTS$TMP_SUFFIX" "$OUT_DEFAULTS"

echo "conf-gen.sh: ${#keys[@]} keys in the registry, ${#has_default[@]} with a generated default (${#order[@]} ordered); wrote $OUT_KEYS and $OUT_DEFAULTS" >&2
