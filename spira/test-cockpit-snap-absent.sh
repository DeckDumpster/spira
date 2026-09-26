#!/usr/bin/env bash
#
# test-cockpit-snap-absent.sh — health.sh emits a diagnostic when SPIRA_SNAP is absent.
#
# WHAT IS TESTED
#   PATH MISMATCH — cockpit.env exists at the XDG-derived SPIRA_RUN alternative while
#     the reader's SPIRA_RUN points to the repo-derived path (or vice versa). The banner
#     must identify the file's actual location and the path being read.
#
#   NO SNAPSHOT — cockpit.env exists nowhere. The notice must say so without claiming a
#     mismatch that does not exist.
#
#   CLEAN — when cockpit.env IS at SPIRA_RUN neither message fires.
#
# POSITIVE CONTROL FIRST. The absent case is verified before the present case so that a
# banner that never fires does not silently pass the "clean" assertion.
#
# No database, no network.
#
# covers: cockpit/health.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PANE="$HERE/../cockpit/health.sh"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s\n' "$1"; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "test-cockpit-snap-absent.sh"

if [ ! -f "$PANE" ]; then
    fail=$((fail+1)); printf '  FAIL  cannot find pane at %s\n' "$PANE"
    printf '%d passed, %d failed\n' "$pass" "$fail"; exit 1
fi

PD="$TMP/pane"
mkdir -p "$PD/repo/.runtime/spira" \
          "$PD/home/.local/share/spira/run" \
          "$PD/bin"

printf '#!/bin/sh\necho active\n' > "$PD/bin/mock-systemctl"
chmod +x "$PD/bin/mock-systemctl"

# pane_at <spira_run> <rows> — render health.sh with SPIRA_RUN set to the given path.
# HOME is the scratch dir, so XDG_DATA_HOME is unset and the XDG alternative resolves to
# $PD/home/.local/share/spira/run.
pane_at() {
    local run="$1" rows="$2"
    env -i PATH="$PATH" HOME="$PD/home" TERM=dumb LC_ALL=C.UTF-8 \
        SPIRA_CONF="$PD/no.conf" SPIRA_REPO="$PD/repo" SPIRA_RUN="$run" \
        SPIRA_SYSTEMCTL="$PD/bin/mock-systemctl" \
        bash "$PANE" once "$rows" 0 2>/dev/null \
      | sed 's/\x1b\[[?0-9;]*[a-zA-Z]//g'
}

# ---- NO SNAPSHOT: cockpit.env not found at any known derivation ----------------------
# POSITIVE CONTROL FIRST, same render: before asserting the specific "no snapshot" wording,
# confirm a diagnostic fires at all — a banner that never fires would otherwise pass the
# specific-wording check by having nothing to compare it against. This used to be two
# separate `health.sh once` renders of the identical fixture; one render answers both.
echo
echo "no snapshot — cockpit.env found nowhere"

rm -f "$PD/repo/.runtime/spira/cockpit.env" \
       "$PD/home/.local/share/spira/run/cockpit.env"
nosnap="$(pane_at "$PD/repo/.runtime/spira" 40)"
if printf '%s\n' "$nosnap" | grep -qiE 'no snapshot|path mismatch'; then
    ok "absent snap fires a diagnostic"
else
    bad "absent snap fired no diagnostic"
fi
if printf '%s\n' "$nosnap" | grep -qi 'no snapshot'; then
    ok "no cockpit.env anywhere renders a 'no snapshot' notice"
else
    bad "no cockpit.env anywhere did not say 'no snapshot'; got: $(printf '%s\n' "$nosnap" | head -5)"
fi
# PATH MISMATCH must not fire when there is nothing to find.
if printf '%s\n' "$nosnap" | grep -qi 'path mismatch'; then
    bad "no-snap case falsely reported PATH MISMATCH"
else
    ok "no-snap case does not falsely report PATH MISMATCH"
fi

# ---- PATH MISMATCH: cockpit.env at XDG alternative, not at SPIRA_RUN ---------------
echo
echo "path mismatch — cockpit.env at XDG path while SPIRA_RUN is the repo path"

# REPO path: $PD/repo/.runtime/spira  (SPIRA_RUN — no file here)
# XDG path:  $PD/home/.local/share/spira/run  (file here)
printf 'SP_NEXT_N=0\nSP_AWAITING_N=0\n' > "$PD/home/.local/share/spira/run/cockpit.env"
rm -f "$PD/repo/.runtime/spira/cockpit.env"
mismatch="$(pane_at "$PD/repo/.runtime/spira" 40)"
if printf '%s\n' "$mismatch" | grep -qi 'path mismatch'; then
    ok "cockpit.env at XDG path fires PATH MISMATCH when SPIRA_RUN is the repo path"
else
    bad "cockpit.env at XDG path did not fire PATH MISMATCH; got: $(printf '%s\n' "$mismatch" | head -8)"
fi
# The alternative path must appear so the operator knows where to look.
if printf '%s\n' "$mismatch" | grep -qF '.local/share/spira'; then
    ok "PATH MISMATCH names the alternative path"
else
    bad "PATH MISMATCH did not name the alternative path"
fi
# The reader path must also appear.
if printf '%s\n' "$mismatch" | grep -qF '.runtime/spira'; then
    ok "PATH MISMATCH names the reader path"
else
    bad "PATH MISMATCH did not name the reader path"
fi

# ---- CLEAN: no diagnostic when cockpit.env exists at SPIRA_RUN ----------------------
echo
echo "clean — no diagnostic when cockpit.env exists at SPIRA_RUN"

rm -f "$PD/home/.local/share/spira/run/cockpit.env"
printf 'SP_NEXT_N=0\nSP_AWAITING_N=0\n' > "$PD/repo/.runtime/spira/cockpit.env"
clean="$(pane_at "$PD/repo/.runtime/spira" 40)"
if printf '%s\n' "$clean" | grep -qiE 'path mismatch|no snapshot'; then
    bad "snap present still fired an absent-snap diagnostic"
else
    ok "no absent-snap diagnostic when cockpit.env exists at SPIRA_RUN"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
