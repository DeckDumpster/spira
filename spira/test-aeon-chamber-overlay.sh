#!/usr/bin/env bash
#
# test-aeon-chamber-overlay.sh — the rendered builder brief says how to verify exactly once,
# and an operator overlay under SPIRA_CHAMBER_OVERLAY wins over the release text.
#
# TWO THINGS UNDER TEST
# ----------------------
# 1. THE GOLDEN CHECK (sp-eibeu). chamber/builder.md's own Tests section says not to run the
#    full landing gate; aeon.sh used to also render a hard-coded GATE_BRIEF into {{GATE}} that
#    said the opposite — "run the landing gate through the runner". Builders followed the
#    later, more detailed instruction and burned their session on it. The brief now has
#    exactly one instruction on how to verify; this pins that there is no second one.
# 2. THE OVERLAY MECHANISM. aeon.sh reads chamber/$FAYTH.md from the release, then applies
#    $SPIRA_CHAMBER_OVERLAY/$FAYTH.md (whole-file replace), $FAYTH.<section>.md (replaces one
#    "## <section>" block) and $FAYTH.append.md (appended). A hand edit to the release
#    checkout is reverted by the next skew refresh with nothing to say so (sp-r1ca2); an
#    overlay survives it and doctor reports it by name.
# 3. {{FINISH}} IS THE RESTRICTED PATH'S (sp-wmcvb). The lifecycle machine is the only mode
#    (sp-v62vn): the brief tells the model it has no bd and finishes through `work submit`,
#    and never renders the retired bd-close Finishing text.
#
# POSITIVE CONTROL: the overlay assertions plant a section and an append file and require
# both to appear in the rendered task file — an overlay directory nothing reads from would
# otherwise look identical to one applied correctly.
#
# defect: sp-eibeu, sp-wmcvb
# tier: T1
# covers: aeon/src/* spira/chamber/builder.md spira/conf.sh doctor/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-chamber-overlay
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonchamberov || { echo "test-aeon-chamber-overlay: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
       GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed
git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
# THE REAL BUILDER PERSONA, not a synthetic stand-in — the golden check means nothing
# against a brief this suite invented.
cp "$HERE/chamber/builder.md" "$HERE/chamber/builder.fayth" "$SPIRA_HOME/chamber/"
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
SPIRA_CHAMBER_OVERLAY="$TMP/overlay-empty"   # deliberately absent for the golden check
# SPIRA_CHAMBER EXPLICITLY: the complete fixture declares a fixed chamber path of its own
# now (no longer derived from SPIRA_HOME when unset), so the fixture persona built above
# under $SPIRA_HOME/chamber would otherwise never be found.
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_REPO_MAP="$SPIRA_REPO_MAP" \
    SPIRA_CHAMBER_OVERLAY="$SPIRA_CHAMBER_OVERLAY" SPIRA_CHAMBER="$SPIRA_HOME/chamber"

command -v aeon >/dev/null 2>&1 \
    || { printf 'test-aeon-chamber-overlay: aeon is not on PATH\n' >&2; exit 1; }

BIN="$TMP/bin"; mkdir -p "$BIN"
export TMP
tl_config SPIRA_AGENT="$BIN/claude"
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$TMP/claude-argv"
cat /dev/stdin        > "$TMP/claude-stdin"
printf '{"type":"result","subtype":"success","duration_ms":1,"turns":1,"num_turns":1,"total_cost_usd":0}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

aeon() { command aeon --home "$SPIRA_HOME" "$@" 2>/dev/null; }

T_LABEL="test-chamber-overlay-bead"
make_bead() {
    local _labels="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},$T_LABEL,repo:fixture"
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" create "chamber overlay test bead" --type task \
        -l "$_labels" 2>/dev/null \
        | grep -oE 'sp-[a-z0-9]+' | head -1
}
# The stub claude never closes its bead, so aeon.sh's cleanup releases it straight back to
# ready — and being the OLDEST ready bead with this label set, it would be reclaimed ahead
# of the next section's freshly created one. Take it out of ready between sections so each
# aeon() call below is provably claiming and rendering the bead this test just made. Its
# closed row is FIXTURE STATE, declared as data (an upsert of the same row), never a bd
# close driven around the lifecycle machine (sp-voip5).
close_bead() { testdb_restate "$1" closed; }

# THE LIFECYCLE MACHINE IS THE ONLY MODE (sp-v62vn), so every aeon run below takes the
# restricted path, exercised end to end against a stub spira-lc (SPIRA_LC_BIN is retired, sp-gypjk — spira-lc is found by name on PATH, which is why this
# stub is dropped ahead of the real binary on PATH rather than pointed to by env var) and a
# stub `work` — this suite is about brief rendering, not lc_claim_bead's own protocol
# correctness (test-aeon-lifecycle-cutover.sh covers that against a real spira-lc server), so
# the stub answers just enough of spira-lc's `show`/`list`/`event` surface for the claim to
# apply. `list` backs Store::lifecycle_snapshot (spira-claim's `select --blockers machine`,
# sp-s9675.2): the real spira-lc always answers with a JSON array, even an empty one, never
# truly-empty stdout — and machine mode fails a candidate closed when its own bead_id is
# absent from that array (DESIGN.md §7 "fails closed on a missing own row"), so the stub
# must report this section's one candidate READY by its real id, not just any id. It reads
# that id straight from bd — the same ready set aeon itself just claimed from — rather than
# a hardcoded one, so it stays correct across every bead each section creates (close_bead
# takes a section's bead out of that set).
# work-env.sh is retired (sp-zpaq0): the aeon binary builds its own restricted environment.
STUB_BIN="$TMP/lc-bin"; mkdir -p "$STUB_BIN"
cat > "$STUB_BIN/spira-lc" <<'STUB'
#!/usr/bin/env bash
case "$1" in
    show) printf '{"bead":{"state":"READY","version":0,"holder":null,"lease_until":null}}\n' ;;
    list)
        ids="$(BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" list --status open --no-assignee \
            --exclude-type epic,event --limit 0 --json 2>/dev/null \
            | grep -oE '"id"[[:space:]]*:[[:space:]]*"sp-[a-zA-Z0-9]+"' | grep -oE 'sp-[a-zA-Z0-9]+')"
        rows=""
        for id in $ids; do
            rows="${rows:+$rows,}{\"bead_id\":\"$id\",\"state\":\"READY\"}"
        done
        printf '[%s]\n' "$rows"
        ;;
    event) exit 0 ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$STUB_BIN/spira-lc"
