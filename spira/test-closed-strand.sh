#!/usr/bin/env bash
#
# test-closed-strand.sh — stranded-bead detection and recovery (sp-bf31a).
#
# ONE SCENARIO:
#
#   ANOMALY SPLIT. SP_UNLANDED_N is split into SP_STRANDED_N (older than cert window)
#      and SP_CERT_N (within cert window). A bead closed 5 minutes ago counts as
#      awaiting cert, not stranded.
#
# tier: T3
# covers: landing-pass/* spira/lib.sh cockpit-collect/src/* cockpit/ops/src/health.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Invoked by name on this suite's PATH (sp-gypjk).

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-closed-strand
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up strand || { echo "test-closed-strand: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
timeout 5 git -C "$REPO" push -q origin main
timeout 5 git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$SH/chamber"
lc_path_stub "$SH" "$TMP/lcfix"

cp "$HERE/lib.sh" "$HERE/conf.sh" \
   "$HERE/incident.sh" "$HERE/suite-covers.sh" "$SH/"
copy_conf_registry "$SH"
# `skew` is a compiled binary now (sp-yyk47): landing-pass's own `skew_refresh` resolves it
# by bare name on PATH, which is `$SH` first here, so the binary must actually be there.
cp "$(command -v skew)" "$SH/skew"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand        'exit 0'
# THE RUST SENTINEL (sentinel.sh is gone): sentinel, spira-claim and landing-pass are found by
# name on PATH; the fixture home $SH goes FIRST on PATH, so its stubs (strand, gate.sh, ...)
# are the ones a bare name reaches (sp-gypjk).
stub sending       'exit 0'
stub reflect.sh    'exit 0'
stub gate.sh       'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub confine.sh    'exit 0'
stub gh            'exit 1'
stub ask.sh        'true'
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,spira-ask,spira-ci"\nFAYTH_MAX_CONCURRENT=0\n' \
    > "$SH/chamber/t.fayth"

printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$REPONAME" "$REPO" push main '' '' > "$SH/repo-map"

B()        { timeout 5 bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"; chmod +x "$TMP/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"; chmod +x "$TMP/launch"

sentinel() {
    # SPIRA_RUN/SPIRA_DB/SPIRA_HOME_REPO/SPIRA_FAYTHS/SPIRA_NOTIFY/SPIRA_REPO_MAP are
    # registered keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare via tl_config,
    # not the per-call env prefix below, which no process reads them from any more.
    # round 2 fix (pattern 6): SPIRA_CHAMBER no longer derives from SPIRA_HOME.
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_FAYTHS="t" SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$SH/repo-map" \
        SPIRA_CHAMBER="$SH/chamber"
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$REPO" \
    SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no.conf" PATH="$SH:$PATH" \
        command sentinel 2>&1
}

landing() {
    rm -f "$RUN/landing.progress"
    # SPIRA_RUN/SPIRA_DB/SPIRA_HOME_REPO/SPIRA_REPO_MAP/SPIRA_GH are registered keys (per
    # Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare via tl_config, not the per-call env
    # prefix below, which no process reads them from any more.
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh"
    SPIRA_HOME="$SH" SPIRA_REPO="$REPO" \
    SPIRA_CONF="$TMP/no.conf" PATH="$SH:$PATH" \
        landing-pass land 2>&1
}

PAST="2026-09-01T00:00:00Z"

echo "test-closed-strand.sh"

# ======================================================================================
echo
echo "SCENARIO: ANOMALY SPLIT — SP_STRANDED_N vs SP_AWAITING_N:"
# ======================================================================================
# This scenario invokes cockpit-collect directly (like test-cockpit-unlanded.sh).
REAL_BD="$(PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" command -v bd 2>/dev/null)"
if [ -z "$REAL_BD" ]; then
    echo "SKIP strand-anomaly-split: no bd binary" >&2
else
    BASE_PATH="$PATH"
    BD_PATH="${SPIRA_PATH:-}"
    TESTDB_BD_PATH="$(command -v bd)"

    ALPHA="$TMP/alpha"
    git init -q -b main "$ALPHA"
    git -C "$ALPHA" commit --allow-empty -m "init" -q
    git -C "$ALPHA" checkout -q -b spira/sp-old
    git -C "$ALPHA" commit --allow-empty -m "sp-old work" -q
    git -C "$ALPHA" checkout -q main
    git -C "$ALPHA" checkout -q -b spira/sp-new
    git -C "$ALPHA" commit --allow-empty -m "sp-new work" -q
    git -C "$ALPHA" checkout -q main

    MAP="$TMP/cmap"
    printf 'alpha | %s | push | main | |\n' "$ALPHA" > "$MAP"
    RUN2="$TMP/run2"; mkdir -p "$RUN2"
    SCOPE=alpha

    # sp-old: closed 200 minutes ago (beyond the 90-min cert window → stranded)
    # sp-new: closed 5 minutes ago (within cert window → awaiting)
    AGO200="$(date -u -d '200 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
              || date -u -v-200M +%Y-%m-%dT%H:%M:%SZ)"
    AGO5="$(date -u -d '5 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
            || date -u -v-5M +%Y-%m-%dT%H:%M:%SZ)"

    lc_fix_init "$TMP/lcfix"
    # Both were handed on by their builders and neither has landed: that is their lifecycle
    # row, which is what the collector reads (sp-mve9i), not bd's `closed`.
    lc_bead SUBMITTED sp-old "$(git -C "$ALPHA" rev-parse spira/sp-old)" 0
    lc_bead SUBMITTED sp-new "$(git -C "$ALPHA" rev-parse spira/sp-new)" 0
    testdb_reset
    testdb_seed <<JSONL2
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":["$SCOPE"],"updated_at":"$PAST"}
{"id":"sp-old","title":"stranded","status":"closed","priority":1,"closed_at":"$AGO200","labels":["$SCOPE","plan","repo:alpha"]}
{"id":"sp-new","title":"awaiting","status":"closed","priority":1,"closed_at":"$AGO5","labels":["$SCOPE","plan","repo:alpha"]}
JSONL2
    printf '{"type":"system","subtype":"init"}\n' > "$RUN2/sp-old.log"
    printf '{"type":"system","subtype":"init"}\n' > "$RUN2/sp-new.log"

    # The collector's funnel reads spira-lc; with no store it prints ? (CANNOT TELL). An empty
    # throwaway store is a real zero, so the beads here are all the done stage.
    DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
    LC_BIN="$(command -v spira-lc 2>/dev/null || true)"
    if [ -z "$LC_BIN" ] || [ -z "$DOLT_BIN" ]; then
        echo "SKIP strand-anomaly-split: spira-lc or dolt missing (needed for spira_lifecycle)" >&2
        tl_summary
        exit 0
    fi
    LC_PORT=$((3309 + (RANDOM % 500)))
    mkdir -p "$TMP/lc-data"
    cat > "$TMP/lc-server.yaml" <<YAML
