#!/usr/bin/env bash
#
# test-archive-growing.sh — a live transcript that grows while the sweep compresses it.
#
# A transcript appended to between being hashed and being compressed failed the whole sweep
# (zstd "Incomplete read"), or stored a body whose digest was not the recorded one. One live
# file must never fail the sweep, and the stored body must match its recorded sha256.
#
# CASES:
#   - a compressor shim appends to the source mid-sweep: the sweep exits 0 and verify is sound;
#   - the next sweep sees the grown file and archives it (restore returns the grown bytes);
#   - positive control: verify does go red on a body that is not the recorded one.
#
# tier: T1
# covers: spira/archive.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-archive-growing.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

PROJ="$TMP/projects/-slug"; mkdir -p "$PROJ" "$TMP/shim" "$TMP/h"
LIVE="$PROJ/live.jsonl"
for i in 1 2 3; do echo "{\"type\":\"assistant\",\"timestamp\":\"2026-10-03T20:0$i:00Z\",\"sessionId\":\"s1\"}"; done > "$LIVE"

for tool in zstd gzip; do
    real="$(command -v "$tool" 2>/dev/null)" || continue
    cat > "$TMP/shim/$tool" <<EOF
#!/usr/bin/env bash
[ -n "\${GROW:-}" ] && echo '{"type":"assistant","timestamp":"2026-10-03T20:59:00Z","sessionId":"s1"}' >> "$LIVE"
exec "$real" "\$@"
EOF
    chmod +x "$TMP/shim/$tool"
done

arch() {
    env -i PATH="$TMP/shim:$PATH" HOME="$TMP/h" SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_DB="$TMP/db" SPIRA_ARCHIVE="$TMP/arch" SPIRA_TOKEN_PROJECTS="$TMP/projects" ${GROW:+GROW=1} \
        archive.sh "$@" 2>&1
}

GROW=1 arch sweep >/dev/null; rc=$?
is "a file that grows during the sweep does not fail it" 0 "$rc"
arch verify >/dev/null; rc=$?
is "the body stored is the body hashed" 0 "$rc"

out="$(arch sweep)"; rc=$?
is   "the next pass exits 0" 0 "$rc"
want "and archives the grown file" "1 stored" "$out"
got="$(arch restore live | wc -l)"
is "the restored copy carries the appended line" "$(wc -l < "$LIVE" | tr -d ' ')" "$(echo $got)"

body="$(ls "$TMP"/arch/bodies/-slug/live.jsonl.*)"
echo corrupt > "$body"
arch verify >/dev/null; rc=$?
[ "$rc" -ne 0 ] && ok "positive control: verify goes red on a wrong body" || bad "positive control: verify goes red on a wrong body" "rc=0"

tl_summary
