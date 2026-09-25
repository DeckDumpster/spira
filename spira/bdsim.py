#!/usr/bin/env python3
# bdsim.py — pure, in-memory bd query functions, driven by a canned-JSON fixture.
#
#   python3 bdsim.py <fixture-file> <bd-subcommand> [args...]
#
# This is the engine behind SPIRA_BDJSON_FIXTURE: lib.sh's bdq() calls here instead of the
# real `bd` binary when that variable is set, so every cockpit.sh *_keys function gets its
# bd reads from a JSON file instead of a live store.
#
# The fixture is either a flat JSON array of bead objects (serves `list` and `show` from the
# one array), or a JSON object with "list"/"show"/"memories" keys holding the shape each
# subcommand needs. An unreadable or malformed fixture prints nothing and exits 1 — the same
# outward shape as a `bd` that cannot reach its store, which is what every *_keys function's
# `?` branch is already written to detect.
#
# ONLY THE SHAPES THE REWRITTEN SUITES ACTUALLY ISSUE ARE SIMULATED. list's --status/--all/
# --label/--limit, show's positional ids, and bare memories. Anything else exits naming what
# it does not simulate: a query this cannot answer belongs in test-cockpit-bd-contract.sh
# against real bd, not modeled here (law-prefer-the-real-dependency — a hand-written model of
# bd's filtering would drift from the real one and the drift would surface as failures in
# code that is actually correct).

import json
import sys


def bd_list(beads, status, labels, all_, limit):
    def keep(b):
        if status is not None:
            if b.get("status") not in status:
                return False
        elif not all_ and b.get("status") == "closed":
            return False
        if labels and not set(labels).issubset(set(b.get("labels") or [])):
            return False
        return True

    out = [b for b in beads if keep(b)]
    return out[:limit] if limit else out


def bd_show(pool, ids):
    by_id = {b["id"]: b for b in pool if "id" in b}
    return [by_id[i] for i in ids if i in by_id]


def _parse_list_args(args):
    status, labels, all_, limit = None, [], False, 0
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--json":
            pass
        elif a == "--all":
            all_ = True
        elif a == "--status":
            i += 1
            status = set(args[i].split(","))
        elif a in ("--label", "-l"):
            i += 1
            labels.extend(x for x in args[i].split(",") if x)
        elif a in ("--limit", "-n"):
            i += 1
            limit = int(args[i])
        else:
            sys.exit("bdsim: list %r is not simulated" % (a,))
        i += 1
    return status, labels, all_, limit


def main(argv):
    if len(argv) < 3:
        sys.exit("usage: bdsim.py <fixture-file> <bd-subcommand> [args...]")
    fixture_path, sub, rest = argv[1], argv[2], argv[3:]
    try:
        with open(fixture_path, encoding="utf-8") as f:
            fixture = json.load(f)
    except (OSError, ValueError):
        sys.exit(1)

    if isinstance(fixture, list):
        top_list, top = fixture, {}
    elif isinstance(fixture, dict):
        top_list, top = fixture.get("list", []), fixture
    else:
        sys.exit(1)

    if sub == "list":
        status, labels, all_, limit = _parse_list_args(rest)
        print(json.dumps(bd_list(top_list, status, labels, all_, limit)))
    elif sub == "show":
        ids = [a for a in rest if not a.startswith("-")]
        show_src = top.get("show", top_list)
        pool = list(show_src.values()) if isinstance(show_src, dict) else show_src
        print(json.dumps(bd_show(pool, ids)))
    elif sub == "memories":
        print(json.dumps(top.get("memories", {})))
    else:
        sys.exit("bdsim: subcommand %r is not simulated" % (sub,))


if __name__ == "__main__":
    main(sys.argv)
