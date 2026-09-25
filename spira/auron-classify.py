#!/usr/bin/env python3
"""auron-classify.py — Auron's whole judgement, as a pure function of observations.

    auron.sh gathers -> this classifies -> auron.sh speaks

Reads ONE json object on stdin and writes zero or more alert objects, one json per
line, to stdout: {"key", "title", "evidence"}. It has no clock, opens no file, runs no
command and reaches no network. `now` is handed in, the sentinel log arrives as a
string, and every threshold is a field. That is what makes the rules below testable
against a captured window rather than against a hypothesis.

WHY THIS IS SPLIT OUT AT ALL. A watchdog nobody has watched fire is a hypothesis, and
the expensive failure here is not a missed stall but a false one: an alert that cries
during every ordinary long landing gets ignored, and then it is worse than nothing
(law-alerts-must-be-actionable). Split, each rule can be replayed over the real
sentinel log — which is how the two thresholds below were chosen rather than guessed.

THE MEASUREMENTS THE THRESHOLDS COME FROM. 962 real sentinel passes over 33 hours,
read out of the log on 2026-09-06, after landing moved off the pass (sp-gatecost):

  gap between completed passes   p50 124s   p90 137s   p99 161s   max 245s (12h)
                                                                  max 436s (24h, pre-fix)
  passes matching `starved`      2 of 962, in ONE streak of 2

So pass_stale defaults to 600s — 2.4x the worst gap ever seen here, five missed timer
ticks — and starve_passes to 5, against an observed maximum run of 2. The condition
this is aimed at ran for 76 consecutive passes before a human noticed it by eye.

EVERY RULE FAILS QUIET, NEVER LOUD, WITH ONE EXCEPTION. If a rule cannot see enough to
judge — too few passes in the tail, a mirror nothing is configured to write — it says
nothing, because a guess is worse than silence from something whose only power is
speech. The exception is `sentinel-unobservable`: a log that cannot be READ is itself
the alert, because "I could not look" and "all clear" must never render as the same
pixels (law-absence-needs-a-positive-control).
"""
import json
import re
import sys

# The sentinel's own log format. Both halves are load-bearing and neither is guessed:
# `log()` in lib.sh prints exactly `<iso8601-Z> spira: <message>`, and sentinel.sh
# opens every pass with `state: ...` and ends every completed one with `pass complete`.
# Lines that are not the sentinel's — a `git` merge notice, sending.sh's REAPED rows —
# carry no timestamp and are skipped rather than misparsed.
LINE = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)Z spira: (.*)$")
# `plan_ready` today; `ready` in the passes written before the plan's count was named
# apart from every fayth's own. Accepting both costs one `(?:plan_)?` and is the
# difference between reading a rotated-in older tail and misreading it.
STATE = re.compile(r"^state: goal=\S+ open=(\d+) (?:plan_)?ready=(\d+) in_progress=(\d+) aeons=(\d+)")

# The three things a pass can say about summoning, and only the first is a summon.
# CHECK 7 logs exactly one line per fayth per pass, so their ABSENCE means the pass
# never reached the summon check at all.
SUMMONED = "— summoning"
# A deliberate withhold is not a stall. The account's own five-hour window is the one
# authority allowed to say "ready work, no aeon, and that is correct", and a watchdog
# that alerts through it is alerting on a decision the harness made on purpose.
WITHHELD = ("out of capacity",)


