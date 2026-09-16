#!/usr/bin/env bash
# tokens.sh — what the account actually spends, and on what.
#
# WHY IT EXISTS. The 5x plan hit its session limit in most five-hour windows and nobody could
# say which half of the system was responsible — the harness or the interactive session. The
# harness is the larger half. This measures both from the transcripts that are already on disk;
# it starts no session and spends nothing to answer.
#
# WHAT COUNTS. Every assistant turn records a usage block: input_tokens (fresh), cache_creation
# (writing the prompt cache), cache_read (re-reading it) and output_tokens. Cache read dominates
# by two orders of magnitude, because the whole context is re-read on EVERY turn — so the number
# that predicts a rate limit is context-per-turn multiplied by turns, not output.
#
# DEDUPE BY MESSAGE ID, GLOBALLY. A resumed session copies earlier turns into a new transcript
# file; counting per file inflated this corpus by 11,600 turns. The id is the API call, so it is
# the unit that was actually billed.
#
# THREE BUCKETS, THREE INPUTS, because the point is to attribute:
#   aeons      $SPIRA_RUN/*.log + worktree transcripts — every aeon session
#   archivist  $SPIRA_RUN/archivist/cwd transcripts   — automated archive sweeps
#   session    the remaining transcripts               — the interactive sessions
# Aeons and the archivist are identified by the encoded working directory they run from;
# session is a positive test for the rest, not an else-branch. Message ids are deduped
# across all inputs so a turn is counted once.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh" >/dev/null 2>&1
# BOTH FROM CONFIGURATION, with no literal fallback here. The window is a fact about the plan
# and the transcript directory is a fact about the client, so neither is knowable from this
# file — and a default written in twice is how two programs come to disagree about what
# "inside the window" means while both look right.
WINDOW_H="$SPIRA_TOKEN_WINDOW_H"
PROJECTS="$SPIRA_TOKEN_PROJECTS"

python3 - "$SPIRA_RUN" "$PROJECTS" "$WINDOW_H" "${1:-env}" <<'PY'
import json, sys, glob, os, collections, datetime as dt

run, projects, window_h, mode = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4]
now = dt.datetime.now(dt.timezone.utc)
cut = now - dt.timedelta(hours=window_h)
KEYS = ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens")

# The client encodes project directories: every / and . becomes -.
wt_root = os.path.normpath(os.path.join(run, "worktree"))
wt_prefix = wt_root.replace("/", "-").replace(".", "-") + "-"
arc_cwd = os.path.normpath(os.path.join(run, "archivist", "cwd"))
arc_prefix = arc_cwd.replace("/", "-").replace(".", "-")

def scan(paths, want_assistant_only, seen, window_only=False):
    """-> (all_time, in_window) counters. Dedupes on message id via the caller's seen set.

    window_only skips files untouched since the cutoff. A file whose last write predates the
    window cannot hold a turn inside it, and the corpus is 23 billion tokens of history that
    would otherwise be re-read on every collector pass — instrumentation that costs more than
    the thing it measures is its own bug. all_time is then partial and callers must not use it.
    """
    tot, win = collections.Counter(), collections.Counter()
    for f in paths:
        if window_only:
            try:
                if dt.datetime.fromtimestamp(os.path.getmtime(f), dt.timezone.utc) < cut:
                    continue
            except OSError:
                continue
        try: fh = open(f, errors="ignore")
        except OSError: continue
        with fh:
            for ln in fh:
                if '"usage"' not in ln: continue
                try: o = json.loads(ln.strip())
                except Exception: continue
                if want_assistant_only and o.get("type") != "assistant": continue
                m = o.get("message")
                if not isinstance(m, dict): continue
                u, mid = m.get("usage"), m.get("id")
                if not u or not mid or mid in seen: continue
                seen.add(mid)
                ts = o.get("timestamp") or ""
                try: when = dt.datetime.fromisoformat(ts.replace("Z", "+00:00"))
                except Exception: when = None
                tot["turns"] += 1
                for k in KEYS: tot[k] += u.get(k) or 0
                if when and when >= cut:
                    win["turns"] += 1
                    for k in KEYS: win[k] += u.get(k) or 0
    return tot, win

