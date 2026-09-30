#!/usr/bin/env python3
# watchtower-czar-outcome.py <now> <outcome_mins> <unclaimed_mins> <json-file>
#
# Extracted from watchtower.sh's --czar-outcome-check (formerly an inline
# `python3 - <<PYEOF` heredoc) so it can be table-tested without a bd round-trip
# (test-watchtower-czar-outcome-classify.sh). JSON arrives as a file, not argv or
# stdin, for the same reason strand.sh's payloads do: a trigger-bead list can exceed
# MAX_ARG_STRLEN, and a heredoc script source cannot also read stdin.
#
# Reads the czar-trigger beads' JSON (bd list --label czar-trigger --all --json
# --limit 0 --brief) and prints one of two lines per finding, exactly as the caller
# expects:
#   UNCLAIMED <id> <ref>
#   NOT_CLEARED <id> <ref>
import sys, json
from datetime import datetime, timezone


def ts(s):
    if not s:
        return None
    try:
        return int(datetime.strptime(s.rstrip('Z'), '%Y-%m-%dT%H:%M:%S').replace(tzinfo=timezone.utc).timestamp())
    except Exception:
        return None


def main():
    now_s = int(sys.argv[1])
    out_secs = int(sys.argv[2]) * 60
    unc_secs = int(sys.argv[3]) * 60
    data = json.loads(open(sys.argv[4]).read() or '[]')
    if not isinstance(data, list):
        data = [data]

    by_ref = {}
    for b in data:
        ref = b.get('external_ref') or ''
        if not ref.startswith('incident:queue-'):
            continue
        b['_ct'] = ts(b.get('created_at'))
        b['_cla'] = ts(b.get('closed_at'))
        by_ref.setdefault(ref, []).append(b)

    for ref, beads in by_ref.items():
        beads.sort(key=lambda b: b.get('_ct') or 0)
        newest = beads[-1]
        status = newest.get('status', '')
        ct = newest.get('_ct')

        if status in ('open', 'in_progress'):
            if ct and (now_s - ct) >= unc_secs:
                print('UNCLAIMED', newest['id'], ref)
            continue

        # Outcome check: find any closed bead followed by a newer bead after its close_at
        for i, bead in enumerate(beads):
            if bead.get('status') != 'closed':
                continue
            cla = bead.get('_cla')
            if not cla or (now_s - cla) < out_secs:
                continue
            if any(b.get('_ct') and b['_ct'] > cla for b in beads[i + 1:]):
                print('NOT_CLEARED', bead['id'], ref)
                break


if __name__ == '__main__':
    main()
