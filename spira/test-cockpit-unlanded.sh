#!/usr/bin/env bash
#
# test-cockpit-unlanded.sh — closed beads: landed vs. anomaly (branch, no landstate); the
# worked/landed row is 24h-scoped; landing detection uses subject forms only
# (law-aeon-commits-name-their-bead).
#
#   ./test-cockpit-unlanded.sh
#
# FIVE BEADS, FOUR SHAPES:
#   sp-aaa  closed, branch exists, no landstate   → SP_UNLANDED_N anomaly
#   sp-bbb  closed, "spira: land sp-bbb" on base  → landed (SP_LANDED)
#   sp-ccc  closed, no branch, a commit BODY (not subject) mentions it → neither anomaly
#           nor landed (absorbed from test-cockpit-queue-section.sh case (b); cluster 2,
#           coverage row 11 — the anomaly/body-mention rows in the queue suites moved here)
#   sp-ddd  closed, branch in master-based repo, no landstate → SP_UNLANDED_N anomaly
#   sp-oldd closed 48h ago with a landing-subject commit, but outside the 24h window →
#           excluded from SP_CLOSED and SP_LANDED entirely (absorbed from
#           test-cockpit-landed.sh; coverage row 10)
#
# defect: sp-a5ga sp-884p
# covers: spira/cockpit.sh cockpit/health.sh
# scar: closed beads with a branch but no landstate were invisible; UNLND is now QUEUE. The
#       worked/landed row once counted all-time under a 24h header, producing nonsense
#       against scoped counts, and a body mention was once enough to mark a bead landed.
#
# SPIRA_BDJSON_FIXTURE, NOT A REAL STORE. This classification lives in the same unsent_keys
# function as the unsent backlog (test-cockpit-unsent.sh); its only bd read is a `list
# --status closed` folded into that function. The query shape itself is covered once in
# test-cockpit-bd-contract.sh, against real bd. Git stays real — it is cheap, and the
# landing detection this suite exists for is exactly a question about commit history.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
BASE_PATH="$PATH"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

ALPHA="$TMP/alpha"; BETA="$TMP/beta"

# Alpha: main-based. sp-bbb is landed via a "spira: land sp-bbb" subject.
# sp-aaa has a branch but no landstate (anomaly). sp-ccc has no branch, and is later
# mentioned only in a commit BODY, never a landing subject.
git init -q -b main "$ALPHA"
git -C "$ALPHA" commit --allow-empty -m "init" -q
git -C "$ALPHA" commit --allow-empty -m "spira: land sp-bbb" -q
git -C "$ALPHA" commit --allow-empty -m "spira: land sp-oldd" -q
git -C "$ALPHA" checkout -q -b spira/sp-aaa
git -C "$ALPHA" commit --allow-empty -m "sp-aaa work" -q
git -C "$ALPHA" checkout -q main
# A commit whose body mentions sp-ccc but subject is not a landing form.
git -C "$ALPHA" commit --allow-empty -F - -q <<'EOF'
other work: fixes an unrelated issue

This commit mentions sp-ccc in the body but is not a landing commit.
EOF

# Beta: master-based. sp-ddd has a branch but no landstate (anomaly).
git init -q -b master "$BETA"
git -C "$BETA" commit --allow-empty -m "init" -q
git -C "$BETA" checkout -q -b spira/sp-ddd
git -C "$BETA" commit --allow-empty -m "sp-ddd work" -q
git -C "$BETA" checkout -q master

MAP="$TMP/repo-map"
cat > "$MAP" <<MAP
# name | path | land | base | format | gate
alpha | $ALPHA | push | main | |
beta  | $BETA  | push | master | |
MAP

RUN="$TMP/run"; mkdir -p "$RUN"
SPIRA_SCOPE_LABEL=alpha

for b in sp-aaa sp-bbb sp-ccc sp-ddd sp-oldd; do
    printf '{"type":"system","subtype":"init"}\n' > "$RUN/$b.log"
done

AGO20="$(date -u -d '20 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-20M +%Y-%m-%dT%H:%M:%SZ)"
AGO15="$(date -u -d '15 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-15M +%Y-%m-%dT%H:%M:%SZ)"
AGO10="$(date -u -d '10 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-10M +%Y-%m-%dT%H:%M:%SZ)"
AGO5="$(date -u -d '5 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-5M +%Y-%m-%dT%H:%M:%SZ)"
AGO48H="$(date -u -d '48 hours ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-48H +%Y-%m-%dT%H:%M:%SZ)"

