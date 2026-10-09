#!/usr/bin/env bash
#
# test-repo-map.sh — the repo map: lanes column parsing and mode expansion, and bdq's
# refusal of a repo: label the map does not name.
#
#   ./test-repo-map.sh
#
# Each absence assertion is preceded by a presence assertion on the same path
# (law-absence-needs-a-positive-control).
#
# defect: sp-5q5mi sp-f9vu sp-s42p
# tier: T1
# covers: spira/lib.sh doctor/src/* UC-config-store-preflight-12 UC-config-store-preflight-13
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-repo-map.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# A MINIMAL ENVIRONMENT with non-default label values where possible, so assertions against
# defaults are not trivially satisfied by literals in the code
# (law-gates-run-in-a-clean-environment).
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T" PATH="$T:$PATH"
# locate_home no longer searches: SPIRA_HOME IS the home, and conf.sh reads <home>/conf.d
# for the registry — SPIRA_HOME is explicitly pinned to $T above, so it never falls back
# to its own BASH_SOURCE location (which has one).
ln -s "$HERE/conf.d" "$T/conf.d"
tl_config SPIRA_RUN="$T/run" SPIRA_DB="$T/no-db"
# Non-default label values to prove mode expansion reads conf vars, not literals.
tl_config SPIRA_PLAN_LABEL="plan" SPIRA_INCIDENT_LABEL="incident" SPIRA_GROOMER_LABEL="groom" \
    SPIRA_MAECHEN_LABEL="maechen-sweep" SPIRA_SPIKE_LABEL="spike" SPIRA_CZAR_LABEL="czar-trigger"

# A throwaway repo-map. SPIRA_REPO_MAP is set per section.
MAP="$T/repo-map"

. "$HERE/lib.sh"

# ==========================================================================================
echo
echo "criterion 1 — six-column row (no lanes column) admits all lanes"
# ==========================================================================================
printf 'alpha | /tmp/alpha | push | origin/main | | true\n' > "$MAP"
# repo_field/repo_root/spira_repo_lanes (lib.sh -> _spira_config_repo -> `spira-config
# repo`) read SPIRA_REPO_MAP from this shell's literal environment (repo_registry(),
# spira-config/src/main.rs, builds its Registry from std::env::vars() directly — the repo
# registry is not part of the one-source-of-config resolve() path). tl_config alone (which
# only writes $SPIRA_TOML's override) does not reach it; a plain shell copy is required too.
SPIRA_REPO_MAP="$MAP"
tl_config SPIRA_REPO_MAP="$MAP"

# POSITIVE CONTROL: the row IS found (repo_field returns something for it).
is "six-col: path column resolves" "/tmp/alpha" "$(repo_field alpha path)"

out="$(spira_repo_lanes alpha)"
want "six-col: plan in result"              "plan"          "$out"
want "six-col: incident in result"          "incident"      "$out"
want "six-col: groom in result"             "groom"         "$out"
want "six-col: maechen-sweep in result"     "maechen-sweep" "$out"
want "six-col: spike in result"             "spike"         "$out"
want "six-col: czar-trigger in result"      "czar-trigger"  "$out"

# ==========================================================================================
echo
echo "criterion 2 — mode names expand to their lane sets"
# ==========================================================================================
cat > "$MAP" <<'ROW'
alpha | /tmp/alpha | push | origin/main | | true | consume
beta  | /tmp/beta  | push | origin/main | | true | develop
gamma | /tmp/gamma | push | origin/main | | true | self
ROW

# consume -> plan only
out_c="$(spira_repo_lanes alpha)"
is   "consume: yields plan"                 "plan"         "$out_c"
nowant "consume: no incident"               "incident"     "$out_c"
nowant "consume: no groom"                  "groom"        "$out_c"

# develop -> plan incident groom spike
out_d="$(spira_repo_lanes beta)"
want   "develop: contains plan"             "plan"         "$out_d"
want   "develop: contains incident"         "incident"     "$out_d"
want   "develop: contains groom"            "groom"        "$out_d"
want   "develop: contains spike"            "spike"        "$out_d"
nowant "develop: no maechen-sweep"          "maechen-sweep" "$out_d"
nowant "develop: no czar-trigger"           "czar-trigger" "$out_d"

# self -> all six labels
out_s="$(spira_repo_lanes gamma)"
want   "self: contains plan"                "plan"         "$out_s"
want   "self: contains incident"            "incident"     "$out_s"
want   "self: contains groom"              "groom"        "$out_s"
want   "self: contains spike"              "spike"        "$out_s"
want   "self: contains maechen-sweep"       "maechen-sweep" "$out_s"
want   "self: contains czar-trigger"        "czar-trigger" "$out_s"

# gate column is unchanged when lanes is present (positive control that gate still parses).
is "gate still reads correctly with lanes present" "true" "$(repo_field alpha gate)"
is "gate still reads correctly for self row"       "true" "$(repo_field gamma gate)"

# ==========================================================================================
echo
echo "criterion 3 — explicit lane list yields exactly those lanes"
# ==========================================================================================
printf 'alpha | /tmp/alpha | push | origin/main | | true | plan,incident\n' > "$MAP"

out_e="$(spira_repo_lanes alpha)"
want   "explicit: contains plan"            "plan"         "$out_e"
want   "explicit: contains incident"        "incident"     "$out_e"
nowant "explicit: no groom"                 "groom"        "$out_e"
nowant "explicit: no spike"                 "spike"        "$out_e"

