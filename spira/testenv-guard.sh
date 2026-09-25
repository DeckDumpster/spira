#!/usr/bin/env bash
# testenv-guard.sh — tiny prelude for suites that declare `# requires: testenv` but do
# not source testlib.sh. Refuses before any of the suite's own code runs, when
# SPIRA_IN_TESTENV is not 1. testlib.sh carries the same check (suite_testenv_unmet in
# suite-covers.sh) for suites that source it instead — this file exists so the ones that
# don't still get it.
#
#   HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
#   . "$HERE/testenv-guard.sh"
#
# covers: spira/testenv-batch.sh spira/suite-covers.sh
_TG_SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck disable=SC1090
. "$_TG_SELF/suite-covers.sh"
if suite_testenv_unmet "${BASH_SOURCE[1]:-$0}"; then
    printf 'Bail out! %s requires: testenv — run via spira/testenv-batch.sh, not directly (SPIRA_IN_TESTENV != 1)\n' \
        "$(basename "${BASH_SOURCE[1]:-$0}")"
    exit 2
fi
unset _TG_SELF
