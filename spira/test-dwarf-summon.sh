#!/usr/bin/env bash
#
# test-dwarf-summon.sh — `dwarf summon <rune> <bead> --by <who> --reason <why>` starts the
#   sentinel's own dwarf on one named bead, outside the caps and the world's state.
#
# THE PROPERTY UNDER TEST. A scheduler that cannot schedule its own fix still needs a build
# path that is a real dwarf (rune prompt, fences, build-dir lease, submit) and not a
# hand-briefed subagent. Every case runs the real `dwarf` binary through the real unit call
# (a stand-in for systemd-run that runs the unit's command), against a real git repository
# and the fixture's bd; the lifecycle machine is testlib's stand-in, which records each Claim
# event's kind verbatim.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control): the ordinary call declines
# under the same halt that the summons walks through.
#
# defect: sp-8mbnup
# tier: T2
# covers: dwarf/src/* runtime/src/units.rs lifecycle/src/bead.rs spira/lib.sh spira/testlib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=120

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
echo "test-dwarf-summon.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-dwarf-summon
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up dwarfsummon || { echo "test-dwarf-summon: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; timeout 5 git clone -q "$ORIGIN" "$REPO" 2>/dev/null
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; timeout 5 git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/runes"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"; tl_config SPIRA_RUN="$SPIRA_RUN"
tl_config SPIRA_RUNES_DIR="$SPIRA_HOME/runes"
tl_repo_map "fixture | $REPO | push | origin/main | |"
cat > "$SPIRA_HOME/runes/builder.rune" <<RUNE
RUNE_NAME=builder
RUNE_WORKING_EXITS="SUBMITTED ask park hold"
RUNE_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
RUNE_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=600
RUNE
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/runes/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export TMP
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":1}\n'
SHIM
chmod +x "$BIN/claude"
dwarf_fixture_agent "$BIN/claude"
tl_config SPIRA_AGENT="$SPIRA_AGENT"
lc_dwarf_mirror "$TMP/lc"; export PATH="$TMP/lc:$BIN:$PATH"
command -v dwarf >/dev/null 2>&1 || { echo "test-dwarf-summon: dwarf is not on PATH" >&2; exit 1; }

# The unit call: what systemd-run does with the argv the dwarf builds — record the unit name,
# apply the --setenv pairs (the suite keeps its own PATH and release), run the command.
cat > "$BIN/fake-systemd-run" <<'CALL'
#!/usr/bin/env bash
for a in "$@"; do
    case "$a" in
        --unit=*) printf '%s\n' "${a#--unit=}" >> "$TMP/units" ;;
        --setenv=PATH=*|--setenv=SPIRA_RELEASE=*) ;;
        --setenv=*) export "${a#--setenv=}" ;;
    esac
done
while [ $# -gt 0 ] && [[ "$1" == --* ]]; do shift; done
"$@"
CALL
chmod +x "$BIN/fake-systemd-run"
export SPIRA_CALL="$BIN/fake-systemd-run"

# slay's own behaviour (keep-work, the branch, the row) is test-slay.sh's; its seam resolves
# spira-lc from the release, out of this suite's reach. This one stops the pid the pidfile
# names, releases the stand-in's row and records the arguments the summons called it with.
cat > "$BIN/slay" <<'SLAY'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMP/slay-args"
id="$2"
for pf in "$SPIRA_RUN"/dwarf-*-"$id".pid; do kill -TERM "$(cat "$pf")" 2>/dev/null; rm -f "$pf"; done
spira-lc holder-dead "$id" slay
SLAY
chmod +x "$BIN/slay"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' "$1" "$_lbl" | testdb_seed
}
summon() {   # summon <bead> <dwarf summon flags...>
    local id="$1"; shift
    rm -rf "$SPIRA_RUN/worktree"
    SPIRA_RELEASE="$(tl_release)" dwarf --home "$SPIRA_HOME" summon builder "$id" "$@" > "$TMP/out" 2>&1
}
ordinary() { rm -rf "$SPIRA_RUN/worktree"; SPIRA_RELEASE="$(tl_release)" dwarf --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1; }
claim_kind() { grep "^$1 " "$SPIRA_RUN/lc-claim-kinds.log" 2>/dev/null | tail -1 | cut -d' ' -f2-; }

