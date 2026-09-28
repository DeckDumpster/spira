# sp-8cylj: incident intake produced no payload — host mismatch, not permissions

## What happened

Incident `sp-8cylj` was filed against `dolt-beads.service` for a SIGTERM at
13:17:30 that allegedly bypassed `RefuseManualStop=yes`, killed the :3307
listener while the main PID kept draining a `git gc/repack` of the dolt
git-remote-cache, leaving the store unreachable fleet-wide for several
minutes.

Intake itself recorded: "gathered NO payload ... systemctl/journalctl
produced nothing — suspect the unit name or a journal this user cannot read."

## What this session found

- `hostname` on the aeon session assigned to this incident: `agent-swarm-test`.
- `systemctl list-units --all | grep -i dolt` on that host: **no matches at
  all** — the unit does not exist here, not merely inactive.
- `systemctl show dolt-beads.service` returned a blank/zeroed record
  (`ActiveState=inactive`, `MainPID=0`, `RefuseManualStop=no`,
  `ExecMainStartTimestamp=` empty) rather than an error — this is exactly
  what a non-existent unit looks like, and it is indistinguishable from "unit
  exists but is idle" without also checking `list-units --all`.
- `journalctl -u dolt-beads.service --since ... --until ...` printed
  `-- No entries --`, which is exactly what an unreadable-journal case would
  also print.
- `bd -C /home/ryan/spira/db show sp-8cylj` succeeded promptly, proving the
  beads store itself is reachable right now from this host — whatever outage
  the incident describes has already ended (or never touched this host).

## Conclusion

The aeon assigned to `sp-8cylj` is running on a host (`agent-swarm-test`)
that never ran `dolt-beads.service`. Intake's own guess — "unit name or a
journal this user cannot read" — is the wrong diagnosis; the real cause is
that the incident names a unit that lives on a different host than the one
the aeon loop happened to schedule this session onto. No amount of retrying
`systemctl`/`journalctl` here will ever produce the evidence, because the
evidence was never on this box.

The root cause of the incident itself — how a SIGTERM "on client request"
bypassed `RefuseManualStop=yes` and killed only the listener while the main
PID kept running — is still open and can only be diagnosed on the host that
actually runs `dolt-beads.service`, using its journal from 13:17:30.

See `sop-incident-host-mismatch` for the runbook this session wrote from
this finding.
