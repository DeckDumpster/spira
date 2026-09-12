#!/usr/bin/env bash
#
# test-hermetic.sh — the fence that stops a suite reading the real box.
#
#   ./test-hermetic.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# law-gates-run-in-a-clean-environment was law and was re-violated twice in a day, both times
# caught by a human reading a failure rather than by anything mechanical. Re-violation is what
# promotes a rule from prose to a program, and hermetic.sh is that program; this is the proof
# that it can actually refuse.
#
# The failure mode being fenced is nastier than an ordinary red, and it is why the fence has
# to be mechanical. A suite that asks the real systemd whether a unit is running is GREEN for
# as long as the box happens to be in the state its author had — it turns red the day the
# machine changes rather than the day the code does, it then refuses work that is correct,
# and there is nothing in its output pointing anywhere except at the branch. Both failures the
# previous gate produced on the day it was deleted were exactly this.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION AND EVERYTHING AFTER IT IS READ THROUGH IT
# (law-absence-needs-a-positive-control). A fence that reports the shipped tree clean is
# indistinguishable from a fence whose matcher never fires, from a bad path, or from an empty
# glob — and the reassuring reading is the one all three give. So a bare `systemctl` is
# planted, the fence is required to name its file and its line, the plant is withdrawn, and
# only then is the shipped tree's silence worth anything.
#
# BOTH HALVES ARE EXERCISED, because they fail differently. `--scan` is the matcher and is
# fed one planted shape at a time. The whole-tree walk is the part that finds the suites at
# all, so it is run against a scratch repository built for the purpose rather than only
# against the real one — a walker that resolved the wrong root would pass every assertion a
# `--scan` test could make.
#
# THE PROGRAM RUNS IN AN EMPTY ENVIRONMENT. It is a fence over hermeticity; a suite for it
# that let the box decide its verdict would be the joke it is meant to prevent.
#
# defect: sp-4d8v
# covers: spira/hermetic.sh spira/gate-spira.sh spira/gate-fences.sh
# scar: plant() used a hand-written stub list that omitted `literal-lint.sh` when c515322 added
# it as gate-spira.sh's fourth fence — gate refused with "literal-lint.sh is missing" on both
# mini-gate invocations; gate_fence_stubs derives the fence list from gate-spira.sh at call time
# (sp-i7u). Seen red (2 failures): FAIL the installed tree can announce itself: wanted
# [MARKER-INSTALLED] in [gate: spira/literal-lint.sh is missing — refusing to land unchecked]
# shellcheck disable=SC1090
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-hermetic.sh"

