#!/usr/bin/env python3
"""answers.py -- what the operator SAID, since the last time anybody looked.

The operator speaks to this harness in exactly two ways, and both are writes to a bead:

  a CLOSE carrying a reason   -- the verdict on an escalation; the reason IS the answer
  a COMMENT on any bead       -- including one they can never close, because an FYI is
                                 created closed and closing it again means nothing

Only the first had a delivery path. A comment on an FYI reached no session at all: the
close-cursor below cannot see one, because a comment does NOT bump the bead's updated_at
and the row `bd list` returns carries `comment_count` but not the bodies. So a reply sat
unread until the operator said so themselves -- the second time the same escalation queue
lost an answer, which is why this is a program and not a note.

WHOSE VOICE. A close made by an agent is indistinguishable from the operator's on the row:
there is no `closed_by` on it, and the Dolt committer is the literal string `beads` whatever
BEADS_ACTOR says. But beads DOES record the actor, in its audit events -- `bd history <id>
--events` yields an event of type `closed` carrying `actor`. That is authoritative and needs
no new convention, so both legs here filter on it: a verdict is reported only when the
operator's own actor name is on the event. Without that a session acts on its own echo, and
the failure is silent because the text of the announcement looks exactly like a real answer.
The panel must therefore CLOSE as the operator, not merely comment as them.

A CURSOR, NOT A TAIL. There is no file to follow; the record is a row. Each leg keeps a
high-water mark and reports only what is past it, so this is quiet when nothing has been
answered -- which is most of the time, and what makes it safe to run from a Monitor or at
session start. A bare timestamp cannot express "reported everything up to this second": two
writes can share one second, and one of them would be replayed forever or lost. So the mark
carries the timestamp AND the keys already reported AT that timestamp. A mark that is gone
while its sibling survives is treated as CLEARED rather than new and adopts the sibling's
position: the remedy for a poisoned state file is to delete it, and seeding at `now` instead
would silently swallow every answer given since the last read.

Reads the bead list on stdin, because which database to address is the caller's decision
and there is one place that decides it. Shells out only for the bodies, one process at a
time: an unbounded fan-out over every bead spawned hundreds of concurrent `bd` processes
against a single server and drove a four-core box into swap.
"""

import json
import os
import subprocess
import sys

# The fan-out ceiling. Sequential already bounds concurrency to one; this bounds the WALL
# CLOCK of a session-start hook when a cursor is far behind or a database is large. It is
# announced on stderr when it binds, never silently obeyed: a check that quietly stops
# looking reports "nothing was answered" for the one reason that is not true.
SCAN_MAX = int(os.environ.get("SPIRA_ANSWER_SCAN_MAX", "200"))


def json_only(text):
    """`bd --json` can print warnings on stdout BEFORE the payload."""
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


class Mark:
    """A high-water mark that survives a tie.

    `ts` is the newest thing reported. `seen` is every key reported AT that exact ts, which
    is what makes a second write in the same second reportable exactly once rather than
    never or forever. Older formats -- a bare timestamp on one line -- are read as a mark
    with no ties, so an installed cursor keeps working.
    """

    def __init__(self, path):
        self.path = path
        self.ts, self.seen = "", set()
        try:
            with open(path) as fh:
                raw = fh.read().strip()
        except Exception:
            return
        if not raw:
            return
        doc = json_only(raw)
        if isinstance(doc, dict):
            self.ts = doc.get("ts") or ""
            self.seen = set(doc.get("seen") or [])
        else:
            self.ts = raw.splitlines()[0].strip()

    def fresh(self):
        """True when there is no mark yet, so the caller SEEDS rather than replays history."""
        return not self.ts

    def adopt(self, ts):
        """Take another mark's position as this one's, with no ties carried over.

        A LOST MARK IS COVERED BY ITS SIBLING. Seeding a missing mark at `now` is right for a
        watcher being armed for the first time and wrong for one whose state was cleared: the
        remedy for a poisoned state file is to delete it, and a silent seed then swallows
        every answer given between the last read and the deletion — invisibly, because a
        watcher that has lost its place and one with nothing to say print exactly the same
        thing. Adopting the surviving sibling's timestamp re-reports a little rather than
        losing anything, which is the direction an escalation queue must fail in.

        No ties are adopted, because the sibling's ties are keys from the other leg and mean
        nothing here; the cost is at most one duplicate line at that exact second.
        """
        self.ts, self.seen = ts, set()

    def is_new(self, ts, key):
        if not ts:
            return False
        if ts > self.ts:
            return True
        return ts == self.ts and key not in self.seen

    def write(self, reported, now):
        """`reported` is [(ts, key), ...] -- everything this pass emitted."""
        ts = self.ts
        for t, _ in reported:
            if t > ts:
                ts = t
        if not ts:
            ts = now
        seen = {k for t, k in reported if t == ts}
        if ts == self.ts:
            seen |= self.seen
        tmp = self.path + ".tmp"
        with open(tmp, "w") as fh:
            json.dump({"ts": ts, "seen": sorted(seen)}, fh)
        os.replace(tmp, self.path)
        self.ts, self.seen = ts, seen


