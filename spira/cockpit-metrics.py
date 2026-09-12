#!/usr/bin/env python3
"""cockpit-metrics.py — the four Spira numbers this build already got wrong.

    cockpit-metrics.py <sentinel.log> <aeon-ledger.log> [window-hours]

Prints flat `KEY=value` lines on stdout. It is pure: two files in, keys out, no bead
queries and no git. That is what makes it testable against fixtures, which matters
because every number here is a claim about whether the harness is working, and a
dashboard that lies confidently is worse than no dashboard.

WHY THESE FOUR, AND NOT A STAT DUMP
-----------------------------------
The selection rule is the operator's: instrument the numbers that SURPRISED us. For Spira those
are the ones this build got wrong, each expensively:

  born vs lived    the first aeon the sentinel ever summoned was killed inside the same
                   second, after 1.6s of CPU: the service is Type=oneshot with the default
                   KillMode=control-group, so systemd tore down the whole cgroup when the
                   pass finished. The sentinel reported "summoned" every two minutes into
                   an empty log. A summon is not a worker.

  false ACTs       CHECK 6 re-landed one branch forever because `git branch -D` cannot
                   delete a branch a worktree holds, and the error went to /dev/null:
                     06:29:33 ACT landed spira/sp-stranded
                     06:31:35 ACT landed spira/sp-stranded
                   `acted` was therefore never 0, and CHECK 8 fires only when `acted` is 0,
                   so a harness spinning on one branch could never notice it was starved.
                   The reclaim probe had already done the same thing by matching its own
                   idle message. Twice is a pattern; this is the meter for it.

  passes since     CHECK 8 is the only tier that reaches a model, and the two bugs above
  judgement        both worked by making it unreachable. "It has never fired" and "it fired
                   40 passes ago" are the same picture from the outside and mean opposite
                   things, so the number is rendered rather than inferred.

  closed vs        law-closed-is-not-landed, as a running total instead of a rule. Computed
  landed           by cockpit.sh, which has the commit graph; not here.

WHAT COUNTS AS A FALSE ACT
--------------------------
An ACT that repeats in the IMMEDIATELY FOLLOWING pass. A real state transition happens
once: a branch lands once, a bead is poisoned once, a lease is reclaimed once. The same
action reported twice two minutes apart means the first one did not take, which is the
exact shape of both bugs above.

Consecutive, not merely repeated within the window: reopening the same bead after an aeon
has failed it again is a genuine second action, and those passes are an hour apart, not
one apart. Summoning is excluded outright — an aeon is summoned every pass that has ready
work and a free slot, and that is the system working.

A missing or unreadable input renders `?` for the keys that depend on it, never 0. The
first version of the town collector returned 0 from its exception handler, so a broken
parser displayed as "all clear" and displaced the suspicion that would have prompted a
look.
"""
import os
import re
import sys
from datetime import datetime, timedelta, timezone

# `<iso8601> spira: state: ...` opens every pass, unconditionally and before any check can
# exit early. `pass complete` does NOT: CHECK 7 exits straight after summoning, so counting
# passes by their completion line would undercount exactly the busy passes.
PASS_RE = re.compile(r"^(\S+) spira: state: ")
ACT_RE = re.compile(r"^(\S+) spira: ACT (.*)$")
LEDGER_RE = re.compile(r"^(\S+) (born|awake|done) (\S+)(?: (.*))?$")

# Legitimately once per pass; not evidence of anything failing.
NOT_AN_EVENT = (re.compile(r"^summoned a \S+ aeon$"), re.compile(r"^invoked reflection$"))

# CHECK 8's precondition, read off the state line: open work, nothing ready, nothing
# running. Counting it is what separates "judgement never fired" from "judgement was never
# NEEDED" — two opposite health states that rendered as the same scary number, and one of
# them was a real bug that hid here for 76 passes.
STARVED_RE = re.compile(r"state: goal=\S+ open=(\d+) plan_ready=(\d+) in_progress=(\d+)")


def parse_ts(s):
    try:
        return datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except (ValueError, TypeError):
        return None


def read(path):
    with open(path, "r", errors="replace") as f:
        return f.read().splitlines()


