#!/usr/bin/env bash
#
# test-repo-label.sh — bdq create refuses a repo: label absent from the repo map.
#
#   ./test-repo-label.sh
#
# THE DEFECT THIS PREVENTS. A bead filed with repo:spira-harness is never claimed:
# the summon-time fence catches it, but only after wasting a summon (4 burned for
# sp-nlhy, 2026-09-08). The refusal belongs at file time, before bd is called.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control). The bad-label case
# is tested before the clean-map case, confirming the check fires on a known offender.
#
# The test also runs against the shipped repo-map.example (not just the fixture) to
# prove the check reads whatever file SPIRA_REPO_MAP names. A check backed by a
# hardcoded list would accept the fixture's names while refusing example-map names
# like "home" — making that section fail and revealing the drift (sp-s42p).
#
# defect: sp-f9vu sp-s42p
# tier: T1
# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-repo-label.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A minimal repo map with two known entries.
MAP="$TMP/repo-map"
printf 'spira    | /srv/spira     | push | origin/main |  |\n' > "$MAP"
printf 'widget   | /srv/widget    | pr   | origin/main |  |\n' >> "$MAP"

# A stub bd that records its arguments and exits 0, so "valid label" calls reach it.
STUB_BD="$TMP/bd"
printf '#!/usr/bin/env bash\nprintf "bd-called\\n"; exit 0\n' > "$STUB_BD"
chmod +x "$STUB_BD"

# Source lib.sh with a fixture environment so it does not read the real database. The
# compiled `bdq` binary each bash -c execs resolves fresh from SPIRA_TOML, never from this
# process's inherited env, so the registered keys go through tl_config and SPIRA_TOML is
# threaded through env -i to reach it.
tl_config SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$MAP" SPIRA_BD="$STUB_BD"
run_check() {   # run_check <label-string> -> "ok" or "refused:<stderr>"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" \
        bash -c '. "$1/lib.sh"; _bdq_check_repo_label create title --labels "$2"' \
            -- "$HERE" "$1" 2>&1
}

run_bdq_create() {  # run_bdq_create <labels> -> combined stdout+stderr, exits as bdq does
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" BD_TIMEOUT=10 \
        bash -c '. "$1/lib.sh"; bdq create title --labels "$2"' \
            -- "$HERE" "$1" 2>&1
}

# ==========================================================================
echo
echo "POSITIVE CONTROL: bad label is refused:"
# ==========================================================================

out="$(run_check "spira,plan,repo:spira-harness" || true)"
want "bad label: error mentions the offending name" "spira-harness"  "$out"
want "bad label: error names a valid key"           "spira"          "$out"

out2="$(run_bdq_create "spira,plan,repo:spira-harness" || true)"
want "bdq create: refused for bad label"            "spira-harness"  "$out2"
nowant "bdq create: bd not called for bad label"    "bd-called"      "$out2"

# ==========================================================================
echo
echo "valid label passes through to bd:"
# ==========================================================================

out3="$(run_bdq_create "spira,plan,repo:spira" 2>&1)" || true
nowant "good label: no refusal error"  "is not in the repo map"  "$out3"
want   "good label: bd was called"     "bd-called"               "$out3"

# ==========================================================================
echo
echo "no repo: label passes through:"
# ==========================================================================

out4="$(run_bdq_create "spira,plan" 2>&1)" || true
nowant "no repo: no refusal"  "is not in the repo map"  "$out4"
want   "no repo: bd was called"  "bd-called"            "$out4"

# ==========================================================================
echo
echo "error message names ALL valid keys:"
# ==========================================================================

out5="$(run_check "repo:unknown" || true)"
want "all keys: spira present"  "spira"   "$out5"
want "all keys: widget present" "widget"  "$out5"

# ==========================================================================
echo
echo "using the shipped repo-map.example — proves the check reads the file:"
# ==========================================================================
# A check backed by a hardcoded list of names (rather than reading the map)
# would accept the fixture's names (spira, widget) while refusing a name like
# "home" that only appears in repo-map.example, making this section fail and
# revealing the drift. The fixture tests the mechanism; this section proves
# the mechanism reads whatever file SPIRA_REPO_MAP names.

