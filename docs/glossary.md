# Glossary

| Term | Meaning |
|---|---|
| **absolve** | break-glass clearing of a red recorded against a tip; needs evidence |
| **dwarf** | one called instance of a persona; claims one bead, works it in a worktree, exits |
| **ask** | an escalation to the operator, with its own machine (`OPEN` → answered, default taken or withdrawn) |
| **base / landing ref** | the ref work must reach to count as landed |
| **batch** | a round's record in the lifecycle store |
| **batcher** | cuts and lands rounds; also the Judge persona for unattributed reds |
| **bead** | one work item: content in the beads store, state in `spira_lifecycle` |
| **bdq** | the one chokepoint through which the harness invokes `bd` |
| **break-glass** | a move the machine allows only with a reason, evidence and the evidence tip |
| **broker** | `broker token\|read` | `token` mints the GitHub App installation token behind `gh-app.sh`; `read` answers `run-view`, `run-jobs` and `artifact-download` for a mapped repo, without log content. |
| **certified** | a gate or round passed an exact tip |
| **runes** | `spira/runes/`: persona definitions and briefs |
| **Concierge** | the operator's long-lived session; claims nothing |
| **cockpit** | the operator's tmux workspace: session, mail, decisions, ops dashboard |
| **delivery** | the per-bead sub-machine of `IN_DELIVERY` |
| **disposition** | how a session was cut short: `lapsed`, `thrash`, `slain` |
| **escalation model** | the persona's stronger model used after a bead is rejected twice |
| **express** | the flag that puts a bead ahead of the line |
| **rune** | a persona definition (`.rune` plus `.md` brief) |
| **fence** | a static check that fails the gate on a class of violation |
| **gate** | per-branch certification, defined by the judged tree's `gate.steps` |
| **hold** | an orthogonal suspension of a bead: `poison`, `ask`, `wait`, `manual` |
| **lane** | a scheduling slot outside the task pool, or a bead kind a repository admits |
| **lease** | a dwarf's time-bounded hold on a bead, renewed while it works |
| **Loom** | the read-only web view of where work is stuck |
| **mender** | persona that triages one failing suite of a round |
| **operator** | the human who decides escalations |
| **partition** | the set of beads a persona may claim, as a label predicate |
| **persona** | a rune, the role a dwarf plays |
| **plane** | `work`, `observability` or `maintenance`: how `world` groups units |
| **poison** | the hold on a bead that exhausted its attempts |
| **release** | an immutable `<releases>/<sha>/` directory; `current` points at the one in force |
| **round** | a set of beads merged and proven together on a round VM, then landed as one fast-forward |
| **round VM** | the disposable machine a round's full corpus runs on |
| **screen / sift** | the cheap pre-round check for conflict, duplicate and policy gaps |
| **sentinel** | the heartbeat that reconciles the graph and calls dwarves |
| **SOP** | a runbook (`sop-<slug>`) for a recurring incident, read by `ops` |
| **stack** | the certified tips of a bead's unlanded prerequisites, recorded at claim |
| **statute** | a standing rule (`law-<slug>`) that must name its enforcing mechanism |
| **suite** | a `spira/test-*.sh` test script with `# covers:` and `# tier:` headers |
| **tip** | the exact commit a submission or certification names |
| **trigger bead** | a bead a timer files to call a persona (groom, warden, sweep) |
| **work** | the dwarf's only client onto lifecycle state |
| **world** | the work as a whole; `world stop/start/drain/resume` control it |
