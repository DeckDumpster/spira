# landing-fixture.sh — shared by test-landing*.sh: a real bd on a throwaway database, a real
# bare remote and clone, a fixture harness copy with gate.sh/confine.sh/mail/gh stubs, and the
# helpers landing/seed/branch/drop_branch/reap_during_gate/status_of/notes_of. The stub gate is
# also the clock: reaping or reopening from inside it reproduces a mid-pass race exactly.
# Sourced after testlib.sh, with HERE and LAND_SUITE set.
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require "$LAND_SUITE"
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing || {
    printf 'SKIP %s: testdb not available\n' "$LAND_SUITE" >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
timeout 5 git -C "$REPO" push -q origin main
timeout 5 git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"
lc_path_stub "$SH" "$TMP/lcfix"
# A repo's base is no longer derived (no remote-HEAD auto-detection to fall back on) — the
# map row must name it explicitly, or landing-pass can never say a commit "is/is not on
# origin/main" (the "names the branch it lost"/"says the commit is on the base" assertions).
printf '%s | %s | push | origin/main | |\n' "$REPONAME" "$REPO" > "$SH/repomap"

# conf.sh travels with lib.sh — lib.sh refuses to run without it, and a harness that copies
# one and not the other fails at source time, which reads as landing being broken.
# incident.sh IS THE REAL ONE, not a stub. The claim under test is "one incident, however
# many branches and however many passes", and that dedupe is incident.sh's dedupe on the
# external ref — a stub would reproduce the surface remembered here and prove nothing about
# it (law-prefer-the-real-dependency). What landing.sh owns is the KEY it hands over; what
# the intake owns is finding the open bead under it, and both have to hold for the count to
# stay at one.
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
# THE OUTCOME STREAM IS RECORDED, NOT MERELY SWALLOWED, and it is stubbed EXPLICITLY. Without
# a stub, mail would write to $SPIRA_MAIL on the host — a suite writing into the operator's
# live mailbox is not a risk worth leaving to an unset variable
# (law-gates-run-in-a-clean-environment).
stub mail '[ "${1:-}" = send ] || exit 0; printf "%s\n" "$*" >> "$EMITTED"; cat >> "$EMITTED"; printf "\n" >> "$EMITTED"'
export EMITTED="$TMP/events"; : > "$EMITTED"
events() { cat "$RUN/events.log" 2>/dev/null; }

# THE GATE IS ALSO THE REAPER, and that is the whole fixture. A pass holds its branch list
# across the gate, which is the only long call in it, so a branch removed from inside the
# stub is removed at precisely the point the Hel removed the real one — after the list
# was read and before the loop reached the tail of it. Anything staged outside the pass would
# be testing a list that was stale before it was taken, which is a different bug.
#
# It speaks the gate's PROTOCOL, not just its exit status: landing.sh reads the VERDICT line
# for the reason it records.
stub gate.sh '
r="$SPIRA_RUN/reap-during-gate"
if [ -s "$r" ]; then
    while read -r id; do
        [ -n "$id" ] || continue
        git -C "'"$REPO"'" worktree remove --force "'"$RUN"'/worktree/$id" >/dev/null 2>&1
        git -C "'"$REPO"'" branch -D "spira/$id" >/dev/null 2>&1
    done < "$r"
    : > "$r"
fi
# THE TIP AS THE LOOP SAW IT. The gate is the last thing the loop does to a branch, so the
# tip recorded here is the branch as the loop left it — and anything that moves it afterwards
# moved it after the loop had walked past. That is the whole discriminator for the sweep
# below: without it "the branch is on the base" is true whether the loop rebased it or the
# sweep did, and the case would pass against a landing.sh with no sweep in it at all.
mkdir -p "$SPIRA_RUN/tip-at-gate"
git -C "'"$REPO"'" rev-parse "$1" > "$SPIRA_RUN/tip-at-gate/${1//\//-}" 2>/dev/null
# A WITHHELD VERDICT, which is what leaves a branch judged, rebased and standing. The
# repository gate tree being busy is far and away the commonest way a real pass walks past a
# branch it will not reach again, and it is the state every sweep case starts from. The
# status is the protocol constant, not a literal: a fixture asserting against 75 would go on
# passing if landing.sh stopped meaning 75 by it.
w="$SPIRA_RUN/withhold-gate"
if [ -s "$w" ] && grep -qx "$1" "$w"; then
    echo "gate: VERDICT=NO_VERDICT reason=stub-busy branch=$1 repo=${2:-?}" >&2
    exit "${SPIRA_GATE_NOVERDICT:?the gate protocol constant is not in the environment}"
fi
# A DWARF ARRIVING MID-PASS, on the PASS path only — so it lands in the window between the
# loop walking past an earlier branch and the landing that sweeps it, which is the window the
# sweep repeats its own liveness check for. Each line is "<bead> <pid>"; the pid belongs to a
# process the suite started, because dwarf_alive reads /proc and will not be fooled by a
# pidfile naming something that is not a runner.
c="$SPIRA_RUN/claim-during-gate"
if [ -s "$c" ]; then
    while read -r id pid; do
        [ -n "$id" ] || continue
        printf "%s\\n" "$pid" > "$SPIRA_RUN/dwarf-builder-$id.pid"
        printf '%s' "$(( $(date +%s) + 3600 ))" > "$SPIRA_RUN/dwarf-builder-$id.lease"
    done < "$c"
    : > "$c"
fi
echo "gate: VERDICT=PASS reason=${GATE_REASON:-stub} branch=$1 repo=${2:-?}" >&2; exit 0'

# gh IS NEVER REACHED FROM A SUITE. pr_merged sits on the path to a reopen, and left to the
# real binary it would decide a verdict here from whether this box happens to be logged in
# to a forge — the ambient configuration law-gates-run-in-a-clean-environment names. A stub
# that always fails is the honest fixture: this repository lands by push and has no pull
# requests, so "no merged pull request" is the true answer.
stub gh 'exit 1'
# The base-fail tests below overwrite the gate stub; keep a copy so the sweep tests
# can restore it.
cp "$SH/gate.sh" "$TMP/gate-full.sh"

B() { timeout 5 bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

landing() {
    rm -f "$RUN/landing.progress"
    # SPIRA_REPO_MAP EXPLICITLY and nothing else inherited: a pass that falls back reads the
    # repositories the operator has registered and counts THEIR branches, so a suite
    # asserting about one fixture branch would be asserting about a box.
    # AND THE ESCALATION CHANNEL IS PINNED SHUT. Two paths out of this pass reach the
    # operator — the repeated-NO_VERDICT ask and the intake's Fenrir escalation — and both
    # resolve their command from configuration. Left unpinned a suite inherits whatever this
    # box has installed there and puts a fixture's verdict in a real pane
    # (law-gates-run-in-a-clean-environment).
    # SPIRA_HOME_REPO IS PINNED, AND TO A NON-DEFAULT. conf.sh only derives it when it is
    # unset, so a dwarf session that exports it hands this pass the name of a real repository
    # — and the incident below is labelled `repo:<name>`, so the suite would file a fixture's
    # finding against somebody's actual checkout and then assert against whatever leaked.
    # SPIRA_BD IS PINNED TO THE FIXTURE'S EMBEDDED BINARY. conf.sh resolves it from PATH when
    # unset; the test's counter-script section temporarily exports SPIRA_BD and then unsets it,
    # and PATH-resolution after that depends on TESTDB_BIN existing in PATH. Pinning here
    # removes the dependency: the pass always uses the binary testdb_up chose regardless of
    # what the counter-script section left in the environment.
    # SPIRA_ID_PREFIX IS PINNED. A session running on a non-default-prefix harness hands
    # other_beads_on_conflicts a pattern that does not match the sp- ids used in this
    # fixture's commits, so the function returns empty and the parallel-duplicate note
    # omits the bead name it is written to carry (law-gates-run-in-a-clean-environment).
    tl_config SPIRA_BATCHER_ENABLE=1 SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
        SPIRA_REPO_MAP="$SH/repomap" SPIRA_GH="$SH/gh"
    # SPIRA_RUN AS A PLAIN ENV VAR TOO, not only in config: landing-pass's own process reads
    # it through the one source (SPIRA_TOML), but gate.sh is a plain child process that
    # inherits whatever landing-pass's OWN environment held — real.rs's with_env() never
    # clears or re-derives it. Without this, gate.sh's own "$SPIRA_RUN/reap-during-gate" read
    # resolves against an empty SPIRA_RUN, so the reap-during-gate fixture never fires and
    # the branch it names is never actually removed (sp-zzz staying present makes the pass
    # read it as "already landed", not "gone since this pass began").
    SPIRA_HOME="$SH" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        PATH="$SH:$PATH" landing-pass land 2>&1
}
notes_of() { B show "$1" 2>/dev/null; }

seed() {
    testdb_reset
    rm -rf "$LC_FIX/bead" "$LC_FIX/show"; mkdir -p "$LC_FIX/bead" "$LC_FIX/show"
    rm -rf "$RUN/tip-at-gate"; rm -f "$RUN/withhold-gate" "$RUN/claim-during-gate"
    testdb_seed <<'JSONL'
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}
branch() {               # branch <id> [file] [content] — a closed bead with a branch of its own
    local id="$1" f="${2:-$1.txt}" c="${3:-$1}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z","closed_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"%s","depends_on_id":"sp-epic","type":"parent-child"}]}\n' \
        "$id" "$id" "$id" | testdb_seed
    # The builder's hand-off is the bead's lifecycle row, not bd `closed` (sp-mve9i).
    lc_bead SUBMITTED "$id" "$(git -C "$RUN/worktree/$id" rev-parse HEAD 2>/dev/null)" 0
}
drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1
}
reap_during_gate() { printf '%s\n' "$@" > "$RUN/reap-during-gate"; }
