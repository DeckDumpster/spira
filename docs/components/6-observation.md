## 4.6 Observation

| Component | Binaries / units | Purpose, reads and writes |
|---|---|---|
| **watchd** | `watchd manifest\|units\|exec\|status\|drain\|peek\|tail\|next\|notify\|restart\|prune`; `spira-watch@<name>.service`, `spira-notify.timer` (5 min), `spira-refresh.timer` (1 min) | The watcher manifest (`spira/watchers`, plus `*.watchers` in `$SPIRA_WATCHERS_OVERLAY`): rows `name\|kind\|target[\|health]`, kinds `daemon`, `log`, `extern`, `off`. Per-watcher contract: an append-only `$SPIRA_RUN/watchd/<name>.log` and a `.cursor` of lines delivered. `notify` mails the operator for events no reader took. |
| **loom** | `loom`; `spira-loom.service` | Read-only HTTP server on `SPIRA_LOOM_ADDR` (loopback by default) showing where work is stuck. Reads the lifecycle read model and the cockpit snapshot; never runs `bd`; refuses to start without a database. |
| **cockpit-collect** | `cockpit-collect collect`; `spira-cockpit.service` (under `spira-supervise`) | Runs the probe set on 5 s / 60 s / 600 s cadences as isolated subprocesses and merges fragments into `$SPIRA_RUN/cockpit.env`. |
| **cockpit** | `cockpit.sh`; `cockpit/ops` (`health layout rebuild resolve reply lc-view`); `cockpit-ensure.timer` (1 min) | The operator's tmux workspace. `layout` builds and heals the layout; `rebuild` makes a cockpit from nothing after the tmux server dies; `health` repaints the ops dashboard from `cockpit.env`. |
| **metrics stack** | `spira/metrics-stack.sh vm\|logs\|collector\|grafana`; `spira-metrics-{vm,logs,collector,grafana}.service` | VictoriaMetrics, VictoriaLogs (wide events; keeps `SPIRA_OTEL_KEEP_DAYS`), an OpenTelemetry Collector (span files → metrics; OTLP logs → VictoriaLogs on loopback :4318) and Grafana, under podman. Needs `SPIRA_METRICS_DATA`; otherwise the units idle. Producers push to `SPIRA_METRICS_ENDPOINT`; empty disables. |
| **heimdall** | `heimdall [--report]`; `spira-heimdall.timer` (2 min) | Watchdog over the loop: classifies a stalled loop and only speaks, raising or clearing one alert bead per cause. Repairs nothing. |
| **watchtower** | `watchtower`; `spira-watchtower.timer` (30 min) | Hands Ops the pipeline's vital signs and runs cheap detectors for "every unit green but nothing landing": throttle, czar outcome, PR stall, release skew, deploy fault, slow query, drift, disk and memory. |
| **perf-watch** | `perf-watch --probes FILE`, `perf-watch happy`; `spira-perf-watch.timer` (5 min), `spira-perf-happy-path.timer` (1 h) | Times the hot read paths end to end and alarms past a limit with the top callers. |
| **census** | `census [--with-suppressed]` | Ranks failure classes by distinct beads since the last watermark, suppressing classes with an open remedy. |
| **strand** | `strand report\|check\|throttle-state` | Reports work that exists and is not moving, with the reason and the one action that clears it; `check` acts where the fix is mechanical. |
| **doctor** | `doctor` | Read-only runtime health ([install](../install.md)). |
| **intent-report** | `intent-report` | Prints certification latency, no-verdict rate, round time per member and certified-to-landed latency for a window from `$SPIRA_RUN/tsd`. Operator tool. |
