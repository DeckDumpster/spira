# select-globs.sh — file-bucket declarations for select.sh. Sourced, never executed.
#
# THREE BUCKETS:
#   inert   matches SELECT_INERT  → contributes nothing to suite selection
#   source  matches SELECT_SOURCE → must be claimed; unclaimed is an error
#   unknown everything else       → today's behaviour (all-suites fallback when unclaimed)
#
# Override either set from the environment before sourcing select.sh.
# covers: spira/select.sh

SELECT_INERT="${SPIRA_SELECT_INERT:-*.md *.txt}"
SELECT_SOURCE="${SPIRA_SELECT_SOURCE:-spira/*.sh}"