def sentinel_metrics(lines, since):
    """passes, acts, false acts, and how long since the judgement tier fired."""
    starved = 0          # passes meeting CHECK 8's precondition: work open, none ready, none running
    passes = []          # one list of ACT texts per pass, oldest first
    judged_at = None     # index of the pass in which reflection was last invoked
    for line in lines:
        m = PASS_RE.match(line)
        if m:
            ts = parse_ts(m.group(1))
            if ts is None or ts < since:
                # A pass older than the window closes any pass being accumulated and
                # starts nothing, so out-of-window ACTs are never attributed to an
                # in-window pass — which would manufacture a false ACT at the boundary.
                passes = []
                judged_at = None
                starved = 0
                continue
            passes.append([])
            sm = STARVED_RE.search(line)
            if sm:
                o, r, ip = (int(x) for x in sm.groups())
                if o > 0 and r == 0 and ip == 0:
                    starved += 1
            continue
        m = ACT_RE.match(line)
        if m and passes:
            text = m.group(2).strip()
            passes[-1].append(text)
            if text == "invoked reflection":
                judged_at = len(passes) - 1

    n_passes = len(passes)
    acts = sum(len(p) for p in passes)
    false_acts = 0
    for i in range(1, n_passes):
        prev = set(passes[i - 1])
        for text in passes[i]:
            if any(r.match(text) for r in NOT_AN_EVENT):
                continue
            if text in prev:
                false_acts += 1

    out = {
        "SP_PASSES": n_passes,
        "SP_ACTS": acts,
        "SP_FALSE_ACTS": false_acts,
        # Per pass, because "3 false ACTs" means nothing without knowing over how many
        # passes. Two decimals: one false ACT in a day of two-minute passes is 0.00 at one.
        "SP_FALSE_PER_PASS": "%.2f" % (false_acts / n_passes) if n_passes else "?",
    }
    out["SP_STARVED_PASSES"] = starved
    if judged_at is None:
        # NEVER FIRED IS NOT A NUMBER ON ITS OWN. If the DAG has never been starved,
        # judgement had nothing to do and ">282" reads as a fault that is not there; if it
        # HAS been starved and judgement still never ran, that is the real alarm. Say which.
        out["SP_SINCE_JUDGEMENT"] = "n/a" if starved == 0 else "NEVER (%d starved)" % starved
    else:
        out["SP_SINCE_JUDGEMENT"] = n_passes - 1 - judged_at
    return out


def ledger_metrics(lines, since):
    """born vs lived: a summon that produced a process, vs one that reached a decision.

    `born` is written by aeon.sh within milliseconds of exec. `awake` is written once the
    claim has resolved — the first thing an aeon does that takes real time. An aeon killed
    in its first second has the first and not the second, which is precisely the cgroup
    teardown bug, and no other failure has that signature.
    """
    born = lived = worked = thrash = 0
    for line in lines:
        m = LEDGER_RE.match(line)
        if not m:
            continue
        ts = parse_ts(m.group(1))
        if ts is None or ts < since:
            continue
        event, rest = m.group(2), (m.group(4) or "").strip()
        if event == "born":
            born += 1
        elif event == "awake":
            lived += 1
            # Healthy no-ops, all three; only a claim is work. `capacity` is the
            # CONCURRENCY cap, `paused` is the account being out of API capacity — two
            # unrelated conditions that share a word, which is most of why the second one
            # went unhandled for so long. Missing `paused` here would count every aeon that
            # correctly declined during an outage as one that worked, so the panel would
            # report peak throughput for the whole of an outage.
            if rest not in ("capacity", "paused", "idle", ""):
                worked += 1
        elif event == "done" and "status=requeue-thrash" in rest:
            # THRASH REQUEUES ARE NOT FAILURES and are not counted as attempts, but they
            # are worth tracking: a bead that is thrash-requeued repeatedly points at a
            # genuine blocker the aeon cannot escape on its own.
            thrash += 1
    return {
        "SP_AEON_BORN": born,
        "SP_AEON_LIVED": lived,
        # Clamped at zero: an aeon born in the last second of the window has not reached
        # its disposition yet, and one negative reading would render as an alarm.
        "SP_AEON_STILLBORN": max(0, born - lived),
        "SP_AEON_WORKED": worked,
        "SP_AEON_THRASH": thrash,
    }


