#!/usr/bin/env bash
#
# test-landstate-checks-deleted.sh — the landstate CHECK 5 (sentinel, closed-not-landed)
# and the groomer's three STATE sweeps (landed-but-open, closed-never-landed,
# closed-no-branch, plus the blocked-by-unlanded follow-up) are DELETED, not switched off.
#
# HISTORY. This file was test-legacy-state-checks-ungated.sh (sp-pswer.1): a trap that kept
# these checks alive and ungated until an ON-path equivalent existed. CHECK5-LC
# (sentinel/src/lifecycle.rs) is that equivalent: it reports the same three drifts from the
# spira-lc rows. The cutover (sp-sa8pn) meant to delete them; instead CHECK 5 was held off by
# an env-only drop-in (SPIRA_SKIP_CLOSED_CHECK=1) that a unit re-render deleted on
# 2026-10-04, and CHECK 5 paged five times in fourteen minutes on a verdict-only bead.
# sp-jnwbn deletes them, so no re-render can bring them back. This suite is the trap the
# other way: reintroducing any of them, or the skip switch, fails here.
#
# CHECK5-LC itself stays and is asserted present, so this cannot pass by deleting the
# replacement along with the legacy check.
#
# defect: sp-pswer.1 sp-jnwbn
# tier: T1
# covers: sentinel/src/pass.rs sentinel/src/cfg.rs sentinel/src/lifecycle.rs groomer/src/sweep.rs groomer/src/seam.rs spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-landstate-checks-deleted.sh"

ROOT="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

audit_body()      { awk '/^    fn audit\(/{f=1} f{print} f && /^    }$/{exit}' "$1"; }
rust_sweep_body() { awk '/^pub fn sweep\(/{f=1} f{print} f && /^}$/{exit}' "$1"; }

# ---------------------------------------------------------------------------------------
# SENTINEL: no check5.rs, audit() calls no landstate check5, no skip switch anywhere.
# ---------------------------------------------------------------------------------------
is "sentinel/src/check5.rs is gone" no "$([ -e "$ROOT/sentinel/src/check5.rs" ] && echo yes || echo no)"

body="$(audit_body "$ROOT/sentinel/src/pass.rs")"
want   "audit() was extracted"                  "fn audit(" "$body"
nowant "audit() does not call the landstate check5" "self.check5(" "$body"
# sp-mve9i: CHECK5-LC is gone too — each of its shapes was bd status disagreeing with the
# lifecycle row, and bd status is inert for work beads (design §3.4). The rowless control stays.
nowant "audit() no longer runs CHECK5-LC"        "check5_lc(" "$body"
nowant "lifecycle.rs no longer defines check5_lc" "pub fn check5_lc" "$(cat "$ROOT/sentinel/src/lifecycle.rs")"
want   "audit() still runs the rowless control"  "check_rowless(" "$body"

nowant "sentinel reads no SPIRA_SKIP_CLOSED_CHECK" "SPIRA_SKIP_CLOSED_CHECK" \
    "$(cat "$ROOT"/sentinel/src/*.rs)"
is "no conf.d entry declares SPIRA_SKIP_CLOSED_CHECK" no \
    "$([ -e "$ROOT/spira/conf.d/SPIRA_SKIP_CLOSED_CHECK" ] && echo yes || echo no)"

# SEEN RED: the same extraction over a scratch pass.rs with the legacy call restored.
restored="$TMP/pass.rs"
sed 's/^        self\.check4(snap);$/        self.check4(snap);\n        self.check5(snap);/' \
    "$ROOT/sentinel/src/pass.rs" > "$restored"
want "SEEN RED: a restored check5 call is caught" "self.check5(" "$(audit_body "$restored")"

# ---------------------------------------------------------------------------------------
# GROOMER: sweep() and the seam no longer reach the three landstate detectors.
# ---------------------------------------------------------------------------------------
sbody="$(rust_sweep_body "$ROOT/groomer/src/sweep.rs")"
want "sweep() was extracted" "pub fn sweep(" "$sbody"
for fn in detect_landed_but_open detect_closed_unlanded_states detect_false_blockers; do
    nowant "groomer/src does not reach $fn" "$fn" "$(cat "$ROOT"/groomer/src/*.rs)"
    # sp-mve9i: strand's subcommands are gone, so lib.sh keeps no shim onto them either.
    nowant "lib.sh defines no $fn shim" "$fn() {" "$(cat "$ROOT/spira/lib.sh")"
done
for sub in detect-landed-but-open detect-closed-unlanded-states detect-false-blockers; do
    nowant "lib.sh calls no strand $sub" "strand $sub" "$(cat "$ROOT/spira/lib.sh")"
done
for kind in landed-but-open closed-never-landed closed-no-branch blocked-by-unlanded; do
    nowant "groomer/src names no $kind remedy" "$kind" "$(cat "$ROOT"/groomer/src/*.rs)"
done

# SEEN RED for the groomer side.
restored_groomer="$TMP/sweep.rs"
sed 's/^    let inc = seam\.detect_incident_needs_builder()?;$/    let lbo = seam.detect_landed_but_open()?;\n&/' \
    "$ROOT/groomer/src/sweep.rs" > "$restored_groomer"
want "SEEN RED: a restored landed-but-open sweep is caught" \
    "detect_landed_but_open" "$(rust_sweep_body "$restored_groomer")"

tl_summary
