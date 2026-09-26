# sp-cpzge: brain-guard in-place-write false-positive (moot)

## What was filed

brain-guard.sh's PostToolUse in-place-write arm (brain repo,
`.claude/guards/brain-guard.sh`) fired on its own prescribed remedy: when no
PreToolUse inode snapshot existed for a call, the arm fell back to comparing
mtime alone, and `git checkout -- <path>` changes mtime exactly like the
violation it was meant to catch.

## Investigation

The guard no longer exists. Brain commit `a173f02` ("refactor: retire the
brain guard system (per Ryan, test-plan review V12)") deleted
`.claude/guards/` in its entirety and removed all fifteen PreToolUse/
PostToolUse registrations from `.claude/settings.json` — the system was
retired, not repaired, on Ryan's direction.

Checked whether spira's own guard hooks (`spira/hooks/aeon-fence.sh` and
siblings) carry the same mtime-only fallback pattern: they do not use
inode snapshots or mtime comparison at all, so this class of false positive
has no analog in this repo to fix.

## Outcome

Nothing to change. The lesson survives independent of the deleted code: a
check that cannot distinguish a violation from the remedy it just prescribed
will punish whoever complies with it.