# ===========================================================================================
echo
echo "a halted world: the ordinary call claims nothing, the summons claims the named bead"
# ===========================================================================================
testdb_reset; seed sp-sm-halt
: > "$SPIRA_RUN/world.halted"
ordinary
want "positive control: the ordinary call declines when halted" "halted" "$(cat "$TMP/out")"
is   "positive control: and claims nothing" "" "$(claim_kind sp-sm-halt)"

: > "$TMP/units"
summon sp-sm-halt --by concierge --reason "loop fix"
want "the summons claims the named bead with the world halted" "claimed sp-sm-halt" "$(cat "$TMP/out")"
want "the claim event carries the requester" '"by":"concierge"' "$(claim_kind sp-sm-halt)"
want "the claim event carries the reason" '"reason":"loop fix"' "$(claim_kind sp-sm-halt)"
want "no previous holder when none held it" '"previous_holder":null' "$(claim_kind sp-sm-halt)"
rm -f "$SPIRA_RUN/world.halted"

# ===========================================================================================
echo
echo "the summoned unit takes no pool slot"
# ===========================================================================================
unit="$(tail -1 "$TMP/units")"
want "the unit is named as a summoned dwarf of the rune" "spira-dwarf-builder-summoned-" "$unit"
testdb_reset; seed sp-sm-plain
: > "$TMP/units"
ordinary
is "positive control: an ordinary call's claim carries no summons" '"summoned":null' "$(claim_kind sp-sm-plain | grep -o '"summoned":null')"

# ===========================================================================================
echo
echo "a bead a live dwarf holds is taken over: that dwarf stops, its branch tip stays"
# ===========================================================================================
testdb_reset; seed sp-sm-held
timeout 5 git -C "$REPO" fetch -q origin main 2>/dev/null
git -C "$REPO" checkout -q -B spira/sp-sm-held origin/main
printf 'old dwarf work\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-sm-held — the old dwarf's work"
OLDTIP="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" checkout -q main
mkdir -p "$SPIRA_RUN/lc-claim"; printf 'dwarf-old@1.1' > "$SPIRA_RUN/lc-claim/sp-sm-held"
( sleep 300 & echo $! > "$SPIRA_RUN/dwarf-builder-sp-sm-held.pid" )
OLDPID="$(cat "$SPIRA_RUN/dwarf-builder-sp-sm-held.pid")"
is "positive control: the old dwarf is alive" yes "$([ -d "/proc/$OLDPID" ] && echo yes || echo no)"
summon sp-sm-held --by concierge --reason "take it over"
for _ in 1 2 3 4 5; do [ -d "/proc/$OLDPID" ] || break; sleep 1; done
is   "the held dwarf is stopped" no "$([ -d "/proc/$OLDPID" ] && echo yes || echo no)"
is   "its branch tip is still in the branch" 0 "$(git -C "$REPO" merge-base --is-ancestor "$OLDTIP" spira/sp-sm-held; echo $?)"
is   "slay was called with the work kept and the requester in the why" "--bead sp-sm-held --keep-work --why summoned by concierge: take it over" "$(cat "$TMP/slay-args")"
want "the summoned dwarf claimed on that bead" "claimed sp-sm-held" "$(cat "$TMP/out")"
want "the claim names the requester" '"by":"concierge"' "$(claim_kind sp-sm-held)"
want "the claim names the reason" '"reason":"take it over"' "$(claim_kind sp-sm-held)"
want "the claim names the dwarf it replaced" '"previous_holder":"dwarf-old@1.1"' "$(claim_kind sp-sm-held)"

# ===========================================================================================
echo
echo "a summons without its requester and reason is refused before anything runs"
# ===========================================================================================
testdb_reset; seed sp-sm-bare
summon sp-sm-bare
wantrc "no requester: exit 1" 1 $?
is "and nothing was claimed" "" "$(claim_kind sp-sm-bare)"

tl_summary
