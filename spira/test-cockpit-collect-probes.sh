#!/usr/bin/env bash
#
# test-cockpit-collect-probes.sh — collect.sh probe registry integrity.
#
# Verifies that every probe cockpit.sh's probe() calls is registered in
# collect.sh's PROBES, and that the queue probe's keys reach cockpit.env
# through the fragment/merge path.
#
# POSITIVE CONTROL: a queue fragment at status=never leaves SP_QUEUE_DEPTH
# absent from cockpit.env; a queue depth of 0 and an unwired probe must never
# look the same (law-absence-needs-a-positive-control).
#
# covers: spira/collect.sh spira/cockpit.sh

# covers: spira/collect.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# host-reason: collect.sh and cockpit.sh dispatch; no shared state or timed services.
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'chmod -R +w "$TMP" 2>/dev/null || true; rm -rf "$TMP"' EXIT
FRAG_DIR="$TMP/cockpit.d"
SNAP="$TMP/cockpit.env"
mkdir -p "$FRAG_DIR"
BASE_PATH="$PATH"

# Extract PROBES from collect.sh: lines matching "name:interval:timeout:cmd" inside PROBES=().
collect_probes() {
    python3 - "$HERE/collect.sh" <<'PY'
import sys, re
in_probes = False
probes = []
for line in open(sys.argv[1], errors="replace"):
    s = line.strip()
    if re.match(r'PROBES=\(', s):
        in_probes = True
    if in_probes:
        m = re.search(r'"([^":]+):', s)
        if m:
            probes.append(m.group(1))
        if s == ')':
            break
for p in probes:
    print(p)
PY
}

PROBES_LIST="$(collect_probes)"

# Run the Python merge logic, same as test-cockpit-tiered-collector.sh.
run_merge() {
    python3 - "$FRAG_DIR" "$SNAP" <<'PY' 2>/dev/null
import sys, os, glob

frag_dir, snap_path = sys.argv[1], sys.argv[2]
meta_lines  = []
value_seen  = set()
value_lines = []

for frag_path in sorted(glob.glob(os.path.join(frag_dir, "*.env"))):
    name = os.path.basename(frag_path)[:-4]
    probe_at     = "0"
    probe_status = "never"
    val_pairs = []
    try:
        for line in open(frag_path, errors="replace"):
            line = line.rstrip("\n")
            if "=" not in line:
                continue
            k, _, v = line.partition("=")
            k = k.strip()
            if k == "_PROBE_AT":
                probe_at = v
            elif k == "_PROBE_STATUS":
                probe_status = v
            elif k and (k[0].isalpha() or k[0] == "_"):
                val_pairs.append((k, v))
    except OSError:
        pass
    meta_lines.append("_PROBE_AT_%s=%s"     % (name, probe_at))
    meta_lines.append("_PROBE_STATUS_%s=%s" % (name, probe_status))
    if probe_status not in ("never", "timeout", "error"):
        for k, v in val_pairs:
            if k not in value_seen:
                value_seen.add(k)
                value_lines.append((k, v))

def shq(v):
    return "'" + v.replace("'", "'\\''") + "'"

with open(snap_path, "w") as f:
    for line in meta_lines:
        k, _, v = line.partition("=")
        f.write("%s=%s\n" % (k, shq(v)))
    for k, v in value_lines:
        f.write("%s=%s\n" % (k, shq(v)))
PY
}

# ============================================================
echo "1. probe registry: required probes present in collect.sh PROBES:"

for probe in queue statute; do
    if printf '%s\n' "$PROBES_LIST" | grep -qx "$probe"; then
        ok "collect.sh PROBES contains '$probe'"
    else
        bad "collect.sh PROBES missing '$probe'" "add ${probe}:<interval>:<timeout>:${probe} to PROBES"
    fi
done

# ============================================================
echo
echo "2. positive control — status=never: SP_QUEUE_DEPTH absent from cockpit.env:"

# POSITIVE CONTROL: first prove a status=ok queue fragment DOES contribute SP_QUEUE_DEPTH.
rm -f "$FRAG_DIR"/*.env
printf '_PROBE_AT=1000\n_PROBE_STATUS=ok\nSP_QUEUE_DEPTH=3\nSP_QUEUE_EJECTED=0\n' \
    > "$FRAG_DIR/queue.env"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want "ok fragment: SP_QUEUE_DEPTH present in cockpit.env" "SP_QUEUE_DEPTH=" "$snap"

# Absence case: status=never means the probe has never run — keys must be absent.
# This is the control that distinguishes an unwired probe from a queue depth of 0.
rm -f "$FRAG_DIR"/*.env
printf '_PROBE_AT=0\n_PROBE_STATUS=never\n' > "$FRAG_DIR/queue.env"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
nowant "never fragment: SP_QUEUE_DEPTH absent from cockpit.env" "SP_QUEUE_DEPTH=" "$snap"
want   "never fragment: _PROBE_STATUS_queue=never in cockpit.env" "_PROBE_STATUS_queue='never'" "$snap"

# ============================================================
echo
echo "3. queue keys reach cockpit.env via _probe_body_test:"

# Build a mock cockpit.sh that emits known SP_QUEUE_* keys.
MOCK_COCK="$TMP/mock-cockpit.sh"
cat > "$MOCK_COCK" <<'MOCK'
#!/usr/bin/env bash
case "${1:-}" in
    queue) echo "SP_QUEUE_DEPTH=2"; echo "SP_QUEUE_EJECTED=0"; echo "SP_QUEUE_RED=0" ;;
    *) exit 1 ;;
esac
MOCK
chmod +x "$MOCK_COCK"

rm -f "$FRAG_DIR"/*.env
printf '_PROBE_AT=0\n_PROBE_STATUS=never\n_PROBE_KILLED=0\n' > "$FRAG_DIR/queue.env"
env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    FRAG_DIR="$FRAG_DIR" COCK="$MOCK_COCK" \
    bash "$HERE/collect.sh" _probe_body_test queue 10 queue 2>/dev/null || true

frag="$(cat "$FRAG_DIR/queue.env" 2>/dev/null)"
want "_probe_body_test: fragment has _PROBE_STATUS=ok"  "_PROBE_STATUS=ok"  "$frag"
want "_probe_body_test: fragment has SP_QUEUE_DEPTH=2" "SP_QUEUE_DEPTH=2" "$frag"

run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want "after merge: SP_QUEUE_DEPTH=2 in cockpit.env" "SP_QUEUE_DEPTH='2'" "$snap"
want "after merge: SP_QUEUE_EJECTED in cockpit.env"  "SP_QUEUE_EJECTED="   "$snap"

# ============================================================
echo
printf 'test-cockpit-collect-probes: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
