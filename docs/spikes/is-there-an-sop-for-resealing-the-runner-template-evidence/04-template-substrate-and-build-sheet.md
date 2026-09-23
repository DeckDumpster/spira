# 04 — the substrate script and the build sheet

Two sources from the provision repository (`ephemeral-ci`) at `origin/main` = `487a7d5`,
retrieved 2026-09-23 via `git show origin/main:<path>`. Excerpted, not whole: the full files
are 351 and 783 lines and only the reseal-relevant parts are kept.

## scripts/template-substrate.sh — header (lines 1-30)

The build sheet's own instruction is *"run the script, do not paste the list"*.

```bash
#!/usr/bin/env bash
# covers: docs/TEMPLATE.md (the "Packages and tooling" and "Container store" sections)
#
# The machine properties every ephemeral runner needs, as a script rather than a
# paragraph.
#
#   bash scripts/template-substrate.sh            install/apply anything missing
#   bash scripts/template-substrate.sh --check    report what is missing; exit 1 if any
#
# RUN THIS WHEN BUILDING THE TEMPLATE, BEFORE CONVERTING THE VM. It is idempotent,
# so it is also safe at the head of a job to verify a clone came up correctly --
# though a --check failure there means the template is wrong, not the job.
#
# WHY THIS EXISTS. docs/TEMPLATE.md carried this list as prose with bash blocks a
# human pasted. Nothing executed it and nothing checked it, so the list drifted
# from what the runners actually needed and every consuming repository rediscovered
# the gap separately, one CI round-trip at a time. In a single afternoon two repos
# independently hit the same missing pasta, the same uid-pinning bug and the same
# boot-time dpkg race; a third had already solved two of them months earlier in a
# file nothing else could see. A dependency a human has to remember is a dependency
# that is missing on the next machine.
#
# WHAT BELONGS HERE, AND WHAT DOES NOT. This file is the SUBSTRATE: properties of
# the machine that every consumer needs and no consumer should have to know about.
# A dependency of one repository's suite -- uv, Chromium's shared libraries, a Rust
# toolchain, metric-compatible fonts for visual regression -- belongs in that
# repository's own runner-deps check, which declares what IS its own. The test for
# the boundary: if a second repository would be surprised to need it, it is not
# substrate.
set -uo pipefail
```

## scripts/template-substrate.sh — the apt-automation check (lines 245-272)

This is the `--check` half. **Note that it accepts `disabled` as clean**, while
`provision.sh`'s post-mask verify requires literally `masked` (see `03-`). So
`template-substrate.sh --check` exiting 0 does not by itself prove the template will satisfy
provisioning; the runbook's own "prints masked three times" step is the stricter one and is
the one to trust.

```bash
}

# APT AUTOMATION HAS NOTHING TO OFFER A VM THAT LIVES THIRTY MINUTES, and it
# holds the dpkg lock at exactly the moment provisioning wants it. This is the
# durable half of the fix; APT_LOCK_WAIT above is the half that still works on a
# box where somebody has re-enabled it.
# ALL THREE UNITS. unattended-upgrades.service does the upgrades;
# apt-daily-upgrade.timer schedules them; apt-daily.timer schedules the preceding
# apt-get update. Masking only the service leaves the timers running, which can
# still race for the lock at boot.
# MASKED, NOT MERELY DISABLED. A disabled unit can be pulled back in as another
# unit's dependency; masking is what makes that impossible. Purging is finer still
# and satisfies this check by making the unit absent.
check_unattended() {
    local state u
    command -v systemctl >/dev/null 2>&1 || return 0
    for u in unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer; do
        state="$(systemctl is-enabled "$u" 2>/dev/null)"
        case "$state" in
            ''|masked|disabled|'not-found'|linked-runtime) ;;
            *)
                lack "$u masked" "it is '$state'; apt automation races provisioning for the dpkg lock at boot — one run in several fails with a dependency list that worked the run before"
                return 1
                ;;
        esac
    done
    return 0
}
```

## scripts/template-substrate.sh — the apply half (lines 337-350)

Idempotent, and it ends at `masked`, not `disabled`.

```bash
if command -v systemctl >/dev/null 2>&1; then
    for _apt_unit in unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer; do
        case "$(systemctl is-enabled "$_apt_unit" 2>/dev/null)" in
            ''|masked|'not-found') ;;
            *)
                note "masking $_apt_unit (apt automation races provisioning for the dpkg lock)"
                $SUDO systemctl disable --now "$_apt_unit" \
                    || note "could not disable $_apt_unit"
                $SUDO systemctl mask "$_apt_unit" \
                    || note "could not mask $_apt_unit"
                ;;
        esac
    done
fi
```

## docs/TEMPLATE.md — the overall shape (lines 1-10)

```
# Proxmox VM template build sheet — ephemeral CI runner

This document describes exactly what the Proxmox VM template must contain so
that every ephemeral runner cloned from it can execute `bash deploy/ci.sh` to
completion without further provisioning. Build the template by hand in a
Proxmox console, validate it with the checklist at the bottom, then convert
it to a template. **Do not register the GitHub Actions runner inside the
template.** Registration is per-token and per-run; a template with a
pre-registered runner clones into N VMs all claiming to be the same agent.

```

## docs/TEMPLATE.md — "Converting to a template" (line 746)

This is the tail of a reseal: the two console steps that turn the fixed VM back into a
template.

```
## Converting to a template

After all verification steps pass, in the Proxmox console:

1. Shut the VM down cleanly (`sudo poweroff`).
2. Right-click the VM → **Convert to template**.

Clones created with **Linked Clone** (`--full 0`) share the template's base
disk and are created in seconds. Each clone boots as a fresh VM, reads the
token `provision.sh` injected via cloud-init, and self-registers via
`start-runner.sh` before the runner agent picks up its job.

## /tmp must not be a tmpfs
```

## docs/TEMPLATE.md — qemu-guest-agent (lines 323-335)

Not optional. `provision.sh` polls `/agent/ping` and will not proceed without it, so a
template resealed without the agent enabled makes every provisioning call time out rather
than fail with a legible message.

```bash
sudo apt install -y qemu-guest-agent
sudo systemctl enable --now qemu-guest-agent
```

Confirm with `systemctl is-active qemu-guest-agent` -> `active`. The host-side half is
`qm set <TEMPLATE_VMID> --agent 1`.

## One thing the build sheet decides that nothing downstream will report

`provision.sh` sets no `--cores`, no `--sockets` and no `--memory`; clones inherit the
template's config wholesale. A reseal that creates a new template from a clone therefore
carries the CPU and memory sizing forward silently — which is correct when the clone came from
the template being replaced, and is a silent regression if it came from anywhere else.
