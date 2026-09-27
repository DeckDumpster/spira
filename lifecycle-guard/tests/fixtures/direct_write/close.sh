#!/usr/bin/env bash
set -uo pipefail

id="$1"
bd close "$id" --reason done
bdq update "$id" --status open
