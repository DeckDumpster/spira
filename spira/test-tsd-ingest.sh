#!/usr/bin/env bash
#
# test-tsd-ingest.sh — tsd-ingest.sh: pulling a CI run's tsd/*.jsonl rows into
# the local run/tsd/ tree.
#
# WHAT THIS SUITE CHECKS.
#   1. POSITIVE CONTROL: a run whose batch-results artifact carries a tsd/suite-timing.jsonl
#      file gets those rows appended into $SPIRA_RUN/tsd/suite-timing.jsonl, unchanged.
#   2. IDEMPOTENT: a second call for the same run id is a no-op (the marker short-circuits
#      before any gh call, so a re-run cannot duplicate rows).
#   3. MERGE: ingesting a second, different run appends alongside rows already there —
#      existing local/prior-CI rows are not overwritten.
#   4. NO ARTIFACT: a run with no batch-results artifact is a clean no-op, not a failure.
#
# A STUB gh. What is tested is tsd-ingest.sh's artifact handling and merge/idempotency, not
# a real GitHub response.
#
# covers: spira/tsd-ingest.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/sbin" "$T/run"

ingest() {
    SPIRA_HOME="$HERE" SPIRA_REPO="$HERE/.." SPIRA_RUN="$T/run" SPIRA_DB="$T/db" \
        SPIRA_PATH="$T/sbin" SPIRA_CONF="$T/no.conf" \
        bash "$HERE/tsd-ingest.sh" "$@"
}

# mkzip <zip-path> <arcname> <content> — build a real zip with python's zipfile (only
# `unzip` ships in the testenv image, not the `zip` compressor).
mkzip() {
    python3 -c '
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as zf:
    zf.writestr(sys.argv[2], sys.argv[3])
' "$1" "$2" "$3"
}

# A stub gh: "run list"/"artifacts" return a fixed id; the zip download drops a real zip
# file (built once, below) so unzip has something real to act on.
ZIP="$T/artifact.zip"
mkzip "$ZIP" "tsd/suite-timing.jsonl" \
    '{"ts":"2026-09-25T01:00:00Z","host":"gha-runner-9","family":"suite-timing","suite":"ci.sh","wall_secs":42}
'

cat > "$T/sbin/gh" <<GHSTUB
#!/usr/bin/env bash
case "\$*" in
    *"actions/runs/100/artifacts"*)  printf '900\n' ;;
    *"actions/artifacts/900/zip"*)   cat "$ZIP" ;;
    *"actions/runs/200/artifacts"*)  printf '\n' ;;
    *) exit 0 ;;
esac
GHSTUB
chmod +x "$T/sbin/gh"

# --------------------------------------------------------------------------------------
# PART 1: POSITIVE CONTROL — run 100's tsd row lands in run/tsd/suite-timing.jsonl.
# --------------------------------------------------------------------------------------
echo "run with a batch-results artifact:"
out1="$(ingest test-org/test-repo 100 2>&1)"
FAM="$T/run/tsd/suite-timing.jsonl"
[ -f "$FAM" ] && ok "suite-timing.jsonl created" || bad "no suite-timing.jsonl after ingest"
want "ingest reports one family file" "ingested 1 family" "$out1"
is   "one row ingested" "1" "$(grep -c . "$FAM" 2>/dev/null || echo 0)"
want "the ingested row is run 100's" "ci.sh" "$(cat "$FAM" 2>/dev/null)"
want "the row keeps its own host id (CI runner, not the coordinator)" \
    "gha-runner-9" "$(cat "$FAM" 2>/dev/null)"

# --------------------------------------------------------------------------------------
# PART 2: IDEMPOTENT — a second ingest of run 100 adds nothing.
# --------------------------------------------------------------------------------------
echo
echo "second ingest of the same run:"
ingest test-org/test-repo 100 >/dev/null 2>&1
is "still exactly one row — the marker made the repeat a no-op" \
   "1" "$(grep -c . "$FAM" 2>/dev/null || echo 0)"

# --------------------------------------------------------------------------------------
# PART 3: MERGE — a different run's artifact appends alongside, not over.
# --------------------------------------------------------------------------------------
echo
echo "ingest a second, different run:"
ZIP2="$T/artifact2.zip"
mkzip "$ZIP2" "tsd/suite-timing.jsonl" \
    '{"ts":"2026-09-25T02:00:00Z","host":"gha-runner-3","family":"suite-timing","suite":"other.sh","wall_secs":7}
'
cat > "$T/sbin/gh" <<GHSTUB
#!/usr/bin/env bash
case "\$*" in
    *"actions/runs/100/artifacts"*)  printf '900\n' ;;
    *"actions/artifacts/900/zip"*)   cat "$ZIP" ;;
    *"actions/runs/101/artifacts"*)  printf '901\n' ;;
    *"actions/artifacts/901/zip"*)   cat "$ZIP2" ;;
    *) exit 0 ;;
esac
GHSTUB
chmod +x "$T/sbin/gh"
ingest test-org/test-repo 101 >/dev/null 2>&1
is   "two rows now — merge, not overwrite" "2" "$(grep -c . "$FAM" 2>/dev/null || echo 0)"
want "run 100's row is still there" "ci.sh"    "$(cat "$FAM" 2>/dev/null)"
want "run 101's row was added"      "other.sh" "$(cat "$FAM" 2>/dev/null)"

# --------------------------------------------------------------------------------------
# PART 4: NO ARTIFACT — a clean no-op, not a failure.
# --------------------------------------------------------------------------------------
echo
echo "run with no batch-results artifact:"
out4="$(ingest test-org/test-repo 200 2>&1)"; rc4=$?
wantrc "no-artifact run exits 0" 0 "$rc4"
want   "no-artifact run says so" "no batch-results artifact" "$out4"
is     "row count unchanged" "2" "$(grep -c . "$FAM" 2>/dev/null || echo 0)"

tl_summary
