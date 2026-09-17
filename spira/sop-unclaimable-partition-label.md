MATCH: unclaimable.*no matching partition|no persona's partition labels
SYMPTOM: Bead is marked UNCLAIMABLE because it lacks a required partition label from the persona system
CHECK: bd show <bead> | grep -E 'partition:(groom|incident|maechen-sweep|plan|spike)|(incident|plan|groom|maechen-sweep|spike)\b'
FIX: bd label add <bead> partition:incident
ROOT_CAUSE_FIX: Multiple scripts creating beads without partition labels: auron.sh alerts, gate-spira.sh budget chore, review.sh findings, and incident.sh itself. Fixed by: adding SPIRA_INCIDENT_LABEL to auron alerts and audit probe; SPIRA_PLAN_LABEL to gate budget and review findings; SPIRA_INCIDENT_LABEL to incident.sh LABELS variable. Partition labels ensure beads are claimable by their respective personas (ops for incidents, builder for plans).
ESCALATE: If the partition type needs to be something other than 'incident', determine which persona should claim this bead and add its partition label instead. If the recurrence continues despite the code fix, it may indicate a different code path bypassing the fixed files.
REF: wiki/notes/standard-operating-procedures.md
