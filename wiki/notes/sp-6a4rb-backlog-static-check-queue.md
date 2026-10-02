# sp-6a4rb backlog-growth: queue.sh static check (sp-qzw0t)

Continuing the sp-6a4rb / sp-39wgj / sp-fv6wy / sp-btb4d / sp-qzw0t diagnosis chain.
Prior static checks ruled out O(n^2) in landing.sh's per-branch loop, confine.sh,
and gate.sh (see sp-qzw0t description). This note covers the one item those left
unchecked: queue.sh.

## Finding

`queue.sh cmd_step` (queue.sh:325-336) — the only queue.sh entry point landing.sh
calls (landing.sh:1665, 1676, 1754, all three sites *outside* the per-branch loop,
same shape as the spira_repos post-loop bookkeeping already ruled out) — has no
loop over the branch list or backlog itself:

    cmd_step() {
        bash "$HERE/verdict.sh" "$name"
        _batch_cut "$name"          # -> bash "$HERE/batch.sh" "$1"
        [ "$(repo_land "$name")" = "queue.local" ] && cmd_publish "$name"
    }

It's three fixed-cost calls per repo per pass: verdict.sh, batch.sh (via
`_batch_cut`), optionally cmd_publish. No O(n) or O(n^2) mechanism at this level.

`queue.sh`'s own `open-batch` (queue.sh:791-856, the spira-lc admission path) DOES
contain a per-candidate loop with a per-id `bd`/`bdjson` read inside it (lines
817-826, `spira_bead_status` + `bead_has_label`/`bdjson show` per candidate,
same shape as confine.sh's per-branch re-read already flagged as O(n) round-trip
cost). But `cmd_step` — the call landing.sh actually makes — never calls
`open-batch`; that function is reached only via queue.sh's own `open-batch`
subcommand (hand-invoked / spira-lc cutover path), not the automated landing hot
path. So it does not contribute to sp-6a4rb's per-pass backlog-growth curve.

## Not yet checked

`batch.sh` itself (reached via `_batch_cut` -> `bash "$HERE/batch.sh" "$1"`,
called once per repo per pass from `cmd_step`) has NOT been read for a per-branch
or per-backlog-entry loop. It is the last unchecked script in the landing hot
path chain (landing.sh loop, confine.sh, gate.sh, queue.sh all now checked).
Filed as sp-<followup> to continue.

## Standing conclusion (unchanged from sp-qzw0t's prior static findings)

No O(n^2) mechanism found anywhere read so far. The two confirmed O(n)-per-pass,
scales-with-backlog costs are:
- confine.sh's per-branch `bdjson show $ID` (landing.sh:801-804 doc comment)
- queue.sh open-batch's per-candidate `bd`/`bdjson` read (NOT on the automated
  hot path, so likely irrelevant to sp-6a4rb specifically)

If batch.sh also comes back clean, the honest remaining explanation is O(n)
per-branch cost simply multiplying with a bigger backlog (capacity/scheduling),
not a hidden O(n^2) bug — meaning sp-6a4rb's "something pathological" premise
may not hold. A live instrumented pass (scan / per-branch re-read / gate run /
verdict / merge / push timings) against the current backlog is still the only
way to settle this for certain; no live pass has been run yet across this whole
diagnosis chain (four+ sessions of static reading only).
