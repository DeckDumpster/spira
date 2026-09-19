#!/usr/bin/env bash
# test-archivist-cited-bead.sh — archivist's mail-filing path: a question that cites a
#   work bead does not block it. The guard in mail.sh refuses the blocking edge and wires
#   a relates_to link instead (sp-aybfy: law-blocking-edges-are-real-dependencies).
#
# SEEN RED: the work bead is confirmed to have 0 open deps before the archivist files.
# Without the guard, mail.sh would add a blocking edge and this would have 1 dep.
# The guard must refuse that edge; the bead stays at 0 deps.
#
# covers: spira/mail.sh spira/archivist.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-archivist-cited-bead.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-archivist-cited-bead
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up archivist-cited-bead || { echo "testdb_up failed"; exit 1; }

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_HOME="$TMP/home"
mkdir -p "$SPIRA_HOME/chamber"

MAIL="$HERE/mail.sh"
run() { bash "$MAIL" "$@"; }

bead_open_deps() {   # count open BLOCKING deps only; relates-to links are not blockers
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
deps = d[0].get("dependencies") or []
print(len([x for x in deps if x.get("status") != "closed" and x.get("dependency_type") == "blocks"]))' 2>/dev/null
}

seed_work_bead() {
    printf '{"id":"%s","title":"work bead %s","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-01-01T00:00:00Z"}\n' \
        "$1" "$1" | testdb_seed >/dev/null 2>&1
}

send_archivist_ask() {   # send_archivist_ask <work-bead> → captured stderr
    SPIRA_MAIL_LINT_CONSIDERED="test" run send operator \
        --from "Archivist <archivist@spira>" \
        --subject "Should this finding become a statute?" \
        --kind question \
        --default "yes, enact it" \
        --bead "$1" <<'BODY' 2>&1 >/dev/null
## Question

Should this finding become a statute?

## Default

yes, enact it
BODY
}

# ==========================================================================
echo
echo "archivist ask citing a task bead: cited bead is NOT blocked"
# ==========================================================================
# POSITIVE CONTROL: 0 open deps before the ask is filed. Without the guard,
# mail.sh would add a blocking dep here and this assertion would fail.
WORK="sp-arc-cited"
seed_work_bead "$WORK"

open_before="$(bead_open_deps "$WORK")"
is "SEEN RED: work bead has 0 open deps before archivist ask" "0" "$open_before"

errs="$(send_archivist_ask "$WORK")"

# Guard must log its refusal.
want "guard logged the blocking-edge refusal" "blocking edge refused" "$errs"

# Work bead must still have 0 open deps (not blocked by the ask).
open_after="$(bead_open_deps "$WORK")"
is "cited work bead is not blocked after archivist ask" "0" "$open_after"

# The ask (decision bead) must still be filed — guard refuses the edge, not the ask.
newest_msg="$(ls -t "$SPIRA_MAIL/operator/new/" 2>/dev/null | head -1)"
dec_bead="$([ -n "$newest_msg" ] && awk '/^[[:space:]]*$/ { exit }
    tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
' "$SPIRA_MAIL/operator/new/$newest_msg")"
is "ask (decision bead) was still filed despite guard" "0" "$([ -n "$dec_bead" ] && echo 0 || echo 1)"
lacks "X-Spira-Bead is the decision bead, not the work bead" "$WORK" "${dec_bead:-}"

# ==========================================================================
echo
echo "SPIRA_MAIL_ALLOW_BLOCKING=1 override restores the blocking edge"
# ==========================================================================
WORK_OVR="sp-arc-override"
seed_work_bead "$WORK_OVR"

is "SEEN RED: override work bead has 0 deps before ask" "0" "$(bead_open_deps "$WORK_OVR")"

SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_ALLOW_BLOCKING=1 \
    run send operator \
        --from "Archivist <archivist@spira>" \
        --subject "Override: should this be a statute?" \
        --kind question \
        --default "yes" \
        --bead "$WORK_OVR" <<'BODY' >/dev/null 2>&1
## Question

Override test.

## Default

yes
BODY

open_ovr="$(bead_open_deps "$WORK_OVR")"
is "with override, work bead IS blocked" "1" "$open_ovr"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
