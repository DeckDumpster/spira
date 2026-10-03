#!/usr/bin/env bash
#
# boundary.sh — render `boundary` into every document that publishes it.
#
#   boundary.sh render          the table, to stdout
#   boundary.sh write           into each target's marker region; prints what moved
#   boundary.sh check           exit 1 if any target's region is stale
#
# WHY A RENDERER AND NOT TWO HAND-KEPT TABLES. A boundary documented on only one side of
# itself is not a boundary. The shape of the failure is already on the record here: the
# cockpit's shell tools were pointed at one database while the panel still wrote to another,
# and within the hour a conversation was split across two stores and an answer reached
# nobody. Two copies of an ownership table drift the same way and just as quietly, so there
# is one source — `boundary` — and both documents carry a generated region.
#
# REGENERATED WHOLE, NEVER PATCHED. Editing a marker region does nothing: the next run
# overwrites it, and `check` fails the gate in the meantime. Amend `boundary` instead.
#
# THE WIKI TARGET IS OPTIONAL, AND THAT IS THE POINT. This file ships with the harness, and
# the harness must start from a clean clone with no wiki anywhere on the machine — rule 2 of
# the boundary it renders. A missing wiki page is skipped in silence; a missing README is an
# error, because that one is the harness's own.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MANIFEST="${SPIRA_BOUNDARY:-$HERE/boundary}"

# The README is resolved relative to THIS script, so a run inside an aeon's worktree edits
# that worktree and not the shared checkout.
#
# THE WIKI TARGET IS CONFIGURED, NOT DERIVED. It used to be found by walking up from here,
# which worked only while the two repositories shared a parent — and a derived path that
# happens to resolve is indistinguishable from one that was meant. `SPIRA_WIKI` is empty by
# default, which is the state of every clone that has no wiki at all.
. "$HERE/conf.sh"
README="$(cd "$HERE/.." && pwd -P)/README.md"
WIKI_REL="${SPIRA_WIKI_BOUNDARY_REL:-wiki/projects/spira/repo-boundary.md}"
WIKI="${SPIRA_WIKI:+$SPIRA_WIKI/$WIKI_REL}"

BEGIN='<!-- BOUNDARY:BEGIN -->'
END='<!-- BOUNDARY:END -->'

[ -f "$MANIFEST" ] || { echo "boundary: no manifest at $MANIFEST" >&2; exit 1; }

render() {
    python3 - "$MANIFEST" <<'PY'
import sys

OWNERS = [
    ("spira",   "### Ships in `spira` — the harness",
                "Generic mechanism. A colleague clones this and it carries none of the "
                "operator's data."),
    ("brain",   "### Stays in `brain` — the wiki",
                "Everything whose write target is a page. It may read the harness freely; "
                "the harness may not require it."),
    ("neither", "### In neither repository — data",
                "It belongs to whoever runs the harness. No shared repository holds it, and "
                "no beads database is ever public."),
]

rows = []
for line in open(sys.argv[1]):
    line = line.strip()
    if not line or line.startswith("#") or "|" not in line:
        continue
    parts = [p.strip() for p in line.split("|", 2)]
    if len(parts) != 3:
        sys.exit(f"boundary: line needs three fields: {line}")
    owner, path, what = parts
    if owner not in {o for o, _, _ in OWNERS}:
        sys.exit(f"boundary: unknown owner {owner!r}: {line}")
    rows.append((owner, path, what))

if not rows:
    sys.exit("boundary: manifest has no rows")

out = []
for owner, heading, blurb in OWNERS:
    mine = [r for r in rows if r[0] == owner]
    if not mine:
        continue
    out += [heading, "", blurb, "", "| path | what it is |", "|---|---|"]
    for _, path, what in mine:
        # A cell with a space in it is prose, not a path, and backticking prose makes the
        # data rows read as if the database were a file.
        cells = ", ".join(
            (f"`{p.strip()}`" if " " not in p.strip() else p.strip())
            for p in path.split(",")
        )
        out.append(f"| {cells} | {what.replace('|', '\\|')} |")
    out.append("")

print("\n".join(out).rstrip())
PY
}

# Replace the marker region of one file. Prints "changed" or "same"; a file without the
# markers is an error rather than a silent no-op, because a document that quietly stopped
# publishing the boundary is the failure this whole file exists to prevent.
apply() {                # apply <file> <rendered-file>
    python3 - "$1" "$2" "$BEGIN" "$END" <<'PY'
import sys
path, body_path, begin, end = sys.argv[1:5]
src = open(path).read()
body = open(body_path).read().rstrip()
i, j = src.find(begin), src.find(end)
if i < 0 or j < 0 or j < i:
    sys.exit(f"boundary: {path} has no {begin} / {end} pair")
new = src[:i] + begin + "\n\n" + body + "\n\n" + src[j:]
if new == src:
    print("same")
else:
    open(path, "w").write(new)
    print("changed")
PY
}

# Every spira-owned path must exist on the filesystem: a deletion that leaves its row
# standing passes the staleness check undetected, which is how the rows this bead removes
# survived the gate for three commits after the files were deleted.
fscheck() {
    REPO="$(cd "$HERE/.." && pwd -P)"
    python3 - "$MANIFEST" "$REPO" <<'PY'
import sys, os

manifest, repo = sys.argv[1], sys.argv[2]
rc = 0
n = 0
for line in open(manifest):
    line = line.strip()
    if not line or line.startswith("#") or "|" not in line:
        continue
    parts = [p.strip() for p in line.split("|", 2)]
    if len(parts) != 3:
        continue
    owner, path_field, _ = parts
    if owner != "spira":
        continue
    for p in path_field.split(","):
        p = p.strip()
        if " " in p:
            continue  # prose, not a filesystem path
        full = os.path.join(repo, p.rstrip("/"))
        if not os.path.exists(full):
            print(f"boundary: {p!r} is in the manifest but does not exist in the repository", file=sys.stderr)
            rc = 1
        n += 1
print(n)
sys.exit(rc)
PY
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
BODY="$TMP/body.md"

case "${1:-write}" in
render)
    render
    ;;

write)
    render > "$BODY" || exit 1
    [ -f "$README" ] || { echo "boundary: no README at $README" >&2; exit 1; }
    res="$(apply "$README" "$BODY")" || exit 1
    echo "boundary: README $res"
    if [ -f "$WIKI" ]; then
        res="$(apply "$WIKI" "$BODY")" || exit 1
        echo "boundary: wiki page $res"
    elif [ -n "$WIKI" ]; then
        echo "boundary: no wiki page at $WIKI — skipped"
    else
        echo "boundary: SPIRA_WIKI is unset — no wiki page to publish into"
    fi
    ;;

check)
    render > "$BODY" || exit 1
    rc=0
    N="$(fscheck)" || rc=1
    for f in "$README" "$WIKI"; do
        [ -f "$f" ] || continue
        cp "$f" "$TMP/copy"
        if [ "$(apply "$TMP/copy" "$BODY")" = "changed" ]; then
            echo "boundary: $f is stale — run spira/boundary.sh write" >&2
            rc=1
        fi
    done
    [ -f "$README" ] || { echo "boundary: no README at $README" >&2; rc=1; }
    [ "$rc" = 0 ] && echo "fence: boundary checked $N shipped paths and every target"
    exit $rc
    ;;

*)
    echo "usage: boundary.sh {render|write|check}" >&2
    exit 2
    ;;
esac
