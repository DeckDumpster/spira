#!/usr/bin/env bash
#
# test-cockpit-czar-triggers.sh — czar_triggers_keys: the four incident:queue-* classes map
# to SP_CZAR_{DEADLOCK,ATTRIB,SORT,STALL}_{FIRED,BY,OUTCOME}.
#
#   ./test-cockpit-czar-triggers.sh
#
# Before this suite, gap #1 of docs/test-plan/cockpit-observability.md: no test-*.sh
# mentioned SP_CZAR or czar_triggers at all. Runs the real `cockpit.sh czar_triggers`
# subcommand (registered in collect.sh's PROBES, not a full `once`) against a stub bd.
#
# A DOCUMENTED DEFECT, NOT FIXED HERE (per this bead's instructions): a bd call that
# produces no parseable JSON — empty stdout, or a schema-mismatch line that bdjson's
# json_only filters to nothing — is indistinguishable downstream from "queried fine, no
# beads for this class", so every key renders `-` (looks like "never fired") instead of
# `?` (honest unknown). Filed as sp-s088v.2.
#
# tier: T1
# covers: spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

# Run just the czar_triggers probe against a given mock bd binary.
run_czar() {
    local bd_path="$1"
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_BD="$bd_path" \
        bash "$HERE/cockpit.sh" czar_triggers 2>/dev/null
}

field() { printf '%s\n' "$1" | grep "^$2=" | sed "s/^$2=//"; }

# =============================================================================
# NEVER FIRED — an honest empty list renders "-", not "?". This is the correct baseline
# the two refusal cases below must be read against.
# =============================================================================
echo "never fired: empty list -> -, not ?"

BD_EMPTY="$TMP/bd-empty"
cat > "$BD_EMPTY" <<'EOF'
#!/usr/bin/env bash
printf '[]'
EOF
chmod +x "$BD_EMPTY"

out="$(run_czar "$BD_EMPTY")"
is "SP_CZAR_DEADLOCK_FIRED=- on empty" "-" "$(field "$out" SP_CZAR_DEADLOCK_FIRED)"
is "SP_CZAR_DEADLOCK_BY=- on empty"    "-" "$(field "$out" SP_CZAR_DEADLOCK_BY)"
is "SP_CZAR_STALL_OUTCOME=- on empty"  "-" "$(field "$out" SP_CZAR_STALL_OUTCOME)"

# =============================================================================
# DOCUMENTED DEFECT: silent refusal (bd exits 0, empty stdout — probe-fault.sh's own
# "failure mode 2") is indistinguishable from the empty-list case above, so it renders
# "-" too. A collector reading "-" cannot tell "no incident has ever fired" from
# "the query never ran" — exactly the confusion law-absence-needs-a-positive-control
# exists to prevent. Wanted: "?". Got, and asserted here as current behaviour: "-".
# =============================================================================
echo ""
echo "DOCUMENTED DEFECT: silent-empty-output refusal reads as never-fired, not ?"

BD_SILENT="$TMP/bd-silent"
cat > "$BD_SILENT" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$BD_SILENT"

out="$(run_czar "$BD_SILENT")"
is "current (defective) behaviour: SP_CZAR_DEADLOCK_FIRED=-" "-" "$(field "$out" SP_CZAR_DEADLOCK_FIRED)"

# =============================================================================
# DOCUMENTED DEFECT, same root cause: a schema-mismatch message never reaches the
# python parser at all — bdjson's json_only only passes lines starting with [ or {, so
# the error text is filtered to nothing before czar_triggers_keys ever sees it. Same
# "-" result as the two cases above, proving this is one defect with two triggers, not
# two separate ones.
# =============================================================================
echo ""
echo "DOCUMENTED DEFECT: a schema-mismatch refusal message reads as never-fired too"

BD_SCHEMA="$TMP/bd-schema"
cat > "$BD_SCHEMA" <<'EOF'
#!/usr/bin/env bash
echo "schema version mismatch: database is at v61, binary knows up to v53"
exit 0
EOF
chmod +x "$BD_SCHEMA"

out="$(run_czar "$BD_SCHEMA")"
is "current (defective) behaviour: SP_CZAR_ATTRIB_FIRED=-" "-" "$(field "$out" SP_CZAR_ATTRIB_FIRED)"

