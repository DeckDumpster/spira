# work — the aeon's lifecycle client

## 1. Intent

`work <verb>` is how a builder aeon acts on its one bound bead when it has no `bd` (lifecycle
design §3.5). The verbs are `show`, `note`, `submit`, `done`, `blocked`, `file-followup`,
`split` and `superseded-by`. It holds no database code. It speaks one JSON line to spira-lc's
socket (`SPIRA_LC_SOCKET`; default `/run/spira-lc/sock` in system mode, `/run/user/<uid>/spira-lc/sock` — install's `lc-serve.service` — in same-user mode, see `spira_config::resolve::lc_socket_default`) and prints the reply.

## 2. Contract

- **In:** `SPIRA_WORK_BEAD_ID` (the bound bead; any other bead id in the arguments is
  refused), `SPIRA_FAYTH`/`SPIRA_WORK_ACTOR` (the actor), and git `HEAD` of the worktree for
  `submit`.
- **Exit:** the machine's own exit code on a reply. `2` means cannot tell: unbound, socket
  unreachable, or a malformed reply; retrying is safe. `3` means refused: a foreign bead, an
  unknown verb; do not retry.
- **Callers:** only the model, and only under the aeon's restricted environment (design
  §3.5; `aeon/src/restrict.rs`, formerly the `work-env.sh` wrap, retired sp-zpaq0) and the
  `{{FINISH}}` brief, and every persona prompt in the chamber plus the archivist's (sp-st0mm).
  No harness script invokes `work`.

## 2a. Lane verbs (sp-st0mm)

**Order (operator, 2026-10-05): no aeon model runs `bd`.** Every bead operation a persona
needs is a `work` verb; the spira-lc broker (`spira-lc/src/work.rs`) is the only thing that
calls bd or a bd-reaching tool. Beyond the bound verbs above, the lane verbs name their own
target, and run unbound too (the archivist has no bead; it sends `-` in the bound slot):

| verb | does |
|---|---|
| `ask --subject S --kind question\|fyi --default D [--class C] [--bead ID] [--body-file -]` | `mail send operator`, sent as the persona |
| `read <id> [--json]`, `list [--status S] [--label L] [--title-contains T] [--limit N] [--all] [--json]`, `search <q> [--status S] [--limit N] [--json]` | reads |
| `note-on <id> <text\|->`, `label-add\|label-remove <id> <label>`, `relate <a> <b>`, `reopen <id> --evidence T` | bd writes to another bead |
| `dep-add <id> <on> [--type T]` | `bead.sh dep add` (refuses a blocks edge onto an incident) |
| `close-other <id> --evidence T` | `groomer close` |
| `file "<t>" (--for P --repo R \| --kind K [--repo R]) [--priority N] [--parent ID] [--body-file F]` | `bead.sh file`, plus the lifecycle row for work |
| `groom\|incident\|sop\|queue\|landing-pass\|strand <sub> ...`, `census [--with-suppressed]` | the harness tool, `<sub>` from the allow table |
| `fence <class>` | `czar-fence.sh`'s answer (0 act, 1 shadow) from the installation's own `SPIRA_CZAR_STAGE_<CLASS>` |

**Who may run what** is one table, `ALLOW` in `spira-lc/src/work.rs`; an operation absent
from it is refused for everyone. The persona is the `--actor` the client appends from its own
environment; a caller-typed `--actor`/`--stdin` is refused. A body (`-`, or a
`--body-file`/`--reason-file`/`--why` file) is read by the client and sent as `--stdin`,
since the broker cannot read the aeon's files.

**Limit:** the persona comes from the aeon's environment (`SPIRA_FAYTH`), which the model's
own shell can override; the broker does not yet authenticate the caller (peer credentials).

## 3. Lifecycle switch (retired)

`lifecycle_enforce` once gated every verb: off, `work` refused (exit 3) before touching the
socket, since the machine was not the host's record. After the cutover (2026-10-05) the
machine is the only record, and sp-v62vn retired the switch: every verb goes to the socket,
an unreachable socket is `cannot tell` (exit 2), and a config or environment still saying off
is refused by spira-config, naming its exit. The aeon's restricted environment
(`aeon/src/restrict.rs`) no longer carries `SPIRA_LIFECYCLE_ENFORCE`.

**Tests:** `tests/switch.rs::a_verb_reaches_the_socket`, `an_unreachable_socket_is_cannot_tell`.

