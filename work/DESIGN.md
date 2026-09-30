# work — the aeon's lifecycle client

## 1. Intent

`work <verb>` is how a builder aeon acts on its one bound bead when it has no `bd` (lifecycle
design §3.5). The verbs are `show`, `note`, `submit`, `done`, `blocked`, `file-followup`,
`split` and `superseded-by`. It holds no database code. It speaks one JSON line to spira-lc's
socket (`SPIRA_LC_SOCKET`, default `/run/spira-lc/sock`) and prints the reply.

## 2. Contract

- **In:** `SPIRA_WORK_BEAD_ID` (the bound bead; any other bead id in the arguments is
  refused), `SPIRA_FAYTH`/`SPIRA_WORK_ACTOR` (the actor), and git `HEAD` of the worktree for
  `submit`.
- **Exit:** the machine's own exit code on a reply. `2` means cannot tell: unbound, socket
  unreachable, or a malformed reply; retrying is safe. `3` means refused: a foreign bead, an
  unknown verb, or the switch is off; do not retry.
- **Callers:** only the model, and only under the aeon's restricted environment (design
  §3.5; `aeon/src/restrict.rs`, formerly the `work-env.sh` wrap, retired sp-zpaq0) and the
  `{{FINISH}}` brief. Both apply only when `LIFECYCLE_ENFORCE=1`. No harness script
  invokes `work`.

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
