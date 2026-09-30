#!/usr/bin/env bash
# test-attribute.sh — attribute.sh: local, parallel red-suite attribution with bisection.
#
# WHAT THIS PROVES. One round, four members (A, B, P, Q) merged onto a common base,
# three red suites, each shaped differently:
#   S0  red on the base alone (BASE_FAIL)         — never blamed on a member
#   S1  red only when A is merged (single)        — B, P, Q are each innocent alone
#   S2  red only when P AND Q are both merged (bisect) — neither alone reproduces it
# attribute.sh must name S0/BASE, S1/A, S2/{P,Q} — the three method rows the bead asks
# for (base, single, bisect) — from one call, with all four members' passes run in
# parallel.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): B is an innocent decoy
# present in every merge; if attribution ever blamed a member wrongly, B is where a
# broken matcher (e.g. one that just names "the round" rather than reproducing per
# member) would show up first.
#
# host-reason: needs podman (real testenv-batch.sh containers) and git worktrees
# covers: spira/attribute.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

command -v podman >/dev/null 2>&1 || skip "podman not on PATH"

TESTENV=testenv.sh   # the SUT, by name on the suite's PATH (sp-gypjk)
ATTRIBUTE=attribute.sh   # the SUT, by name on the suite's PATH (sp-gypjk)
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-attribute.sh"

# Pre-flight: confirm the image and user systemd are usable (test-testenv-batch.sh's
# own B0 check) before paying for the fixture below.
PRE_CNAME="spira-attr-preflight-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || skip "container did not start"
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    skip "user systemd not available"
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true

# ===========================================================================
# FIXTURE — bare remote + a local clone that plays the role of the repo
# attribute.sh operates on. Suites live only on the base branch (master):
# every merge the attribution builds still carries them, since the exec path
# always runs ${workspace}/spira/<suite> from the tree under test, never from
# a host-side suite directory (testenv-batch.sh:329, default SUITE_DIR).
# ===========================================================================
REMOTE="$TMP/remote"
REPO="$TMP/repo"

git init -q --initial-branch=master "$REMOTE"
git -C "$REMOTE" config user.email "test@spira.local"
git -C "$REMOTE" config user.name "Spira Test"
touch "$REMOTE/placeholder"
git -C "$REMOTE" add placeholder
git -C "$REMOTE" commit -q -m "initial (master)"

git clone -q --local "$REMOTE" "$REPO"
git -C "$REPO" config user.email "test@spira.local"
git -C "$REPO" config user.name "Spira Test"

mkdir -p "$REPO/spira"

cat > "$REPO/spira/test-attr-s0.sh" << 'EOF'
#!/usr/bin/env bash
# S0: red on the base alone, unconditionally — never a member's fault.
printf 'not ok 1 - s0 always red (base_fail fixture)\n'
exit 1
EOF

cat > "$REPO/spira/test-attr-s1.sh" << 'EOF'
#!/usr/bin/env bash
# S1: red only when the marker A's commit adds is present.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$HERE/../single-a" ]; then
    printf 'not ok 1 - s1 red: single-a marker present\n'
    exit 1
fi
printf 'ok 1 - s1 green\n'
exit 0
EOF

cat > "$REPO/spira/test-attr-s2.sh" << 'EOF'
#!/usr/bin/env bash
# S2: red only when BOTH P's and Q's markers are present together.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$HERE/../together-p" ] && [ -f "$HERE/../together-q" ]; then
    printf 'not ok 1 - s2 red: together-p and together-q both present\n'
    exit 1
fi
printf 'ok 1 - s2 green\n'
exit 0
EOF
chmod +x "$REPO"/spira/test-attr-s*.sh
git -C "$REPO" add spira
git -C "$REPO" commit -q -m "add attribution fixture suites"
BASE_SHA="$(git -C "$REPO" rev-parse master)"

# Member branches, each one commit past the base.
git -C "$REPO" branch spira/attr-a master
git -C "$REPO" worktree add -q --detach "$TMP/wa" spira/attr-a
touch "$TMP/wa/single-a"
git -C "$TMP/wa" add single-a
git -C "$TMP/wa" -c user.email=t@t -c user.name=t commit -q -m "A: single-red marker"
git -C "$REPO" branch -f spira/attr-a "$(git -C "$TMP/wa" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$TMP/wa"

