#!/usr/bin/env bash
#
# test-config-fence.sh — config-fence.sh refuses a new file outside spira-config/ that
# names spira.toml or repo-map, parses TOML config, or writes the config path; the shipped
# tree, behind its grandfather list, is clean (sp-a8gna, per Ryan 2026-09-28).
#
# covers: spira/config-fence.sh spira/config-fence-allow spira/gate-fences.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-config-fence.sh"

fence() { bash "$HERE/config-fence.sh" "$@" 2>&1; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

new_scratch() {   # new_scratch <name> -> path to a scratch git repo carrying its own copy
                  # of the fence and an empty allow-list
    local d="$TMP/$1"
    mkdir -p "$d/spira"
    git -C "$d" init -q
    cp "$HERE/config-fence.sh" "$d/spira/config-fence.sh"
    : > "$d/spira/config-fence-allow"
    printf '%s' "$d"
}

# ──────────────────────────────────────────────────────────────────────────────
# POSITIVE CONTROL 1 — NAME: a literal "spira.toml" reference outside spira-config/.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "POSITIVE CONTROL — names spira.toml:"

R1="$(new_scratch root-name)"
printf '#!/bin/sh\n# see spira.toml for the defaults\n' > "$R1/spira/planted.sh"

out="$(bash "$R1/spira/config-fence.sh" 2>&1)"; rc=$?
is   "SEEN RED: literal spira.toml reference -> exit 1" "1" "$rc"
want "and it names the file"       "planted.sh" "$out"
want "and it names the kind"       "name"       "$out"

rm "$R1/spira/planted.sh"
out="$(bash "$R1/spira/config-fence.sh" 2>&1)"; rc=$?
is "SEEN GREEN: plant withdrawn -> exit 0" "0" "$rc"

# ──────────────────────────────────────────────────────────────────────────────
# POSITIVE CONTROL 2 — PARSE: a raw TOML/legacy parser run on a file that also names the
# config. A parser used on something else entirely must NOT fire (the false positive
# deps-lint.sh and lockfile-lint.sh would be, if this fence were not gated on co-occurrence).
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "POSITIVE CONTROL — parses TOML config directly:"

R2="$(new_scratch root-parse)"
printf '#!/bin/sh\n# reads spira.toml\npython3 -c "import tomllib"\n' > "$R2/spira/planted.sh"

out="$(bash "$R2/spira/config-fence.sh" 2>&1)"; rc=$?
is   "SEEN RED: tomllib on a file naming spira.toml -> exit 1" "1" "$rc"
want "and it names both kinds" "name,parse" "$out"

rm "$R2/spira/planted.sh"
out="$(bash "$R2/spira/config-fence.sh" 2>&1)"; rc=$?
is "SEEN GREEN: plant withdrawn -> exit 0" "0" "$rc"

printf '#!/bin/sh\npython3 -c "import tomllib"\n' > "$R2/spira/unrelated.sh"
out="$(bash "$R2/spira/config-fence.sh" 2>&1)"; rc=$?
is "tomllib with no config reference is not this defect -> exit 0" "0" "$rc"
rm "$R2/spira/unrelated.sh"

# ──────────────────────────────────────────────────────────────────────────────
# POSITIVE CONTROL 3 — WRITE: a redirect into the config path with no literal filename in
# sight (the variable alone), which the NAME check cannot catch by itself.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "POSITIVE CONTROL — writes the config path via its resolved variable:"

R3="$(new_scratch root-write)"
printf '#!/bin/sh\nprintf "[spira]\\n" > "$SPIRA_TOML"\n' > "$R3/spira/planted.sh"

out="$(bash "$R3/spira/config-fence.sh" 2>&1)"; rc=$?
is   "SEEN RED: redirect into \$SPIRA_TOML -> exit 1" "1" "$rc"
want "and it names the kind" "write" "$out"

rm "$R3/spira/planted.sh"
out="$(bash "$R3/spira/config-fence.sh" 2>&1)"; rc=$?
is "SEEN GREEN: plant withdrawn -> exit 0" "0" "$rc"

# ──────────────────────────────────────────────────────────────────────────────
# GRANDFATHER LIST: an entry in config-fence-allow is skipped even though it violates.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "GRANDFATHERED entries are skipped:"

R4="$(new_scratch root-allow)"
printf '#!/bin/sh\n# spira.toml\n' > "$R4/spira/legacy.sh"
out="$(bash "$R4/spira/config-fence.sh" 2>&1)"; rc=$?
is "ungrandfathered offender still fails -> exit 1" "1" "$rc"

printf 'spira/legacy.sh\n' > "$R4/spira/config-fence-allow"
out="$(bash "$R4/spira/config-fence.sh" 2>&1)"; rc=$?
is "the same file, once listed, is clean -> exit 0" "0" "$rc"

# ──────────────────────────────────────────────────────────────────────────────
# spira-config/ ITSELF IS OUT OF SCOPE — it is the one thing allowed to do all three.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "spira-config/ is exempt by directory, not by the allow-list:"

R5="$(new_scratch root-exempt)"
mkdir -p "$R5/spira-config/src"
printf 'fn main() { let _ = toml::from_str::<toml::Value>("x=1"); }\n' \
    > "$R5/spira-config/src/main.rs"
out="$(bash "$R5/spira/config-fence.sh" 2>&1)"; rc=$?
is "a crate inside spira-config/ is never scanned -> exit 0" "0" "$rc"

# ──────────────────────────────────────────────────────────────────────────────
# THE SHIPPED TREE IS CLEAN, behind its own grandfather list.
#
# In the gate container a worktree's .git FILE resolves to the host, which is not
# bind-mounted inside the container: git exits non-zero and config-fence.sh exits 3. Build a
# portable mirror from the real files and run against that instead (same seam as
# test-payload-argv-lint.sh) — the set of files is identical; only the git plumbing differs.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "shipped tree:"

out="$(fence)"; rc=$?
if [ "$rc" = 3 ]; then
    MIRROR="$TMP/shipped-mirror"
    mkdir -p "$MIRROR"
    SHIPPED="$(cd "$HERE/.." && pwd -P)"
    cp -a "$SHIPPED/." "$MIRROR/"
    rm -rf "$MIRROR/.git"
    git init -q -b main "$MIRROR"
    git -C "$MIRROR" config user.email t@t
    git -C "$MIRROR" config user.name t
    git -C "$MIRROR" add .
    git -C "$MIRROR" commit -q -m mirror >/dev/null
    out="$(bash "$MIRROR/spira/config-fence.sh" 2>&1)"; rc=$?
fi
is "the real tree passes behind config-fence-allow" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"
want "and it reports how many files it checked" "clean" "$out"

tl_summary
