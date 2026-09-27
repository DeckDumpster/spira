#!/usr/bin/env python3
"""unanswered.py -- threads where the operator spoke last and nobody answered.

Companion to answers.py; see cockpit/unanswered.sh for the why a thread whose newest comment
is theirs is an open obligation. Reads the attention-surface bead list on stdin and fetches
every comment on every candidate in ONE `bd sql` query, not one `bd comments <id>` process per
bead -- that fan-out is what took unanswered.sh to 14s against a loaded Dolt (sp-xsl8i).
"""

import datetime
import json
import os
import subprocess
import sys


def json_only(text):
    i = min((text.find(c) for c in "[{" if text.find(c) >= 0), default=-1)
    if i < 0:
        return None
    try:
        return json.loads(text[i:])
    except Exception:
        return None


def rows_of(doc, key):
    if doc is None:
        return []
    if isinstance(doc, list):
        return doc
    return doc.get(key) or []


def bd(cfg, args):
    try:
        out = subprocess.run(
            [cfg["bd"], "-C", cfg["db"]] + args,
            capture_output=True, text=True, timeout=30,
            env=dict(os.environ, BEADS_NO_AUTO_IMPORT="1"),
        ).stdout
    except Exception:
        return None
    return json_only(out)


def sql_in(ids):
    return ",".join("'%s'" % i.replace("'", "''") for i in ids)


def comment_threads(cfg, ids):
    """Every comment on these ids, in ONE query. {id: [comment, ...]}, oldest first."""
    if not ids:
        return {}
    rows = rows_of(bd(cfg, ["sql", "--json",
        "SELECT issue_id, id, author, text, created_at FROM comments WHERE issue_id IN (%s) "
        "ORDER BY created_at" % sql_in(ids)]), "rows")
    threads = {}
    for r in rows:
        threads.setdefault(r.get("issue_id") or "", []).append(r)
    return threads


def whose_turn(rows, human):
    """The operator's own last word on this thread, or None if it is not their turn.

    Sorted, not taken positionally: `created_at` is second-resolution, so two comments in the
    same second are a tie nothing orders. The tie resolves toward "he is owed a reply" -- a
    thread wrongly listed costs a glance; a thread wrongly dropped is the silence this file
    exists to end. Conditionally sorted -- an unstamped comment sorts before every stamped
    one, so an incomplete thread keeps arrival order rather than being reshuffled around a
    blank.
    """
    if rows and all(len(c.get("created_at") or "") >= 16 for c in rows):
        rows = sorted(rows, key=lambda c: c["created_at"])
        newest = rows[-1]["created_at"]
        tail = [c for c in rows if c["created_at"] == newest]
    else:
        tail = rows[-1:]
    his = [c for c in tail if (c.get("author") or "") == human]
    return his[-1] if his else None


def main():
    args = dict(a.split("=", 1) for a in sys.argv[1:] if "=" in a)
    cfg = {
        "bd": args.get("bd") or os.environ.get("BD_BIN") or "bd",
        "db": args.get("db") or os.environ.get("SPIRA_DB") or "",
    }
    ask = args.get("ask_label") or os.environ.get("SPIRA_ASK_LABEL") or "needs-operator"  # literal-ok: Python fallback for direct invocation without conf.sh
    human = (args.get("human") or os.environ.get("COCKPIT_HUMAN")
             or os.environ.get("SPIRA_OPERATOR_ACTOR") or "operator")
    now = datetime.datetime.now(datetime.timezone.utc)

    issues = rows_of(json_only(sys.stdin.read()), "issues")
    cand_ids = [i.get("id") for i in issues
                if ({"insight", ask, "overseer"} & set(i.get("labels") or []))
                and (i.get("comment_count") or 0) > 0 and i.get("id")]

    threads = comment_threads(cfg, cand_ids)

    out = []
    for ident in cand_ids:
        last = whose_turn(threads.get(ident, []), human)
        if not last:
            continue
        ts = last.get("created_at") or ""
        try:
            t = datetime.datetime.fromisoformat(ts.replace("Z", "+00:00"))
            mins = int((now - t).total_seconds() / 60)
        except Exception:
            mins = -1
        out.append((mins, ident, ts[:16], (last.get("text") or "").strip().replace("\n", " ")[:70]))

    out.sort(reverse=True)
    for mins, ident, ts, text in out:
        print("%s\t%d\t%s\t%s" % (ident, mins, ts, text))
    return 0


if __name__ == "__main__":
    sys.exit(main())