log_level: warning
listener:
  port: $LC_PORT
  max_connections: 50
data_dir: "$TMP/lc-data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
    "$DOLT_BIN" sql-server --config "$TMP/lc-server.yaml" > "$TMP/lc-server.log" 2>&1 & # batch-job: long-lived fixture listener, killed by the suite teardown
    LC_SERVER_PID=$!
    trap 'kill "$LC_SERVER_PID" >/dev/null 2>&1; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
    for _ in $(seq 1 50); do
        timeout 5 "$DOLT_BIN" --data-dir "$TMP/lc-data" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1 && break
        sleep 0.2
    done
    # round 3 fix (pattern 7): SPIRA_LC_PASSWORD_FILE is a registered key; undeclared, it
    # resolves to the complete fixture's placeholder /fixture/userhome/.../spira-lc.credential,
    # which does not exist. Declare this suite's own (empty-password) credential file.
    : > "$TMP/lc-data/credential"
    tl_config SPIRA_LC_PASSWORD_FILE="$TMP/lc-data/credential"
    LC_ENV=(SPIRA_LC_BIN="$LC_BIN" SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$LC_PORT" \
            SPIRA_LC_DB=spira_lifecycle SPIRA_LC_DATA_DIR="$TMP/lc-data" \
            SPIRA_LC_USER=root SPIRA_LC_PASSWORD="" SPIRA_LC_DOLT_BIN="$DOLT_BIN")
    env "${LC_ENV[@]}" "$LC_BIN" admin-apply-ddl "$HERE/../lifecycle/schema.sql" >"$TMP/lc-schema.log" 2>&1 \
        || { echo "spira_lifecycle schema failed: $(cat "$TMP/lc-schema.log")" >&2; exit 1; }

    # SPIRA_HOME_REPO/SPIRA_SCOPE_LABEL/SPIRA_RUN/SPIRA_DB/SPIRA_BD/SPIRA_REPO_MAP/
    # SPIRA_FAYTHS/SPIRA_PATH/SPIRA_CERT_WINDOW_MINS are registered keys (per Ryan
    # 2026-10-05, ONE SOURCE OF CONFIG): declare via tl_config and thread SPIRA_TOML
    # through env -i, which clears it and which no process reads these from any more.
    tl_config SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SCOPE" SPIRA_RUN="$RUN2" \
        SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" SPIRA_REPO_MAP="$MAP" \
        SPIRA_FAYTHS=t SPIRA_PATH="$BD_PATH" SPIRA_CERT_WINDOW_MINS=90
    cout="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 "${LC_ENV[@]}" \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
        SPIRA_REPO="$ALPHA" SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" \
        SPIRA_TOML="$SPIRA_TOML" \
        cockpit-collect once 2>/dev/null)"

    cval() { printf '%s' "$cout" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

    is "SP_UNLANDED_N is 2 (both have branch, no landstate)" "2" "$(cval SP_UNLANDED_N)"
    is "SP_STRANDED_N is 1 (sp-old closed 200 min ago)"      "1" "$(cval SP_STRANDED_N)"
    is "SP_CERT_N is 1 (sp-new closed 5 min ago)"            "1" "$(cval SP_CERT_N)"
fi

echo
tl_summary
