#!/usr/bin/env bash
# law-synth.sh — regenerate wiki/notes/common-law.md from the statute book.
#
# rule.sh's default synth hook (see SPIRA_WIKI_HOOK in conf.sh). REGENERATED WHOLE, NEVER
# PATCHED: this page is a derived copy of `bd memories`, so editing it by hand does nothing —
# the next `rule.sh enact` overwrites it. Amend the statute instead.
#
# WITH NO SPIRA_WIKI CONFIGURED, this exits 0 having written nothing: the statute book itself
# (one beads database) is the only thing every aeon actually reads at summon, so a harness
# with no wiki checkout is not missing a dependency, it just has no second copy to keep in
# sync (law-a-hand-fix-names-its-root-cause).
set -uo pipefail

SPIRA_HOME_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$SPIRA_HOME_DIR/conf.sh"
export BEADS_NO_AUTO_IMPORT=1
DB="$SPIRA_DB"
OUT=wiki/notes/common-law.md

if [ -z "${SPIRA_WIKI:-}" ]; then
    echo "law-synth: SPIRA_WIKI is not configured — nothing to synthesise."
    exit 0
fi
OUT_ABS="$SPIRA_WIKI/$OUT"

command -v bd >/dev/null || { echo "law-synth: bd not on PATH" >&2; exit 1; }
[ -d "$DB/.beads" ] || { echo "law-synth: $DB has no .beads — refusing to guess a database" >&2; exit 1; }
statutes=$(bd -C "$DB" memories --json 2>/dev/null) || {
  echo "law-synth: could not read the statute book at $DB" >&2; exit 1; }

# REFUSE TO WRITE FROM A NEAR-EMPTY DATABASE. A store with .beads but zero law- memories is
# almost certainly the wrong one; the same guard the beads exporter applies to sp- beads.
# Two checks, in order: zero law- entries refuses unconditionally, and a count under half the
# committed page's heading count refuses unless LAW_SYNTH_OVERRIDE=1 (a floor that still
# allows legitimate bulk retirements while catching a near-empty replacement store).
_law_count=$(printf '%s\n' "$statutes" | python3 -c '
import json, sys
book = json.load(sys.stdin)
n = sum(1 for k, v in book.items() if isinstance(v, str) and k.startswith("law-"))
print(n)
' 2>/dev/null) || _law_count=0

if [ "${_law_count:-0}" -eq 0 ]; then
    echo "law-synth: $DB has no law- statutes — refusing to overwrite the wiki page." >&2
    echo "           Point SPIRA_DB at the correct store, or set LAW_SYNTH_OVERRIDE=1 to force." >&2
    exit 1
fi

# Count the committed page's headings (each statute = one ### heading). If the page does not
# exist at HEAD (e.g. a fresh wiki checkout), _committed_count=0 and the floor check below is
# skipped — correct for a first write; the zero-statute guard above already covers empty dbs.
_committed_count=$(git -C "$SPIRA_WIKI" show "HEAD:$OUT" 2>/dev/null | grep -c '^### ') || _committed_count=0

if [ "${LAW_SYNTH_OVERRIDE:-0}" != "1" ] && \
   [ "${_committed_count:-0}" -gt 0 ] && \
   [ "${_law_count}" -lt $(( _committed_count / 2 )) ]; then
    echo "law-synth: refusing — database has ${_law_count} statute(s) but committed page has ${_committed_count}." >&2
    echo "           This looks like the wrong database. Set LAW_SYNTH_OVERRIDE=1 to force." >&2
    exit 1
fi

# STATUTES ARRIVE ON A FILE, NOT IN argv: the book crossed MAX_ARG_STRLEN (128KB, the
# per-argument cap) at 114 statutes, and execve then fails with "Argument list too long".
_statutes_file="$(mktemp)"
trap 'rm -f "$_statutes_file"' EXIT
printf '%s' "$statutes" > "$_statutes_file"

mkdir -p "$(dirname "$OUT_ABS")"
python3 - "$_statutes_file" "$(TZ="${SPIRA_TZ:-UTC}" date '+%Y-%m-%d')" "$OUT_ABS" <<'PY'
import json, sys, textwrap

with open(sys.argv[1]) as _f:
    book = json.load(_f)
today, out = sys.argv[2], sys.argv[3]
# `bd memories --json` mixes metadata into the map (e.g. schema_version: 1), so keep
# only string-valued entries.
mem = {k: v.strip() for k, v in sorted(book.items()) if isinstance(v, str)}
laws = {k: v for k, v in mem.items() if k.startswith("law-")}
local = {k: v for k, v in mem.items() if not k.startswith("law-")}

def title(key):
    return key[len("law-"):].replace("-", " ").capitalize()

body = []
body.append("---")
body.append("type: note")
body.append("created: 2026-08-30")
body.append(f"updated: {today}")
body.append("tags: [governance, spira, generated, law]")
body.append("aliases: [Statutes, The statute book]")
body.append("---")
body.append("")
body.append("# Common law")
body.append("")
body.append(
    "**Generated — do not edit.** Regenerated whole by `spira/law-synth.sh` from the "
    "beads database, which is the source of truth for codified judgement. Editing this "
    "page has no effect; the next run overwrites it."
)
body.append("")
body.append(
    "These statutes are injected into **every aeon's session at summon** by "
    "the harness's `spira/aeon.sh`, which reads them whole out of the same database rather than "
    "from any copy. That is the whole point: a preference stated once should never be "
    "re-derived by an agent, and should never come back as the same decision twice."
)
body.append("")
body.append(
    f"**{len(laws)} statutes** in force as of {today}. They live in one beads database, "
    "which is the source of truth; this page is its git-backed copy. Amend one with "
    "the harness's `rule.sh enact <slug> \"<text>\"`, which regenerates this page as its "
    "second step."
)
body.append("")
body.append("## The ladder")
body.append("")
body.append(
    "A rule tightens each time it is re-violated: **custom → advisory → statute → "
    "mechanism**. A statute on this page has reached rung three — written law that every "
    "agent reads. Rung four is a program that refuses, and the promotion trigger is a "
    "second violation."
)
body.append("")
body.append("## Statutes")
body.append("")

for k, v in laws.items():
    body.append(f"### {title(k)}")
    body.append("")
    body.append(f"`{k}`")
    body.append("")
    body.extend(textwrap.wrap(v, 92))
    body.append("")

if local:
    body.append("## Other memories")
    body.append("")
    body.append(
        "Not statutes — standard operating procedures (`sop-`), which only the Ops "
        "persona is given, and operational facts kept beside them. Listed so this page "
        "is a complete backup of the store."
    )
    body.append("")
    for k, v in local.items():
        body.append(f"- **`{k}`** — {' '.join(v.split())}")
    body.append("")

open(out, "w").write("\n".join(body))
print(f"law-synth: wrote {out} — {len(laws)} statutes, {len(local)} other memories")
PY