[ "${XDG_RUNTIME_DIR:-}" = "/run/user/1001" ] || {
    printf 'SKIP test-hermetic.sh: not running inside testenv container\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
. "$HERE/gate-fences.sh"

fence() {                # fence <args...> -> the shipped fence's output, box excluded
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$HERE/hermetic.sh" "$@" 2>&1
}

# THE WALKER RESOLVES ITS ROOT FROM WHERE IT ITSELF SITS, so pointing the shipped copy at a
# scratch tree is not possible and must not be faked: the control below runs a COPY that lives
# inside the scratch repository, which is the only way its walk is really over that tree.
fence_at() {             # fence_at <scratch-root> [args...] -> that copy's output
    local root="$1"; shift
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/hermetic.sh" "$@" 2>&1
}

# probe <body> -> the hits `--scan` reports for a suite whose body is that text.
#
# The preamble is the shape every real suite has, so a fixture is judged under the same
# scratch-variable closure the shipped suites get rather than in a vacuum.
probe() {
    { printf '#!/usr/bin/env bash\n'
      printf 'TMP="$(mktemp -d)"\n'
      printf 'REPO="$TMP/repo"\n'
      printf '%s\n' "$1"
    } > "$TMP/test-probe.sh"
    fence --scan "$TMP/test-probe.sh"
}

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL. A whole scratch repository, because the walker's job is finding the
# suites and no amount of `--scan` testing exercises that.
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/hermetic.sh" "$ROOT/spira/hermetic.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

out="$(fence_at "$ROOT")"; rc=$?
is   "an empty tree refuses to report clean"     "3" "$rc"
want "and says why"                              "refusing to report clean" "$out"

# The plant is a bare `systemctl` on a known line: line 1 is the shebang, so it is line 2.
printf '#!/usr/bin/env bash\nsystemctl --user is-active spira-sentinel.timer\n' \
    > "$ROOT/spira/test-planted.sh"
out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: a bare systemctl in a suite is refused" "1" "$rc"
want "and it names the suite and the line"              "spira/test-planted.sh:2" "$out"
want "and it names the command"                         "systemctl" "$out"
want "and it says how to stand the call down"           "hermetic-ok" "$out"

# THE ONE EXEMPTION IS EXACT AND SCOPED. This suite is skipped because its content is the
# offender list, and a `case` pattern that leaked would silently exempt every neighbour whose
# name began the same way — which is a hole nobody would ever see, since the fence would go on
# printing "clean".
printf '#!/usr/bin/env bash\nsystemctl --user is-active foo\n' \
    > "$ROOT/spira/test-hermetic-extra.sh"
out="$(fence_at "$ROOT")"
want "the exemption does not extend to a neighbouring name" "test-hermetic-extra.sh:2" "$out"
rm -f "$ROOT/spira/test-hermetic-extra.sh"

# And the suite that IS exempt is passed over rather than merely happening to be clean.
cp "$HERE/test-hermetic.sh" "$ROOT/spira/test-hermetic.sh"
out="$(fence_at "$ROOT")"
nowant "while the suite whose content is the offender list is skipped" "test-hermetic.sh:" "$out"
rm -f "$ROOT/spira/test-hermetic.sh"

# Withdrawn, and only now is a green reading evidence of anything.
rm -f "$ROOT/spira/test-planted.sh"
printf '#!/usr/bin/env bash\nTMP="$(mktemp -d)"\ngit -C "$TMP" status\n' > "$ROOT/spira/test-clean.sh"
out="$(fence_at "$ROOT")"; rc=$?
is   "GREEN AFTER: the same tree without the plant passes" "0" "$rc"
want "and says how many suites it looked at"               "1 suite(s)" "$out"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, this now means something.
# ---------------------------------------------------------------------------------------
out="$(fence)"; rc=$?
is   "every shipped suite is hermetic" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

# ---------------------------------------------------------------------------------------
# THE UNROUTABLE LIST — no argument makes any of these local, so naming one is always a hit.
# ---------------------------------------------------------------------------------------
for c in systemctl systemd-run journalctl loginctl gh gt crontab; do
    want "$c is refused wherever it appears" "$c" "$(probe "$c --user foo")"
done
want "a wrapper does not hide it"  "systemctl" "$(probe 'out="$(env -i PATH=/x systemctl show u)"')"
want "nor does a timeout"          "gh"        "$(probe 'timeout 30 gh pr list')"
want "nor a compound command"      "systemctl" "$(probe 'if systemctl --user is-enabled u; then :; fi')"
want "nor a loop body"             "systemctl" "$(probe 'for u in a b; do systemctl restart "$u"; done')"

# ---------------------------------------------------------------------------------------
# THE DIRECTABLE LIST — hermetic when pointed somewhere disposable, and only then.
# ---------------------------------------------------------------------------------------
want   "bd with no database is refused"   "bd"  "$(probe 'bd list --json')"
want   "git with no repository is refused" "git" "$(probe 'git ls-files')"
want   "dolt with no database is refused" "dolt" "$(probe "dolt sql -q 'select 1'")"
is     "git in the suite's own scratch is fine" "" "$(probe 'git -C "$REPO" status')"
is     "and so is a path derived from it"       "" "$(probe 'WT="$REPO/w"
git -C "$WT" commit -qm x')"
is     "and a bare path under it"               "" "$(probe 'bd -C "$TMP/db" ready')"

# THE DISCRIMINATING PAIR for SPIRA_DB. The variable is scratch only in a suite that calls
# `testdb_up`; the same line without it is reading whatever database the operator configured,
# which is the violation wearing the safe line's clothes.
is   "bd -C \$SPIRA_DB is fine under a checked fixture" "" "$(probe 'testdb_up probe || exit 1
bd -C "$SPIRA_DB" ready')"
want "and refused without one"             "bd"  "$(probe 'bd -C "$SPIRA_DB" ready')"

# UNCHECKED testdb_up. testdb_up unsets SPIRA_DB on failure, so an unchecked call leaves
# the suite to die loudly rather than writing to production — but hermetic.sh can see the
# exit status was discarded and says so directly, which is a static property it can enforce.
# The discriminating pair: the same call with `||` must be clean.
want "unchecked testdb_up is refused"     "testdb_up" "$(probe 'testdb_up probe')"
want "unchecked testdb_up with redirect is refused" "testdb_up" \
     "$(probe 'testdb_up probe >/dev/null')"
is   "testdb_up checked with || is fine"  "" "$(probe 'testdb_up probe || exit 1
bd -C "$SPIRA_DB" ready')"
is   "testdb_up in if condition is fine"  "" "$(probe 'if testdb_up probe; then
bd -C "$SPIRA_DB" ready
fi')"

# `$HERE` is the real checkout, not scratch, and a suite that reads it reads the box.
want "the checkout the suite lives in is not scratch" "git" "$(probe 'git -C "$HERE" log --oneline -1')"

# ---------------------------------------------------------------------------------------
# WHAT IS NOT CODE. Every one of these carries the words being hunted, and a matcher that
# could not tell them apart would be reworded around rather than obeyed.
# ---------------------------------------------------------------------------------------
is "a comment line is not a call"      "" "$(probe '# systemctl --user restart everything')"
is "a trailing comment is not a call"  "" "$(probe 'f() {   # every direct `bd ready` outside this helper')"
is "a word inside a string is not one" "" "$(probe 'is "systemctl was asked" "1" "$rc"')"
is "nor one being printed"             "" "$(probe "printf 'systemctl %s\\\\n' \"\$*\" >> \"\$LOG\"")"
is "a heredoc body is a file, not this suite" "" "$(probe 'cat > "$TMP/shim" <<'"'"'S'"'"'
systemctl "$@"
git commit -qm x
S')"
is "a herestring does not open one"    "" "$(probe 'x="$(cat <<< "hello")"')"

