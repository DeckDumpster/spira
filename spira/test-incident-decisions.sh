#!/usr/bin/env bash
#
# test-incident-decisions.sh — the intake's decision layer against a stateful stand-in bd:
# the dedup lookup is label-keyed and never shows a bead, a declared repo is recorded and an
# undeclared one is flagged for triage, and recurrence notes stay bounded for an unchanged payload.
#
# tier: T1
# covers: spira/incident.sh spira/incident-stub-bd.py incident/* UC-ops-detection-remediation-04 UC-ops-detection-remediation-05 UC-ops-detection-remediation-06
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-incident-decisions.sh"

STUB_BD="$HERE/incident-stub-bd.py"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/run"
ln -s "$HERE/conf.d" "$TMP/home/conf.d"

export STUB_BD_STATE="$TMP/state.json" STUB_BD_LOG="$TMP/bd.log"
lc_mirror_bd "$TMP/lc"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/home/mail"
chmod +x "$TMP/home/mail"

HOME_REPO="fixture-home"

file_incident() {  # file_incident <ref> <title> <payload> [VAR=val ...]
    local ref="$1" title="$2" payload="$3"; shift 3
    tl_config SPIRA_BD="$STUB_BD" SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_HOME_REPO="$HOME_REPO"
    printf '%s' "$payload" | \
        env -i HOME="$HOME" PATH="$PATH" \
        STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" SPIRA_LC_BIN="$SPIRA_LC_BIN" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$TMP/home" PATH="$TMP/home:$PATH" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/run/decisions-test.lock" \
        SPIRA_INCIDENT_REPO= \
        SPIRA_TOML="$SPIRA_TOML" \
        "$@" \
        incident.sh file "$title" - 2>/dev/null
}

reset_store() { rm -f "$STUB_BD_STATE" "$STUB_BD_LOG" "$TMP/lc/facts.tsv"; }

field_of() {  # field_of <ref> <field> -> value of the field on the bead carrying that ref
    python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
for b in d["beads"].values():
    if b.get("external_ref") == sys.argv[2]:
        v = b.get(sys.argv[3], "")
        print(" ".join(v) if isinstance(v, list) else v)
        break
' "$STUB_BD_STATE" "$1" "$2"
}
bead_count() { python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["beads"]))' "$STUB_BD_STATE"; }
calls() { grep -c "^$1\b" "$STUB_BD_LOG" 2>/dev/null || true; }

# ======================================================================================
echo
echo "UC-04 — the dedup lookup is label-keyed and never shows a bead:"
# ======================================================================================
ref="incident:decisions-keyed"
for i in 1 2 3; do file_incident "$ref" "keyed incident" "payload $i" >/dev/null; done
is "three filings of one ref leave one bead" 1 "$(bead_count)"
case " $(field_of "$ref" labels) " in *" ref:"*) ok "the bead carries a ref: label" ;; *) bad "the bead carries a ref: label" "$(field_of "$ref" labels)" ;; esac
[ "$(calls list)" -gt 0 ] && ok "the log saw the list calls (the counter could have fired)" || bad "the log saw the list calls" "no list line in $STUB_BD_LOG"
want "a list call filtered on the ref: label" "--label ref:" "$(cat "$STUB_BD_LOG")"
is "no bd show call was issued" 0 "$(calls show)"

reset_store
python3 -c '
import json
print(json.dumps({"id": "sp-legacy1", "title": "legacy", "labels": ["spira", "incident"], "external_ref": "incident:decisions-legacy", "status": "open", "closed_at": None, "notes": ""}))
' | "$STUB_BD" seed
file_incident "incident:decisions-legacy" "legacy incident" "again" >/dev/null
is "a legacy bead with no ref: label is found through the fallback" 1 "$(bead_count)"

# ======================================================================================
echo
echo "UC-05 — a declared repo is recorded, an undeclared one is flagged for triage:"
# ======================================================================================
reset_store
ref="incident:decisions-declared"
file_incident "$ref" "declared incident" "p" SPIRA_INCIDENT_REPO="$HOME_REPO" >/dev/null
is "the declared repo is recorded on the bead" "$HOME_REPO" "$(field_of "$ref" repo)"
case " $(field_of "$ref" labels) " in *" needs-repo-triage "*) bad "a declared repo is not flagged for triage" "$(field_of "$ref" labels)" ;; *) ok "a declared repo is not flagged for triage" ;; esac

ref="incident:decisions-undeclared"
file_incident "$ref" "undeclared incident" "p" >/dev/null
case " $(field_of "$ref" labels) " in *" needs-repo-triage "*) ok "an undeclared repo is flagged needs-repo-triage" ;; *) bad "an undeclared repo is flagged needs-repo-triage" "$(field_of "$ref" labels)" ;; esac
is "an undeclared repo records no repo" "" "$(field_of "$ref" repo)"
want "the triage note names the missing declaration" "SPIRA_INCIDENT_REPO" "$(field_of "$ref" notes)"

# ======================================================================================
echo
echo "UC-06 — recurrence notes are bounded for an unchanged payload, a changed one is recorded in full:"
# ======================================================================================
reset_store
ref="incident:decisions-notes"
X="$(python3 -c 'print("x" * 500, end="")')"
Y="$(python3 -c 'print("y" * 500, end="")')"
len_notes() { field_of "$ref" notes | wc -c; }
file_incident "$ref" "notes incident" "$X" >/dev/null
file_incident "$ref" "notes incident" "$X" >/dev/null
before="$(len_notes)"
file_incident "$ref" "notes incident" "$X" >/dev/null
after="$(len_notes)"
[ "$((after - before))" -le 120 ] && [ "$after" -gt "$before" ] \
    && ok "an unchanged payload grows the notes by at most 120 B" \
    || bad "an unchanged payload grows the notes by at most 120 B" "grew $((after - before)) B"
is "the unchanged payload is recorded exactly once" 1 "$(field_of "$ref" notes | grep -o "$X" | wc -l)"
file_incident "$ref" "notes incident" "$Y" >/dev/null
is "a changed payload is recorded in full" 1 "$(field_of "$ref" notes | grep -o "$Y" | wc -l)"

tl_summary
