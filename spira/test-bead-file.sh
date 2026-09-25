#!/usr/bin/env bash
#
# test-bead-file.sh — bead.sh file: kind routing, the lane-admission guard, and the
#   filing-time refusals (D1: merges test-bead-file-kinds.sh + test-bead-lane-guard.sh,
#   which shared an identical fixture; adds the G12 refusal rows and the unmapped-repo
#   refusal now that sp-pnhtt's repo-map check is reachable).
#
# law-a-lane-runs-only-where-the-repo-admits-it: a persona may cut work only against a
# repository whose effective lanes admit it. bead.sh file is the first enforcement point.
# Each refusal case is preceded by a positive control on the same path
# (law-absence-needs-a-positive-control).
#
# covers: spira/bead.sh spira/lib.sh spira/schema.sh UC-dispatch-01 UC-dispatch-02 UC-dispatch-03
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"
mkdir -p "$T/chamber" "$T/run"

# ---------------------------------------------------------------------------
# FAKE FAYTHS. builder (plan lane), maechen (maechen-sweep lane), empty (no labels).
# ---------------------------------------------------------------------------
cat > "$T/chamber/builder.fayth" <<'FAYTH'
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan}"
FAYTH_TOOLS="Bash,Edit,Write"
FAYTH_MODEL=claude-sonnet-5
FAYTH_MAX_CONCURRENT=4
FAYTH
cat > "$T/chamber/maechen.fayth" <<'FAYTH'
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_MAECHEN_LABEL:-maechen-sweep}"
FAYTH_TOOLS="Bash"
FAYTH_MODEL=claude-haiku-4-5-20251001
FAYTH_MAX_CONCURRENT=1
FAYTH
cat > "$T/chamber/empty.fayth" <<'FAYTH'
FAYTH_LABELS=""
FAYTH_TOOLS="Bash"
FAYTH_MODEL=claude-haiku-4-5-20251001
FAYTH_MAX_CONCURRENT=1
FAYTH

# ---------------------------------------------------------------------------
# REPO MAP. testrepo is plan-only (default); dev-repo is develop (no maechen-sweep);
# self-repo admits every lane.
# ---------------------------------------------------------------------------
cat > "$T/repo-map" <<'MAP'
testrepo  | /tmp/test | push | origin/main | | true | plan
dev-repo  | /tmp/dev  | push | origin/main | | true | develop
self-repo | /tmp/self | push | origin/main | | true | self
MAP

# ---------------------------------------------------------------------------
# STUB BD. Records argv for create calls; exits 0.
# ---------------------------------------------------------------------------
STUB_BD="$T/stub-bd"
BD_LOG="$T/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    create) printf '%s\n' "$*" >> "$BD_LOG_PATH"; printf 'sp-test\n'; exit 0 ;;
    *)      exit 0 ;;
esac
STUB
chmod +x "$STUB_BD"

run_bead() {
    : > "$BD_LOG"
    env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_DB="$T/db" \
        SPIRA_HOME="$T" \
        SPIRA_REPO="$T" \
        SPIRA_REPO_MAP="$T/repo-map" \
        SPIRA_MAECHEN_LABEL="maechen-sweep" \
        SPIRA_GROOMER_LABEL="groom" \
        SPIRA_SPIKE_LABEL="spike" \
        SPIRA_CZAR_LABEL="czar-trigger" \
        SPIRA_INCIDENT_LABEL="incident" \
        SPIRA_PLAN_LABEL="plan" \
        SPIRA_SCOPE_LABEL="testscope" \
        SPIRA_BEAD_LANE_OVERRIDE="${SPIRA_BEAD_LANE_OVERRIDE:-}" \
        bash "$HERE/bead.sh" file "$@" 2>&1
}

# =====================================================================================
# UC-dispatch-01/02: kind routing
# =====================================================================================
out="$(run_bead "work bead" --for builder --repo testrepo)"; rc=$?
is   "work: exits 0"          "0"       "$rc"
want "work: bd create called" "create"  "$(cat "$BD_LOG")"

out="$(run_bead "something happened" --kind event)"; rc=$?
is     "event: exits 0"                    "0"             "$rc"
want   "event: bd create called"           "create"        "$(cat "$BD_LOG")"
want   "event: --type event passed"        "--type event"  "$(cat "$BD_LOG")"
want   "event: scope label present"        "testscope"     "$(cat "$BD_LOG")"
nowant "event: no partition label plan"    " plan"         "$(cat "$BD_LOG")"
nowant "event: no partition label incident" "incident"     "$(cat "$BD_LOG")"

out="$(run_bead "something is wrong" --kind escalation)"; rc=$?
is   "escalation: exits 0"           "0"                    "$rc"
want "escalation: --type escalation" "--type escalation"    "$(cat "$BD_LOG")"
want "escalation: scope label"       "testscope"            "$(cat "$BD_LOG")"

out="$(run_bead "an idea" --kind proposal)"; rc=$?
is   "proposal: exits 0"         "0"                "$rc"
want "proposal: --type proposal" "--type proposal"  "$(cat "$BD_LOG")"

out="$(run_bead "a thing we learned" --kind insight)"; rc=$?
is     "insight: exits 0"                 "0"                "$rc"
want   "insight: bd create called"        "create"           "$(cat "$BD_LOG")"
want   "insight: --type chore"            "--type chore"     "$(cat "$BD_LOG")"
want   "insight: --status closed"         "--status closed"  "$(cat "$BD_LOG")"
want   "insight: -p 4 (default priority)" "-p 4"             "$(cat "$BD_LOG")"
want   "insight: insight label present"   "insight"          "$(cat "$BD_LOG")"
nowant "insight: no partition label plan" " plan"            "$(cat "$BD_LOG")"