# =============================================================================
# POSITIVE CONTROL for the refusal cases above: a bd that returns JSON the parser
# genuinely cannot load (truncated) still hits the outer except and renders "?" — so
# the parser DOES have a working "?" path, it is only unreachable through bdjson's own
# filtering. Without this the three "-" results above could just mean "?" is dead code.
# =============================================================================
echo ""
echo "positive control: unparseable-but-nonempty JSON still renders ?"

BD_TRUNCATED="$TMP/bd-truncated"
cat > "$BD_TRUNCATED" <<'EOF'
#!/usr/bin/env bash
printf '[{"id":"sp-x1"'
EOF
chmod +x "$BD_TRUNCATED"

out="$(run_czar "$BD_TRUNCATED")"
is "SP_CZAR_DEADLOCK_FIRED=? on truncated JSON" "?" "$(field "$out" SP_CZAR_DEADLOCK_FIRED)"
is "SP_CZAR_STALL_OUTCOME=? on truncated JSON"  "?" "$(field "$out" SP_CZAR_STALL_OUTCOME)"

# =============================================================================
# ONE FIRED, PENDING — an open incident bead in the DEADLOCK class.
# =============================================================================
echo ""
echo "one fired, open -> pending"

BD_PENDING="$TMP/bd-pending"
cat > "$BD_PENDING" <<'EOF'
#!/usr/bin/env bash
printf '[{"id":"sp-p1","status":"open","assignee":"aeon-fake","external_ref":"incident:queue-deadlock-batch-open","created_at":"2026-09-20T10:00:00Z","closed_at":null}]'
EOF
chmod +x "$BD_PENDING"

out="$(run_czar "$BD_PENDING")"
is "SP_CZAR_DEADLOCK_FIRED" "2026-09-20T10:00Z" "$(field "$out" SP_CZAR_DEADLOCK_FIRED)"
is "SP_CZAR_DEADLOCK_BY"    "aeon-fake"          "$(field "$out" SP_CZAR_DEADLOCK_BY)"
is "SP_CZAR_DEADLOCK_OUTCOME" "pending"          "$(field "$out" SP_CZAR_DEADLOCK_OUTCOME)"
# The other three classes were not fired by this fixture.
is "SP_CZAR_ATTRIB_FIRED=- (not this class)" "-" "$(field "$out" SP_CZAR_ATTRIB_FIRED)"

# =============================================================================
# ONE FIRED, CLOSED, NO RECURRENCE — closed with nothing created after it closed.
# =============================================================================
echo ""
echo "one fired, closed, nothing created after -> outcome yes (the fix held)"

BD_HELD="$TMP/bd-held"
cat > "$BD_HELD" <<'EOF'
#!/usr/bin/env bash
printf '[{"id":"sp-h1","status":"closed","assignee":"aeon-fake","external_ref":"incident:queue-sort-failed-ranking","created_at":"2026-09-20T10:00:00Z","closed_at":"2026-09-20T11:00:00Z"}]'
EOF
chmod +x "$BD_HELD"

out="$(run_czar "$BD_HELD")"
is "SP_CZAR_SORT_OUTCOME=yes (fix held)" "yes" "$(field "$out" SP_CZAR_SORT_OUTCOME)"

# =============================================================================
# TWO FIRED, RECURRENCE — both closed, the second created after the first's closed_at:
# the fix did not hold.
# =============================================================================
echo ""
echo "two fired, second created after first closed -> outcome no (recurred)"

BD_RECUR="$TMP/bd-recur"
cat > "$BD_RECUR" <<'EOF'
#!/usr/bin/env bash
printf '[
  {"id":"sp-r1","status":"closed","assignee":"aeon-one","external_ref":"incident:queue-loop-stalled","created_at":"2026-09-20T10:00:00Z","closed_at":"2026-09-20T11:00:00Z"},
  {"id":"sp-r2","status":"closed","assignee":"aeon-two","external_ref":"incident:queue-loop-stalled","created_at":"2026-09-21T09:00:00Z","closed_at":"2026-09-21T10:00:00Z"}
]'
EOF
chmod +x "$BD_RECUR"

out="$(run_czar "$BD_RECUR")"
# The newest bead (sp-r2) reports BY and OUTCOME; sp-r2 was created after sp-r1 closed,
# so the earlier fix is judged not to have held.
is "SP_CZAR_STALL_BY (newest)" "aeon-two" "$(field "$out" SP_CZAR_STALL_BY)"
is "SP_CZAR_STALL_OUTCOME (recurred)" "no" "$(field "$out" SP_CZAR_STALL_OUTCOME)"

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
