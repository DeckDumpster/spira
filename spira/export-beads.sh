#!/usr/bin/env bash
# export-beads.sh — regenerate the wiki's bead mirror (raw/spira-beads/spira.jsonl) from the stores.
#
# REGENERATED WHOLE, NEVER PATCHED: one JSON object per bead, sorted by id, each carrying the
# lifecycle store's state as `lifecycle_state` beside the content store's own `status`, which
# no longer decides anything. Statute-book memories follow as {"_type":"memory"} lines.
# Commits to the wiki checkout only when the file's content changes; an unchanged pass still
# touches it, because its mtime is what the mirror-stale alarm (auron) reads.
#
# With no SPIRA_WIKI configured this exits 0 having written nothing. A store that returns
# fewer than half the beads already mirrored is refused, not written.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh" || exit 1

if [ -z "${SPIRA_WIKI:-}" ]; then
    echo "export-beads: SPIRA_WIKI is not configured — nothing to mirror."
    exit 0
fi
[ -d "$SPIRA_WIKI" ] || { echo "export-beads: SPIRA_WIKI '$SPIRA_WIKI' is not a directory" >&2; exit 1; }

REL="raw/spira-beads/spira.jsonl"
OUT="$SPIRA_WIKI/$REL"
mkdir -p "$(dirname "$OUT")" || exit 1
WORK="$(mktemp -d)" || exit 1
trap 'rm -rf "$WORK"' EXIT INT TERM

timeout 300 spira-lc content list --all --json >"$WORK/content.json" || { echo "export-beads: content list failed" >&2; exit 1; } # batch-job: full-store dump on a timer
timeout 120 spira-lc list-all >"$WORK/state.tsv" || { echo "export-beads: lifecycle list failed" >&2; exit 1; } # batch-job: full-store dump on a timer
timeout 60 spira-lc content memories --json >"$WORK/memories.json" || echo '{}' >"$WORK/memories.json" # batch-job: full-store dump on a timer

previous=0
[ -f "$OUT" ] && previous=$(grep -c '"id": *"sp-' "$OUT" 2>/dev/null || echo 0)

python3 -I - "$WORK" "$previous" >"$WORK/new.jsonl" <<'PY' || exit 1
import json, sys
work, previous = sys.argv[1], int(sys.argv[2])
issues = [i for i in json.load(open(work + "/content.json")) if str(i.get("id", "")).startswith("sp-")]
if not issues:
    sys.exit("export-beads: the content store returned no sp- bead — refusing to write")
if previous and len(issues) * 2 < previous:
    sys.exit("export-beads: %d beads against %d mirrored — refusing to shrink the mirror" % (len(issues), previous))
state = {}
for line in open(work + "/state.tsv"):
    parts = line.rstrip("\n").split("\t")
    if len(parts) >= 2 and parts[0].startswith("sp-"):
        state[parts[0]] = parts[1]
for i in sorted(issues, key=lambda x: x["id"]):
    i["lifecycle_state"] = state.get(i["id"], "")
    print(json.dumps(i, sort_keys=True, separators=(",", ":")))
for k, v in sorted(json.load(open(work + "/memories.json")).items()):
    print(json.dumps({"_type": "memory", "key": k, "value": v}, sort_keys=True, separators=(",", ":")))
PY

if [ -f "$OUT" ] && cmp -s "$WORK/new.jsonl" "$OUT"; then
    touch "$OUT"
    echo "export-beads: unchanged"
    exit 0
fi
cp "$WORK/new.jsonl" "$OUT.tmp" && mv "$OUT.tmp" "$OUT" || exit 1

count=$(grep -c '"id":"sp-' "$OUT")
newest=$(grep '"id":"sp-' "$OUT" | python3 -I -c '
import json, sys
print(max((json.loads(l).get("created_at", "") for l in sys.stdin), default=""))')
cat >"$SPIRA_WIKI/raw/spira-beads/manifest.md" <<MANIFEST
# Spira beads — JSONL mirror

Machine-written by \`spira/export-beads.sh\`. **Do not hand-edit**: the next run overwrites it.

Snapshot: **$(date '+%Y-%m-%d %H:%M %Z')** — $count beads, newest created $newest.

\`spira.jsonl\` is the current mirror. A \`spira-pre-*.jsonl\` beside it is frozen history from before
a store reset, never current. Each bead carries \`lifecycle_state\` (authoritative) beside the
content store's \`status\`; dependencies and comments are not mirrored.
MANIFEST

if git -C "$SPIRA_WIKI" rev-parse --git-dir >/dev/null 2>&1; then
    git -C "$SPIRA_WIKI" add -- "$REL" raw/spira-beads/manifest.md \
        && git -C "$SPIRA_WIKI" commit -q -m "chore: spira beads JSONL mirror $(date +%F) ($count beads)" -- "$REL" raw/spira-beads/manifest.md \
        || { echo "export-beads: commit failed" >&2; exit 1; }
fi
echo "export-beads: wrote $count beads"