def sending_metrics(lines, since, now=None):
    """The Sending: work sent, work refused, and work that would not go.

    The refusals are the point. Sending a branch is routine; DECLINING to send one a live
    aeon still holds is the check that stops the Sending destroying work in flight, and
    KEEPing one that is closed but not yet an ancestor of main is the check that stops a
    bead's word being taken over the commit graph. FAILED is the fiend precursor: before
    sending.sh existed, a branch left behind by its aeon could not be deleted (git refuses
    while a worktree holds it), so the landing check re-merged and re-pushed it every two
    minutes forever. Unsent work comes back (2026-09-05).
    """
    if now is None:
        now = datetime.now(timezone.utc)
    # COUNT BRANCHES, NOT LOG LINES. held/kept were incremented per matching line, and the
    # sending runs every two minutes — so one branch a live aeon held for eight hours
    # counted 240 times and the pane read "held 221" for 23 distinct branches. A number
    # that large invites the reader to think something is wrong; the truth was that the
    # same refusal was working, repeatedly.
    # EVERY ONE OF THESE IS A SET OF BRANCH IDS. held/kept were made sets when "held 221"
    # turned out to be 23 branches; sent/failed were left as bare counters under that very
    # comment and had the identical defect — the pane read "184 fiends" for TWO branches,
    # sp-gate-rebuild (89 passes) and sp-supersede-key (4). A refusal that repeats every two
    # minutes is the check working, not a backlog growing, and only a set can say so.
    sent_ids, failed_ids = set(), set()
    held_ids, kept_ids = set(), set()
    # Most recent timestamp attributed to a FAILED line, so health.sh can dim the alarm
    # once the condition has been resolved for long enough that no action is needed.
    failed_last_ts = None
    # sending.sh's own output carries NO timestamp of its own — it is printed inside a pass
    # and captured verbatim. So attribute each line to the most recent timestamped line
    # above it. Lines before any timestamp are pre-window and dropped rather than counted,
    # because a count that silently includes the whole file would make the 24h window a lie.
    cur = None
    for line in lines:
        m = PASS_RE.match(line) or ACT_RE.match(line)
        if m:
            cur = parse_ts(m.group(1))
        if cur is None or cur < since:
            continue
        t = line.strip()
        if t.startswith("SENT"):
            parts = t.split()
            if len(parts) > 1: sent_ids.add(parts[1])
        elif t.startswith("HELD"):
            parts = t.split()
            if len(parts) > 1: held_ids.add(parts[1])
        elif t.startswith("KEEP"):
            parts = t.split()
            if len(parts) > 1: kept_ids.add(parts[1])
        # ONE EVENT, COUNTED ONCE. This used to also match sentinel.sh's own line, "sending
        # reported a branch it could not delete" — which is a per-pass SUMMARY of this very
        # output, emitted only when a FAILED line is already present in it. Counting both
        # doubled every failure: 93 real refusals rendered as 186. A second layer reporting
        # the same event is not a second observation of it.
        elif t.startswith("FAILED"):
            parts = t.split()
            if len(parts) > 1:
                failed_ids.add(parts[1])
                if cur is not None and (failed_last_ts is None or cur > failed_last_ts):
                    failed_last_ts = cur
    if failed_last_ts is not None:
        failed_age_m = str(int((now - failed_last_ts).total_seconds() / 60))
    else:
        failed_age_m = "?"
    return {
        "SP_SENT": len(sent_ids),
        "SP_SENT_HELD": len(held_ids),
        "SP_SENT_KEPT": len(kept_ids),
        "SP_SENT_FAILED": len(failed_ids),
        "SP_SENT_FAILED_AGE_M": failed_age_m,
    }


