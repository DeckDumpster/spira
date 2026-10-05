# spira-lc — DESIGN

The lifecycle machine's CLI and system service. The transition tables are the `lifecycle`
crate (pure); this binary is the I/O around them. This document starts with sp-arpjt, which
moved the three shell libraries every harness caller sourced — `lc.sh`, `lc-delivery.sh`,
`lifecycle-cert.sh` (615 lines) — into it as **caller verbs**.

## 1. Intent

One place where "apply this lifecycle event on behalf of a caller" is implemented, typed,
and switched. Before sp-arpjt that was three sourced bash libraries that each re-parsed
`spira-lc show` JSON with inline python, spelled event JSON by hand, and each had its own
copy of the on/off switch — and a caller that had not sourced the right one silently had
no lifecycle writes at all (aeon's `lc_hold` answered "command not found" for as long as
it existed).

## 2. Contract

**Primitives** (unchanged; what the `serve` daemon answers): `show`, `list`, `history`,
`event`, the cutover verbs, `classify`, `work`, `serve`. Exit codes on `event`: 0 applied,
3 refused, 2 cannot tell.

**Caller verbs** — compositions of `show`/`list`/`event`, run client-side (callers.rs):

| verb | replaces | exit |
|---|---|---|
| `hold <id> <poison\|ask\|wait\|operator> [cause] [actor=sentinel]` | `lc_hold` | 0 applied · 1 no row · 2 cannot tell · 3 refused |
| `unhold <id> <kind> [actor]`, `release <id> [actor]`, `holder-dead <id> [actor]` | `lc_unhold`, `lc_release`, `lc_holderdead`/`lc_holder_dead` | same |
| `drop <id> <reason> [actor]`, `returned <id> <reason> [actor]` | `lc_drop`, `lc_returned` | same; the typed reason is `unwanted` / `batch-ejected`, the prose stays the caller's |
| `content-on-base <id> <proof> [actor=sending]` | `lc_content_on_base` | same |
| `state <id>` | `lc_show` + a state read | prints the state; 0 · 1 no row · 2 cannot tell |
| `holds <id>` · `held <id> <kind>` | `lc_holds` · `lc_held` | kinds one per line, 0 · 0 held / 1 not |
| `list-held <kind>` · `list-state <state>` · `list-all` | `lc_list_*` | `id` · `id\tlease\tholds,` · `id\tstate\tholder`; always 0 |
| `deliver pr-merged <repo> <id> <branch> <sha>` · `pr-closed <id>` · `push-delivered <id> <sha>` · `push-requeued <id> <tip>` · `push-returned <id>` | `lc_deliver_*` | 0 applied · 1 skipped (no delivery row / wrong state, logged) · 2/3 as `event`; lib.sh `log` lines on stdout |
| `certify <id> <tip> <pass\|red\|infra> [detail] [actor=lifecycle-cert]` · `resubmit <id> <tip> [actor]` | `lc_certify` · `lc_resubmit` | 0 · 2 cannot tell · 3 refused/skipped; each outcome appended to `$SPIRA_RUN/lifecycle-cert.log` as before |

**Two verbs touch no lifecycle state and ignore the switch.** `content-landed <repo> <branch> <base>`
(0 base holds every change the branch makes · 1 not · 2 usage) and `close-on-land <id> [sha]` (closes a
submitted, unclosed work bead citing the sha, marks the ledger LANDED, reaps the branch through
`sending`; best-effort, always 0). The first replaces lib.sh's bash merge-tree check; the second
is what every landing path calls to close a landed bead (sending's seam directly; the queue still
through lib.sh's one-line shim until sp-du6dl), in place of the landing pass's own subcommand.

**The switch is read before anything else.** `lifecycle_enforce` (`SPIRA_LIFECYCLE_ENFORCE`,
else `spira.lifecycle_enforce`, else off — spira-config's one resolver). Off, every caller
verb answers exactly what the shell function answered off — 2 for events/state/certify, an
empty 0 for the reads, 1 for `held`, the "no delivery row … inert" log line and 1 for
`deliver` — **without** opening the socket, a connection, or a repository (tests/switch.rs
proves it with a listener that counts connections). conf.sh now exports the switch it resolved, so a
script's children agree with the script, as the sourced functions did by construction.

**CAS from the caller's own read.** Every event verb reads the row once and applies its
event under that row's `(state, version)` — a stale view is refused (3), never forced.
`certify` keeps lifecycle-cert.sh's tip invariant: WORKING/REWORK/CERTIFIED-at-another-tip
are first moved by `Submit` (REWORK has no such transition, so it is refused and the verdict
skipped — the shell's behaviour too), and the verdict lands only on SUBMITTED.
`deliver pr-merged`'s proof is `merge-tree` when the merge commit holds the branch's
content (lib.sh `content_landed`, ported), else `gh-merged`.

## 3. Decisions

- **Client-side composition, not new daemon verbs.** A daemon from an older release still
  answers a newer client; `deliver pr-merged`'s git read runs as the caller, in the caller's
  checkout, never as the service user.
- **Positional arguments mirroring the shell functions**, so every call site is a
  one-word swap (`lc_hold "$id" wait …` → `spira-lc hold "$id" wait …`) and parity is
  checkable call for call.
- **Events are built from the `lifecycle` types** (`BeadEventKind`, `HoldCause`,
  `GateRedReason`, …) and serialized, not spelled as strings — byte-identical to what the
  shell spelled (asserted), and a new variant cannot be misspelled.
- **Intended differences, named.** (1) An unparseable `show` answer is "cannot tell" (2)
  where the shell read empty fields as "no row" (1). (2) `pr-pass-branch.sh`'s delivery log
  lines lose its `landing-pass <id>:` prefix (they are spira-lc's own lines now). (3) The
  gate's certification actor is `gate` (it was `lib.sh`, the `$0` of the gate's seam).
  (4) Every lib.sh caller now reaches the machine when the switch is on, not only those that
  had sourced lc.sh (aeon's `park_unmapped`/rapid-recur holds and the eviction-race hold
  were silent no-ops in ON mode before).
- **Transitional adapters.** `lc_certify`/`lc_resubmit` remain in lib.sh as one-line calls
  of `spira-lc certify/resubmit` for batch.sh's stale-certification sweep, whose own port
  (wave 2b) calls the verbs directly; delete them with batch.sh.

## 4. Parity (sp-arpjt)

The retired libraries (local/main) and these verbs against ONE fake machine answering the
primitives over the service socket and logging every request: 40 calls × both switch
positions — every verb, every refusal and absence, every delivery exit and wrong-state
skip, every certify arm — identical exit code, stdout, primitive request sequence (so the
exact event JSON, CAS and order), final state, and certification log: 80/80.

## 5. Landstate semantics

Where the landstate ledger's facts live once its readers are deleted: `DESIGN-landstate.md`.