def write_witness(path, rows):
    """Record the ids this pass actually saw, for a health assertion to grep.

    SIGHT AND POSITION ARE DIFFERENT FACTS and are kept in different files. A cursor names a
    bead only in the instant one is reported, so a check over it would call a healthy quiet
    watcher DEGRADED; and when they shared one file, clearing a poisoned state took the proof
    of sight with it. This is written on every pass and holds nothing but ids.

    ONLY THE WATCHER WRITES IT. The assertion answers "can THAT process see", so a session
    hook refreshing the same file would let a dead watcher read healthy. That is why the path
    arrives as an argument rather than being derived here — the hook does not pass one.

    An empty payload TRUNCATES rather than leaving the last good list standing: a query that
    returned no bead of ours is exactly the state the assertion exists to catch, and holding
    the previous answer would report sight that is no longer proved. A read that FAILED never
    reaches here — the caller holds its silence instead, because a failed read is not an empty
    database and turning one into an alarm is the false kind (law-alerts-must-be-actionable).
    """
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        for r in rows:
            ident = r.get("id") or ""
            if ident:
                fh.write(ident + "\n")
    os.replace(tmp, path)


def bd(cfg, args, timeout=30):
    try:
        out = subprocess.run(
            [cfg["bd"], "-C", cfg["db"]] + args,
            capture_output=True, text=True, timeout=timeout,
            env=dict(os.environ, BEADS_NO_AUTO_IMPORT="1"),
        ).stdout
    except Exception:
        return None
    return json_only(out)


def cap(candidates, what):
    if len(candidates) <= SCAN_MAX:
        return candidates
    sys.stderr.write(
        "answers.py: %d beads to check for %s, scanning the %d most recently updated "
        "(raise SPIRA_ANSWER_SCAN_MAX)\n" % (len(candidates), what, SCAN_MAX))
    return sorted(candidates, key=lambda r: r.get("updated_at") or "", reverse=True)[:SCAN_MAX]


def closed_by(cfg, ident):
    """The actor beads recorded for the most recent close of this bead, or None.

    `bd history --events` is the only place the closing actor exists; the issue row has no
    field for it. None means the audit trail could not be read, which is NOT the same as
    "somebody else closed it" -- the caller reports those separately rather than treating an
    unreadable trail as either an answer or a silence.
    """
    doc = bd(cfg, ["history", ident, "--events", "--json"])
    evs = rows_of(doc, "events")
    if not evs:
        return None
    closes = [e for e in evs if (e.get("event_type") or "") == "closed"]
    if not closes:
        return None
    closes.sort(key=lambda e: e.get("created_at") or "")
    return closes[-1].get("actor") or None


def self_closed_ids(path):
    """Ids the harness closed itself, recorded by cockpit/resolve.sh as it closed them.

    A cheap PRE-FILTER only. It saves a `bd history` call on the common case — every close
    the harness makes goes through resolve.sh — and it is never the thing that decides: a
    close made by an agent NOT through resolve.sh is absent from this file and must still be
    caught, which is what the audit event below is for. Trusting the file alone is how an
    agent's own close came back as an answer the operator never gave.
    """
    if not path:
        return set()
    try:
        with open(path) as fh:
            return {line.strip() for line in fh if line.strip()}
    except Exception:
        return set()


