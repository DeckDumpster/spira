#!/usr/bin/env bash
# sop-lint-watch.sh — periodic `sop lint` against production; files an incident on a real finding.
# Refuses (exit 3, files an incident) when lint cannot read the shelf — never reports green then
# (law-a-control-that-cannot-check-must-refuse). `--selftest` is the positive control: a fake
# `sop` that reports a finding must produce a filing, a fake that cannot read must refuse.
set -u
SOP_BIN="${SOP_LINT_SOP:-sop}"
FILER="${SOP_LINT_FILER:-incident.sh}"
file_it() { printf '%s\n' "$2" | "$FILER" file "$1" -; }
run() {
  local out rc
  out=$("$SOP_BIN" lint 2>&1); rc=$?
  if printf '%s' "$out" | grep -q 'could not read the shelf'; then
    file_it "sop lint could not read the shelf (control cannot check)" "$out"; return 3
  fi
  if [ "$rc" -ne 0 ]; then
    file_it "sop lint finding on production shelf" "$out"; return 1
  fi
  return 0
}
if [ "${1:-}" = "--selftest" ]; then
  d=$(mktemp -d); trap 'rm -rf "$d"' EXIT
  printf '#!/bin/sh\necho "bad sop x"; exit 1\n' >"$d/find"; 
  printf '#!/bin/sh\necho "lint: could not read the shelf — refusing"; exit 1\n' >"$d/blind"
  printf '#!/bin/sh\nexit 0\n' >"$d/ok"
  printf '#!/bin/sh\ncat >>"%s/filed"\n' "$d" >"$d/filer"; chmod +x "$d"/*
  export SOP_LINT_FILER="$d/filer"
  SOP_LINT_SOP="$d/ok" "$0"; [ $? -eq 0 ] && [ ! -s "$d/filed" ] || { echo FAIL clean; exit 1; }
  SOP_LINT_SOP="$d/find" "$0"; [ $? -eq 1 ] && [ -s "$d/filed" ] || { echo FAIL finding; exit 1; }
  : >"$d/filed"
  SOP_LINT_SOP="$d/blind" "$0"; [ $? -eq 3 ] && [ -s "$d/filed" ] || { echo FAIL blind; exit 1; }
  echo "selftest ok"; exit 0
fi
run