def iso_to_epoch(s):
    """`2026-09-06T18:26:32` -> epoch seconds, UTC. calendar.timegm without the import."""
    y, mo, d = int(s[0:4]), int(s[5:7]), int(s[8:10])
    h, mi, sec = int(s[11:13]), int(s[14:16]), int(s[17:19])
    # Days since the epoch by civil-from-days (Howard Hinnant's algorithm) — exact, and
    # free of the local timezone that `time.mktime` would drag in. The sentinel stamps
    # in UTC; reading it as local time would shift every threshold by the box's offset.
    yy = y - (mo <= 2)
    era = (yy if yy >= 0 else yy - 399) // 400
    yoe = yy - era * 400
    doy = (153 * (mo + (-3 if mo > 2 else 9)) + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    days = era * 146097 + doe - 719468
    return days * 86400 + h * 3600 + mi * 60 + sec


def parse_passes(text):
    """The sentinel log tail -> one record per pass, in order.

    A pass runs from a `state:` line to the next one, which is what makes the LAST
    record in the tail the pass currently in flight — it has not reached CHECK 7 yet
    and must not be read as one that declined to summon. `complete` is how a caller
    tells the two apart. The FIRST record is dropped when the tail begins mid-pass,
    for the same reason from the other end: a fragment is not evidence.
    """
    passes = []
    cur = None
    for raw in text.splitlines():
        m = LINE.match(raw)
        if not m:
            continue
        at, msg = iso_to_epoch(m.group(1)), m.group(2)
        st = STATE.match(msg)
        if st:
            cur = {"at": at, "open": int(st.group(1)), "ready": int(st.group(2)),
                   "in_progress": int(st.group(3)), "aeons": int(st.group(4)),
                   "summoned": False, "withheld": False, "check7": 0,
                   "complete": False, "last": at}
            passes.append(cur)
            continue
        if cur is None:
            continue
        cur["last"] = at
        if msg.startswith("CHECK7 "):
            cur["check7"] += 1
        if SUMMONED in msg:
            cur["summoned"] = True
        if any(w in msg for w in WITHHELD):
            cur["withheld"] = True
        if msg.startswith("pass complete"):
            cur["complete"] = True
    return passes


def last_completion(passes):
    """The epoch of the most recent `pass complete`, or None if the tail holds none."""
    for p in reversed(passes):
        if p["complete"]:
            return p["last"]
    return None


def starved(p):
    """Ready work, PROVABLY free capacity, and nothing summoned.

    `aeons == 0` is the positive observation, and it is why this rule reads the state
    line rather than the CHECK 7 lines. A free slot cannot be inferred from a decline
    — `at concurrency cap` says the slots are full and says nothing about whether that
    is true — but zero live aeons means at least one slot is free under any roster,
    because FAYTH_MAX_CONCURRENT is at least 1 for every fayth that exists.

    The first version of this rule asked instead whether the pass logged any CHECK 7
    line, on the theory that a pass wedged before the summon check logs none. Replayed
    over the real log it fired on 76 consecutive passes that were perfectly healthy:
    they predate CHECK 7 logging entirely, so their silence was a fact about the
    sentinel's vocabulary and not about its behaviour. An absent line means "this did
    not happen" only where the line would certainly have been written.
    """
    return p["ready"] > 0 and p["aeons"] == 0 and not p["summoned"] and not p["withheld"]


def fmt_age(sec):
    if sec < 120:
        return "%ds" % sec
    if sec < 7200:
        return "%dm" % (sec // 60)
    return "%.1fh" % (sec / 3600.0)


def main():
    try:
        o = json.load(sys.stdin)
    except Exception as exc:                      # noqa: BLE001 — any parse failure
        sys.stderr.write("auron-classify: unreadable observations: %s\n" % exc)
        return 2

    now = int(o["now"])
    th = o.get("thresholds") or {}
    pass_stale = int(th.get("pass_stale", 600))
    starve_passes = int(th.get("starve_passes", 5))
    mirror_stale = int(th.get("mirror_stale", 90000))
    ghost_stale = int(th.get("ghost_stale", 1800))
    out = []

    def alert(key, title, evidence):
        out.append({"key": key, "title": title, "evidence": evidence.strip()})

    world_halted = bool(o.get("world_halted"))
    world_draining = bool(o.get("world_draining"))

    # -- the sentinel's own pulse --------------------------------------------------------
    log_path = o.get("sentinel_log_path") or "the sentinel log"
    if not o.get("sentinel_log_readable"):
        alert("sentinel-unobservable",
              "Auron cannot read the sentinel log — the loop's liveness is unknown",
              "PATH      %s\nREASON    %s\n\n"
              "Nothing below this line was checked. An unreadable log is not a quiet\n"
              "one: every other reading Auron takes about the loop is derived from it,\n"
              "so a healthy pane here would be a guess wearing a measurement's clothes."
              % (log_path, o.get("sentinel_log_error") or "unreadable"))
        passes = []
    else:
        passes = parse_passes(o.get("sentinel_log") or "")
        done = last_completion(passes)
        # NO COMPLETION IN THE TAIL IS NOT "STALE SINCE THE EPOCH". It is one of two
        # different things — a loop that stopped long ago, or a box where Auron came up
        # first — and the floor tells them apart without a special case: measure from
        # whichever is later, the tail's own start or Auron's first run. On a fresh
        # install nothing fires for pass_stale seconds, by which time a working sentinel
        # has written a completion; on a genuinely dead one it fires on schedule.
        floor = max(int(o.get("auron_first") or 0),
                    passes[0]["at"] if passes else 0)
        since = done if done is not None else floor
        age = now - since
        # A deliberately halted or draining loop is expected to stop completing passes.
        # Firing here would train the reader to discount the one alert that matters when
        # the loop genuinely dies (law-alerts-must-be-actionable).
        if since and age > pass_stale and not world_halted and not world_draining:
            timer = o.get("sentinel_timer") or "unknown"
            # WHICH FAILURE IT IS, in the evidence rather than in a second alert. A dead
            # timer and a pass wedged mid-flight both read as "nothing completed", and
            # they have different fixes; the log's own mtime is what separates them.
            mtime = int(o.get("sentinel_log_mtime") or 0)
            quiet = now - mtime if mtime else -1
            if quiet >= 0 and quiet < pass_stale // 2:
                shape = ("the log is still being WRITTEN (%s ago) but no pass has "
                         "FINISHED — a pass is wedged mid-flight" % fmt_age(quiet))
            elif timer != "active":
                shape = "spira-sentinel.timer is %s — the loop is not being run at all" % timer
            else:
                shape = ("nothing has been written to the log in %s either — the pass is "
                         "not running, or is dying before it can log"
                         % (fmt_age(quiet) if quiet >= 0 else "an unknown time"))
            tail = "\n".join(
                l for l in (o.get("sentinel_log") or "").splitlines()[-12:])
            alert("sentinel-stalled",
                  "The Spira sentinel has not completed a pass in %s" % fmt_age(age),
                  "LAST PASS %s ago%s\n"
                  "THRESHOLD %ds (ordinary pass ~22s, worst gap ever measured here 436s)\n"
                  "TIMER     spira-sentinel.timer is %s\n"
                  "LOG       %s, last written %s ago\n"
                  "SHAPE     %s\n\n"
                  "Nothing is being claimed, landed, reaped or reclaimed while this holds.\n"
                  "Aeons already running are unaffected and will finish.\n\n"
                  "--- sentinel.log (tail) ---\n%s"
                  % (fmt_age(age), "" if done is not None else " (no completed pass in the tail at all)",
                     pass_stale, timer, log_path,
                     fmt_age(quiet) if quiet >= 0 else "never", shape, tail))

        # -- summon starvation ----------------------------------------------------------
        # The pass in flight is excluded: it has not reached CHECK 7 yet, so counting it
        # would read every ordinary pass's first twenty seconds as a stall.
        finished = [p for p in passes if p["complete"]]
        if len(finished) >= starve_passes:
            window = finished[-starve_passes:]
            # STARVATION IS A STATEMENT ABOUT A LOOP THAT IS RUNNING. Over a window that
            # has itself gone stale it is not merely redundant with sentinel-stalled, it
            # is false: "five passes in a row summoned nothing" says nothing when the
            # newest of the five is an hour old and no sixth was ever attempted. Two
            # alerts for one cause is the same wallpaper problem as ten beads for one
            # alert, arriving one level up.
            fresh = (now - window[-1]["at"]) <= pass_stale
            if fresh and all(starved(p) for p in window):
                rows = "\n".join(
                    "  %ds ago  ready=%-3d aeons=%d  summoned=%s"
                    % (now - p["at"], p["ready"], p["aeons"], "no")
                    for p in window)
                alert("summon-starved",
                      "%d ready beads, no aeon running, and %d passes in a row summoned nothing"
                      % (window[-1]["ready"], starve_passes),
                      "PASSES    %d consecutive, spanning %s\n"
                      "READY     %d claimable beads at the last pass\n"
                      "AEONS     0 — every slot is free, so this is not the concurrency cap\n"
                      "WITHHELD  no — the account's window did not decline\n"
                      "NOISE     2 passes of 962 matched this rule over 33 hours, in one "
                      "streak of 2; the threshold is %d\n\n"
                      "The loop is running and is refusing to start work it has capacity for.\n"
                      "The last time this happened it ran for 76 passes before a human saw it.\n\n"
                      "--- the passes ---\n%s"
                      % (starve_passes, fmt_age(now - window[0]["at"]),
                         window[-1]["ready"], starve_passes, rows))

    # -- the database ---------------------------------------------------------------------
    # This alert can only ever reach the fallback file, by construction: the channel it
    # would otherwise use is the thing it is reporting broken.
    if not o.get("db_reachable", True):
        alert("db-unreachable",
              "The Spira beads database did not answer",
              "DB        %s\nERROR     %s\n\n"
              "Every claim, close, lease and label goes through this. Aeons already\n"
              "running will fail their next bd call; the sentinel's passes will complete\n"
              "having done nothing. This alert is in %s because it could not be written\n"
              "to beads — which is the condition it is reporting."
              % (o.get("db_path") or "?", o.get("db_error") or "no answer",
                 o.get("fallback_path") or "the fallback file"))

    # -- the mirror -----------------------------------------------------------------------
    # Silent where no exporter is configured — a colleague who does not mirror the
    # database has nothing here to be stale. Loud where one IS configured and has
    # produced nothing, because that is the positive control failing.
    mir = o.get("mirror") or {}
    if mir.get("configured"):
        if not mir.get("exists"):
            alert("mirror-stale",
                  "The beads exporter is configured but has written no mirror",
                  "EXPORTER  %s\nEXPECTED  %s\n\n"
                  "The mirror is the copy of the database that reaches git, review and\n"
                  "the phone. Dolt's own remote is the other half and fails independently;\n"
                  "with neither, a database loss is unrecoverable and silent."
                  % (mir.get("exporter") or "?", mir.get("path") or "?"))
        else:
            age = now - int(mir.get("mtime") or 0)
            if age > mirror_stale:
                alert("mirror-stale",
                      "The beads mirror has not been rewritten in %s" % fmt_age(age),
                      "PATH      %s\nWRITTEN   %s ago\nTHRESHOLD %s\nEXPORTER  %s\n\n"
                      "The exporter runs on a timer and rewrites this every pass in which\n"
                      "the database changed. Silence for longer than a day means the timer,\n"
                      "the exporter or its refusal check is broken, not that nothing changed."
                      % (mir.get("path") or "?", fmt_age(age), fmt_age(mirror_stale),
                         mir.get("exporter") or "?"))

    # -- leases nobody reclaimed -----------------------------------------------------------
    # strand.sh reclaims a ghost lease within minutes and escalates once if it cannot.
    # Auron's subject is therefore not the ghost — it is the REAPER, still reporting the
    # same ghost long after its own action should have cleared it.
    ghosts = []
    for key, e in sorted((o.get("strands") or {}).items()):
        if not key.startswith("ghost:"):
            continue
        first = int((e or {}).get("first") or 0)
        if first and now - first > ghost_stale:
            ghosts.append((key.split(":", 1)[1], now - first, int((e or {}).get("acted") or 0),
                           int((e or {}).get("escalated") or 0)))
    if ghosts:
        rows = "\n".join(
            "  %-22s stranded %s ago, acted=%s escalated=%s"
            % (i, fmt_age(a), "yes" if ac else "no", "yes" if es else "no")
            for i, a, ac, es in ghosts)
        alert("lease-unreclaimed",
              "%d bead(s) have held a lease with no live aeon for over %s"
              % (len(ghosts), fmt_age(ghost_stale)),
              "THRESHOLD %s — strand.sh reclaims a ghost within one pass and escalates once\n"
              "if it cannot, so a ghost still standing after this is the REAPER failing,\n"
              "not a worker dying.\n\n"
              "Each of these beads reads as in_progress, so nothing else will claim it and\n"
              "everything downstream of it stays blocked.\n\n"
              "--- ghosts ---\n%s" % (fmt_age(ghost_stale), rows))

    # -- unit restart loops ---------------------------------------------------------------
    # A Restart=always unit never reaches 'failed' so incident intake misses it. Auron
    # detects it by comparing NRestarts to the baseline recorded at the window start.
    # The threshold and window arrive pre-computed from bash (baseline management lives
    # there because it is stateful across passes). The classifier's job here is only to
    # format the evidence and emit the alert key.
    restart_threshold = int(th.get("restart_threshold", 5))
    for ra in (o.get("restart_alerts") or []):
        unit = ra.get("unit") or "?"
        delta = int(ra.get("delta") or 0)
        if delta <= restart_threshold:
            continue
        current = int(ra.get("current") or 0)
        window = int(ra.get("window") or 3600)
        journal = (ra.get("journal") or "").strip()
        key = "restart-loop:" + unit
        alert(key,
              "%s restarted %d time(s) in the last %s" % (unit, delta, fmt_age(window)),
              "UNIT      %s\n"
              "RESTARTS  %d in this %s window (running total %d, threshold %d)\n\n"
              "A Restart=always unit never reaches 'failed' — incident intake misses it.\n"
              "This is Auron's notification of an active restart loop.\n\n"
              "--- last journal lines ---\n%s"
              % (unit, delta, fmt_age(window), current, restart_threshold, journal))

    for a in out:
        sys.stdout.write(json.dumps(a, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
