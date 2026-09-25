#!/usr/bin/env python3
#
# incident-stub-bd.py — a stand-in bd for T1 suites exercising incident.sh's decision
# layer (UC-ops-detection-remediation-04, -05, -06, -07) without a Dolt fixture.
#
# NOT SHIPPED AS PRODUCTION CODE: nothing but a test's SPIRA_BD ever points here. It is a
# small stateful bd, not a canned-response mock, because incident.sh's dedup and recurrence
# behaviour is a sequence of calls that must see each other's writes (a second filing must
# find the bead the first filing created) — a fixed response cannot reproduce that, and a
# gap in a hand-written model surfaces as a false pass in correct code
# (law-prefer-the-real-dependency's own reasoning applied to a case where the real
# dependency is deliberately out of scope for cost, not correctness).
#
# STATE lives in a JSON file named by STUB_BD_STATE (env). Every invocation is appended,
# space-joined, to STUB_BD_LOG (env) so a suite can count or grep calls (e.g. "0 bd show
# calls" for UC-04's dedup-efficiency property).
#
# COMMANDS IMPLEMENTED (the subset incident.sh and lib.sh's bump_*/recurs_of call):
#   list --status S[,S...] [--closed-after DATE] --limit N [--label L] --json
#   create <title> --type T --priority P --labels L --external-ref REF --body-file F --silent
#   show <id> --json
#   label add|remove|list <id> [<label>]
#   note <id> <text>
#   set-state <id> <key=value>
#   sql <query>            -- only INSERT INTO events(...) and SELECT COUNT(*) FROM events
#   seed                    -- test-only backdoor: merge a JSON bead dict from stdin into
#                              state without going through incident.sh, for planting
#                              beads that predate this test run (e.g. UC-04's noise beads).
import json
import os
import re
import sys


def load_state(path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return {"beads": {}, "events": [], "next_id": 1}


def save_state(path, state):
    with open(path, "w") as f:
        json.dump(state, f)


def main():
    argv = sys.argv[1:]
    if argv[:1] == ["-C"]:
        argv = argv[2:]

    log_path = os.environ.get("STUB_BD_LOG")
    if log_path:
        with open(log_path, "a") as f:
            f.write(" ".join(argv) + "\n")

    state_path = os.environ["STUB_BD_STATE"]
    state = load_state(state_path)

    if not argv:
        return 0
    cmd = argv[0]

    if cmd == "seed":
        bead = json.load(sys.stdin)
        state["beads"][bead["id"]] = bead
        save_state(state_path, state)
        return 0

    if cmd == "list":
        statuses = []
        label = None
        closed_after = None
        i = 1
        while i < len(argv):
            if argv[i] == "--status":
                statuses = argv[i + 1].split(","); i += 2
            elif argv[i] == "--label":
                label = argv[i + 1]; i += 2
            elif argv[i] == "--closed-after":
                closed_after = argv[i + 1]; i += 2
            else:
                i += 1
        # bd's --label takes a comma list meaning AND: every named label must be present.
        want_labels = label.split(",") if label else []
        rows = []
        for b in state["beads"].values():
            if statuses and b.get("status") not in statuses:
                continue
            if want_labels and not all(l in (b.get("labels") or []) for l in want_labels):
                continue
            if closed_after and (b.get("closed_at") or "") < closed_after:
                continue
            rows.append(b)
        print(json.dumps(rows))
        return 0

    if cmd == "create":
        title = argv[1] if len(argv) > 1 else ""
        labels, ext_ref, itype, priority = [], "", "bug", "2"
        i = 2
        while i < len(argv):
            if argv[i] == "--labels":
                labels = argv[i + 1].split(","); i += 2
            elif argv[i] == "--external-ref":
                ext_ref = argv[i + 1]; i += 2
            elif argv[i] == "--type":
                itype = argv[i + 1]; i += 2
            elif argv[i] == "--priority":
                priority = argv[i + 1]; i += 2
            else:
                i += 1
        bid = "sp-stub%d" % state["next_id"]
        state["next_id"] += 1
        state["beads"][bid] = {
            "id": bid, "title": title, "type": itype, "priority": priority,
            "labels": labels, "external_ref": ext_ref, "status": "open",
            "closed_at": None, "notes": "",
        }
        save_state(state_path, state)
        print(bid)
        return 0

    if cmd == "show":
        bid = argv[1]
        print(json.dumps(state["beads"].get(bid, {})))
        return 0

    if cmd == "label":
        sub, bid = argv[1], argv[2]
        bead = state["beads"].setdefault(bid, {"id": bid, "labels": []})
        bead.setdefault("labels", [])
        if sub == "add":
            lab = argv[3]
            if lab not in bead["labels"]:
                bead["labels"].append(lab)
        elif sub == "remove":
            lab = argv[3]
            if lab in bead["labels"]:
                bead["labels"].remove(lab)
        elif sub == "list":
            print("\n".join(bead["labels"]))
            return 0
        save_state(state_path, state)
        return 0

    if cmd == "note":
        bid, text = argv[1], argv[2]
        bead = state["beads"].setdefault(bid, {"id": bid, "notes": ""})
        bead["notes"] = (bead.get("notes") or "") + ("\n\n" if bead.get("notes") else "") + text
        save_state(state_path, state)
        return 0

    if cmd == "set-state":
        bid, kv = argv[1], argv[2]
        k, _, v = kv.partition("=")
        state["beads"].setdefault(bid, {"id": bid})[k] = v
        save_state(state_path, state)
        return 0

    if cmd == "sql":
        query = argv[1]
        m = re.search(
            r"INSERT INTO events .*VALUES \('([^']*)', '([^']*)', '([^']*)', '([^']*)', '([^']*)', \w+\(\)\)",
            query,
        )
        if m:
            _, issue_id, event_type, actor, new_value = m.groups()
            state["events"].append({"issue_id": issue_id, "event_type": event_type, "new_value": new_value})
            save_state(state_path, state)
            return 0
        m = re.search(
            r"SELECT COUNT\(\*\) FROM events WHERE issue_id='([^']*)' AND event_type='([^']*)'",
            query,
        )
        if m:
            issue_id, event_type = m.groups()
            n = sum(1 for e in state["events"]
                    if e["issue_id"] == issue_id and e["event_type"] == event_type)
            print("header")
            print("----")
            print(n)
            return 0
        return 0

    return 0


if __name__ == "__main__":
    sys.exit(main())
