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
# SPIRA-CONFIG READS THE SAME FILES AT RUN TIME (spira/conf.d, plus spira/conf.toml.d, keys only
# the typed config carries, which this script never lists); nothing is compiled from them. TYPE,
# MAX and SCHEMA below are read by it, not here.
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

# One awk pass does the whole generation: the registry is ~400 files, and the bash it replaces
# (a read loop per file, a case per pair of keys for the dependency edges) cost seconds of CPU
# on every fresh checkout that sourced conf.sh.
files=()
empty=""
for f in "$CONFD"/*; do
    [ -f "$f" ] || continue
    if [ -s "$f" ]; then files+=("$f"); else empty="$empty${f##*/}"$'\n'; fi
done

if [ "${#files[@]}" -eq 0 ] && [ -z "$empty" ]; then
    echo "conf-gen.sh: $CONFD holds no key files — refusing to generate an empty allowlist" >&2
    exit 1
fi

counts="$(EMPTY_FILES="$empty" OUT_KEYS_TMP="$OUT_KEYS$TMP_SUFFIX" OUT_DEFAULTS_TMP="$OUT_DEFAULTS$TMP_SUFFIX" LC_ALL=C awk '
function sorted(arr, n,   i, j, gap, t) {
    for (gap = int(n / 2); gap > 0; gap = int(gap / 2))
        for (i = gap + 1; i <= n; i++) {
            t = arr[i]
            for (j = i; j > gap && (arr[j - gap] "") > (t ""); j -= gap) arr[j] = arr[j - gap]
            arr[j] = t
        }
}
function leave(code) { failed = code; exit code }
BEGIN {
    ne = split(ENVIRON["EMPTY_FILES"], names, "\n")
    for (i = 1; i <= ne; i++) {
        if (names[i] == "") continue
        keys[++nk] = names[i]
        if (names[i] ~ /^[A-Z_]/) BODY[names[i]] = ""
        else print "conf-gen.sh: " names[i] " — filename is not an uppercase KEY; skipping" > "/dev/stderr"
    }
}
FNR == 1 {
    if (key != "" && in_default) {
        print "conf-gen.sh: " file ": DEFAULT heredoc never closed (missing SPIRA_CONF_DEFAULT_EOF)" > "/dev/stderr"
        leave(1)
    }
    if (key != "") BODY[key] = body
    file = FILENAME
    n = split(file, parts, "/")
    key = parts[n]
    keys[++nk] = key
    in_default = 0
    body = ""
    skipping = (key !~ /^[A-Z_]/)
    if (skipping) {
        print "conf-gen.sh: " file " — filename is not an uppercase KEY; skipping" > "/dev/stderr"
        key = ""
    }
}
skipping { next }
in_default {
    if ($0 == "SPIRA_CONF_DEFAULT_EOF") in_default = 0
    else body = body $0 "\n"
    next
}
/^(TYPE|GROUP|DOC|MAX|SCHEMA)=/ { next }
$0 == "DEFAULT<<'"'"'SPIRA_CONF_DEFAULT_EOF'"'"'" { in_default = 1; next }
$0 == "" { next }
{ print "conf-gen.sh: " file ": unrecognised line, ignoring: " $0 > "/dev/stderr" }
END {
    if (failed) exit failed
    if (key != "" && in_default) {
        print "conf-gen.sh: " file ": DEFAULT heredoc never closed (missing SPIRA_CONF_DEFAULT_EOF)" > "/dev/stderr"
        exit 1
    }
    if (key != "") BODY[key] = body
    sorted(keys, nk)

    print "# GENERATED by spira/conf-gen.sh from spira/conf.d/ — DO NOT EDIT BY HAND." > ENVIRON["OUT_KEYS_TMP"]
    print "# Add, rename or remove a key by touching its file under spira/conf.d/, then either" > ENVIRON["OUT_KEYS_TMP"]
    print "# re-run conf-gen.sh or just source conf.sh again — _spira_conf_gen_ensure notices" > ENVIRON["OUT_KEYS_TMP"]
    print "# this file is stale and regenerates it for you." > ENVIRON["OUT_KEYS_TMP"]
    print "SPIRA_CONF_KEYS=\"" > ENVIRON["OUT_KEYS_TMP"]
    for (i = 1; i <= nk; i++) print keys[i] > ENVIRON["OUT_KEYS_TMP"]
    print "\"" > ENVIRON["OUT_KEYS_TMP"]
    close(ENVIRON["OUT_KEYS_TMP"])

    # A key carries a real default when its body holds `: "${KEY:=` or `: "${KEY=` and a later `}"`.
    nd = 0
    for (i = 1; i <= nk; i++) {
        k = keys[i]
        if (!(k in BODY)) continue
        for (form = 1; form <= 2; form++) {
            open = ": \"${" k (form == 1 ? ":=" : "=")
            at = index(BODY[k], open)
            if (at && index(substr(BODY[k], at + length(open)), "}\"")) { dkeys[++nd] = k; break }
        }
    }
    # An edge for every OTHER defaulted key the body names as `$KEY` or `${KEY`.
    for (i = 1; i <= nd; i++) {
        k = dkeys[i]
        for (j = 1; j <= nd; j++) {
            o = dkeys[j]
            if (o != k && (index(BODY[k], "$" o) || index(BODY[k], "${" o))) {
                indeg[k]++
                dependents[o] = dependents[o] " " k
            }
        }
    }
    # Kahn, a round at a time; keys are already in byte order, so a round is too.
    placed = 0
    while (placed < nd) {
        navail = 0
        for (i = 1; i <= nd; i++) {
            k = dkeys[i]
            if (!(k in done) && indeg[k] == 0) avail[++navail] = k
        }
        if (navail == 0) {
            print "conf-gen.sh: dependency cycle among registry defaults — refusing to generate:" > "/dev/stderr"
            for (i = 1; i <= nd; i++) if (!(dkeys[i] in done)) print "  " dkeys[i] > "/dev/stderr"
            exit 1
        }
        for (i = 1; i <= navail; i++) {
            k = avail[i]
            done[k] = 1
            order[++placed] = k
            m = split(dependents[k], deps, " ")
            for (j = 1; j <= m; j++) indeg[deps[j]]--
        }
    }
    print "# GENERATED by spira/conf-gen.sh from spira/conf.d/ — DO NOT EDIT BY HAND." > ENVIRON["OUT_DEFAULTS_TMP"]
    print "# Topologically sorted: a key'"'"'s default here always runs after any other registry" > ENVIRON["OUT_DEFAULTS_TMP"]
    print "# key its own default text references, regardless of this directory'"'"'s own (merely" > ENVIRON["OUT_DEFAULTS_TMP"]
    print "# alphabetical) file order." > ENVIRON["OUT_DEFAULTS_TMP"]
    for (i = 1; i <= placed; i++) printf "%s", BODY[order[i]] > ENVIRON["OUT_DEFAULTS_TMP"]
    close(ENVIRON["OUT_DEFAULTS_TMP"])
    print nk, nd, placed
}' "${files[@]}")"
read -r n_keys n_default n_ordered <<<"$counts"

mv -f "$OUT_KEYS$TMP_SUFFIX" "$OUT_KEYS"
mv -f "$OUT_DEFAULTS$TMP_SUFFIX" "$OUT_DEFAULTS"

echo "conf-gen.sh: $n_keys keys in the registry, $n_default with a generated default ($n_ordered ordered); wrote $OUT_KEYS and $OUT_DEFAULTS" >&2
