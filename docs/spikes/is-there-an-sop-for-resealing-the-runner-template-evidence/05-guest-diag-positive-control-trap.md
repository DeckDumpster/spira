# 05 — the trap: why a clean guest-diag line proves nothing

This is the finding that decides whether the reseal is still owed, and it is the reason the
question stayed open for a day. Source: this repository's own `.github/workflows/gate.yml`,
read at `2e23aa3d` on 2026-09-23. Verified independently here rather than taken from the note
on the open Proxmox ask.

A gate run printed, at 03:14:42Z:

```
guestdiag: unattended-upgrades=masked apt-daily=masked apt-daily-upgrade=masked
```

exit 0 — which reads as "the template is sealed". It is not. The gate masks all three units
**inside the job**, at line 173, and prints the diagnostic at line 293 — 120 lines and several
steps later.

## The masking step (lines 164-177), verbatim

```yaml
      - name: Stop apt automation
        # The runner template ships apt-daily and apt-daily-upgrade enabled. On a fresh
        # clone the persistent timer fires within the first hour, the upgrade restarts
        # services, and the runner is SIGTERMed mid-suite: "The runner has received a
        # shutdown signal". The guest journal showed apt-daily-upgrade starting 30-64s
        # before each of the three kills on 2026-09-18 (sp-hu3bk). `stop` lets an
        # upgrade already running finish its current package rather than leave dpkg
        # half-configured. The durable fix is in the template and provisioning (sp-vp0me).
        run: |
          sudo systemctl mask --now apt-daily.timer apt-daily-upgrade.timer unattended-upgrades.service
          sudo systemctl stop apt-daily-upgrade.service apt-daily.service unattended-upgrades.service || true
          sudo dpkg --configure -a || true
          systemctl is-enabled apt-daily.timer apt-daily-upgrade.timer unattended-upgrades.service || true

```

## The diagnostic (lines 288-297), verbatim

```yaml
          # only place guest vitals can be recorded and still be readable.
          #
          # Sampled, not summarised: a single reading after the fact cannot show a
          # climb, and the climb is the thing being diagnosed.
          printf 'guestdiag: uptime %s\n' "$(uptime -p 2>&1 || true)"
          printf 'guestdiag: unattended-upgrades=%s apt-daily=%s apt-daily-upgrade=%s\n' \
            "$(systemctl is-enabled unattended-upgrades.service 2>&1 || true)" \
            "$(systemctl is-enabled apt-daily.timer 2>&1 || true)" \
            "$(systemctl is-enabled apt-daily-upgrade.timer 2>&1 || true)"
          systemctl list-timers --all --no-pager 2>&1 | sed 's/^/guestdiag: /' || true
```

## What this establishes

The diagnostic is reading the gate's own workaround, not the template's state. Any Spira gate
run will print all three `masked` whether the template is sealed or not, so the line has **zero
discriminating power** for this question and must never be cited for it.

Note also line 165's comment, written by the harness about itself:

> The runner template ships apt-daily and apt-daily-upgrade enabled.

That is the harness's own standing statement that the template is drifted, and the workaround
exists because of it. It is not proof of today's state either — a comment is not a measurement
— but it is the reason the workaround is still in the file.

## The shape of the trap, generally

Two readings look like proof of a sealed template and are not:

1. **A clean `guestdiag` line in a gate run.** Refuted above.
2. **A provision job that did not print `provision.sh: masking apt automation in guest N`.**
   That line is unconditional in the code carrying the check, so its absence means the action
   that ran predates the check — the run says nothing either way. This is
   `law-absence-needs-a-positive-control` in its exact form: check for the line before
   trusting the silence.