out="$(run_bead "a thing we learned" --kind insight -p 2)"; rc=$?
is     "insight p2: exits 0"     "0"    "$rc"
want   "insight p2: -p 2 passed" "-p 2" "$(cat "$BD_LOG")"
nowant "insight p2: not -p 4"    "-p 4" "$(cat "$BD_LOG")"

out="$(run_bead "an event" --kind event --repo testrepo)"; rc=$?
is   "event+repo: exits 0"               "0"             "$rc"
want "event+repo: repo:testrepo present" "repo:testrepo" "$(cat "$BD_LOG")"

out="$(run_bead "an event" --kind event)"; rc=$?
is     "event no-repo: exits 0"          "0"      "$rc"
nowant "event no-repo: no repo: label"   "repo:"  "$(cat "$BD_LOG")"

out="$(run_bead "await something" --kind gate)"; rc=$?
is     "gate: exits non-zero"         "2"        "$rc"
want   "gate: names bd gate"          "bd gate"  "$out"
nowant "gate: bd create NOT called"   "create"   "$(cat "$BD_LOG")"

out="$(run_bead "a thing" --kind nosuchkind)"; rc=$?
is     "unknown kind: exits non-zero"          "2"           "$rc"
want   "unknown kind: names the kind"          "nosuchkind"  "$out"
nowant "unknown kind: bd create NOT called"    "create"      "$(cat "$BD_LOG")"

out="$(run_bead "a thing" --kind event --for builder)"; rc=$?
is     "kind+for: exits non-zero"         "2"                   "$rc"
want   "kind+for: names exclusion"        "mutually exclusive"  "$out"
nowant "kind+for: bd create NOT called"   "create"              "$(cat "$BD_LOG")"

out="$(run_bead "work bead" --repo testrepo)"; rc=$?
is   "no-for: exits non-zero" "2"     "$rc"
want "no-for: mentions --for" "--for" "$out"

# =====================================================================================
# G12: refusals with no prior coverage
# =====================================================================================
out="$(run_bead "work bead" --for builder)"; rc=$?
is   "no-repo: exits non-zero"  "2"     "$rc"
want "no-repo: mentions --repo" "--repo" "$out"

out="$(run_bead "work bead" --for nosuchpersona --repo testrepo)"; rc=$?
is     "unknown persona: exits non-zero"        "2"                "$rc"
want   "unknown persona: names the persona"     "nosuchpersona"    "$out"
nowant "unknown persona: bd create NOT called"  "create"           "$(cat "$BD_LOG")"

out="$(run_bead "work bead" --for empty --repo testrepo)"; rc=$?
is     "label-less persona: exits non-zero"       "2"      "$rc"
want   "label-less persona: names the persona"    "empty"  "$out"
nowant "label-less persona: bd create NOT called" "create" "$(cat "$BD_LOG")"

# =====================================================================================
# UC-dispatch-03 / G11: unmapped repo (sp-pnhtt) — refused before bd is ever called,
# for every kind, not only work
# =====================================================================================
out="$(run_bead "work bead" --for builder --repo notarepo)"; rc=$?
is     "unmapped repo (work): exits non-zero"       "2"          "$rc"
want   "unmapped repo (work): names the repo"       "notarepo"   "$out"
want   "unmapped repo (work): names valid keys"     "testrepo"   "$out"
nowant "unmapped repo (work): bd create NOT called" "create"     "$(cat "$BD_LOG")"

out="$(run_bead "an event" --kind event --repo notarepo)"; rc=$?
is     "unmapped repo (event): exits non-zero"       "2"        "$rc"
want   "unmapped repo (event): names the repo"       "notarepo" "$out"
nowant "unmapped repo (event): bd create NOT called" "create"   "$(cat "$BD_LOG")"

# =====================================================================================
# D1/UC-dispatch-03: lane admission guard
# =====================================================================================
out="$(run_bead "maechen sweep pass" --for maechen --repo self-repo)"; rc=$?
is   "self-repo: exits 0"          "0"      "$rc"
want "self-repo: bd create called" "create" "$(cat "$BD_LOG")"

# POSITIVE CONTROL proved above: the check can pass. Now prove it refuses.
out="$(run_bead "maechen sweep pass" --for maechen --repo dev-repo)"; rc=$?
is     "dev-repo: exits non-zero"             "2"                        "$rc"
want   "dev-repo: names repo in message"      "dev-repo"                 "$out"
want   "dev-repo: names lane in message"      "maechen-sweep"            "$out"
want   "dev-repo: names refusing declaration" "repo-map"                 "$out"
want   "dev-repo: names override variable"    "SPIRA_BEAD_LANE_OVERRIDE" "$out"
nowant "dev-repo: bd create NOT called"       "create"                   "$(cat "$BD_LOG")"

out="$(SPIRA_BEAD_LANE_OVERRIDE=1 run_bead "maechen sweep pass" --for maechen --repo dev-repo)"; rc=$?
is   "override: exits 0"          "0"      "$rc"
want "override: bd create called" "create" "$(cat "$BD_LOG")"

# plan is in every lane set; builder should never be refused regardless of repo mode.
out="$(run_bead "implement feature X" --for builder --repo dev-repo)"; rc=$?
is   "builder plan: exits 0"          "0"      "$rc"
want "builder plan: bd create called" "create" "$(cat "$BD_LOG")"

tl_summary
