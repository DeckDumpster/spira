MATCH: unclaimable.*no matching partition|no persona's partition labels
SYMPTOM: Bead is marked UNCLAIMABLE because it lacks a required partition label from the persona system
CHECK: bd show <bead> | grep -E 'partition:(groom|incident|maechen-sweep|plan|spike)'
FIX: bd label add <bead> partition:incident
ESCALATE: If the partition type needs to be something other than 'incident', determine which persona should claim this bead and add its partition label instead. If this recurs (same failure fingerprint within 7 days), the root cause is likely a bead creation path that bypasses bead.sh file (which assigns partition labels from the persona's fayth). Investigate: review.sh, auron.sh, gh-intake.sh, and gate-spira.sh for direct bdq create calls that lack --for <persona> or explicit -l partition:* labels. Fix by adding proper partition labels or routing through bead.sh.
REF: wiki/notes/standard-operating-procedures.md
