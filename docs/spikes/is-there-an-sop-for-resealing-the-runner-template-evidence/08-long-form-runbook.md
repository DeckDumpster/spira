# 08 — the long form, preserved

The REF the runbook points at, preserved verbatim because a citation that cannot be re-read is
not a citation. Source: the brain wiki, `wiki/notes/reseal-ci-runner-template.md`, added by
commit `0922e7f` on 2026-09-22. Retrieved 2026-09-23.

---

---
type: note
created: 2026-09-22
updated: 2026-09-22
tags: [spira, ephemeral-ci, proxmox, runbook, ops]
sources: []
aliases: [Template reseal, Reseal 9110]
---

# Resealing the ephemeral CI runner template

Console procedure for the Proxmox side of [[spira]]'s CI. The short form is on the
SOP shelf as `sop-template-apt-drift`; this is the long form it points at.

## What "drifted" means

Every ephemeral runner is a clone of Proxmox template **9110** on node
`hypervisor`. Three systemd units must be `masked` inside that template:

```
unattended-upgrades.service
apt-daily.timer
apt-daily-upgrade.timer
```

If any of them is enabled, the clone inherits it, the timer fires at a random
point in the first hour, `needrestart` restarts the runner service, and the job
dies mid-suite with *"The runner has received a shutdown signal"*. The other face
of it is a race for `/var/lib/dpkg/lock-frontend` that reads as a broken
dependency list. Both were measured on 2026-09-18 (`sp-hu3bk`, `sp-3q0cz`).

The template was **last measured drifted on 2026-09-18** — all three enabled. No
reseal has been recorded since. (per Ryan's open ask `sp-400g1`, item 2)

## Why it is now blocking, not housekeeping

`provision.sh` used to warn and continue. Since `sp-3q0cz` (commit 25d5625) it
**refuses**:

```
::error::provision.sh: template 9110 has apt automation enabled;
reseal the template (bash scripts/template-substrate.sh) before provisioning
```

That code reached consumers when ephemeral-ci's `v1` tag was advanced to
`487a7d5` at **2026-09-23 03:58Z**. Every repository pinning
`DeckDumpster/ephemeral-ci/provision@v1` — spira, deckdumpster, pokedumpster —
now runs the check on every job. A drifted template stops all CI at provision.

## The trap: what a clean reading looks like when you are reading the wrong thing

Two readings look like proof and are not:

1. **`guestdiag: unattended-upgrades=masked apt-daily=masked …` in a Spira gate
   run.** Spira's own `gate.yml` has a *Stop apt automation* step (lines 164–175)
   that masks all three in the job, and it runs before the diagnostic. The diag
   is reporting the workaround, not the template.
2. **A provision job that did not print `provision.sh: masking apt automation in
   guest N`.** That line is unconditional in the code that carries the check. Its
   absence means the action that ran predates the check, so the run says nothing
   about the template either way. Check for the line before trusting the silence.
   ([[common-law]]: `law-absence-needs-a-positive-control`)

## The procedure — clone, fix, repoint

Recommended over editing 9110 in place. A template's base disk is read-only, so
un-templating it is fiddly and not reversible in one step; a fresh template is,
because the old one stays where it is until you are satisfied. Nothing needs to
be drained: you are modifying a copy, and clones of 9110 are unaffected.

### 1. On the hypervisor, as root — pick an id and confirm the target

```bash
pvesh get /cluster/nextid
qm config 9110 | grep -E 'template|name|agent|memory|cores'
```

### 2. Full clone, then start it

```bash
NEW=9111                       # whatever nextid gave you
qm clone 9110 "$NEW" --full 1 --name ci-runner-template-resealed
qm start "$NEW"
```

Full, not linked. A linked clone shares the base disk you are trying to leave
behind.

### 3. Mask apt automation the moment the agent answers

The clone boots with the timers **enabled** — that is the defect — so an
unattended upgrade can start while you are working. Close that window first:

```bash
until qm guest cmd "$NEW" ping >/dev/null 2>&1; do sleep 2; done
qm guest exec "$NEW" -- /bin/bash -c \
  'systemctl mask --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer'
```

### 4. Inside the VM, run the substrate script

Open the VM's console in the Proxmox web UI and log in as `runner` (or
`qm terminal "$NEW"` for the serial console; `Ctrl-O` exits). `ephemeral-ci` is
a public repository, so this needs no credentials:

```bash
git clone --depth 1 https://github.com/DeckDumpster/ephemeral-ci /tmp/eci
sudo bash /tmp/eci/scripts/template-substrate.sh          # apply
bash /tmp/eci/scripts/template-substrate.sh --check       # must exit 0
echo "check rc=$?"
```

The script is idempotent and masks all three units itself; step 3 only closes
the race. `--check` names whatever is still missing. Do not convert until it
exits 0.

### 5. Verify the three units by name

Not by the script's own summary — by the units:

```bash
systemctl is-enabled unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer
```

All three must print `masked`. Anything else means you are not done.

### 6. Clean up and shut down

```bash
rm -rf /tmp/eci
sudo poweroff
```

### 7. Convert, back on the hypervisor

```bash
qm status "$NEW"                      # wait for: status: stopped
qm template "$NEW"
qm config "$NEW" | grep template      # expect: template: 1
```

### 8. Repoint the consumers

`TEMPLATE_VMID` is an **organisation** variable, so one edit covers spira,
deckdumpster and pokedumpster:

> https://github.com/organizations/DeckDumpster/settings/variables/actions

Set it to the new id. Rolling back is setting it to `9110` again.

### 9. Prove it with a positive control

```bash
gh workflow run gate.yml --repo DeckDumpster/spira --ref main
```

In that run's **provision** job:

- `provision.sh: masking apt automation in guest N` must be **present** — if it
  is missing the check never ran and the result means nothing;
- `::error::… has apt automation enabled` must be **absent**.

Cite the run id when closing `sp-400g1`.

### 10. Destroy the old template

Only after a batch has landed green on the new one:

```bash
qm destroy 9110
```

## The companion item — `PVE_CA_CERT`

Advancing `v1` also armed the teardown side. `scripts/teardown.sh` goes through
`pvapi.sh`, which uses `--cacert` with **no insecure fallback by design**, and
reads the PEM from a temp file written from the org variable `PVE_CA_CERT`.
`provision.sh` does *not* need it — it carries its own `curl -k` API helper — so
provisioning will work while teardown fails, which leaks one VM per run. With
the reaper red (`sp-wy2yl`, `sp-7f4f9`) nothing collects them, and a full node
stalls provisioning behind the capacity wait.

Set `PVE_CA_CERT` to the contents of `/etc/pve/pve-root-ca.pem` from the
hypervisor, at organisation level, in the same sitting as the reseal.

## Related

[[spira]] · [[cockpit]] · [[common-law]]
