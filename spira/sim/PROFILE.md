# Where one sim world's wall time goes

`sim run` prints `sim: profile …` on success; `test-sim-scenarios.sh` echoes it per world.
Measured in the sim lane (4 worlds side by side), seed 7, happy-path unless noted.

| phase | before | after |
|---|---|---|
| world up (db 0.1 s, seed repo 3 s, lifecycle schema 1.2 s, config 0.25 s, serve 0.1 s) | 4.6 s | 4.6 s |
| actor/step commands (16 in happy-path; 9–73 across scenarios) | 29 s | 30 s |
| probes after each event (happy-path 32 → 16; certify-twice 138 → 73) | 5.2 s | 2.6 s |
| teardown | 0.2 s | 0.2 s |
| suite wall (11 worlds, PAR 4) | 200 s | 181 s |

Commands are 80% of a world. Each is a real process tree: every script start sources
`conf.sh` (a staleness loop over `conf.d/*` plus `spira-config resolve`, ~0.15 s idle and
over 1 s when four worlds share the cores), and a world makes dozens of them. The first
`landing-pass land` that lands a bead costs 16 s in every scenario (later ones 2.5–4 s);
`cut.sh` is 8.7 s. Nothing in the driver sleeps; the wall time is process work, not waiting.

Landed here: probe only after an event that ran a command (state cannot change otherwise;
same seed, same trace). Not landed: the command cost, which needs `conf.sh` resolved once
per world rather than once per script, or the landing path made native.
