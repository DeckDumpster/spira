# SOP draft: idle-while-ready-unclaimable-count (sp-gcare)

MATCH: idle-while-ready|idle_while_ready|unclaimable.count
SYMPTOM: idle-while-ready watcher says ready beads are unclaimable (sp-jmydu premise).
CHECK: grep -n ready_args in spira-claim; confirm it already excludes epic,event.
FIX: premise is stale; the count already excludes epic,event. Remaining gaps are certified-not-landed blockers and the fayth: preference. File those as beads; do not widen the count.
ESCALATE: unclaimable beads persist with no such blocker.

Drafted here because `sop write` prints "failed to write" (rc=0) even for a valid probe body, with SPIRA_WIKI at brain or worktree. Cause unestablished.