def self_metrics(sent_lines, ledger_lines, self_since, now):
    """Short-window SELF metrics: what is repeating now, and regression tripwires.

    THREE SIGNALS, each actionable on its own:

    REPEATING ACTs — an ACT text that appears in consecutive passes ending at the most
    recent pass within the window. A run of 2+ qualifies. The duration covers the span
    from the run's first pass to its last, so "× 3 (6m)" is three passes two minutes apart.
    Only runs that extend to the final pass count: a burst that stopped before the last
    pass is not "repeating now".

    died-at-birth — aeons with a 'born' entry but no 'awake' within the window. Zero is the
    correct value for a healthy system; any non-zero count gets an alert with the timestamp
    of the last occurrence.

    stalled passes — passes meeting CHECK 8's precondition (open work, nothing ready, nothing
    running) within the window. Same logic: zero is healthy, non-zero is an alert.

    The window is SPIRA_SELF_WINDOW minutes (default 60). A 24h burst outside the window
    produces SP_SELF_REPEATING_N=0 and no alert rows.
    """
    # --- repeating ACTs ---
    # Parse every pass in the short window. Each pass is (timestamp, [act_text, ...]).
    # We exclude NOT_AN_EVENT here for the same reason sentinel_metrics does: a summon is
    # legitimately once per pass, and counting it as a false repeat would always fire.
    passes = []       # list of (ts, [act_text, ...])
    starved_tss = []  # timestamps of stalled passes within the window
    cur_pass = None   # most recent in-window pass being accumulated

    for line in sent_lines:
        m = PASS_RE.match(line)
        if m:
            ts = parse_ts(m.group(1))
            if ts is None or ts < self_since:
                cur_pass = None
                continue
            cur_pass = (ts, [])
            passes.append(cur_pass)
            sm = STARVED_RE.search(line)
            if sm:
                o, r, ip = (int(x) for x in sm.groups())
                if o > 0 and r == 0 and ip == 0:
                    starved_tss.append(ts)
            continue
        m = ACT_RE.match(line)
        if m and cur_pass is not None:
            text = m.group(2).strip()
            if not any(r.match(text) for r in NOT_AN_EVENT):
                cur_pass[1].append(text)

    out = {}
    n = len(passes)
    repeating = []  # list of (run_len, dur_m, act_text)

    if n >= 2:
        # For each ACT text present in the last pass, count backwards: how many consecutive
        # final passes also had this text? A run of 2+ means it is repeating right now.
        last_acts = set(passes[-1][1])
        for act_text in last_acts:
            run_len = 0
            run_first_ts = passes[-1][0]
            for i in range(n - 1, -1, -1):
                if act_text in passes[i][1]:
                    run_len += 1
                    run_first_ts = passes[i][0]
                else:
                    break
            if run_len >= 2:
                dur_s = (passes[-1][0] - run_first_ts).total_seconds()
                dur_m = max(1, int(dur_s / 60))
                repeating.append((run_len, dur_m, act_text))

    repeating.sort(key=lambda x: -x[0])
    out["SP_SELF_REPEATING_N"] = len(repeating)
    for i, (run_len, dur_m, act_text) in enumerate(repeating):
        out["SP_SELF_REPEATING%d" % i] = "%s \xd7 %d passes (%dm)" % (act_text, run_len, dur_m)

    # --- stalled passes ---
    out["SP_SELF_STARVED_W"] = len(starved_tss)
    if starved_tss:
        ago_m = max(0, int((now - max(starved_tss)).total_seconds() / 60))
        out["SP_SELF_STARVED_LAST"] = "%dm ago" % ago_m
    else:
        out["SP_SELF_STARVED_LAST"] = "-"

    # --- died-at-birth ---
    # Track 'born' events in the window. An aeon with a 'born' but no subsequent 'awake'
    # before the scan ends is stillborn within the window. We iterate forward so that an
    # 'awake' later in the file removes the matching 'born' from consideration.
    born_map = {}   # aeon name -> born_ts
    for line in ledger_lines:
        m = LEDGER_RE.match(line)
        if not m:
            continue
        ts = parse_ts(m.group(1))
        if ts is None or ts < self_since:
            continue
        event, name = m.group(2), m.group(3)
        if event == "born":
            born_map[name] = ts
        elif event == "awake":
            born_map.pop(name, None)

    stillborn_tss = list(born_map.values())
    out["SP_SELF_STILLBORN_W"] = len(stillborn_tss)
    if stillborn_tss:
        ago_m = max(0, int((now - max(stillborn_tss)).total_seconds() / 60))
        out["SP_SELF_STILLBORN_LAST"] = "%dm ago" % ago_m
    else:
        out["SP_SELF_STILLBORN_LAST"] = "-"

    return out


def main():
    if len(sys.argv) < 3:
        sys.exit("usage: cockpit-metrics.py <sentinel.log> <aeon-ledger.log> [hours]")
    hours = float(sys.argv[3]) if len(sys.argv) > 3 else 24.0
    now = datetime.now(timezone.utc)
    since = now - timedelta(hours=hours)

    out = {}
    for path, fn, keys in (
        (sys.argv[1], sentinel_metrics,
         ("SP_PASSES", "SP_ACTS", "SP_FALSE_ACTS", "SP_FALSE_PER_PASS", "SP_SINCE_JUDGEMENT",
          "SP_STARVED_PASSES")),
        (sys.argv[2], ledger_metrics,
         ("SP_AEON_BORN", "SP_AEON_LIVED", "SP_AEON_STILLBORN", "SP_AEON_WORKED",
          "SP_AEON_THRASH")),
    ):
        try:
            out.update(fn(read(path), since))
        except Exception:
            # A probe that fails says so. It never says 0.
            out.update({k: "?" for k in keys})
    # Sending metrics need `now` to compute SP_SENT_FAILED_AGE_M, so they are called
    # outside the generic loop rather than bending it to pass a third argument.
    try:
        out.update(sending_metrics(read(sys.argv[1]), since, now))
    except Exception:
        for k in ("SP_SENT", "SP_SENT_HELD", "SP_SENT_KEPT", "SP_SENT_FAILED",
                  "SP_SENT_FAILED_AGE_M"):
            out[k] = "?"

    # Short-window SELF metrics. SPIRA_SELF_WINDOW is in minutes (default 60); the window
    # is independent of the history window above so it stays narrow enough to show "now".
    self_win_min = float(os.environ.get("SPIRA_SELF_WINDOW", "60"))
    self_since = now - timedelta(minutes=self_win_min)
    try:
        out.update(self_metrics(read(sys.argv[1]), read(sys.argv[2]), self_since, now))
    except Exception:
        for k in ("SP_SELF_REPEATING_N", "SP_SELF_STILLBORN_W", "SP_SELF_STILLBORN_LAST",
                  "SP_SELF_STARVED_W", "SP_SELF_STARVED_LAST"):
            out[k] = "?"

    out["SP_WINDOW_HOURS"] = ("%g" % hours)
    for k in sorted(out):
        print("%s=%s" % (k, out[k]))


if __name__ == "__main__":
    main()