printf '#!/usr/bin/env bash\nexit 0\n' > "$STUB_BIN/work"
chmod +x "$STUB_BIN/work"
PATH="$STUB_BIN:$PATH"   # the stubs, by name, ahead of the tree's build

# ==========================================================================================
echo "test-aeon-chamber-overlay.sh"
echo
echo "GOLDEN: the rendered builder brief has no gate-run.sh instruction, no overlay present"
# ==========================================================================================
BID_G="$(make_bead)"
[ -n "$BID_G" ] || { printf 'test-aeon-chamber-overlay: could not create golden bead\n' >&2; exit 1; }
aeon builder
# system.md carries the persona identity and its Tests section — everything ahead of the
# <!-- task --> marker in chamber/builder.md; task.md carries the bead body and the rest.
# An aeon reads both, so a rendering assertion checks the pair together.
task_g="$(cat "$SPIRA_RUN/$BID_G.system.md" "$SPIRA_RUN/$BID_G.task.md" 2>/dev/null)"

nowant "SEEN RED CONTROL: no gate-run.sh anywhere in the rendered brief" "gate-run.sh" "$task_g"
nowant "and no stray {{GATE}} placeholder"                                "{{GATE}}"    "$task_g"
want   "the Tests section's own instruction is still there"       "DO NOT run the full landing" "$task_g"
want   "and testenv is the verification path"                     "testenv"                     "$task_g"
close_bead "$BID_G"

# ==========================================================================================
echo
echo "GOLDEN (sp-wmcvb): {{FINISH}} is the restricted work-verb finishing text"
# ==========================================================================================
# The rendered brief tells the model it has no bd and finishes through `work submit`; the
# defect this bead reproduced was builders told the opposite of the path they ran (sp-74gzo).
BID_F1="$(make_bead)"
[ -n "$BID_F1" ] || { printf 'test-aeon-chamber-overlay: could not create finish/restricted bead\n' >&2; exit 1; }
aeon builder
task_f1="$(cat "$SPIRA_RUN/$BID_F1.system.md" "$SPIRA_RUN/$BID_F1.task.md" 2>/dev/null)"
want   "the brief tells the model it has no bd" "You have no \`bd\`" "$task_f1"
want   "and the finishing verb is work submit" "work submit" "$task_f1"
nowant "and the legacy bd-close instruction is absent" "bd -C $SPIRA_DB close" "$task_f1"
# The stub model never calls `work submit`, so the bead is left claimed under the lifecycle
# machine rather than closed through bd — take it out of ready, so it does not linger for a
# later section's make_bead to reclaim.
close_bead "$BID_F1"

