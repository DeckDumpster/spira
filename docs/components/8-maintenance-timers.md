## 4.8 Maintenance timers

Each timer runs the service of the same name. "boot+X" is `OnBootSec`; "every X" is
`OnUnitActiveSec` (X after the last pass *ended*). Timers named in 4.1–4.7 are not repeated
here unless they have no other home.

| Timer | Runs | Cadence | Purpose |
|---|---|---|---|
| `spira-archive` | `archive.sh sweep` | boot+5 min, every 20 min | index session transcripts |
| `spira-escape-census` | `escape-classify.sh census` | weekly | escape-by-class census |
| `spira-export-beads` | `export-beads.sh` | every 6 h | regenerate the bead mirror |
| `spira-nix-gc` | `nix-gc.sh` | weekly | collect Nix garbage keeping the current and previous environment |
| `spira-sop-lint` | `sop-lint-watch.sh` | every 1 h | lint the SOP shelf |
| `spira-statute-audit` | `rule audit --file --for builder --max 5` | weekly | statutes naming tools that do not exist |
| `spira-incident-settle` | `incident settle` | every 5 min | close incidents whose fix landed and detector is quiet |

Long-running services with no timer: `dolt-beads`, `dolt-persist`,
`lc-serve` / `spira-lc` (+ sockets), `sccache-dav`, `spira-loom`, `spira-cockpit`,
`spira-mail-deliver`, `spira-submission`, `spira-metrics-*`, `spira-unit-run-dbus`,
`spira-watch@<name>`.