cat > "$TMP/beads.json" <<JSON
[
  {"id":"sp-aaa","title":"work not landed","status":"closed","priority":1,"closed_at":"$AGO5","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]},
  {"id":"sp-bbb","title":"work already landed","status":"closed","priority":2,"closed_at":"$AGO10","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]},
  {"id":"sp-ccc","title":"work mentioned only in a body","status":"closed","priority":1,"closed_at":"$AGO15","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]},
  {"id":"sp-ddd","title":"work in master repo","status":"closed","priority":0,"closed_at":"$AGO20","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:beta"]},
  {"id":"sp-oldd","title":"landed but outside the 24h window","status":"closed","priority":1,"closed_at":"$AGO48H","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
]
JSON

# Run cockpit.sh unsent — the probe's own subcommand (unsent_keys carries this
# classification too), with bd reads answered from a canned-JSON fixture rather than a
# live store.
unlanded() {    # unlanded <fixture-file>
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
        SPIRA_REPO="$ALPHA" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        SPIRA_BDJSON_FIXTURE="$1" \
        bash "$HERE/cockpit.sh" unsent 2>/dev/null
}

out="$(unlanded "$TMP/beads.json")"
val() { printf '%s' "$out" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

echo "--- counts ---"
is "SP_CLOSED is 24h-scoped (sp-oldd excluded)" "4" "$(val SP_CLOSED)"
is "SP_LANDED is 1 (sp-bbb via 'spira: land' subject)" "1" "$(val SP_LANDED)"
is "SP_UNLANDED_N is 2 (sp-aaa and sp-ddd have branch, no landstate)" "2" "$(val SP_UNLANDED_N)"
# Both sp-aaa (5 min ago) and sp-ddd (20 min ago) are within the default 90-min cert window.
is "SP_CERT_N is 2 (both within cert window)" "2" "$(val SP_CERT_N)"
is "SP_STRANDED_N is 0 (none older than cert window)" "0" "$(val SP_STRANDED_N)"

echo "--- landed detection: subject form only ---"
# Criterion: a commit BODY mentioning an id does NOT count; only landing subjects do.
nowant "sp-aaa is not landed" "SP_LANDED=2" "$out"
nowant "sp-ccc is not in unlanded_n (no branch)" "SP_UNLANDED_N=3" "$out"
# (b), absorbed from test-cockpit-queue-section.sh: sp-ccc's only mention is a commit body,
# so it contributes to neither SP_LANDED nor SP_UNLANDED_N — the count above already proves
# SP_LANDED stayed at 1 despite that commit existing in the log.

echo "--- 24h scope, absorbed from test-cockpit-landed.sh ---"
# sp-oldd carries a genuine "spira: land sp-oldd" subject on main, but closed 48h ago — it
# must be excluded from SP_CLOSED and SP_LANDED entirely, not merely left off SP_UNLANDED_N.
nowant "sp-oldd's landing commit does not inflate SP_CLOSED" "SP_CLOSED=5" "$out"
nowant "sp-oldd's landing commit does not inflate SP_LANDED" "SP_LANDED=2" "$out"

echo "--- pane renders QUEUE, not UNLND ---"
PANE="$HERE/../cockpit/health.sh"
{
    printf '%s\n' "$out" | python3 -c '
import sys
for line in sys.stdin:
    line = line.rstrip("\n")
    if "=" not in line: continue
    k, _, v = line.partition("=")
    k = k.strip()
    if not k or not (k[0].isalpha() or k[0] == "_"): continue
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
'
} > "$RUN/cockpit.env"

REAL_BD="$(command -v "${SPIRA_BD:-bd}" 2>/dev/null)"
pane="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$ALPHA" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_BD="${REAL_BD:-bd}" \
    SPIRA_REPO_MAP="$MAP" SPIRA_FAYTHS=t \
    bash "$PANE" once 0 120 2>/dev/null)"

want "pane renders QUEUE label" "QUEUE" "$pane"
nowant "pane does not render old UNLND label" "UNLND" "$pane"

# ======================================================================================
# THE ZERO CASE — nothing anomalous; queue section says nothing queued. A fresh single-bead
# population, landed via subject, appended to the same growing git history: SP_LANDED is
# scoped to the current query's population, so earlier commits (e.g. "spira: land sp-bbb")
# cannot leak into this count even though they remain in the log.
git -C "$ALPHA" commit --allow-empty -m "spira: land sp-eee" -q
cat > "$TMP/zero.json" <<JSON
[
  {"id":"sp-eee","title":"already landed","status":"closed","priority":1,"closed_at":"$AGO5","labels":["${SPIRA_SCOPE_LABEL}","plan","repo:alpha"]}
]
JSON
printf '{"type":"system","subtype":"init"}\n' > "$RUN/sp-eee.log"

out_zero="$(unlanded "$TMP/zero.json")"
val_z() { printf '%s' "$out_zero" | grep "^$1=" | head -1 | sed "s/^$1=//"; }
is "SP_UNLANDED_N is 0 when no anomalies" "0" "$(val_z SP_UNLANDED_N)"
is "SP_LANDED is 1 for sp-eee" "1" "$(val_z SP_LANDED)"

# ======================================================================================
# POSITIVE CONTROL
want "output contains SP_AT" "SP_AT=" "$out"
want "output contains SP_UNLANDED_N" "SP_UNLANDED_N=" "$out"

echo
printf 'test-cockpit-unlanded: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
