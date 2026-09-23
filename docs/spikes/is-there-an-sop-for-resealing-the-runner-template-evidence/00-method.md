# Method and conditions — spike sp-7oecu, 2026-09-23

Every claim in `../is-there-an-sop-for-resealing-the-runner-template.md` was produced on the
operator's box on 2026-09-23 by the commands recorded in this directory. This file states the
conditions, because an answer that does not say what it could and could not reach is not an
answer.

## What was read, and at which revision

| Source | Revision | How it was read |
| --- | --- | --- |
| harness working tree | `spira/sp-7oecu` off `main` @ `2e23aa3d` | the spike's own worktree |
| harness SOP shelf | live, read through `spira/sop.sh` | `list`, `show`, `match`, `lint` |
| the bead store | live | `bd -C $SPIRA_DB list --all --json`, 3683 beads |
| the provision repo (`ephemeral-ci`) | `origin/main` @ `487a7d5` | `git show origin/main:<path>` |
| the brain wiki | `/wiki/notes/reseal-ci-runner-template.md` @ `0922e7f` | read in place |
| operator mailbox | `$SPIRA_MAIL/operator` | read in place |

The provision repo was read through `git show origin/main:…` rather than from the checkout's
working tree. The checkout sits on an unrelated branch whose `scripts/provision.sh` is 412
lines and carries **no** mask block; `origin/main`'s is 662 lines and carries it at 562-596.
Reading the working tree would have reported the refusal as absent. That is the first trap in
this subject and it is the same shape as the second one (see `05-`).

## What could not be reached, and why

- **The template's actual state.** The Proxmox console and the hypervisor API need the
  operator's credentials. Nothing here can run `qm list` or boot a clone.
- **Any CI log.** An aeon session carries an empty `GH_CONFIG_DIR` and a write-only broker;
  `git fetch` against the provision repo fails with *"aeon: no SSH credentials"*. So the
  evidence the SOP's own CHECK asks for — the presence of
  `provision.sh: masking apt automation in guest N` in a provision log — could not be
  gathered by this spike. It is named as the falsifier instead.
- **Whether the `v1` tag actually moved to `487a7d5`.** The local tag `v1` still resolves to
  `ae8995d` and cannot be refreshed without a fetch. `origin/main` being at `487a7d5` is
  observed; the tag advance at 03:58Z is taken from the brain wiki page and the SOP, not
  verified here.

## Marks used throughout

**Observed** — a command in this directory produced it. **Inferred** — read off code that
was not executed. **Unchecked** — stated by a source this spike trusted but did not confirm.