EXAMPLE_MAP="$HERE/repo-map.example"
example_first="$(awk 'BEGIN{FS="|"} /^[[:space:]]*#/{next}
    NF>1 { gsub(/^[[:space:]]+|[[:space:]]+$/, "", $1); if ($1!="") { print $1; exit } }' \
    "$EXAMPLE_MAP")"

run_example() {   # run_example <labels>
    tl_config SPIRA_REPO_MAP="$EXAMPLE_MAP"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" BD_TIMEOUT=10 \
        bash -c '. "$1/lib.sh"; bdq create title --labels "$2"' \
            -- "$HERE" "$1" 2>&1
}

out_ex1="$(run_example "repo:$example_first" 2>&1)" || true
nowant "example map: first entry ($example_first) accepted"   "is not in the repo map"  "$out_ex1"
want   "example map: first entry reaches bd"                  "bd-called"               "$out_ex1"

out_ex2="$(run_example "repo:definitely-not-a-repo" 2>&1)" || true
want   "example map: absent name is refused"         "is not in the repo map"  "$out_ex2"
nowant "example map: bd not called for absent"       "bd-called"               "$out_ex2"
want   "example map: refusal names a valid key"      "$example_first"          "$out_ex2"

# ==========================================================================
echo
echo "EXEC BOUNDARY: the shim carries SPIRA_REPO_MAP across to the compiled bdq binary even"
echo "though conf.sh's OWN resolution leaves it genuinely unexported, as production does:"
# ==========================================================================
# conf.sh deliberately never exports SPIRA_HOME, SPIRA_REPO, SPIRA_REPO_DERIVED,
# SPIRA_HOME_REPO or SPIRA_REPO_MAP (each a per-copy fact, not configuration). Every section
# above overrides SPIRA_REPO_MAP via `env -i ... SPIRA_REPO_MAP="$MAP" bash -c ...` — but
# `env -i VAR=val` always produces an EXPORTED shell variable once bash starts, and bash
# NEVER strips the export attribute on a later plain reassignment (verified: `export X=1;
# X=2` leaves X exported) — so every section above, including the repo-map.example one,
# accidentally keeps SPIRA_REPO_MAP exported the whole time and never exercises the hazard.
#
# This section instead sets NO SPIRA_REPO_MAP literal in env -i at all — only SPIRA_HOME (as
# every real caller does: a systemd unit, an aeon session, always exports it) — so conf.sh
# resolves the map itself, through `spira-config resolve`'s own typed export set, which
# assigns SPIRA_REPO_MAP with a PLAIN statement (confirmed: `declare -p SPIRA_REPO_MAP` after
# sourcing shows `declare --`, no `-x`). That is the genuine hazard: a value lib.sh's own
# machinery computed and deliberately left unexported, which the compiled `bdq` binary —
# spawned as a child process — cannot see unless this shim threads it through explicitly.
#
# conf.sh refuses outright with no SPIRA_TOML at all (per Ryan 2026-10-05), so this section
# has to pass SPIRA_TOML through for conf.sh's resolution to happen at all — and then declare
# SPIRA_REPO_MAP there (tl_config) rather than as an env -i literal, so the value conf.sh
# resolves and plain-assigns is still the same repo-map.example content $example_first was
# parsed from above.
tl_config SPIRA_REPO_MAP="$EXAMPLE_MAP"
run_ambient() {   # run_ambient <labels> -> relies entirely on conf.sh's own resolution
    env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" \
        BD_TIMEOUT=10 \
        bash -c '. "$1/lib.sh"; bdq create title --labels "$2"' \
            -- "$HERE" "$1" 2>&1
}

out_amb1="$(run_ambient "plan,repo:$example_first" 2>&1)" || true
nowant "ambient map: known entry ($example_first) has no refusal"  "is not in the repo map"  "$out_amb1"
want   "ambient map: known entry reaches bd"                       "bd-called"               "$out_amb1"

out_amb2="$(run_ambient "plan,repo:definitely-not-a-repo" 2>&1)" || true
want   "ambient map: unknown entry is refused"              "is not in the repo map"  "$out_amb2"
nowant "ambient map: bd not called for unknown entry"       "bd-called"               "$out_amb2"
want   "ambient map: refusal names a valid key"             "$example_first"          "$out_amb2"

echo
tl_summary
