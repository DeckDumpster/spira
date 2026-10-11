# Where one sim world's wall time goes

`sim run` prints `sim: profile …` on success; the `test-sim-<scenario>.sh` suites echo it per world.
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
same seed, same trace); and `conf.sh` resolved once per world. `sim run` sources the real
`conf.sh` once (`up.conf-resolve`, ~0.7 s), writes what it exports to `config/conf.env` with
`SPIRA_CONF_LOADED` set, and every command inherits it, so no script re-runs the staleness loop,
`spira-config resolve`, `env-bootstrap` or `check-bd`. The real path is still exercised once per
world: the run fails unless sourcing `conf.sh` on top of the exported env changes nothing.
Only exported keys are carried, so a step that runs `spira-config set` drops `conf.env` and
later commands go back through `conf.sh` (three scenarios do, and keep their old cost). Probes
never see it: the probe refuses a store outside the world, and the resolved config names one.

Same scenario, same box load, with and without `conf.env` (happy-path, seed 7, two runs each):
first `landing-pass land` 5.8–9.5 s with, 16–22 s without; later commands 1.1–2.7 s with,
4.8–5.4 s without. So most of the "16 s first landing" was configuration re-resolved by the
dozens of scripts a land starts, not landing logic. What remains of it is process work in the
landing code (git and timeouts, no sleeps); it is not broken down further and was not changed.

Not landed: making the landing path native; the world is still above 10 s.