AEON_FILES = sorted(glob.glob(os.path.join(run, "*.log")))
ALL_SESS = sorted(glob.glob(os.path.join(projects, "*", "*.jsonl")))
WT_SESS    = [f for f in ALL_SESS if os.path.basename(os.path.dirname(f)).startswith(wt_prefix)]
ARC_SESS   = [f for f in ALL_SESS if os.path.basename(os.path.dirname(f)) == arc_prefix]
SESS_FILES = [f for f in ALL_SESS
              if not os.path.basename(os.path.dirname(f)).startswith(wt_prefix)
              and os.path.basename(os.path.dirname(f)) != arc_prefix]
# env mode is the collector's path and runs constantly, so it reads only what the window can
# touch. report mode is a human asking once, and reads everything.
WIN_ONLY = (mode == "env")
seen = set()
aeon_tot, aeon_win = scan(AEON_FILES, True, seen, WIN_ONLY)
wt_tot, wt_win = scan(WT_SESS, False, seen, WIN_ONLY)
aeon_tot += wt_tot
aeon_win += wt_win
arc_tot, arc_win = scan(ARC_SESS, False, seen, WIN_ONLY)
sess_tot, sess_win = scan(SESS_FILES, False, seen, WIN_ONLY)

def ctx(c): return c["cache_read_input_tokens"] // c["turns"] if c["turns"] else 0
# What a rate limit actually meters is everything the request carried, cache reads included.
def billed(c): return c["input_tokens"] + c["cache_creation_input_tokens"] + c["cache_read_input_tokens"] + c["output_tokens"]

if mode == "env":
    out = {
        "SP_TOK_WINDOW_H":   int(window_h),
        "SP_TOK_AEON_WIN":   billed(aeon_win),
        "SP_TOK_ARC_WIN":    billed(arc_win),
        "SP_TOK_SESS_WIN":   billed(sess_win),
        "SP_TOK_WIN":        billed(aeon_win) + billed(arc_win) + billed(sess_win),
        "SP_TOK_AEON_TURNS": aeon_win["turns"],
        "SP_TOK_ARC_TURNS":  arc_win["turns"],
        "SP_TOK_SESS_TURNS": sess_win["turns"],
        "SP_TOK_AEON_CTX":   ctx(aeon_win) or ctx(aeon_tot),
        "SP_TOK_ARC_CTX":    ctx(arc_win) or ctx(arc_tot),
        "SP_TOK_SESS_CTX":   ctx(sess_win) or ctx(sess_tot),
        "SP_TOK_AEON_OUT":   aeon_win["output_tokens"],
        "SP_TOK_ARC_OUT":    arc_win["output_tokens"],
        "SP_TOK_SESS_OUT":   sess_win["output_tokens"],
        # NOT all-time: env mode reads only files touched inside the window, so these are
        # "billed by files still being written", which is the honest thing the fast path knows.
        # A pane wanting a true total must call `tokens.sh report` (seconds, not milliseconds).
        "SP_TOK_AEON_RECENT": billed(aeon_tot),
        "SP_TOK_ARC_RECENT":  billed(arc_tot),
        "SP_TOK_SESS_RECENT": billed(sess_tot),
    }
    for k, v in out.items(): print(f"{k}={v}")
else:
    def row(name, c):
        print(f"  {name:<10}{c['turns']:>7,} turns  {billed(c):>16,} billed  "
              f"{ctx(c):>9,} ctx/turn  {c['output_tokens']:>10,} out")
    print(f"=== last {window_h:g}h ==="); row("aeons", aeon_win); row("archivist", arc_win); row("session", sess_win)
    print("=== all time ===");            row("aeons", aeon_tot); row("archivist", arc_tot); row("session", sess_tot)
    t = billed(aeon_tot) + billed(arc_tot) + billed(sess_tot)
    if t:
        print(f"\n  session share of all tokens: {billed(sess_tot)*100//t}%")
PY
