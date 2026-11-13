# Replay fixtures

Typed records of the loop's own history, for the replay (sim), the goodput test and its 20-day scenario test. Every line is a closed shape (`sim/tests/it/fixture_schema.rs`), stamped by its producer's own clock in UTC. No certify or gate output, `.result` text, log line, title, body, path or hostname is kept.

Records are newline-delimited JSON, one object per line, in `.ndjson` files: the harness refuses any `*.jsonl` path as beads data (`spira/exclude.sh`).

## Slices

| Slice | Window (UTC) | Why this window |
|---|---|---|
| `1031/` | 2026-10-31T00:00Z to 2026-11-01T00:00Z | the UTC day the 10-31 evidence counted (rounds cut, landed, re-runs); it holds the stacked parent (opened 03:12Z), both harm windows and the halt |
| `1021-1025/` | 2026-10-21T07:00Z to 2026-10-26T07:00Z | Pacific 10-21..10-25, the held-out slice; never used to seed or tune anything |
| `landed-20d.tsv` | 2026-10-12T07:00Z to 2026-10-31T20:01:12Z | Pacific 10-12..10-31, cut at the moment throughput-bounds §1 took its count, so its git column is that table's (its 10-31 row is partial) |

Each slice also holds the earlier events of every batch opened up to 6 h before its start, so a round open at the start arrives with its members; filter on `at` for counts inside the window, as the tallies below do.

## Anonymisation

- Repos are `repo-a` to `repo-l`. Bead ids are `b-<n>`, keeping a dotted child's suffix (`b-7.2` is a child of `b-7`). Batch ids keep their timestamp: `<repo>-<YYYYMMDDTHHMMSSZ>`, `<repo>-r<n>-<MMDDTHHMMSS>` (numbered rounds), `<repo>-q-<ts>` (PR-mode queue rounds), `<repo>-solo-<bead>`.
- Commit shas are 12 hex digits of a keyed hash: equal shas stay equal across every file, and nothing maps back without the key.
- The real-to-anonymised key is private to the capturing host and is not in this repository.

| Repo | Role |
|---|---|
| `repo-a` | the home repo: the harness itself, whose landings stopped behind the landing-stop bead and the disk-preflight bead |
| `repo-b` | the stacked-pointer repo: on 10-31 every round after one parent was refused as stacked on that parent, which sat in ATTRIBUTING and never landed |
| `repo-c` | the repeat-red repo: on 10-31 one member was re-cut at one tip after every red |
| `repo-d` | a held-out lane (10-21..25): one of the two lanes whose rounds were refused almost every time |
| `repo-e` | a held-out lane (10-21..25): the other of the two |
| `repo-f .. repo-l` | the other seven repos, present for completeness |

| Bead | Role |
|---|---|
| `b-1226` | the landing-stop bead |
| `b-1254` | the repeat-red member |
| `b-1261` | the revert of the landing-stop bead |
| `b-1264` | the disk-preflight bead |

## Provenance

Captured by the operator's capture program (`replay-capture/capture.sh <capture-dir> <worktree>` at tools commit 087d2ad, kept with the operator's tools, outside this repository). It reads production and never writes it: the lifecycle through read-only `spira-lc facts-query` SELECTs in 2-hour chunks, `run/tsd` and two logs by copying the window's lines, git through `git log` on each repo's landing ref.

| Step | Captured (UTC) |
|---|---|
| lifecycle-1031 | 2026-11-01T11:23:18Z |
| lifecycle-1021-1025 | 2026-11-01T11:23:55Z |
| lifecycle-landed-20d | 2026-11-01T11:23:58Z |
| tsd | 2026-11-01T11:23:58Z |
| logs | 2026-11-01T11:23:58Z |
| git | 2026-11-01T11:24:01Z |
| written | 2026-11-01T11:24:02Z |

