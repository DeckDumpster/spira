# Where one sim world's time goes

`sim run` prints `sim: profile …` on success (also for `--keep`); the `test-sim-<scenario>.sh`
suites and `test-sim-happy-path.sh` echo it per world. `cpu=` is the CPU of every process the world ran and
reaped: it does not stretch when the box is busy the way wall does, so compare worlds by it.

To see which program spent it, run a suite with `SIM_PROC_TRACE=1`: every command's process tree
is sampled into `proc-trace.log` in the world, and `sim proc-summary <world>` ranks the programs
by own CPU, with the CPU of children too short-lived to be sampled charged to their parent.

The seed repo (the tree committed, with its origin) is built once per tree and kept in
`SPIRA_SIM_CACHE` (default: the temp dir); the suites point it at one directory so every world of a
container copies the same seed, and `sim world seed` builds it ahead of the timed worlds.

## Measured

Sim lane, four worlds side by side, seed 7, one run each, host load average 17–26 on 32 cores.
"before" is the tree this work started from with only the `cpu=` figure added. The wall of a run
moved 2–3x with the host's load between runs of the same code (happy-path `after`: 11 s, 20 s,
28 s); cpu moved 8.6–10 s.

| scenario | before wall / cpu | after wall (3 runs) / cpu |
|---|---|---|
| happy-path | 82 s / 67 s | 11, 20, 28 s / 8.6–10.0 s |
| cut-in-bare-env | 73 s / 58 s | 11, 12, 40 s / 7.2–8.6 s |
| publish-then-cut | 63 s / 58 s | 10, 11, 34 s / 7.0–8.6 s |
| resubmit-moved-tip | 57 s / 53 s | 10, 9, 18 s / 6.5–8.9 s |
| submit-from-other-cwd | 56 s / 52 s | 11, 8, 8 s / 6.5–9.2 s |
| conflicting-siblings | 113 s / 121 s | 18, 24, 36 s / 14.5–17.5 s |
| landstate-lifecycle-agree | 208 s / 228 s | 32, 34, 32 s / 27–32 s |
| long-gate-interleaving | 230 s / 221 s | 35, 31, 45 s / 22–32 s |
| certify-twice | 222 s / 229 s | 30, 40, 41 s / 28–31 s |

Every scenario is at least 5x cheaper in cpu and at least 3x faster in wall on the same box.
Happy-path alone in the lane (no neighbours): `up` 0.7 s, conf 0.25 s, run 9.75 s, cpu 8.8 s.
That is above 10 s of wall by about a second; see "What is left".

Happy-path, per phase (before / after):

| phase | before | after |
|---|---|---|
| world up | 5.3 s (db 0.1, seed repo 3.2, lifecycle schema 1.2, config 0.5, serve 0.3) | 0.7 s (db 0.1; seed copy 0.16 and config resolve 0.25 beside the schema 0.52; serve 0.02) |
| config resolved through `conf.sh` | 0.75 s, after up | inside the 0.7 s above |
| 16 actor/step commands | 73 s wall, 67 s cpu | 8–10 s cpu |
| probes after each event | 2.6 s (16 × 160 ms) | 0.12 s (16 × 4–8 ms) |
| `cut.sh` (publish-then-cut, cut-in-bare-env) | 8.7 s alone | 0.85 s alone |
| first `landing-pass land` | 16 s → 6–9 s with `conf.env` | 4.7 s |

## What was spent, and where it went

Found by sampling each command's process tree; every item is process work, none is a sleep
except where noted.

- **Config resolution in every Rust process: 60 ms in a release build, 240 ms in the test
  profile.** `registry::topo_order` compared every pair of the ~380 defaults with a fresh string
  per comparison. Now linear (and tested against the old algorithm on the real registry): 3 ms
  release. Every harness binary pays this at start, ~150 starts per world.
- **The test profile compiled dependencies and the config crate at opt-level 0.** `toml`,
  `serde_json` and `sha2` are now optimised (as the unit-test profile already did); the gate
  hashed 37 MB of its own binaries per trial at 10 MB/s. Config resolve 13 ms → 4 ms per process.
- **`conf-gen.sh`: 6 s of bash on every fresh checkout** that sources `conf.sh`, and in every
  `release.sh cut`. One awk pass, byte-identical output, 0.15 s.
- **`spira-lc` retried a connect the server had refused**: a fresh store's first DDL asks for a
  database that does not exist yet and waited out 600 ms of backoff. Server refusals are final.
- **The batcher slept 2 s between polls of a round whose jobs finish in milliseconds.** The poll
  gap takes fractions of a second; a world polls every 100 ms.
- **Probes** shelled to `sh`, `sim probe`, `spira-lc` and four `git`s. Now refs are read from
  git's files (checked against `git for-each-ref`, loose, packed and symbolic), a commit's
  history is read once, and the lifecycle is asked over its socket.
- **World up**: the seed repo (`git archive`, `add -A`, commit, push of 2.5k files) is built
  once per tree and copied, packs linked; checkouts are sparse (the landing code's worktrees
  inherit it: 350 ms → 90 ms each); the schema applies beside the copy; `spira-config set` runs
  in-process, once for all settings.
- **The sim's `cargo` stand-in copied a 60 MB binary** (4.8 s on a busy box); it links it.
- **Two worlds' cut jobs raced on the workflow's fixed `/tmp/batch-assert`** once the worlds got
  fast enough to overlap; a step that names the host's `/tmp` now takes a turn.
- **Go tools**: each `bd` call spends ~80 ms of CPU starting; `GOGC=off GOMAXPROCS=2` takes a
  third of it. A pool with nothing certified no longer asks `bd` about base-red fixes.

## What is left

Happy-path CPU, one world, ranked (cpu of the program plus children too short to sample):

| program | cpu | why it is there |
|---|---|---|
| `bd` | 2.9 s | 28 calls at ~100 ms (before the idle-pool and Go-runtime changes): 15 `show` of the one bead, 8 `list` for base-red beads, 4 scans |
| batcher / queue / landing-pass / gate-worker | 2.4 / 1.8 / 1.0 / 1.3 s | each actor tick starts dozens of `git`, `spira-lc` and `bash` seams (211 `git`, 86 `spira-lc`, ~90 `bash` per world) |
| `gate` | 1.8 s | two trials, ~350 ms of its own CPU each, the rest in its `git` and `commit-cite.sh` |

The rest of the gap to 10 s is in the harness code the world exists to run, not in the driver:
`bd show` of the same bead from `commit-cite.sh`, `land-subject` and `landing-pass` in turn (each
a process, 80 ms of Go start-up); `bash -c '. lib.sh; …'` seams that exist only to reach a
function already ported; per-tick scans that run on an idle pool. Keeping them real is the point
of the world, so removing them means changing those programs (a per-process cache of bead reads;
the remaining seams ported to Rust), which is that code's own work, not a faster driver.
