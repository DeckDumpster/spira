# Measurement method — spike sp-yty8w, 2026-09-20

Every number in `../which-lever-halves-the-harness-bd-time.md` was taken on the operator's
box on 2026-09-20 by the commands recorded in this directory. This file states the
conditions, because a cost figure that does not say which conditions produced it is not a
cost figure.

This spike continues `../why-a-bd-call-costs-250ms.md` (sp-krxs8) rather than repeating it.
That document decomposed one bd call; this one costs the four levers the decomposition left
open, and runs the falsifier that document named.

## The box

    /proc/cpuinfo processors   16
    nproc                       1        (nproc honours CPUQuota)
    load average             5.5-7.8     during the session
    bd                       v1.2.1, 202 MB, CGO, -tags gms_pure_go
    bd source                ~/.cache/beads-src at tag v1.2.1 (git describe --tags)
    Go toolchain             go1.27.1 linux/amd64

## The three stores measured

| name | backend | size | where |
|---|---|---|---|
| production | dolt sql-server, port 3307 | 3,083 issues, 67 open | the live store; READS ONLY |
| bench-500 | embedded Dolt | 500 issues (333 open) | a private copy of the session's shared fixture, seeded |
| sentcount | dolt sql-server, port 3308 | 60 issues, all open | built by `testdb_up` in server mode, dropped after |

The embedded fixture was seeded through `testdb.sh`'s shared baseline (`TESTDB_SHARED=1`),
so no database was built from scratch. The copy used for benchmarking was placed on `/`
rather than `/tmp`: `/tmp` here is a tmpfs, and an embedded store read out of RAM is not the
store the suites open.

## The fence

This session ran under `CPUQuota=70%`. The sentinel and the suites run at 40%
(`law-measure-inside-the-fence`). Wall figures taken here must be multiplied by about 1.85
to be read as sentinel cost — that ratio is sp-krxs8's measured 314/170 at those two quotas,
and is used rather than re-measured.

## Why both CPU and wall are reported

CPU time (`getrusage` inside the probe, `/usr/bin/time %U+%S` outside it) is immune to the
quota and to the other aeons sharing the box; it is the stable figure. But for a client
talking to a *server*, CPU on the client side is not the cost — the work happens in the dolt
process — so wall is the honest figure there and CPU understates by design. Both are
printed everywhere; the document says which one it is using and why.

One run in this session shows exactly why an unqualified wall figure cannot be trusted: a
20-iteration `list-open` on the embedded fixture returned p50 1,359 ms and p90 61,817 ms.
The p90 is the box, not the call.

## Reproducing

`rerun.sh` in this directory re-takes every measurement. It needs `bd` on PATH, the Go
toolchain, a checkout of the bd source at v1.2.1, and a shared fixture exported by
`testdb.sh`. It writes to scratch directories and to no store except the sentinel fixture it
builds and drops. The production store is read from and never written.
