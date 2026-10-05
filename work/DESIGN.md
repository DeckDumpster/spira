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
  unknown verb, or the switch is off; do not retry.
- **Callers:** only the model, and only under the aeon's restricted environment (design
  §3.5; `aeon/src/restrict.rs`, formerly the `work-env.sh` wrap, retired sp-zpaq0) and the
  `{{FINISH}}` brief, and every persona prompt in the chamber plus the archivist's (sp-st0mm).
  Both apply only when `LIFECYCLE_ENFORCE=1`. No harness script invokes `work`.

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

**Who may run what** is one table, `ALLOW` in `spira-lc/src/work.rs`; an operation absent
from it is refused for everyone. The persona is the `--actor` the client appends from its own
environment; a caller-typed `--actor`/`--stdin` is refused. A body (`-`, or a
`--body-file`/`--reason-file`/`--why` file) is read by the client and sent as `--stdin`,
since the broker cannot read the aeon's files.

**Limit:** the persona comes from the aeon's environment (`SPIRA_FAYTH`), which the model's
own shell can override; the broker does not yet authenticate the caller (peer credentials).

## 3. Lifecycle switch

**Finding (operator, 2026-09-28):** the lifecycle machine was never deployed on this host.
There is no `spira_lifecycle` database, no `spira_lc` grant, and no service or socket.
**Decision:** `lifecycle_enforce` is THE switch for everything that touches the lifecycle
machine.

**Resolution** (`spira_config::lifecycle_enforce`, the aeon crate's rule):
- `SPIRA_LIFECYCLE_ENFORCE` wins: `1`/`true` is on, and anything else, including empty, is off.
- Else `spira.lifecycle_enforce` from the spira.toml that spira-config discovers:
  `$SPIRA_TOML`, `$SPIRA_REPO/spira.toml`, `$XDG_CONFIG_HOME` or `$HOME/.config`
  `/spira/spira.toml`, or `/etc/spira/spira.toml`.
- Else **off**.

A socket or binary existing never turns it on.

| verb | **off** (production today) | **on** |
|---|---|---|
| every verb | **refused, exit 3, before the socket is touched.** The stderr line names the switch and the legacy path: `bd show`, `bd note`, closing through `bd` per the brief, the brief's escalation, `bead.sh file`. Nothing was done | the request goes to the socket. Unreachable → `cannot tell`, exit 2 (unchanged) |

**Why refuse rather than succeed quietly.** Off, the model is never wrapped in work-env.sh
and its brief names `bd`, so `work` is not on its PATH in the first place. If something does
reach `work` in off mode, a no-op "success" would report a `submit` or a close that never
happened, which is the silent valve the harness's laws exist to prevent. A legacy
translation is impossible by construction, because `work` has no `bd` and no credential.
So the right answer is a loud, non-retryable refusal.

**Tests:**
- `tests/switch.rs::off_refuses_every_verb_without_touching_the_socket`: a live listening
  socket must record no connection for `0`, empty, or `yes`.
- `on_reaches_the_socket`
- `on_unreachable_socket_is_cannot_tell`
- `lib::off_refusal_names_the_switch_and_a_legacy_path_for_every_verb`

**Cutover addition (now `aeon/src/restrict.rs`; the wrapper process it describes, `spira/
work-env.sh`, is retired — sp-zpaq0):** the restricted environment runs `work` under an
allow-list and does not pass `SPIRA_LIFECYCLE_ENFORCE` unless it is present and non-empty
in the aeon's own environment. In on mode, `work` then falls back to the spira.toml it can
find from `HOME`. If on is set only through the environment (spira.conf or a unit), every
verb would be refused, so `restrict.rs` carries `SPIRA_LIFECYCLE_ENFORCE` through
(defaulted to `0`) rather than dropping it. Nothing is needed for off.
