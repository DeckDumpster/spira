#!/usr/bin/env bash
set -uo pipefail

landing-pass mark "$1" LANDED "$2"
env SPIRA_RUN="$RUN" "$SPIRA_HOME/bin/landing-pass" cited-commit "$1" repo main
command landing-pass close-on-land "$1"
# Not oracles: another subcommand, and a verb that cannot be named statically.
landing-pass is-work-type task
landing-pass "$verb" "$1"
