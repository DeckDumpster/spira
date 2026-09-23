# 01 — the shelf already carried the runbook

Observed 2026-09-23. `spira/sop.sh list` is the whole shelf; the relevant line was there
before this spike ran.

```
$ ./spira/sop.sh list | grep template
  sop-template-apt-drift                   232w  Provision refuses with "::error::provision.sh: template <VMI

$ ./spira/sop.sh list | tail -2

30 SOP(s) on the shelf
```

Nothing named `sop-template-reseal` existed and nothing needed to: `sop-template-apt-drift`
*is* the reseal runbook. Its full text as found (232 words, `sop.sh lint` clean):

```
MATCH: apt automation enabled|reseal the template|apt-daily(-upgrade)?\.timer.*enabled|unattended-upgrades=enabled
SYMPTOM: Provision refuses with "::error::provision.sh: template <VMID> has apt automation enabled; reseal the template before provisioning" and exits 1. Every job in every repo pinning DeckDumpster/ephemeral-ci/provision@v1 fails at the provision stage. Live since v1 moved to 487a7d5 on 2026-09-23 03:58Z.
CHECK: The provision log line "provision.sh: masking apt automation in guest N" must be PRESENT; if absent, the action that ran predates the check and the run proves nothing either way. A guest-diag line reporting the units masked is NOT evidence: spira's gate.yml masks all three in-job before the diagnostic runs.
FIX: Operator console work, not an aeon's. Drain, confirm no runner clone is live with qm list, full-clone template 9110 to a new VMID, boot it, run 'sudo bash scripts/template-substrate.sh' from ephemeral-ci main inside it, confirm 'systemctl is-enabled unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer' prints masked three times, poweroff, convert to template, set org variable TEMPLATE_VMID to the new id. Prove it with one dispatched run whose provision log shows the masking line and no ::error::; cite that run id. Roll back by setting TEMPLATE_VMID to 9110.
ESCALATE: Always — Proxmox console and organisation-variable write are Ryan's alone. Raise it on sp-400g1 rather than filing new work. Set PVE_CA_CERT in the same sitting: teardown now requires it with no insecure fallback, and a failing teardown leaks one VM per run while the reaper is red.
REF: brain wiki/notes/reseal-ci-runner-template.md
```

The long form it points at is `wiki/notes/reseal-ci-runner-template.md` in the brain
repository, added by commit `0922e7f` — nine numbered steps, preserved in `08-`.

## The amendment this spike made

One `sop.sh write` upsert. Only the `MATCH` line changed, plus one clause in `SYMPTOM` and one
in `FIX`; the shelf stayed at 30 SOPs.

```
$ ./spira/sop.sh write template-apt-drift /tmp/sop-new.txt
wrote sop-template-apt-drift (246 words)
  matches: apt automation enabled|resea(l|ling)(\s+[A-Za-z0-9-]+){0,3}\s+template|template(\s+[A-Za-z0-9-]+){0,2}\s+resea(l|ling)|template[\s-]reseal|apt-daily(-upgrade)?\.timer.*enabled|unattended-upgrades=enabled
sop-synth: wrote <brain>/wiki/notes/standard-operating-procedures.md — 30 SOP(s)

$ ./spira/sop.sh lint | tail -1
ok — 30 SOP(s) on the shelf, all valid
```

Diff of the stored text:

```diff
-MATCH: apt automation enabled|reseal the template|apt-daily(-upgrade)?\.timer.*enabled|unattended-upgrades=enabled
+MATCH: apt automation enabled|resea(l|ling)(\s+[A-Za-z0-9-]+){0,3}\s+template|template(\s+[A-Za-z0-9-]+){0,2}\s+resea(l|ling)|template[\s-]reseal|apt-daily(-upgrade)?\.timer.*enabled|unattended-upgrades=enabled
-…Live since v1 moved to 487a7d5 on 2026-09-23 03:58Z.
+…Live since v1 moved to 487a7d5 on 2026-09-23 03:58Z. Also the runbook for a planned reseal.
-…Roll back by setting TEMPLATE_VMID to 9110.
+…Roll back by setting TEMPLATE_VMID to 9110. Not one-off: reseal again whenever the substrate script changes.
```

246 words against the 250-word cap, so the next amendment has four words of headroom and will
have to trim something. Noted rather than solved.
