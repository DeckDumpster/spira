## 4.8 Maintenance timers

Each row is a task in `spira-runtime serve` (the scheduler), named as `world scheduler-status`
shows it. The cadences are those of the timers the tasks replaced: "boot+X" was `OnBootSec`;
"every X" was `OnUnitActiveSec` (X after the last pass *ended*). Tasks named in 4.1–4.7 are
not repeated here unless they have no other home.

| Task | Runs | Cadence | Purpose |
|---|---|---|---|
| `sweep.archive` | `archive.sh sweep` | boot+5 min, every 20 min | index session transcripts |
| `sweep.escape_census` | `escape-classify.sh census` | weekly | escape-by-class census |
| `sweep.export_beads` | in-process (`spira-runtime`) | every 6 h | regenerate the bead mirror |
| `watch.sop_lint` | `sop-lint-watch.sh` | every 1 h | lint the SOP shelf |
| `incident.file` | in-process (`spira-runtime`) | on a change under `incidents/`, or when a row's retry or claim comes due; never on a timer | file the incident rows their raiser left unfiled |
| `incident.settle` | `incident settle` | every 5 min | close incidents whose fix landed and detector is quiet |

Long-running services with no timer: `spira-runtime`, `dolt-beads`, `dolt-persist`,
`lc-serve` / `spira-lc` (+ sockets), `sccache-dav`, `spira-loom`,
`spira-metrics-*`, `spira-unit-run-dbus`, `spira-watch@<name>`.
