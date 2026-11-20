# Store relayout runbook

How to move the control store from the old single-file layout to the new one: a main file of
live keys, with audit events and request receipts in day files beside it. `spira-lc
admin-store-relayout` builds the new store from a stopped old one; nothing else does, and
`serve` never runs it. Each entry gives its commands, then **Success** and **If it fails**.

Set these before you copy a command: `STORE` (the value of `SPIRA_LC_STORE_PATH`), `RUN`
(`SPIRA_RUN`), `TRIAL` (a scratch directory on the store's own disk, never the rounds' disk),
and `REV` (the revision of the newest local snapshot, the newest name under
`$RUN/lc-ship/sent/snapshot/`).

The command prints one JSON report on stdout on every path: counts, bytes and elapsed
milliseconds per class (`live`, `events`, `receipts`), or `{"relayout": "refused", ...}`. Exit
0 is done, 3 is refused, 2 is usage. A refusal leaves no file at `--to` and no day file
beside it. The old file is opened immutable and never written.

## Trial on a copy, with the store running

The shipper keeps consistent snapshots of the old file. Measure on one: the store keeps
serving, and the copy is rate-limited so it does not starve the store's disk.

```sh
mkdir -p "$TRIAL"
rsync --bwlimit=100000 "$RUN/lc-ship/sent/snapshot/$REV" "$TRIAL/old.db"
sha256sum "$TRIAL/old.db" | cut -d' ' -f1 | diff - "$RUN/lc-ship/sums/$REV"
/usr/bin/time -v spira-lc admin-store-relayout --from "$TRIAL/old.db" --to "$TRIAL/new/control.db" > "$TRIAL/report.json" 2> "$TRIAL/time.txt"
grep 'Maximum resident set size' "$TRIAL/time.txt"
cat "$TRIAL/report.json"
```

Record the report, the elapsed time and the peak RSS where the cutover is scheduled: the
downtime is that elapsed time plus the stop and start.

When the report is recorded, remove the trial:

```sh
rm -r "$TRIAL"
```

**Success:** the checksum diff prints nothing, `report.json` has `"relayout": "certified"`, and
`events` and `receipts` show what was copied and left behind.
**If it fails:** a checksum that differs means the copy is damaged or the snapshot was pruned
while it copied; copy again from the newest one. A refusal names its reason in `error`: `the
old file has applied N schema migrations` means this `spira-lc` is older than the store;
`certification failed` names a row and is never to be worked around: stop and report it.

## Compaction

A main file built before incremental auto-vacuum keeps its free pages: deleted rows leave them
in the file and `reclaim_pages` skips a file that is not incremental. `spira-lc
admin-store-compact --store <main file>` rewrites the stopped main file once with
`auto_vacuum=INCREMENTAL`; after it the per-commit reclaim gives pages back while the store
serves. Snapshots are written incremental, so a restore keeps the property. Day files are not
touched. It prints one JSON report: `bytes_before`, `free_bytes_before`, `bytes_after`,
`free_bytes_after`, `auto_vacuum` and `elapsed_ms`, or `{"compact": "refused", ...}`. Exit 0
is done, 3 is refused, 2 is usage.

Trial on a copy, with the store running:

```sh
mkdir -p "$TRIAL"
rsync --bwlimit=100000 "$RUN/lc-ship/sent/snapshot/$REV" "$TRIAL/old.db"
sha256sum "$TRIAL/old.db" | cut -d' ' -f1 | diff - "$RUN/lc-ship/sums/$REV"
spira-lc admin-store-compact --store "$TRIAL/old.db" | tee "$TRIAL/report.json"
rm -r "$TRIAL"
```

The downtime is the trial's `elapsed_ms` plus the stop and start. Record it where the stop is
scheduled. Then, announcing the stop first:

```sh
systemctl stop lc-serve.service spira-lc.service
test ! -s "$STORE-wal"
spira-lc store-usage --file
spira-lc admin-store-compact --store "$STORE"
systemctl start lc-serve.service spira-lc.service
spira-lc store-usage --file
```

**Success:** `auto_vacuum` is 2, `free_bytes_after` is 0 and `bytes_after` is about the live size;
on later days `free_bytes` stays near 0 while the drains run.
**If it fails:** `the store is open` means a service is still up; stop it and run again. A
non-empty `-wal` means the store did not close cleanly: start and stop the store once, then
repeat. A refusal writes nothing, so the file is as it was.

## Cutover

The store is down for the whole of this entry. Announce the stop first.

```sh
systemctl stop lc-serve.service spira-lc.service
test ! -e "$STORE-wal"
mv "$STORE" "${STORE%.db}-old-layout.db"
spira-lc admin-store-relayout --from "${STORE%.db}-old-layout.db" --to "$STORE"
systemctl start lc-serve.service spira-lc.service
```

`SPIRA_LC_STORE_PATH` is unchanged: the new store takes the old one's name. Start the store
only on a release that reads the new layout. Report the sizes from the report, and from
`spira-lc store-usage` once the store is up.

**Success:** the report says `certified`; the store starts; `$STORE` and its `*.events-*.db` and
`*.receipts-*.db` files sit beside each other.
**If it fails:** `the store is open` means a service is still up; stop it and run the command
again. A non-empty `-wal` means the store did not close cleanly: move the old file back to
`$STORE`, start the store once and stop it, then repeat. Any other refusal left nothing at
`--to`: fix the cause and repeat, or take the rollback.

## Rollback

```sh
systemctl stop lc-serve.service spira-lc.service
ASIDE="$STORE.new-layout"; mkdir "$ASIDE"
mv "$STORE" "$ASIDE"/
for f in "${STORE%.db}".events-*.db "${STORE%.db}".receipts-*.db; do [ -e "$f" ] && mv "$f" "$ASIDE"/; done
mv "${STORE%.db}-old-layout.db" "$STORE"
```

Then activate the previous release and start the store.

**Success:** the store starts on the previous release at the old file's revision.
**If it fails:** the old file was never written; check that the name restored is the one the
cutover moved.

## Deleting the old file

`${STORE%.db}-old-layout.db` is the only copy of the history older than the retention window.
It stays until the operator who owns the store says to delete it.
