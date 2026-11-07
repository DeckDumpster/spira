# Rounds runbook

How to drive rounds by hand: pausing them, landing one, clearing a stuck lease, and holding the
world for a cutover. Each entry gives its commands, then **Success** and **If it fails**.

Set these before you copy a command: `REPO` (the registered repo), `BATCH` (the round's batch
id), `BEAD` (a CERTIFIED bead), `REV` (the store revision a cutover is pinned to), `WHY` (a
reason a reader months from now can act on), and `SPIRA_TOML` (the config file).

## What proves this runbook

Every command sits in a block headed `<!-- check: NAME -->`, and every line names the exit it
must give: `# exits N`, or `# exits N, retried` for a command re-run until it does. The world
scenario `spira/sim/scenarios/rc-cutover-mode.toml` runs each block against a sim world, in the
order a cutover meets them, and fails on the first command that exits otherwise. A line marked
`# not checked:` says why a sim world cannot run it; nothing else is skipped.

## Pause a repo's rounds lane

A scoped stop on `rounds` holds the batcher's cuts for one repo. Running rounds finish; nothing
new is cut, and every reader shows the stop as deliberate, with its owner and release condition.

<!-- check: pause-rounds -->
```sh
world pause --repo "$REPO" --lanes rounds --reason "$WHY" --until "the drill ends"   # exits 0
world status   # exits 0
world unpause --repo "$REPO" --lanes rounds   # exits 0
```

**Success:** `world status` prints `scoped stop <repo> [rounds] owner=concierge`; after the
unpause it no longer does.
**If it fails:** `unknown lane` means a typo in `--lanes`; `a stop needs a ...` names the missing
argument.

## Drain rounds before an install that removes record files

An install that deletes `run/queue` record files needs zero non-terminal rounds first, or a
round opened by the batcher is lost with its record. Stop the cuts, wait for the rounds in
flight, install, then lift the stop.

<!-- check: drain-rounds -->
```sh
world pause --repo "$REPO" --lanes rounds --reason "$WHY" --until "the install lands"   # exits 0
queue round status "$REPO" | grep -qx 'round=none'   # exits 0, retried
world unpause --repo "$REPO" --lanes rounds   # exits 0
```

**Success:** `queue round status` prints `round=none` for every repo before the install starts.
**If it fails:** a round left in ATTRIBUTING with no driver never drains: abandon it by hand
(the rollback entry's abandon line) with its evidence noted.

## Hand-land a round with round-recert

`round-recert --drive` certifies one round and lands it only on green, in the foreground. It
exits 0 when the round landed and 1 for anything else, the round left open; re-run it once the
cause is fixed.

<!-- check: hand-land -->
```sh
queue round open "$REPO" --members "$BEAD" --name "$BATCH"   # exits 0
round-recert --drive --batch "$BATCH" --repo "$REPO"   # exits 0, retried
```

**Success:** `round-recert: <batch> landed at <head>`.
**If it fails:** `is not green: RED: <steps>` names the red steps; `LAND FAILED` prints the
land verb's own refusal above it.

## Clear a stuck lease

A round whose driver died holds its lease until the TTL lapses. `round-controller lease clear`
releases it only with proof the holder is dead, and says what it saw; a round with no lease
answers `lease=none`.

<!-- check: clear-lease -->
```sh
round-controller lease clear --repo "$REPO" --batch "$BATCH"   # exits 0
```

**Success:** `lease=cleared ... evidence=<proof>` or `lease=none`.
**If it fails:** exit 1 is a refusal: the holder may be alive. Find it before anything else.

## Read cutover-check

`round-controller cutover-check --readiness` reports each registered round repo and the global
preconditions (zero non-terminal rounds, zero `open`/`prepared` files); the world stops only when
every row is green. After a cutover, `--since-rev` waits for one landed round per repo.

<!-- check: cutover-check -->
```sh
round-controller cutover-check --readiness   # exits 1
round-controller cutover-check --since-rev "$REV"   # exits 1
```

**Success:** every row green, exit 0. Any row not green, exit 1 with the failing rows printed; the checked block runs in a world that is not yet green and expects 1.
**If it fails:** a red row names the repo and the precondition; fix that repo first.

## Enter and leave cutover mode

`world cutover-mode` holds a stopped world with scoped stops instead of the halt stamp: every
lane but `rounds` (`claims, publish, land, dwarves, czar, heimdall, remedies`) is stopped on every
registered repo, owner `cutover-mode`, until `world stop`. It refuses unless the world is stopped
and no dwarf is live, records `{rev, reason, owner, since, stops}` at `run/world.cutover-mode`,
and removes `run/world.halted`. Round drivers then pass the halt check and still honour scoped
stops; nothing else lands, claims or remediates. `world start` refuses while it holds.

<!-- check: cutover-enter -->
```sh
world stop --why "$WHY"   # exits 0
world cutover-mode --rev "$REV" --reason "$WHY"   # exits 0
world status | grep -q 'CUTOVER MODE'   # exits 0
world start   # exits 1
```

Drive rounds with the hand-land entry. To leave, stop the world: the stamp is written again and
exactly the stops cutover mode laid are lifted; a stop that was there before it is put back.

<!-- check: cutover-leave -->
```sh
world stop --why "$WHY"   # exits 0
world status | grep -q 'CUTOVER MODE'   # exits 1
```

**Success:** `spira: CUTOVER MODE ... leave with: world stop` while it holds; `spira: HALTED
since` after.
**If it fails:** `the world is not stopped` — run `world stop` first; `dwarf(s) still live` —
wait for them or slay them, then retry.

## Roll back R4 to R3

R3's batcher units and code are intact, and its queue reads rounds from the shared store, so a
rollback reactivates it. Before the batcher runs again, land every GREEN controller round with
the hand-land entry and abandon the rest with their evidence noted.

<!-- check: rollback -->
```sh
release activate "$R3"   # not checked: a sim world has no release to activate
queue round status "$REPO"   # exits 0
queue round abandon "$BATCH" "$REPO" --reason "$WHY"   # exits 0
queue round status "$REPO" | grep -qx 'round=none'   # exits 0
```

**Success:** `round=none` for every repo, then `world start`.
**If it fails:** an abandon that refuses names the lease holding the round: clear it first.
