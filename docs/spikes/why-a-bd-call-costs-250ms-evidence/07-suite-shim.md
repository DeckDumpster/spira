# 07 — a real suite under a counting shim

Transcript, preserved verbatim. See `00-method.md` for conditions.

```
Spike sp-krxs8, 2026-09-20. Session fence CPUQuota=70%; suites run at 40%.

The bead's premise is that ~80% of a suite's wall time is inside bd. Confirmed on a small
fixture-using suite by pointing TESTDB_BD at a wrapper that times each call.

  shim:
    #!/usr/bin/env bash
    s=$(date +%s%N); bd-embedded "$@"; rc=$?; e=$(date +%s%N)
    printf '%s\t%s\n' "$(( (e-s)/1000000 ))" "$1" >> "$BD_SHIM_LOG"; exit $rc

  $ TESTDB_BD=<shim> SPIRA_TESTDB_BD=<shim> bash spira/test-archivist-cited-bead.sh
    suite rc=0 wall=9723ms
    bd calls: 10
    total in bd: 6298ms (65% of wall)
    7 passed, 0 failed

  630 ms per bd call, embedded, on a SMALL fixture, inside a 70% fence. The bead's own
  measurement of test-census (143 calls, avg 407 ms, 58 s of a 72 s suite = 81%) is the
  same phenomenon on a bigger suite.

  Note the per-call figure: 630 ms embedded vs 167 ms server-mode for comparable work (04).
  The gap is the embedded store open, paid once per process (02). This is why the resident
  option (06) does nothing for suites and why fixture work (sp-nyfng) is the suite lever.

  This suite is at the small end — 10 calls. It was chosen to be cheap to run inside the
  spike, not because it is representative of the slow suites. The bead names test-poison
  (340 s) and test-landing (327 s) as the worst, and those are slow because they drive
  whole sentinel and landing passes, each of which makes many bd calls of its own — i.e.
  the N+1 in 04 is a large part of what the slowest suites are waiting for.
```