def verdicts(cfg, rows, mark):
    """Closes made BY THE OPERATOR since the mark.

    Returns [(ts, id, title, reason, is_rejection, is_suit), ...] where is_rejection is True
    when the operator dismissed the bead as not theirs to decide, and is_suit is True when the
    bead is a lawsuit. A rejected premise must never render as RYAN ANSWERED -- the scar is
    answered-since.sh announcing four dismissals as affirmations on 2026-09-10, prompting a
    session to enact statutes for things Ryan had refused to be asked about (sp-kw9bo,
    escalation-taxonomy-2026-09-09.md §3). A suit verdict (retire/amend/uphold) must render
    with its own label so the session knows the statute book changed.

    Detection uses the `premise-rejected` label OR a `premise-rejected:` prefix on the
    close_reason, both of which the panel sets when the operator presses `p`.
    """
    ask, overseer = cfg["ask_label"], "overseer"
    mine = self_closed_ids(cfg.get("self_closed"))
    cands = []
    for r in rows:
        if (r.get("status") or "") != "closed":
            continue
        labels = set(r.get("labels") or [])
        # An FYI is CREATED closed, so its close is never a verdict -- only its comments are.
        if "insight" in labels:
            continue
        if ask not in labels and overseer not in labels:
            continue
        if (r.get("id") or "") in mine:
            continue
        ts = r.get("closed_at") or r.get("updated_at") or ""
        if not mark.is_new(ts, r.get("id") or ""):
            continue
        cands.append(r)

    out = []
    for r in cap(cands, "a verdict"):
        ident = r.get("id") or "?"
        if closed_by(cfg, ident) != cfg["operator_actor"]:
            continue
        reason = (r.get("close_reason") or "").strip() or "(closed with no reason given)"
        labels = set(r.get("labels") or [])
        is_rejection = ("premise-rejected" in labels
                        or reason.startswith("premise-rejected:"))
        is_suit = "ask-suit" in labels
        out.append((r.get("closed_at") or r.get("updated_at") or "",
                    ident, (r.get("title") or "")[:90], reason, is_rejection, is_suit))
    out.sort()
    return out


def comments(cfg, rows, mark):
    """Comments written BY THE OPERATOR since the mark. [(ts, id, title, text, key), ...]

    The trailing key is the mark's tie-breaker: a comment does not bump the bead's
    updated_at, so the comment's own id is what distinguishes two writes in one second.

    Every bead that reaches the operator's attention surface is a candidate, FYIs included
    and whatever their status: the whole defect this closes is that an FYI is closed at birth
    and carries neither the escalation label nor a transition left to report.
    """
    ask = cfg["ask_label"]
    cands = [r for r in rows
             if ({"insight", ask, "overseer"} & set(r.get("labels") or []))
             and (r.get("comment_count") or 0) > 0]

    out = []
    for r in cap(cands, "a comment"):
        ident = r.get("id") or "?"
        thread = rows_of(bd(cfg, ["comments", ident, "--json"]), "comments")
        for c in thread:
            if (c.get("author") or "") != cfg["operator_actor"]:
                continue
            ts = c.get("created_at") or ""
            key = "%s/%s" % (ident, c.get("id") or ts)
            if not mark.is_new(ts, key):
                continue
            out.append((ts, ident, (r.get("title") or "")[:90],
                        (c.get("text") or "").strip(), key))
    out.sort()
    return out