# ---------------------------------------------------------------------------------------
# THE ESCAPE. A suite that genuinely must reach the box says so where the call is, and the
# claim is greppable. The pair matters: the same line without the marker must be red, or the
# marker is not what is standing the fence down.
# ---------------------------------------------------------------------------------------
is   "hermetic-ok on the line stands the fence down" "" \
     "$(probe 'systemctl --user daemon-reload   # hermetic-ok: this one has to')"
is   "and on the line directly above it"             "" \
     "$(probe '# hermetic-ok: the call below has to reach the box
journalctl --user -u spira.service')"
want "while the same call without it is refused" "systemctl" \
     "$(probe 'systemctl --user daemon-reload')"

# ---------------------------------------------------------------------------------------
# IT RUNS IN THE GATE. A fence nothing invokes is a file, and this is the one property no
# amount of testing the matcher can establish.
# ---------------------------------------------------------------------------------------
want "the gate names this fence" "spira/hermetic.sh" "$(cat "$HERE/gate-spira.sh")"
is   "and it is executable"      "0" "$([ -x "$HERE/hermetic.sh" ]; echo $?)"

# ---------------------------------------------------------------------------------------
# AND IT JUDGES THE TREE UNDER TRIAL, NOT THE INSTALLED COPY. The gate extracts a branch to a
# scratch worktree and runs this command there, while also exporting SPIRA_GATE_REPO — which
# is the INSTALLED checkout. gate-spira.sh used to cd to it, stepping out of the tree it was
# judging and running the code already in force. Every branch then passed on the installed
# copy's green, which is the one failure a gate must not have: it passes work it never looked
# at, and the landing pass acts on the pass. It hid until a branch ADDED a file, because a
# branch that only edits existing ones is invisible to it.
#
# DRIVEN, NOT GREPPED. The fences are the first thing the gate does and they exit on the
# spot, so each side answers in milliseconds without reaching the suites — and a marker
# printed by one tree's fence is proof of which tree it stood in.
# ---------------------------------------------------------------------------------------
plant() {                # plant <root> <marker> — a tree whose fences announce which one it is
    mkdir -p "$1/spira"
    cp "$HERE/gate-spira.sh" "$HERE/suite-covers.sh" "$1/spira/"
    # EVERY fence the gate insists on, derived from gate-spira.sh so adding a fence
    # here requires no edit in this suite (gate_fence_stubs fails closed if the list
    # is empty — law-absence-needs-a-positive-control).
    gate_fence_stubs "$HERE/gate-spira.sh" "$1/spira"
    # inventory.sh is the one that speaks: it runs after exclude.sh and before the suites, and
    # the gate prints its output and stops there. Override the stub gate_fence_stubs wrote.
    printf '#!/usr/bin/env bash\necho %s\nexit 1\n' "$2" > "$1/spira/inventory.sh"
}
plant "$TMP/under-trial" MARKER-UNDER-TRIAL
plant "$TMP/installed"   MARKER-INSTALLED

# THE CONTROL FIRST: the installed side must be able to produce its own marker, or "we did not
# see it" is a claim about a fixture that could never have spoken
# (law-absence-needs-a-positive-control).
want "the installed tree can announce itself" "MARKER-INSTALLED" \
     "$(cd "$TMP/installed" && env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
        bash spira/gate-spira.sh 2>&1)"

out="$(cd "$TMP/under-trial" && env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
       SPIRA_GATE_REPO="$TMP/installed" bash spira/gate-spira.sh 2>&1)"
want   "the gate judges the tree it was run in"      "MARKER-UNDER-TRIAL" "$out"
nowant "and not the checkout SPIRA_GATE_REPO names"  "MARKER-INSTALLED"   "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