git -C "$REPO" branch spira/attr-b master
git -C "$REPO" worktree add -q --detach "$TMP/wb" spira/attr-b
touch "$TMP/wb/decoy-b"
git -C "$TMP/wb" add decoy-b
git -C "$TMP/wb" -c user.email=t@t -c user.name=t commit -q -m "B: innocent decoy"
git -C "$REPO" branch -f spira/attr-b "$(git -C "$TMP/wb" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$TMP/wb"

git -C "$REPO" branch spira/attr-p master
git -C "$REPO" worktree add -q --detach "$TMP/wp" spira/attr-p
touch "$TMP/wp/together-p"
git -C "$TMP/wp" add together-p
git -C "$TMP/wp" -c user.email=t@t -c user.name=t commit -q -m "P: together-only half"
git -C "$REPO" branch -f spira/attr-p "$(git -C "$TMP/wp" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$TMP/wp"

git -C "$REPO" branch spira/attr-q master
git -C "$REPO" worktree add -q --detach "$TMP/wq" spira/attr-q
touch "$TMP/wq/together-q"
git -C "$TMP/wq" add together-q
git -C "$TMP/wq" -c user.email=t@t -c user.name=t commit -q -m "Q: together-only half"
git -C "$REPO" branch -f spira/attr-q "$(git -C "$TMP/wq" rev-parse HEAD)"
git -C "$REPO" worktree remove -f "$TMP/wq"

git -C "$REPO" worktree prune

# ===========================================================================
# RUN — one call, four members, three suites, bounded to 2-way parallelism
# (the fixture has 4 members; the box need not offer 32 cores to prove the
# logic). Order matters for the bisect fixture: P and Q must stay adjacent
# through every halving, which a 4-member list (A,B | P,Q) guarantees.
# ===========================================================================
OUT="$TMP/attr.out"
rc=0
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_ATTRIBUTE_MAXPAR=4 \
SPIRA_RUN="$TMP/spira-run" \
    bash "$ATTRIBUTE" \
        --round attr-round --base "$BASE_SHA" \
        --suites test-attr-s0.sh,test-attr-s1.sh,test-attr-s2.sh \
        --members attr-a,attr-b,attr-p,attr-q \
        --repo "$REPO" \
    > "$OUT" 2> "$TMP/attr.err" || rc=$?

cat "$TMP/attr.err" >&2

s0_line="$(grep '^ATTR test-attr-s0.sh ' "$OUT" || true)"
s1_line="$(grep '^ATTR test-attr-s1.sh ' "$OUT" || true)"
s2_line="$(grep '^ATTR test-attr-s2.sh ' "$OUT" || true)"

want "S0: attributed to BASE"       "owner=BASE"     "$s0_line"
want "S0: method is base"           "method=base"    "$s0_line"
want "S0: carries its own not-ok line" "s0 always red" "$s0_line"

want "S1: attributed to A alone"    "owner=attr-a"   "$s1_line"
want "S1: method is single"         "method=single"  "$s1_line"
nowant "S1: B is not blamed"        "attr-b"         "$s1_line"
nowant "S1: P is not blamed"        "attr-p"         "$s1_line"

want "S2: method is bisect"         "method=bisect"  "$s2_line"
# Exact owner field, not a substring match: proves the bisected set is precisely
# {P, Q} — neither A nor B rode along because they merge cleanly in the same half.
is  "S2: owner is exactly the together pair" "owner=attr-p,attr-q" \
    "$(printf '%s\n' "$s2_line" | grep -o 'owner=[^ ]*')"

eject_a="$(grep '^EJECT attr-a ' "$OUT" || true)"
eject_b="$(grep '^EJECT attr-b ' "$OUT" || true)"
eject_p="$(grep '^EJECT attr-p ' "$OUT" || true)"
eject_q="$(grep '^EJECT attr-q ' "$OUT" || true)"

want "EJECT names A for s1"         "test-attr-s1.sh" "$eject_a"
is   "EJECT: B has nothing to fix"  ""                "$eject_b"
want "EJECT names P for s2"         "test-attr-s2.sh" "$eject_p"
want "EJECT names Q for s2"         "test-attr-s2.sh" "$eject_q"

# Wall time is read back from testenv-batch.sh's own .result seconds field, not
# fabricated — a suite that ran at all reports a non-negative integer.
s1_wall="$(printf '%s\n' "$s1_line" | grep -o 'wall=[0-9]*s' || true)"
want "S1: wall time is a recorded, non-empty figure" "wall=" "$s1_wall"

tl_summary