def main():
    args = dict(a.split("=", 1) for a in sys.argv[1:] if "=" in a)
    cfg = {
        "bd": args.get("bd") or os.environ.get("BD_BIN") or "bd",
        "db": args.get("db") or os.environ.get("SPIRA_DB") or "",
        "ask_label": args.get("ask_label") or os.environ.get("SPIRA_ASK_LABEL") or "needs-operator",  # literal-ok: Python fallback for direct invocation without conf.sh
        "operator_actor": (args.get("operator_actor")
                           or os.environ.get("SPIRA_OPERATOR_ACTOR") or "operator"),
        "self_closed": args.get("self_closed") or os.environ.get("SELF_CLOSED") or "",
    }
    who = (args.get("operator") or os.environ.get("SPIRA_OPERATOR") or "the operator").upper()
    fmt = args.get("format") or "monitor"
    now = args.get("now") or __import__("datetime").datetime.now(
        __import__("datetime").timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    rows = rows_of(json_only(sys.stdin.read()), "issues")

    if args.get("witness"):
        write_witness(args["witness"], rows)

    vmark = Mark(args["verdict_cursor"]) if args.get("verdict_cursor") else None
    cmark = Mark(args["comment_cursor"]) if args.get("comment_cursor") else None

    # A mark missing while its sibling survives is a CLEARED mark, not a new one, so it takes
    # the sibling's position rather than seeding at `now` and swallowing the window between.
    if vmark is not None and cmark is not None:
        if vmark.fresh() and not cmark.fresh():
            vmark.adopt(cmark.ts)
        elif cmark.fresh() and not vmark.fresh():
            cmark.adopt(vmark.ts)

    # FIRST RUN SEEDS SILENTLY -- a first run being both marks absent, after the line above.
    # Without this, arming a watcher replays every historical answer as though it had just
    # landed, and a burst of stale verdicts is how a real one goes unread.
    vs, cs = [], []
    if vmark is not None:
        if vmark.fresh():
            vmark.write([], now)
        else:
            vs = verdicts(cfg, rows, vmark)
            vmark.write([(t, i) for t, i, _, _, _, _ in vs], now)
    if cmark is not None:
        if cmark.fresh():
            cmark.write([], now)
        else:
            cs = comments(cfg, rows, cmark)
            cmark.write([(t, k) for t, _, _, _, k in cs], now)

    def _suit_label(reason):
        """One-line description of a suit verdict for the watcher output."""
        if reason.startswith("retire: "):
            slug = reason[len("retire: "):].split()[0]
            return "RETIRED %s" % slug
        if reason.startswith("amend: "):
            slug = reason[len("amend: "):].split()[0]
            return "AMENDED %s" % slug
        return "UPHELD"

    if fmt == "session":
        if not vs and not cs:
            return 0
        print("## %s spoke while you were away (%d)" % (who.title(), len(vs) + len(cs)))
        print()
        for ts, ident, title, reason, is_rejection, is_suit in vs:
            stamp = ts[:16].replace("T", " ")
            if is_rejection:
                why = reason[len("premise-rejected:"):].strip() if reason.startswith("premise-rejected:") else reason
                print("- **PREMISE REJECTED on `%s`** (%s) — %s" % (ident, stamp, title))
                body = "not his to decide; proceed on your default"
                if why:
                    body = "%s. reason: %s" % (body, why)
                print("  > %s" % body.replace("\n", "\n  > "))
            elif is_suit:
                label = _suit_label(reason)
                print("- **%s (`%s`)** (%s) — %s" % (label, ident, stamp, title))
                print("  > %s" % reason.replace("\n", "\n  > "))
            else:
                print("- **verdict on `%s`** (%s) — %s" % (ident, stamp, title))
                print("  > %s" % reason.replace("\n", "\n  > "))
        for ts, ident, title, text, _ in cs:
            print("- **comment on `%s`** (%s) — %s" % (ident, ts[:16].replace("T", " "), title))
            print("  > %s" % text.replace("\n", "\n  > "))
        print()
        print("These are answers, already given. Act on them; do not re-ask. If one")
        print("generalises, enact it as a statute in this session.")
        return 0

    # Headlines open with the answer's own UTC time. Without it `watchd.sh notify` dates a
    # backlog from first sighting, so answers given during an outage — the most overdue —
    # all look new at recovery and get a fresh grace period. Fixed-width, so a reader sorts
    # it as text; the actionable word stays on the line for SPIRA_ACTIONABLE.
    def at(ts):
        return "[%s] " % ts if ts else ""

    for ts, ident, title, reason, is_rejection, is_suit in vs:
        if is_rejection:
            why = reason[len("premise-rejected:"):].strip() if reason.startswith("premise-rejected:") else reason
            print("%sREJECTED THE PREMISE %s — not his to decide; proceed on your default" % (at(ts) + who + " ", ident), flush=True)
            if why:
                print("  reason: %s" % why, flush=True)
        elif is_suit:
            label = _suit_label(reason)
            print("%s%s %s (%s) — %s" % (at(ts), who, label, ident, title), flush=True)
        else:
            print("%s%s ANSWERED %s — %s" % (at(ts), who, ident, title), flush=True)
            print("  verdict: %s" % reason, flush=True)
    for ts, ident, title, text, _ in cs:
        print("%s%s COMMENTED ON %s — %s" % (at(ts), who, ident, title), flush=True)
        for line in text.splitlines() or [""]:
            print("  %s" % line, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
