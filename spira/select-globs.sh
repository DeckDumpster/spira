# select-globs.sh — file-bucket declarations for select.sh. Sourced, never executed.
#
# FOUR BUCKETS:
#   inert    matches SELECT_INERT    → contributes nothing to suite selection
#   source   matches SELECT_SOURCE   → must be claimed; unclaimed is an error
#   plumbing matches SELECT_PLUMBING → all-suites fallback even when claimed
#   unknown  everything else         → today's behaviour (all-suites fallback when unclaimed)
#
# PLUMBING exists because a covers: map can be individually correct — every
# suite's line names files it genuinely covers — and still miss a suite that
# exercises shared build/install/runtime scaffolding through a path its own
# covers: line never mentions. A file that reaches that far cannot be trusted
# to select.sh's per-suite covers: match at all; the whole corpus is the only
# safe answer (law-absence-needs-a-positive-control).
#
# Override any set from the environment before sourcing select.sh.
# covers: spira/select.sh

SELECT_INERT="${SPIRA_SELECT_INERT:-*.md *.txt}"
SELECT_SOURCE="${SPIRA_SELECT_SOURCE:-spira/*.sh}"
SELECT_PLUMBING="${SPIRA_SELECT_PLUMBING:-Makefile Cargo.toml Cargo.lock */Cargo.toml install.sh systemd/* spira/conf.sh spira/lib.sh spira/skew.sh spira/testenv*.sh .github/*}"
