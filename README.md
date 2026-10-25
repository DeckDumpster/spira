# Spira

Spira is an agent harness for software work. You file work items (**beads**) into a store;
Spira summons AI agent sessions (**aeons**) to do them, each in its own git worktree and
branch; and it lands the results onto a repository's landing ref through gated **rounds** that
run the full test corpus before anything merges. Every step between "filed" and "released" is
driven by small timer-activated programs reading and writing explicit state machines, so the
loop keeps going with nobody watching. Humans decide only escalations: the few questions the
loop cannot answer for itself (permissions, policy, destructive actions) are routed to the
operator, and everything else is retried, reworked, held or dropped by rule.

It is a Cargo workspace of single-purpose binaries plus a shell layer in `spira/`, run as
systemd units (`systemd/*.service|timer`) on one Linux box and backed by a Dolt SQL server.

## Why it is shaped this way

- **State machines, not scripts.** A bead, a round, an ask and a release each have an explicit
  lifecycle. A timer-driven program moves one step and exits, so a crash costs one step, not the
  loop.
- **Nothing lands unproven.** Work reaches the landing ref only through a round that has run
  the repository's tests in a clean environment against the exact commit it merges.
- **Humans decide only what the loop cannot.** Everything else is retried, reworked, held or
  dropped by rule, and what is held says who lifts it.
- **Mechanism, not inventory.** The repository ships rules and guards; names, hosts and data
  belong to whoever runs it.

## How work flows

```
 bead ──► aeon ──► gate ──► round ──► landing ──► release
 filed    claims    certifies  merges    fast-forwards   builds, verifies,
 READY    one bead  the tip    a set of  the landing ref  activates the landed
          in its    (tests in  certified                  commit
          worktree  a clean    tips
                    box)
```

1. **File.** A bead is filed by a person, a session, a GitHub issue or a trigger unit.
2. **Summon and claim.** The sentinel counts ready beads and summons aeons; each aeon claims
   exactly one bead (a compare-and-swap, so two cannot win it) and works it on `spira/<id>`.
3. **Submit and certify.** The aeon runs the tests for what it changed and submits; the gate
   certifies the exact commit against the repository's corpus.
4. **Round and land.** Certified tips are merged together in a round, proven again as a set,
   and landed on the landing ref.
5. **Release.** A release is built from the landed commit, verified, and activated by an atomic
   symlink swap; merging never deploys by itself.

The full walk-through, with the binary and unit behind each step, is
[docs/architecture.md](docs/architecture.md).

## Quick start

Spira runs as systemd **user units** on Linux. Prerequisites and the full procedure are in
[docs/install.md](docs/install.md); in short:

```sh
make dist      # build the workspace and package ./spira-<UTC timestamp>.tar.gz
target/release/release install-tarball spira-<ts>.tar.gz --answers answers.txt
export SPIRA_TOML=<path the command printed>
<releases>/current/bin/spira-install --dry-run   # what it would do
<releases>/current/bin/spira-install             # then for real
```

The answers file names the instance, the bead id prefix, the home repository and the data
directories; [docs/install.md](docs/install.md) lists every key.

Check it is healthy:

```sh
doctor          # read-only; exits 1 on any FAIL
ready.sh        # "can this installation receive work right now?"
world status    # halted / draining / running
```

File your first bead with `bead.sh file "<title>" --for builder --repo <name>`; see
[docs/operating.md](docs/operating.md).

## Where to go next

| Read | For |
|---|---|
| [docs/concepts.md](docs/concepts.md) | the vocabulary: bead, aeon, persona, lane, round, gate, release, statute |
| [docs/architecture.md](docs/architecture.md) | components and stores, and a bead from filing to released |
| [docs/components/](docs/components/README.md) | one page per group of binaries and units |
| [docs/install.md](docs/install.md) | prerequisites, building, configuration, starting and stopping, health |
| [docs/operating.md](docs/operating.md) | filing work, queue modes, rounds, releases, when something is stuck |
| [docs/development.md](docs/development.md) | repository layout, building, tests, how a change lands, conventions |
| [docs/glossary.md](docs/glossary.md) | every term, one line each |
| [docs/README.md](docs/README.md) | the whole corpus, including designs, runbooks, spikes and the test plan |

## Contributing

A change to Spira is work like any other: file a bead against this repository and let a
builder aeon take it, or work by hand on a branch. Either way, read
[docs/development.md](docs/development.md) first, and `CLAUDE.md` before editing: this
repository is cloned by people whose infrastructure is not yours, so it ships mechanism (rules,
guards, the reason a check fails closed) and never inventory (repository names, hosts, people,
paths, bead ids).

Run the suites that cover what you changed with the project's test runner, in a container,
never directly on the host. Details are under *Tests* in the development guide.