| File | Source | 1021-1025 lines | 1031 lines |
|---|---|---|---|
| `round.ndjson` | lifecycle `batch` rows and applied `batch` events (state changes), members from `MemberAdded`/`Eject`; `cause` from `run/tsd/cert-pass.jsonl` for a pass's end; `verdict` from `run/tsd/batch-round.jsonl`; `parent` from the batch ids `run/rounds.log` names in its stacked-round refusals | 3436 | 1183 |
| `cert_pass.ndjson` | `run/tsd/cert-pass.jsonl`; `pass_key` is a keyed hash of the batch's (member, tip) set when the lifecycle started that pass | 695 | 252 |
| `transition.ndjson` | applied lifecycle `batch` and `bead` events, heartbeats (`Renew`, `Phase`) left out | 13170 | 4016 |
| `stop.ndjson` | typed by hand (see below): this host keeps scoped stops as current-state files and no stop history | 0 | 5 |
| `land.ndjson` | lifecycle `bead` events entering LANDED, with the land commit from the bead's `delivery` row; `reverts` typed by hand | 471 | 49 |
| `alarm_attempt.ndjson` | `run/reconciler.log` alarm lines (escalate, no-effect mail, raise-ask) and the batcher's failed land-unverified alarms in `run/rounds.log`, each timed by the abandon of the round it names | 35 | 42 |
| `attribution.ndjson` | lifecycle `PassRed` followed by an `Eject` in the same batch before the next pass: the ejected member is the owner, the other members are innocent | 30 | 13 |
| `landed-20d.tsv` | lifecycle `bead` events entering LANDED; git `: land` commits per throughput-bounds §1; direct and round-merge landings | 20 days | |

What the builder left out, and why:

- 1021-1025: 495 batch-round rows joined to a lifecycle batch.
- 1021-1025: 533 batch-round rows with no lifecycle batch within 120 s.
- 1021-1025: 56 cert-pass rows with no lifecycle PassStarted (no pass_key).
- 1021-1025: 2 landings of an unknown repo dropped.
- 1021-1025: 33 red passes with no typed owner (no Eject followed).
- 1031: 242 batch-round rows joined to a lifecycle batch.
- 1031: 1 batch-round rows with no lifecycle batch within 120 s.
- 1031: 1 cert-pass rows with no lifecycle PassStarted (no pass_key).
- 1031: 1 cert-pass rows with no pass number.
- 1031: 147 red passes with no typed owner (no Eject followed).

**Times.** Every lifecycle record keeps the lifecycle's own `at`, and every TSD record its `ts`. `run/rounds.log` has no timestamps, so nothing is timed from it: a round's `verdict` comes from `run/tsd/batch-round.jsonl`, joined to the same repo's lifecycle batch whose Abandon or FastForward is nearest within 120 s, and a batcher alarm is timed by the Abandon of the round its line names. That join is greedy (earliest batch-round row first, each batch taken once), so a per-batch `verdict` is a nearest-time match, not a recorded link; the per-repo verdict counts are what it supports.

**The .ndjson extension is deliberate.** These are fixtures, not a beads export; the rename keeps the fence's rule intact rather than carving an exception into it.

**Typed by hand.** Two harm events were recorded only as prose, so their typed records were written by hand, in N-1's shape:

