#!/usr/bin/env bash
#
# test-land-mode-local.sh — sp-0inic: the repo-map land column accepts queue.forge and
# queue.local; queue.forge behaves exactly as the existing queue alias; queue.local's
# declared ref (local/main) resolves without any caller mistaking the "local" prefix for a
# remote to fetch.
#
# FIVE PROPERTIES FROM THE BEAD'S ACCEPTANCE, each with a positive control so an absence
# means something (law-absence-needs-a-positive-control):
#
#   1. repo_field returns the raw land column untouched — queue.local reads back as
#      queue.local, queue.forge as queue.forge.
#   2. repo_land normalizes the queue.forge alias to queue (so every existing `= queue`
#      dispatcher keeps working against it unchanged) while a bare `queue` row is itself
#      untouched (regression) and queue.local stays distinct from both.
#   3. spira_landref resolves a queue.local row's declared base (local/main) to that LOCAL
#      branch, the same rung-1 declared-base path any other repo's base column already uses.
#   4. ref_remote only answers a prefix that is a REAL configured remote of the given repo —
#      proved both ways: it reports local/main as local when no remote is named "local"
#      (the hazard in the bead), it still resolves origin/main to "origin" when one is
#      configured (regression), and it does NOT hardcode the string "local" as special — a
#      repo that genuinely has a remote called that gets a true answer
#      (law-a-pattern-match-is-not-an-identity-check).
#   5. END TO END: landing.sh's own base-remote fetch (landing.sh ~790, lib.sh's ref_remote)
#      never fetches a remote named "local" for a queue.local row, while an ordinary queue
#      row swept in the same pass still fetches origin — the positive control that proves
#      the git-call log is not simply empty.
#
# tier: T1
# covers: spira/lib.sh landing-pass/*
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Resolved from this tree before any fixture repoints SPIRA_REPO.

echo "test-land-mode-local.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# A MINIMAL ENVIRONMENT, non-default paths throughout, so an assertion that happened to match
# a shipped default would not pass for the wrong reason (law-gates-run-in-a-clean-environment).
mkdir -p "$TMP/run"
export SPIRA_RUN="$TMP/run"
export SPIRA_CONF="$TMP/no-such.conf"
export SPIRA_HOME="$HERE"
export SPIRA_DB="$TMP/no-db"
MAP="$TMP/repo-map"
export SPIRA_REPO_MAP="$MAP"
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$SPIRA_REPO_MAP"

. "$HERE/lib.sh"

# ============================================================================
echo
echo "1/2 — repo_field is raw, repo_land normalizes the queue.forge alias"
# ============================================================================
cat > "$MAP" <<'ROW'
forgerepo | /tmp/nonexistent-forgerepo | queue.forge | origin/main | | |
localrepo | /tmp/nonexistent-localrepo | queue.local | local/main  | | |
plainrepo | /tmp/nonexistent-plainrepo | queue       | origin/main | | |
holdrepo  | /tmp/nonexistent-holdrepo  | hold        |             | | |
ROW

# POSITIVE CONTROL: the rows ARE found at all (a broken map would fail every check below
# for the same reason and look identical to a passing one otherwise).
is "positive control: forgerepo's path resolves" "/tmp/nonexistent-forgerepo" "$(repo_field forgerepo path)"

is "repo_field: queue.forge row reads back raw" "queue.forge" "$(repo_field forgerepo land)"
is "repo_field: queue.local row reads back raw" "queue.local" "$(repo_field localrepo land)"

is "repo_land: queue.forge normalizes to queue (the alias)"       "queue"       "$(repo_land forgerepo)"
is "repo_land: a bare queue row is unchanged (regression)"        "queue"       "$(repo_land plainrepo)"
is "repo_land: queue.local stays distinct from queue"              "queue.local" "$(repo_land localrepo)"
is "repo_land: an unrelated mode is unaffected (regression)"       "hold"        "$(repo_land holdrepo)"

# ============================================================================
echo
echo "3 — spira_landref resolves queue.local's declared base to the local branch"
# ============================================================================
LREPO="$TMP/local-only-repo"
git init -q -b trunk "$LREPO"
git -C "$LREPO" commit -q --allow-empty -m base
git -C "$LREPO" branch local/main trunk

cat > "$MAP" <<EOF
localrepo | $LREPO | queue.local | local/main | | |
EOF

is "spira_landref: queue.local row resolves to local/main" "local/main" "$(spira_landref localrepo)"

# ============================================================================
echo
echo "4 — ref_remote answers from the repo's REAL remotes, never a guess"
# ============================================================================

# POSITIVE CONTROL — the hazard itself: LREPO has no remote at all, so "local/main" split on
# its first slash must NOT be reported as a resolvable remote.
if ref_remote "local/main" "$LREPO" >/dev/null 2>&1; then
    bad "ref_remote: local/main is local when no remote is named that" \
        "got: $(ref_remote "local/main" "$LREPO" 2>/dev/null)"
else
    ok "ref_remote: local/main is local when no remote is named that"
fi

# REGRESSION — an ordinary origin/main ref, with a real origin configured, still resolves
# exactly as it always has.
OREPO="$TMP/origin-repo"; OREMOTE="$TMP/origin-remote.git"
git init -q --bare -b main "$OREMOTE"
git init -q -b main "$OREPO"
git -C "$OREPO" commit -q --allow-empty -m base
git -C "$OREPO" remote add origin "$OREMOTE"
timeout 5 git -C "$OREPO" push -q origin main
timeout 5 git -C "$OREPO" fetch -q origin
is "ref_remote: origin/main still resolves to origin (regression)" "origin" "$(ref_remote "origin/main" "$OREPO")"