# Single lane
printf 'alpha | /tmp/alpha | push | origin/main | | true | spike\n' > "$MAP"
out_sp="$(spira_repo_lanes alpha)"
is     "single lane spike: exact"           "spike"        "$out_sp"
nowant "single lane spike: no plan"         "plan"         "$out_sp"

# ==========================================================================================
echo
echo "criterion 4 — unknown mode name is a hard parse error"
# ==========================================================================================
# POSITIVE CONTROL FIRST: a valid mode name succeeds.
printf 'alpha | /tmp/alpha | push | origin/main | | true | develop\n' > "$MAP"
is "positive: valid mode develop succeeds" "0" "$(spira_repo_lanes alpha >/dev/null 2>&1; echo $?)"

# Unknown mode name: exits non-zero AND names the row in stderr.
printf 'alpha | /tmp/alpha | push | origin/main | | true | fullaccess\n' > "$MAP"
rc=0; err=""; err="$(spira_repo_lanes alpha 2>&1)" || rc=$?
is     "unknown mode: exits non-zero"       "1" "$rc"
want   "unknown mode: names the row"        "alpha"         "$err"

# ==========================================================================================
echo
echo "criterion 5 — unknown lane label is a hard parse error"
# ==========================================================================================
# POSITIVE CONTROL: valid lane list succeeds.
printf 'alpha | /tmp/alpha | push | origin/main | | true | plan,groom\n' > "$MAP"
is "positive: valid lane list succeeds" "0" "$(spira_repo_lanes alpha >/dev/null 2>&1; echo $?)"

# Unknown lane label: exits non-zero AND names the row in stderr.
printf 'alpha | /tmp/alpha | push | origin/main | | true | plan,bogus-lane\n' > "$MAP"
rc=0; err=""; err="$(spira_repo_lanes alpha 2>&1)" || rc=$?
is     "unknown lane: exits non-zero"       "1" "$rc"
want   "unknown lane: names the row"        "alpha"         "$err"
want   "unknown lane: mentions the bad label" "bogus-lane"  "$err"

# ==========================================================================================
echo
echo "criterion 7 — empty lanes field admits all lanes"
# ==========================================================================================
# A trailing pipe with nothing after it: `name | path | land | base | format | gate | `
printf 'alpha | /tmp/alpha | push | origin/main | | true | \n' > "$MAP"
out_empty="$(spira_repo_lanes alpha)"
want "empty lanes field: plan in result"          "plan"          "$out_empty"
want "empty lanes field: incident in result"      "incident"      "$out_empty"
want "empty lanes field: groom in result"         "groom"         "$out_empty"
want "empty lanes field: maechen-sweep in result" "maechen-sweep" "$out_empty"
want "empty lanes field: spike in result"         "spike"         "$out_empty"
want "empty lanes field: czar-trigger in result"  "czar-trigger"  "$out_empty"

# ==========================================================================================
echo
echo "gate column unaffected — pipe-containing gates still parse correctly"
# ==========================================================================================
# A gate with || and case-statement pipes, plus a lanes column.
cat > "$MAP" <<'ROW'
alpha | /tmp/alpha | queue | origin/main | | bash a.sh || bash b.sh | develop
ROW
gate_out="$(repo_field alpha gate)"
want   "pipe gate: contains first command"  "bash a.sh"    "$gate_out"
want   "pipe gate: contains second command" "bash b.sh"    "$gate_out"
nowant "pipe gate: lanes not in gate"       "develop"      "$gate_out"
is     "pipe gate: lanes still parsed"      "plan incident groom spike" "$(spira_repo_lanes alpha)"

# ==========================================================================================
echo
echo "backward compat — six-column rows with complex gates parse without lane artifact"
# ==========================================================================================
cat > "$MAP" <<'ROW'
alpha | /tmp/alpha | queue | origin/main | | bash a.sh || bash b.sh
ROW
bc_gate="$(repo_field alpha gate)"
bc_lanes="$(spira_repo_lanes alpha)"
want   "six-col complex gate: first cmd"    "bash a.sh"    "$bc_gate"
want   "six-col complex gate: second cmd"   "bash b.sh"    "$bc_gate"
want   "six-col: plan in lanes"             "plan"         "$bc_lanes"
want   "six-col: incident in lanes"         "incident"     "$bc_lanes"
nowant "six-col: gate fragment not in lanes" "bash"        "$bc_lanes"


# ===========================================================================================
echo
echo "bdq create refuses a repo: label absent from the repo map"
# ===========================================================================================
TMP="$T"
# A minimal repo map with two known entries.
LMAP="$TMP/label-map"
printf 'spira    | /srv/spira     | push | origin/main |  |\n' > "$LMAP"
printf 'widget   | /srv/widget    | pr   | origin/main |  |\n' >> "$LMAP"

# A stub bd that records its arguments and exits 0, so "valid label" calls reach it.
STUB_BD="$TMP/bd"
printf '#!/usr/bin/env bash\nprintf "bd-called\\n"; exit 0\n' > "$STUB_BD"
chmod +x "$STUB_BD"

# Source lib.sh with a fixture environment so it does not read the real database. The
# compiled `bdq` binary each bash -c execs resolves fresh from SPIRA_TOML, never from this
# process's inherited env, so the registered keys go through tl_config and SPIRA_TOML is
# threaded through env -i to reach it.
tl_config SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$LMAP" SPIRA_BD="$STUB_BD"
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

tl_summary
