#!/usr/bin/env bash
# A test's planted violation: data under tests/fixtures/, never code that runs.
landed "$1" "$2"
cat "$SPIRA_RUN/landstate/$1"