# ==========================================================================================
echo
echo "OVERLAY: a section file replaces one ## heading, an append file is appended"
# ==========================================================================================
SPIRA_CHAMBER_OVERLAY="$TMP/overlay"; mkdir -p "$SPIRA_CHAMBER_OVERLAY"
tl_config SPIRA_CHAMBER_OVERLAY="$SPIRA_CHAMBER_OVERLAY"
printf '## Tests\n\nOperator-overlaid Tests section — run only test-fixture-thing.sh.\n' \
    > "$SPIRA_CHAMBER_OVERLAY/builder.Tests.md"
printf 'Operator append: a standing local note for every builder session.\n' \
    > "$SPIRA_CHAMBER_OVERLAY/builder.append.md"

BID_O="$(make_bead)"
[ -n "$BID_O" ] || { printf 'test-aeon-chamber-overlay: could not create overlay bead\n' >&2; exit 1; }
aeon builder
task_o="$(cat "$SPIRA_RUN/$BID_O.system.md" "$SPIRA_RUN/$BID_O.task.md" 2>/dev/null)"

want   "SEEN RED CONTROL: the section overlay text appears"  "run only test-fixture-thing.sh" "$task_o"
nowant "and the release Tests text it replaced is gone"      "DO NOT run the full landing"     "$task_o"
want   "the append overlay text appears"                     "standing local note for every builder" "$task_o"
want   "surrounding release content is untouched"            "## The bead"                     "$task_o"
close_bead "$BID_O"

# ==========================================================================================
echo
echo "OVERLAY: a whole-file replacement wins outright"
# ==========================================================================================
rm -f "$SPIRA_CHAMBER_OVERLAY/builder.Tests.md" "$SPIRA_CHAMBER_OVERLAY/builder.append.md"
printf 'Whole-file operator brief. Nothing from the release is present.\n\n<!-- task -->\n\n## The bead\n{{BEAD}}\n\n## Finishing\nClose {{BEAD_ID}}.\n' \
    > "$SPIRA_CHAMBER_OVERLAY/builder.md"

BID_W="$(make_bead)"
[ -n "$BID_W" ] || { printf 'test-aeon-chamber-overlay: could not create whole-file bead\n' >&2; exit 1; }
aeon builder
task_w="$(cat "$SPIRA_RUN/$BID_W.system.md" "$SPIRA_RUN/$BID_W.task.md" 2>/dev/null)"

want   "SEEN RED CONTROL: the whole-file overlay text appears" "Whole-file operator brief" "$task_w"
nowant "and release-only text is gone"                          "Guardian"                  "$task_w"
rm -f "$SPIRA_CHAMBER_OVERLAY/builder.md"
close_bead "$BID_W"

# ==========================================================================================
echo
echo "OVERLAY: an injected block (PARK/FIXTURE/DEADLINE) is overridable under blocks/"
# ==========================================================================================
mkdir -p "$SPIRA_CHAMBER_OVERLAY/blocks"
printf 'Operator override of the park block.\n' > "$SPIRA_CHAMBER_OVERLAY/blocks/PARK.md"

BID_B="$(make_bead)"
[ -n "$BID_B" ] || { printf 'test-aeon-chamber-overlay: could not create block-overlay bead\n' >&2; exit 1; }
aeon builder
task_b="$(cat "$SPIRA_RUN/$BID_B.task.md" 2>/dev/null)"
want "SEEN RED CONTROL: the block overlay text appears" "Operator override of the park block" "$task_b"
rm -f "$SPIRA_CHAMBER_OVERLAY/blocks/PARK.md"
close_bead "$BID_B"

# ==========================================================================================
echo
echo "doctor reports an active overlay by name and reports none when the directory is empty"
# ==========================================================================================
mkdir -p "$SPIRA_CHAMBER_OVERLAY"
printf 'Operator append.\n' > "$SPIRA_CHAMBER_OVERLAY/builder.append.md"
tl_config SPIRA_CHAMBER_OVERLAY="$SPIRA_CHAMBER_OVERLAY" SPIRA_DB="$SPIRA_DB"
out_active="$(SPIRA_HOME="$SPIRA_HOME" doctor 2>&1)"
want "doctor names the active overlay file" "builder.append.md" "$out_active"
rm -f "$SPIRA_CHAMBER_OVERLAY/builder.append.md"

EMPTY_OVERLAY="$TMP/overlay-none"
tl_config SPIRA_CHAMBER_OVERLAY="$EMPTY_OVERLAY" SPIRA_DB="$SPIRA_DB"
out_none="$(SPIRA_HOME="$SPIRA_HOME" doctor 2>&1)"
want "doctor reports none active when the overlay directory is empty" "none active" "$out_none"

tl_summary
