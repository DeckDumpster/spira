# 1 — A store client that runs in-process, and what it costs to build one

`bd` ships a public Go SDK for exactly this: `beads.OpenBestAvailable(ctx, beadsDir)` returns
the same `Storage` the CLI uses, against the same backend `metadata.json` names — embedded
Dolt through cgo, or a dolt sql-server. The package doc calls it "the CONSUMER surface: it
opens and uses bd's own storage."

    ~/.cache/beads-src/beads.go        package doc and type aliases
    ~/.cache/beads-src/beads_cgo.go:25 func OpenBestAvailable(ctx, beadsDir) (Storage, error)

So the in-process option needs no re-implementation of the model above SQL and no second
schema. That is the single fact that most changes the costing below.

## The probe

`poc/inproc/main.go` on branch `spike/sp-yty8w-poc`. It opens the store once and runs the
harness's representative calls N times each, reporting CPU (getrusage delta) and wall per
call, and the row count the call returned.

    module spikeprobe
    require github.com/steveyegge/beads v0.0.0
    replace github.com/steveyegge/beads => <bd source checkout at v1.2.1>

    CGO_ENABLED=1 go build -tags gms_pure_go -o probe .

Both flags are required and for the reasons `spira/build-bd.sh` already records: a
CGO_ENABLED=0 build refuses embedded mode at runtime, and a bare cgo build dies at the C
linker on ICU without `gms_pure_go`.

## What it cost to build

- 158 MB binary, 133 lines of Go.
- First build 4 m 53 s wall (the link dominates); incremental rebuilds 27 s with the module
  cache warm. `go mod tidy` pulled the full bd dependency set.
- Two compile errors and one runtime error to get the call list right, all of them the
  SDK telling me its own contract: `Related` refuses without a direction, `CountResult` has
  `Total` and not `Count`, and the `examples/library-usage` program shipped in the bd source
  does not compile against v1.2.1 — it calls `store.GetReadyWork`, which the capability
  refactor replaced with `store.IssueReader().Ready(...)`.
- One trap caught and fixed before it produced a number: `ListRequest.Limit` nil is not the
  CLI's `--limit 0`; it is a default page of 50. The first production run "showed" the
  in-process path serving `list --status open,closed` 4.4x faster than the CLI while
  returning 50 rows against the CLI's 3,083. Every table below prints the row count for that
  reason (`law-absence-needs-a-positive-control`).

Call it a day of work to write, a week to make a library a harness could depend on.

## Cardinality check — the probe and the CLI answer the same questions

Production store, same session:

| call | probe rows | CLI rows |
|---|---|---|
| `list --status open --limit 0` | 67 | 67 |
| `list --status open,closed --limit 0` | 3,083 | 3,083 |
| `list --status open --label spira --limit 0` | 33 | 33 |
| `ready --limit 0` | 63 | 63 |
| `count --status open` | 67 | 67 |
| `query 'status=open AND priority<=1' --limit 0` | 7 | 7 |

The speedups below are therefore not the in-process path doing less work.
