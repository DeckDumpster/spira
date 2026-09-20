# Measurement method — spike sp-krxs8, 2026-09-20

Every number in `../why-a-bd-call-costs-250ms.md` was produced on the operator's box on
2026-09-20 by the commands recorded in this directory. This file states the conditions,
because a cost estimate that does not say which conditions it was taken under is not a
cost estimate.

## The box

    /proc/cpuinfo processors   16
    nproc                       1      <- nproc honours CPUQuota; 16 is the real count
    load average             4.6-6.1   during the session
    RAM                         7 GB total, ~3 GB in page cache
    $SPIRA_DB backend       dolt sql-server (server mode), 3064 issues, 3.6 GB on disk
    test fixture backend    embedded Dolt, private directory, 3064 issues, 439 MB
    bd                      v1.2.1, 202 MB, CGO, `-tags gms_pure_go`, not stripped
    bd source               v1.2.1 exactly (tag verified), cached locally

## The fence

The session that took these measurements ran under `CPUQuota=70%`. That is NOT the fence
the gate, sentinel or suites run under, and the difference is the whole point of
`law-measure-inside-the-fence`:

    spira-sentinel.service    CPUQuota=40%   OnUnitActiveSec=2min
    spira-suites.service      CPUQuota=40%   OnUnitActiveSec=1h
    spira-gate-check.service  CPUQuota=10%   OnUnitActiveSec=2min

So wall-clock figures taken in this session are the 70% row of `03-cpu-quota-scaling.md`
and must be rescaled before being read as a sentinel or suite cost. Where a number is
quoted for one of those units, it was either taken through `systemd-run --property=CPUQuota`
at that unit's quota or derived from the CPU-time figure.

## Why CPU time, not wall time

Wall time on this box is dominated by contention: the same `bd --version` measured
p50=125 ms and max=2317 ms within one 20-run batch while other aeons worked. CPU time
(`/usr/bin/time` %U+%S) is stable to about +/-10 ms across the same runs and is immune to
both throttling and contention, so it is the primary figure throughout. Wall time is
reported alongside it, and the relation between them is measured directly in
`03-cpu-quota-scaling.md`.

## Reproducing

`rerun.sh` in this directory re-takes every measurement. It needs a `bd` on PATH, a
server-mode store at `$SPIRA_DB`, and an embedded fixture; it writes to a scratch
directory and drops it. It does not write to any store.
