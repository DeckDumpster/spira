# What holds the spira store's space — 2026-11-18

Measurement for sp-ko4ecn. Subject: the `spira` Dolt database, restored from the durable copy
(`persist.state` finish 2026-11-18 14:03 PST; newest commit 2026-11-18 22:34Z) with `dolt clone` into a scratch directory, Dolt 2.4.2. The live server was
never queried. (`dolt backup restore` of the same copy copied every table file in 3 minutes and
then never wrote its refs in 30; `dolt clone` completed in about 12.)

The brain's store notes (`the-bead-store-filled-its-tmpfs-and-latched-failed`) record a 6.5 GB
uncompacted store and no breakdown of its contents; this is the first one.

## Result

**Commit history is 99.4% of the store. The rows at HEAD are 49 MB of 8.55 GB.**

| part | bytes | share |
|---|---:|---:|
| rows at HEAD, all tables (fresh repo, one commit, `dolt gc --full`) | 48,750,619 | 0.6% |
| history: 215,525 earlier commits (derived: copy size minus HEAD) | 8,504,357,097 | 99.4% |
| **durable copy on disk** (`du -sb`, clone of the copy) | **8,553,107,716** | 100% |

Live was 6,068,805,760 B after the 2026-11-18 online collection; the durable copy is 8.5 GB
because it has not been compacted since. Inference, not measured on a collected copy: the 49 MB
of current rows cannot account for more than 1% of either size.

## History by depth

`dolt clone --depth N` of the copy (these keep ~15 MB of uncollected overhead over the gc'd HEAD):

| commits kept | store bytes | bytes per added commit |
|---:|---:|---:|
| 1 | 63,765,539 | — |
| 100 | 67,748,379 | 40 KB |
| 1,000 | 94,442,658 | 30 KB |
| 10,000 | 334,809,330 | 27 KB |
| 50,000 | 1,938,391,691 | 40 KB |
| 215,526 (full) | 8,553,107,716 | 40 KB |

Extrapolating the 10,000→50,000 slope (40.1 KB/commit) to 215,526 commits predicts 8.57 GB
against 8.55 GB measured (0.3% off), so the depth curve is linear at about **40 KB per commit**
and the history term is independent of the HEAD measurement above. The history row in the first
table is derived, so it sums to the copy by construction; the extrapolation (0.3% off) is the
only independent check on it.

The history is 215,526 commits in 64 days (2026-09-15 to 2026-11-18), now
3,370 a day on average. The last seven days, from `dolt_log` grouped by date: 4,202 (11-18, partial),
3,598, 4,506, 1,724, 1,061, 1,035, 1,418. One commit is made per `bd` write:

| message prefix | commits |
|---|---:|
| `bd: update` | 159,673 |
| `bd: create` | 25,244 |
| `bd: close` | 16,021 |
| `bd: reopen` | 3,164 |
| `bd: claim` | 3,134 |
| `bd: dep` | 2,952 |
| `dependency: add` | 2,512 |
| `bd: comment` | 2,265 |

About eight commits per issue (26,532). Hypothesis, not measured: each commit rewrites the prolly
tree path to a changed `issues` row, and `issues` rows are wide (about 1.5 KB each at HEAD).

## Rows at HEAD, by table

Each table loaded alone from a `dolt dump` of HEAD into a fresh one-commit repo, then
`dolt gc --full`:

| table | rows | bytes |
|---|---:|---:|
| issues | 26,532 | 38,910,451 |
| dependencies | 17,060 | 5,094,855 |
| labels | 52,185 | 1,510,007 |
| comments | 2,278 | 1,305,032 |
| config | 597 | 299,641 |
| child_counters | 8,935 | 116,719 |
| schema_migrations | 66 | 7,917 |
| 10 others (≤ 4 rows) | ≤ 4 | ≈ 29,000 |

The per-table repos sum to 47.3 MB against 48.75 MB for the joint repo (3%, shared schema and
root overhead). `issues` alone is 80% of the current data.

## Largest consumer and the cut

The largest consumer is **commit history**: ~40 KB per commit across 215k commits. No table
comes close; shrinking `issues` by half would save 20 MB of 8,550.

The cut is to bound history: keep the newest N commits, squash the older ones into a base
commit, and `dolt_gc` fully. At the measured slope, keeping 10,000 commits holds the store near
335 MB and 1,000 near 94 MB, against 8.55 GB. Garbage collection alone cannot do it, because
every commit is reachable from `main`. Inference: that is why the collection took 9.3 GB only to
6.1 GB.

The collector (`store-persist`) is in this repo, but the cut is not a collector change: Dolt 2.4.2
has no truncate-history operation among `dolt gc` flags or `dolt_gc` options, so it needs an ancestry rewrite proven on a scratch clone. It
must run under the persist lock on a live RAM-backed store, and it invalidates every chunk the
durable backup shares with the old history, so the backup must be re-seeded. It also needs the
operator's decision on whether anything audits old log entries. That is a design, not an edit to
this item, and it is filed with the build and acceptance as **sp-ko4ecn.2**.

## Method notes

- Commit count and message mix: `dolt_log` on the clone, offline.
- HEAD size: `dolt dump -r sql` → `dolt init` + load → commit → `dolt gc --full`. Dropping tables
  from a copy that still has history does not shrink it, which is itself the finding.
- Reproduce on any copy: `dolt clone file://<durable>/db/spira x`, then the queries above.