# NEGATIVE CONTROL PROVING THE CHECK IS REAL, NOT A HARDCODED "local" DENYLIST: a repo that
# genuinely has a remote named "local" must have "local/main" answered as THAT remote.
git -C "$OREPO" remote add local "$OREMOTE"
is "ref_remote: a genuinely configured 'local' remote is still recognised" \
   "local" "$(ref_remote "local/main" "$OREPO")"

# Backward compatibility: called with no repo argument at all (a caller that predates this
# change), the old unconditional split is preserved.
is "ref_remote: no-repo call keeps the old unconditional split" "origin" "$(ref_remote "origin/main")"

# ============================================================================
echo
echo "5 — END TO END: landing-pass land never fetches a remote named local for a queue.local row,"
echo "    and a queue row swept in the same pass still fetches origin (positive control)"
# ============================================================================
E_RUN="$TMP/e-run"; E_SH="$TMP/e-spira"
mkdir -p "$E_RUN/worktree" "$E_SH"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$E_SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$E_SH/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$E_SH/gate.sh"; chmod +x "$E_SH/gate.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$E_SH/confine.sh"; chmod +x "$E_SH/confine.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$E_SH/skew"; chmod +x "$E_SH/skew"
printf '#!/usr/bin/env bash\nexit 0\n' > "$E_SH/bd-stub.sh"; chmod +x "$E_SH/bd-stub.sh"

# fixture 1: an ordinary queue.forge repo, real origin, one unlanded branch — must fetch origin.
F_REPO="$TMP/forge-fixture"; F_REMOTE="$TMP/forge-remote.git"
git init -q --bare -b main "$F_REMOTE"
git init -q -b main "$F_REPO"
git -C "$F_REPO" commit -q --allow-empty -m base
git -C "$F_REPO" remote add origin "$F_REMOTE"
timeout 5 git -C "$F_REPO" push -q origin main
timeout 5 git -C "$F_REPO" fetch -q origin
git -C "$F_REPO" checkout -qb spira/sp-efrg main
git -C "$F_REPO" commit -q --allow-empty -m "sp-efrg: work"
git -C "$F_REPO" checkout -q main

# fixture 2: the queue.local repo — no remote at all, base is the local branch, one
# unlanded branch — must never attempt to fetch anything named "local".
L_REPO="$TMP/local-fixture"
git init -q -b trunk "$L_REPO"
git -C "$L_REPO" commit -q --allow-empty -m base
git -C "$L_REPO" branch local/main trunk
git -C "$L_REPO" checkout -qb spira/sp-eloc trunk
git -C "$L_REPO" commit -q --allow-empty -m "sp-eloc: work"
git -C "$L_REPO" checkout -q trunk

cat > "$E_SH/repo-map" <<EOF
forgerepo | $F_REPO | queue.forge | origin/main | | |
localrepo | $L_REPO | queue.local | local/main  | | |
EOF

# git shim: logs every invocation's full argument vector, then execs the real git — so
# landing.sh runs exactly as it would in production, and every git call it makes is on record.
REAL_GIT="$(command -v git)"
GIT_LOG="$TMP/git-calls.log"
: > "$GIT_LOG"
GITSHIM="$TMP/gitshim"; mkdir -p "$GITSHIM"
cat > "$GITSHIM/git" <<SHIM
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$GIT_LOG"
exec "$REAL_GIT" "\$@"
SHIM
chmod +x "$GITSHIM/git"

# conf.sh rebuilds PATH from SPIRA_PATH (plus a fixed tail) rather than inheriting the
# caller's — a plain PATH= prefix here would be overwritten before landing.sh's first git
# call, and the shim would silently stop seeing anything.
# SPIRA_PATH/SPIRA_RUN/SPIRA_DB/SPIRA_BD/SPIRA_REPO_MAP are registered keys (per Ryan
# 2026-10-05, ONE SOURCE OF CONFIG): declare via tl_config, not the env prefix below,
# which no process reads them from any more.
tl_config SPIRA_PATH="$GITSHIM" SPIRA_RUN="$E_RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="$E_SH/bd-stub.sh" SPIRA_REPO_MAP="$E_SH/repo-map"
out="$(PATH="$GITSHIM:$PATH" SPIRA_HOME="$E_SH" SPIRA_REPO="$TMP/no-such-home-repo" \
        PATH="$E_SH:$PATH" landing-pass land 2>&1)"

if grep -Eq 'fetch[^0-9a-zA-Z_.-].* local$' "$GIT_LOG"; then
    bad "landing-pass land: no fetch of a remote named local for the queue.local row" \
        "$(grep -E 'fetch' "$GIT_LOG")"
else
    ok "landing-pass land: no fetch of a remote named local for the queue.local row"
fi

if grep -Eq 'fetch[^0-9a-zA-Z_.-].* origin$' "$GIT_LOG"; then
    ok "landing-pass land: positive control — the queue.forge row still fetched origin"
else
    bad "landing-pass land: positive control — the queue.forge row still fetched origin" \
        "$(cat "$GIT_LOG")"
fi

nowant "landing-pass land: no error naming an unresolvable 'local' remote" \
    "does not appear to be a git repository" "$out"

tl_summary
