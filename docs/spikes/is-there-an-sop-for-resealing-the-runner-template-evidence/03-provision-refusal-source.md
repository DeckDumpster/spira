# 03 — the refusal, at source

Source: `scripts/provision.sh` in the provision repository (`ephemeral-ci`), at
`origin/main` = `487a7d5`. Retrieved 2026-09-23 with
`git show origin/main:scripts/provision.sh | sed -n '555,600p'`. Preserved verbatim.

The refusal the runbook's SYMPTOM quotes is line **593**; the log line its CHECK depends on is
line **583**. The masking of the *running clone* is unconditional and happens before the check
exits, which is why a drifted template still yields a usable VM for the job that is already
running — and why the refusal is about the template rather than about this VM.

```bash
fi
poll_task "$start_upid" || exit 1

# --- Wait for guest agent ---
#
# The guest agent needs time to start after the VM boots. Poll /agent/ping
# until it responds before attempting file-write, which would fail immediately
# if the agent is not yet running.
printf 'provision.sh: waiting for guest agent on VM %s (timeout %ss)\n' "$VMID" "$AGENT_TIMEOUT" >&2
agent_deadline=$(( $(date +%s) + AGENT_TIMEOUT ))
while true; do
    # The inline pvapi() returns non-0 on non-2xx. If pvapi.sh is ever
    # sourced here instead, pvapi() returns 0 even on 500 but sets
    # PVAPI_STATUS. The PVAPI_STATUS:-200 default makes both work: the
    # inline version leaves PVAPI_STATUS unset, so it defaults to 200 on
    # any successful return (which the inline version only does on 2xx).
    _ping_ok=0
    if pvapi POST "/nodes/${PVE_NODE}/qemu/${VMID}/agent/ping" >/dev/null 2>&1; then
        case "${PVAPI_STATUS:-200}" in 2??) _ping_ok=1 ;; esac
    fi
    [ "$_ping_ok" -eq 1 ] && break
    if [ "$(date +%s)" -ge "$agent_deadline" ]; then
        printf 'provision.sh: timed out waiting for guest agent on VM %s (%ss)\n' \
            "$VMID" "$AGENT_TIMEOUT" >&2
        exit 1
    fi
    sleep 2
done

# --- Mask apt automation ---
#
# A fresh clone inherits the template's systemd unit state. If apt timers are
# enabled, unattended-upgrades or apt-daily-upgrade fires at a random point in
# the first hour and needrestart can restart the runner's service, killing the
# job mid-suite with "The runner has received a shutdown signal". Mask before
# credentials land: the runner never starts on a node where an upgrade can
# interrupt it.
#
# If the template was drifted (timers enabled), refuse: a warning that succeeds
# cannot surface a fault that appears 20 minutes later as a dpkg-lock failure
# (law-a-control-that-cannot-check-must-refuse). The guest's units are masked
# before the check exits so the running clone is safe regardless, but a drifted
# template must be resealed (bash scripts/template-substrate.sh inside the
# template VM, then convert) before CI can provision again.
# If the mask call itself fails, also refuse.
#
```

## What the guest script's exit codes mean

From the comment above, verbatim:

```
#   0: all units were already masked/disabled -- template is clean
#   2: one or more were enabled; all are now masked -- template is drifted: refuse
#   1 (or other non-zero): mask or post-mask verify failed -- refuse
```

`disabled` counts as clean for the *drift* test but the post-mask verify requires exactly
`masked`. That asymmetry is why the runbook's FIX tells the operator to confirm three literal
`masked` strings rather than to trust `template-substrate.sh --check`, which also accepts
`disabled` (see `04-`).

## Provenance caveat

The local checkout of this repository sits on an unrelated branch where `scripts/provision.sh`
is 412 lines and contains no mask block at all. Read from the working tree, this evidence is
absent. Always `git show origin/main:` here.
