# beads-store — commit a beads Dolt store's dirty tables through the real engine

Bead: **sp-sghmt** (beads-push may not commit dirty tables in server mode).

## 1. Intent

`beads-push.sh` must commit a beads store's dirty tracked tables (config — statutes,
memories; bd never creates a Dolt commit for those writes) before every push, and must
know for certain whether that commit actually cleared the working set. Two engines can
read a Dolt store and they do not always agree (2026-09-15 scar, sp-e1l1): `bd sql
"SELECT COUNT(*) FROM dolt_status"` answered 0 for a working set the `dolt` CLI, reading
the same database, answered 1 for. `bd dolt commit` returned exit 0 and "Nothing to
commit." for that same dirty set.

Production never hits this: `SPIRA_DOLT_DATA` is configured there, so `beads-push.sh`'s
existing engine resolution picks the `dolt` CLI against that data directory and it works
(verified live for this bead — see the bead's closing note). The gap is every **other**
server-mode store: a store with a running `dolt sql-server` but no local `SPIRA_DOLT_DATA`
configured — which is exactly what a server-mode `testdb.sh` fixture is (`testenv testdb`
points `.beads` at a private server by port; it sets no `SPIRA_DOLT_DATA`). With no data
directory to resolve, `beads-push.sh`'s old fallback was the `bd` engine — the one side of
the 2026-09-15 scar already known not to be trusted for `dolt_status`. Reproduced
2026-09-29 with `test-beads-push-commit.sh` run under `SPIRA_TESTDB_MODE=server`:
"pre-push commit did not clear the working set: 1 dirty table(s) before, 1 after
(engine: bd)."

**The fix: never ask `bd`.** Every Dolt store that can be committed at all exposes a real
`dolt` engine one of two ways — a local `--data-dir`, or a live `sql-server` reachable by
`--host`/`--port` (already recorded in `.beads/metadata.json` by `bd init --server` and
kept current by `testenv testdb`, and present on production's own store). `beads-store`
resolves whichever one exists and talks to it directly; if neither is discoverable, that
is a hard failure, not a silent downgrade to a less trustworthy answer
(law-absence-needs-a-positive-control).

## 2. Scope

- The pre-push commit step only: count `dolt_status`, commit if dirty, re-count to
  confirm it cleared. This is the whole surface `test-beads-push-commit.sh` and the
  production scar cover.
- **Not in scope:** the post-push remote-head verification (`_bp_head`, the fetch step)
  and branch detection in `beads-push.sh` stay as they are. They read *committed* history
  (`dolt_log`), not working-set state, which is the half of the engine disagreement that
  is actually proven broken; `test-beads-push-verify.sh` already covers that path with
  its own fully-stubbed fixture and is unaffected by this change. Folding them in here
  would widen this bead past its acceptance criteria for no proven defect.
- **Not in scope:** fixing `bd`. `bd`/`bd-embedded` are external dependencies (`npm install
  -g @beads/bd`), not part of this repository.

## 3. Contract

Binary `beads-store`, one verb:

```
beads-store commit --db <path> --message <text>
```

- stdout on success: one line, the number of dirty tables that were committed (`0` if the
  working set was already clean — a no-op, never an empty commit).
- exit 0: success (including the clean no-op case).
- exit 1: usage error (bad or missing flag).
- non-zero (2): failure. stderr holds one line naming what could not be determined or
  verified — engine resolution failure, the commit call itself failing, or a commit that
  ran but left the working set still dirty. The caller (`beads-push.sh`) treats any
  non-zero exit as a hard stop, the same way it already treats every other `fail_out` in
  that script: an unverified push is not a backup.

## 4. Engine resolution

Same rule as production's own path today, generalised:

1. `SPIRA_DOLT_DATA` env var, if set and a directory: the `dolt` CLI's `--data-dir` mode
   against it. Database name from `<db>/.beads/metadata.json`'s `dolt_database`, default
   `spira`.
2. Else `<db>/.beads/embeddeddolt/<name>`, if present: `--data-dir` against it, database
   name `<name>`.
3. Else `<db>/.beads/metadata.json`'s `dolt_server_port` (falling back to the bare port in
   `<db>/.beads/dolt-server.port`, which `testenv testdb` also writes): `--host`
   (`dolt_server_host`, default `127.0.0.1`), `--port`, `-u` (`dolt_server_user`, default
   `root`), `-p ''`, `--no-tls` — a direct SQL-client connection to the running server.
   Verified live against production's own store and against a `testenv testdb` server-mode
   fixture for this bead (see bead notes).
4. Else: refuse. No engine could be resolved; this is exactly the "no engine could count
   dolt_status" refusal `beads-push.sh` already made, now made correctly regardless of
   which server-mode shape the store is.

Every SQL call goes through the same `dolt ... --use-db <name> sql -q <query> -r csv`
invocation regardless of which mode resolved, so there is one code path to trust, not two.

## 5. Test strategy

- **Unit, pure:** `resolve()` against a temp directory tree — every branch of §4, plus the
  refusal path, with no live dolt process.
- **Unit, with a stub `dolt`:** `testkit::write_exe` a fake `dolt` script that answers
  `dolt_status` and `DOLT_COMMIT` deterministically, covering: clean store (no-op),
  dirty store that commits and clears, a commit call that fails, and a commit that
  reports success but does not clear (the exact production scar) — asserting the message
  text names the before/after counts and the engine.
- **Suite:** `test-beads-push-commit.sh` unpinned from embedded mode (sp-34ru2's
  `unset SPIRA_TESTDB_MODE` removed) now passes under `SPIRA_TESTDB_MODE=server` because
  `beads-push.sh`'s call site uses this binary instead of `bd dolt commit`.
