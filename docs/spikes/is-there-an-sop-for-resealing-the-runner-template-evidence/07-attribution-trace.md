# 07 — a drifted template is invisible to the harness: the attribution trace

Inferred by reading code at `2e23aa3d` (nothing here was executed against CI — see `00-`).
This is the most consequential thing the spike found and it is filed as **sp-wargl** (P1).

The refusal at `provision.sh:593` never reaches any payload the harness produces. Five steps:

## 1. gate.yml — the `gate` job runs anyway and fails

```yaml
  gate:
    needs: [provision, suites]
    if: ${{ !cancelled() }}
    runs-on: ubuntu-latest
    steps:
      - name: Verdict
        run: |
          if [ "${{ needs.provision.result }}" != 'success' ]; then
            printf 'gate: provision failed — branch was not tested\n' >&2
            exit 75
          fi
          if [ "${{ needs.suites.result }}" != 'success' ]; then
            printf 'gate: suites concluded %s\n' "${{ needs.suites.result }}" >&2
            exit 1
          fi

```

`if: !cancelled()` means the job runs even when provision failed, and exit 75 makes its
conclusion FAILURE — deliberately, so branch protection sees a refusal rather than a skip
(the comment above it says exactly that). Correct for branch protection; it is also what
erases the cause.

## 2. forge.sh check-status maps FAILURE to plain `red`

`spira/forge.sh:55-76` — anything not SUCCESS and not in
`(SKIPPED, NEUTRAL, STALE, CANCELLED)` prints `red`. A drifted template is `red`, not
`harness_fault`.

## 3. forge.sh extracts only Spira-shaped annotations

`spira/forge.sh:113-130`:

```python
import json, sys
try:
    for a in json.load(sys.stdin):
        lvl = a.get('annotation_level', '')
        title = a.get('title', '')
        msg = a.get('message', '')
        path = a.get('path', '')
        if lvl == 'warning' and title == 'flaky suite':
            idx = msg.find(' was red')
            if idx > 0:
                print('flaky: ' + msg[:idx])
        elif lvl == 'error' and title == 'red-twice suite':
            print('red-suite: ' + msg)
        elif lvl == 'failure' and path.startswith('spira/test-') and path.endswith('.sh'):
            print('red-suite: ' + path.split('/')[-1])
except Exception:
```

A provision-stage `::error::` annotation has neither of those two titles nor a path matching
`spira/test-*.sh`, so it is dropped. `red_suites` comes back empty.

## 4. verdict.sh — red with no annotations and >1 member: bisect and requeue everyone

`spira/verdict.sh:260-280`:

```bash
        # Suite-level attribution requires CI to emit annotations whose path matches
        # spira/test-*.sh or whose title is 'red-twice suite'. Without them the queue
        # falls back to binary search: O(log n) CI runs converge to a single member.
        local _half=$(( mc / 2 )) _i=0
        for _mm in "${members_arr[@]}"; do
            _mid="${_mm%%:*}"; _mtip="${_mm##*:}"
            if [ "$_i" -lt "$_half" ]; then
                land_mark_at "$_mid" CERTIFIED "$_mtip" 1
            else
                land_mark "$_mid" CERTIFIED "$_mtip"
            fi
            _i=$(( _i + 1 ))
        done
        "$forge" pr-close "$repo" "$pr_n" 2>/dev/null || true
        rm -f "$batch_file"
        local _all_ids; _all_ids="$(printf '%s\n' "${members_arr[@]}" | cut -d: -f1 | tr '\n' ' ' | sed 's/ /, /g' | sed 's/, $//')"
        printf 'verdict %s: PR %s red — no suite annotations; bisect halved (%d+%d)\n' \
            "$name" "$pr_n" "$_half" "$(( mc - _half ))"
        _attr_notify_red "$pr_n" "$name" "" \
            "No suite annotations: batch bisected, all members requeued" "" "$_all_ids"
        _meter_write "$name" "$mc" 0 0 "$attr_start"
```

The notification that reaches anyone reads *"No suite annotations: batch bisected, all members
requeued"*.

## 5. Down to a single member: ejected as unreproduced red

`spira/verdict.sh:283-285`, verbatim:

```
    # Single member with no annotations: fall through; empty suites_csv routes
    # the repro to diff-derived selection (no suites found → green), which lands
    # on the unreproduced-red track: first occurrence requeues, second ejects.

```

Nothing local provisions a VM, so the local repro of a single member comes back green, and the
member lands on the unreproduced-red track: first occurrence requeues, second **ejects**.

## What this adds up to

A drifted template makes the queue bisect a batch, requeue every member, and then eject
innocent beads one at a time, with every message on the way saying "unreproduced red". The
cause — a sentence the CI log contains in plain text — is never carried into a payload.

And therefore `sop-template-apt-drift`'s `MATCH` line, which exists precisely for that
sentence, **cannot fire on the automated path at all**. The runbook is reachable only by a
human or agent pasting a provision log into a payload by hand. A widened `MATCH` (`02-`) makes
that paste work for more phrasings; it does not create the paste.

## The positive control for "nothing reads it"

An empty grep is indistinguishable from a grep pointed at the wrong place, so:

```
$ grep -rn "provision" spira/*.sh cockpit/*.sh | grep -v "^spira/test-"
(no output)

$ grep -rn "annotation" spira/*.sh | head        # same glob, known-present token
spira/forge.sh:9: …
spira/verdict.sh:258: …
```

The glob resolves and finds `annotation`. It finds no mention of `provision` anywhere in the
harness's own programs.
