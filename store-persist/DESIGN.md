# store-persist — a Dolt store served from RAM, durable on disk

## 1. Intent

Dolt fsyncs its chunk journal on every transaction commit. On a disk that builds saturate, a
4 KB synchronous write takes hundreds of milliseconds and every store call takes seconds. The
live store fits in RAM, so the server's data directory (`SPIRA_DOLT_DATA`) goes on a RAM-backed
filesystem, where fsync costs nothing, and a copy on disk (`SPIRA_DOLT_DURABLE`) is kept current
off the serving path:

- **restore** — before the server starts, every database the copy holds and RAM lacks is
  copied in. A database already in RAM is newer than its copy and is kept.
- **pass** — every database is synced into the copy; the pass records the moment it began.
- **run** — a pass every `SPIRA_DOLT_PERSIST_INTERVAL_SECS`, through the running server.
- **daily rebuild** — once a day `run` collects each live database (through `spira-lc admin-store-gc --db <db>`; only the history bound's final full collection runs `dolt` itself), then rebuilds a copy grown past `SPIRA_DOLT_PERSIST_COMPACT_PCT` of its live size. A failed collection or rebuild appends one line, with Dolt's own error, to `<run>/dolt-collect.log`, which `spira/watchers` reads.
- **collect** — `run` also collects each live database every `SPIRA_DOLT_PERSIST_GC_INTERVAL_SECS` (0 disables). A collection is never refused for want of space: a Dolt error is the report. Both schedules are kept in `<durable>/upkeep.state`, so a restart does not reset them.
- **history bound** — `compact` squashes a database's log past `SPIRA_DOLT_PERSIST_HISTORY_KEEP` (0 disables): everything but the root and the newest `keep - 1` commits becomes one base, so the log holds `keep + 1`. Commit history is what holds the store's space (about 40 KB a commit; the rows at HEAD are under 1%), and `dolt_gc` cannot reach it because every commit is reachable. The rewrite is a `dolt_rebase` on a side branch, never on `main`, whose working set holds the store's uncommitted writes and which a rebase refuses to start on when dirty. Commits that land meanwhile are cherry-picked onto the side branch under the persist lock, and `main` is soft-reset onto it: the working set is untouched, so a commit slipping in after the last look leaves the log, never the data. A full `dolt_gc` follows, and the durable copy is rebuilt whatever its size, since a rewritten history shares no chunks with the old. A rebase replays every squashed commit (about 30 ms each), so the first run on a large log takes hours; a failure leaves the log as it was. Nothing in the tree reads an old `dolt_log` entry (the only readers take the head hash), so the squash imports the dead as stored and certifies nothing it discards.
- **age** — red once the newest finished pass began more than `SPIRA_DOLT_PERSIST_MAX_LAG_SECS` ago.

**The loss bound.** Every write committed before the newest finished pass began is on disk. A
host crash loses the writes since then: at most one interval plus the duration of the pass in
flight. `age` turns that bound into an alarm; it is not a guarantee a saturated disk can be
made to keep.

A pass that copies nothing leaves its record to the filesystem — no flush — so a crash may leave an
older record; the next pass then syncs more, never less. Stop and seed passes always flush.

## 2. The mechanism, and what it is not

**Chosen: `CALL dolt_backup('sync-url', 'file://<copy>/db/<db>')`, one per database per pass.**

- A backup carries branches, tags **and working sets**. The store runs with
  `dolt_transaction_commit: false`, so nearly every write lives only in a working set.
- A sync is incremental: it copies the chunks the copy lacks.
- It runs inside the server, in its own session; serving sessions do not wait on it. The
  disk writes, and their fsyncs, are the backup's, not the commit path's.
- Dolt writes a file backup crash-consistently: each table file is written, fsynced and
  renamed, then the manifest is written, fsynced and renamed, then the directory is fsynced
  (read off `strace` of a sync). A crash mid-sync leaves the previous manifest, naming table
  files that are all still there. One copy per database suffices; no A/B slots.

**Rejected:**

| alternative | why not |
|---|---|
| push-on-write replication (`dolt_replicate_to_remote`) | replicates Dolt commits only, and this store's writes are working-set transactions; synchronous mode puts the remote's disk back on the commit path |
| cluster standby replication | replicates every transaction, but needs a second server (RAM and CPU), a remotes API port, and a role flip to restore from the standby, whose persisted role is `standby`; its lag on the same saturated disk is no better bounded |
| periodic file copy of the data directory | Dolt's own manual: unsafe while any Dolt process reads or writes it |
| block-device snapshots | consistent, but need root and a volume manager per snapshot |
| `dolt backup restore` to restore | correct but slow: it walks every chunk. A 3.3 GB store took 906 s to restore and the same copy took 5 s with a plain file copy, and both opened to the same `dolt_hashof_db()` |

**How a pass reaches the server.** As the server's own local superuser, over TCP, with the
secret from `<data>/.dolt/sql-server.info` passed in the environment and the SQL on stdin. The
CLI run in a database directory would otherwise fall back to opening the store itself when the
server is down, and hold the lock the restarting server needs. An offline pass (the unit's
`ExecStopPost`, and seeding) is refused while a live server holds the directory.

## 3. Layout of the copy

```
<durable>/db/<db>/                      the backup Dolt writes (manifest, table files, oldgen/)
<durable>/meta/db/<db>/repo_state.json  per-database files a backup does not carry
<durable>/meta/db/<db>/config.json
<durable>/meta/root/dolt-server.yaml    the server config, kept for the next reboot
<durable>/meta/root/doltcfg/*           users and grants (privileges.db), branch control
<durable>/git-remote-cache/<db>/        dolt's git remote cache, linked from <data>/<db>/.dolt
<durable>/persist.state                 start_ms, finish_ms, dbs of the newest finished pass
<durable>/store-persist.id              written once seeding succeeded
<durable>/LOCK                          flock held by a pass, a compaction swap, a seed
<durable>/compact/<db>/                 a rebuild in progress
```

The git remote cache is linked to disk because the server never reads it while serving and it
can be larger than the store.

## 4. Guards, each failing closed

- A RAM-backed data directory with `SPIRA_DOLT_DURABLE` empty is refused by `restore` (the
  server does not start) and by `pass`.
- `restore` and `pass` refuse a copy that was never seeded: a missing or mistyped durable path
  must not start an empty store.
- `restore` refuses to run under a live server, refuses a `dolt-server.yaml` whose `data_dir`
  is not `SPIRA_DOLT_DATA`, and refuses a database whose copy has no `repo_state.json`.
- A restored database is staged under `<data>/.dolt/store-persist/` (the server serves every
  child of the data directory, dot-named ones included, but not nested ones), opened offline
  as a check, and only then renamed into place.
- A pass refuses when the copy holds a database the data directory does not serve. That is
  the state a failed restore leaves, and syncing past it would be how a copy goes stale.
- `seed` refuses a copy that already exists; it does not carry the old directory's server
  config, which names the old directory.
- Compaction rebuilds beside the copy without the lock, catches up under the lock, and swaps
  the two with one `RENAME_EXCHANGE`: there is never a moment with no copy.

## 5. Units

- `dolt-beads.service`: `ExecStartPre=store-persist restore`, `ExecStopPost=store-persist
  pass --offline`, `TimeoutStartSec=900` (a restore copies the whole store). With the key
  empty both are no-ops on a disk-backed directory.
- `dolt-persist.service`: `store-persist run`. Installed only when `SPIRA_DOLT_DURABLE` is set.

## 6. Measurements (fixture servers only)

| | value |
|---|---|
| autocommit insert, data dir on tmpfs | 1.27–1.29 ms/txn |
| same, with a sync every second running | 1.31–1.35 ms/txn |
| same, data dir on the saturated disk | 7–26 ms/txn, up to 179 ms/txn under heavier load |
| incremental sync of a 3.3 GB store after hundreds of writes | 0.2–2 s (up to 8 s on a 15 MB store while the disk was saturated) |
| first full sync of a 3.3 GB store | 501 s |
| `dolt backup restore` of that copy / file-copy restore | 906 s / 5 s |
| crash suite (1 s interval, container) | newest pass began 0.3–1.8 s before the SIGKILL; no row acknowledged before it was lost |

`probe/probe.sh` holds the commands these came from.

## 7. RAM

The tmpfs holds the live store, its journals, Dolt's temp spool, and transiently twice a
database while automatic GC writes the new generation before deleting the old. Size a
dedicated tmpfs at about three times the live store and cap it, so a runaway spool fills the
tmpfs rather than the host's memory. Right after a reboot the store occupies up to
`SPIRA_DOLT_PERSIST_COMPACT_PCT` of its live size, until the next collection.