- `b-1226` (the landing-stop bead): `paused` at its first over-cap pass after it landed (cert-pass, 11:23:43Z), `unpaused` when its revert `b-1261` entered LANDED (15:32:49Z). The revert's `land` record carries `reverts: b-1226`, typed from the revert commit's subject.
- `b-1264` (the disk-preflight bead): `paused` at the first EC2 pass refused after it landed (17:24:31Z, `infra` with no suites run), `unpaused` at the next pass whose suites started (19:25:00Z).
- **Where these differ from the work item's prose.** The work item puts the landing stop at about 04:43 to 15:30 PT; the records put it at 04:23 to 08:32 PT (11:23Z to 15:32Z), so its 15:30 reads as a UTC time labelled PT, and its 647-minute cost is about 249 minutes here. It also says the disk preflight refused every EC2 round; in the records only the home repo's EC2 passes stopped running suites (repo-c's kept running), so only repo-a is paused. Take times from these records, not from that prose.
- The world halt at 19:44:36Z is the halt file's modification time. No release falls inside the slice. No stop history exists for 10-21..25, so that slice's `stop.ndjson` is empty.

**The two ids a join needs.** `cert_pass.batch` is the lifecycle batch id, the same id `round.batch` and `attribution.batch` carry. The TSD `round-attribution` family does not: its `round` field is the batcher's start time in unix seconds, never a batch id, and over the 20 days it holds 0 rows. Reds and their owners are therefore taken from the lifecycle (`attribution.ndjson`), not from `round-attribution`.

## The measured tolerance

`landed-20d.tsv` columns: `day`, `lifecycle_landed` (distinct bead ids entering LANDED that Pacific day), `git_land` (throughput-bounds §1's method), `hand_landed_dir` (beads landed by a direct first-parent commit or a round merge, with no land commit that day), `delta` (lifecycle minus git), `cause`. `cause` joins, with `+`, every cause found among that day's disagreeing ids, from the closed set `hand-landing`, `day-boundary`, `relanded`, `batch-multi-member`, `unexplained`; a day with any unexplained id reads `unexplained` first, and `-` means nothing differs.

- **hand-landing**: the lifecycle landed it and git has a direct landing of it, or the lifecycle's own ContentOnBase landed it and no land commit names it (the second rests on the lifecycle's judgement alone, not on git evidence).
- **day-boundary**: the other side has it on the adjacent day.
- **relanded**: it entered LANDED, or got a land commit, on another day as well. This includes a lag of more than a day between the two sides, not only a second landing.
- **batch-multi-member**: one land commit carried several ids (an integration land naming several beads, a member's own commits merged under another member's land, a dotted parent and child, or a member entering LANDED in the same second as one that has a land commit).
- **unexplained**: none of these. Two of them are not beads: §1's token rule reads `: land law-…` subjects as landings, and the column keeps that so it stays equal to §1.

`tolerance_per_day` is the largest |delta| among days whose cause is not unexplained. It and the count of unexplained days are in the tallies.

## Tallies

Each is the output of the command beside it, run from the repository root.

**10-31: rounds cut per repo**

```sh
jq -r 'select(.state_from=="NONE" and .at>="2026-10-31")|.repo' sim/fixtures/replay/1031/round.ndjson | sort | uniq -c
```

```
     41 repo-a
    136 repo-b
    114 repo-c
```

**10-31: rounds landed per repo**

```sh
jq -r 'select(.state_to=="LANDED" and .at>="2026-10-31")|.repo' sim/fixtures/replay/1031/round.ndjson | sort | uniq -c
```

```
     20 repo-a
      9 repo-b
      1 repo-c
```

**10-31: rounds abandoned per repo, by the batch-round verdict joined to each (none = no batch-round row)**

```sh
jq -r 'select(.state_to=="ABANDONED" and .at>="2026-10-31")|"\(.repo) \(.verdict // "none")"' sim/fixtures/replay/1031/round.ndjson | sort | uniq -c
```

```
     21 repo-a none
     44 repo-b blocked
     49 repo-b ejected
      3 repo-b none
     31 repo-b refused
     13 repo-c blocked
     96 repo-c ejected
      1 repo-c none
      3 repo-c refused
```

**10-31: certification passes per repo by cause**

```sh
jq -r '"\(.repo) \(.cause)"' sim/fixtures/replay/1031/cert_pass.ndjson | sort | uniq -c
```

```
     21 repo-a green
     12 repo-a infra
     18 repo-a over-cap
     11 repo-a red-attributed
     38 repo-b green
      3 repo-b infra
     50 repo-b red-attributed
      1 repo-c green
      1 repo-c infra
     97 repo-c red-attributed
```

**10-31: green passes against rounds landed, per repo (green landed)**

```sh
for r in repo-a repo-b repo-c; do echo "$r $(jq -r --arg r $r 'select(.repo==$r and .cause=="green")|.at' sim/fixtures/replay/1031/cert_pass.ndjson | wc -l) $(jq -r --arg r $r 'select(.repo==$r and .state_to=="LANDED" and .at>="2026-10-31")|.at' sim/fixtures/replay/1031/round.ndjson | wc -l)"; done
```

```
repo-a 21 20
repo-b 38 9
repo-c 1 1
```

**10-31: distinct beads certified green (the members of every round a green pass moved to GREEN)**

```sh
jq -n -r --slurpfile c sim/fixtures/replay/1031/cert_pass.ndjson --slurpfile r sim/fixtures/replay/1031/round.ndjson '[$c[]|select(.cause=="green")|.batch] as $g | [$r[]|select(.state_to=="GREEN" and (.batch as $b|$g|index($b)))|.members[].bead]|unique|length'
```

```
46
```

**10-31: the most-repeated (member, tip) pass starts, per repo**

```sh
jq -r 'select(.state_from=="OPEN" and .state_to=="CI_RUNNING" and .at>="2026-10-31")|.repo as $r|.members[]|"\($r) \(.bead)@\(.tip)"' sim/fixtures/replay/1031/round.ndjson | sort | uniq -c | sort -rn | head -3
```

```
     99 repo-c b-1254@05c3a8aaa5c7
     49 repo-b b-1239@b70d36ef6e67
     31 repo-b b-1245@28f6f7e0ae0a
```

**10-31: the home repo's longest gap between landings, in minutes**

```sh
jq -r 'select(.repo=="repo-a" and .at>="2026-10-31")|.at|fromdate' sim/fixtures/replay/1031/land.ndjson | awk 'NR>1{g=$1-p; if(g>m)m=g} {p=$1} END{printf "%.0f\n", m/60}'
```

```
390
```

**10-31: beads filed (reached READY from another state, or dropped), landable (reached READY, not dropped) and dropped**

```sh
jq -s '[.[]|select(.kind=="bead")] as $b | ([$b[]|select(.to=="READY" and .from!="READY")|.id]|unique) as $r | ([$b[]|select(.to=="DROPPED")|.id]|unique) as $d | {filed:(($r+$d)|unique|length), landable:([$r[]|select(. as $x|$d|index($x)|not)]|length), dropped:($d|length)}' sim/fixtures/replay/1031/transition.ndjson
```

```
{"filed":201,"landable":38,"dropped":163}
```

**10-31: rounds stacked on an unlanded parent (distinct children), and the parent**

```sh
jq -r 'select(.parent)|"\(.parent) \(.batch)"' sim/fixtures/replay/1031/round.ndjson | sort -u | awk '{n[$1]++} END{for(p in n) print n[p], p}'
```

```
29 repo-b-20261031T031222Z
```

**10-31: the landing-stop bead's stop window, in minutes**

```sh
jq -r 'select(.bead=="b-1226")|.at|fromdate' sim/fixtures/replay/1031/stop.ndjson | paste -sd' ' | awk '{printf "%.1f\n", ($2-$1)/60}'
```

```
249.1
```

**10-31: alarm attempts by caller, kind and outcome**

```sh
jq -r '"\(.caller) \(.kind) \(.outcome) \(.reason_code)"' sim/fixtures/replay/1031/alarm_attempt.ndjson | sort | uniq -c
```

```
     29 batcher land-unverified failed incident-no-id
     10 reconciler escalate delivered none
      3 reconciler no-effect-mail failed mail-lint
```

**10-21..25: rounds cut and landed in each held-out lane (cut landed)**

```sh
for r in repo-d repo-e; do echo "$r $(jq -r --arg r $r 'select(.repo==$r and .state_from=="NONE" and .at>="2026-10-21T07")|.at' sim/fixtures/replay/1021-1025/round.ndjson | wc -l) $(jq -r --arg r $r 'select(.repo==$r and .state_to=="LANDED" and .at>="2026-10-21T07")|.at' sim/fixtures/replay/1021-1025/round.ndjson | wc -l)"; done
```

```
repo-d 299 26
repo-e 88 6
```

**10-21..25: rounds abandoned in each held-out lane, by verdict**

```sh
jq -r 'select((.repo=="repo-d" or .repo=="repo-e") and .state_to=="ABANDONED" and .at>="2026-10-21T07")|"\(.repo) \(.verdict // "none")"' sim/fixtures/replay/1021-1025/round.ndjson | sort | uniq -c
```

```
      2 repo-d ejected
      1 repo-d none
    270 repo-d refused
     82 repo-e refused
```

**20 days: the git_land column**

```sh
awk -F'\t' '!/^#/ && $1!="day"{print $3}' sim/fixtures/replay/landed-20d.tsv | paste -sd' '
```

```
77 104 158 119 180 103 108 33 100 93 65 101 77 107 34 52 72 65 65 19
```

**20 days: git_land total**

```sh
awk -F'\t' '!/^#/ && $1!="day"{t+=$3} END{print t}' sim/fixtures/replay/landed-20d.tsv
```

```
1732
```

**20 days: tolerance_per_day (largest |delta| on a day with no unexplained bead)**

```sh
awk -F'\t' '!/^#/ && $1!="day" && $6!~/unexplained/{d=$5<0?-$5:$5; if(d>m)m=d} END{print m+0}' sim/fixtures/replay/landed-20d.tsv
```

```
19
```

**20 days: unexplained days**

```sh
awk -F'\t' '!/^#/ && $1!="day" && $6~/unexplained/' sim/fixtures/replay/landed-20d.tsv | wc -l
```

```
6
```

**20 days: largest |delta| on any day**

```sh
awk -F'\t' '!/^#/ && $1!="day"{d=$5<0?-$5:$5; if(d>m)m=d} END{print m+0}' sim/fixtures/replay/landed-20d.tsv
```

```
19
```
