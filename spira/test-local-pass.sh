#!/usr/bin/env bash
#
# test-local-pass.sh — release.sh --dispatch refuses a commit with no recorded
# acceptance-local A-D pass, and goes ahead with one (or a named, logged override).
#
# defect: sp-x334k
# tier: T1
# covers: spira/release.sh spira/acceptance-local.sh spira-config/src/local_pass.rs spira-config/src/main.rs
# scar: a release cut and a publish PR were gated only by the operator remembering to run the local round first.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v spira-config >/dev/null 2>&1 || skip "spira-config not on PATH"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

UPSTREAM="$TMP/upstream"; REPO="$TMP/repo"
git init -q -b trunk "$UPSTREAM"
printf 'a\n' > "$UPSTREAM/f"; git -C "$UPSTREAM" add f; git -C "$UPSTREAM" commit -qm "sp-aaa — first"
git clone -q "$UPSTREAM" "$REPO" 2>/dev/null
SHA="$(git -C "$REPO" rev-parse HEAD)"

SH="$TMP/spira"; mkdir -p "$SH" "$TMP/bin" "$TMP/run"
for f in release.sh lib.sh conf.sh; do [ -f "$HERE/$f" ] && cp "$HERE/$f" "$SH/"; done
copy_conf_registry "$SH"
printf 'fixture | %s | push | origin/trunk | sp | |\n' "$REPO" > "$TMP/rmap"
printf '#!/bin/sh\necho "$@" >> "%s/ghq.calls"\n' "$TMP" > "$TMP/bin/ghq"; chmod +x "$TMP/bin/ghq"

dispatch() {
    env -i PATH="$TMP/bin:$PATH" HOME="$HOME" SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_REPO_MAP="$TMP/rmap" SPIRA_REPO="$SH" SPIRA_ID_PREFIX=sp "$@" \
        bash "$SH/release.sh" cut fixture --dispatch 2>&1
}

OUT="$(dispatch)"; RC=$?
[ "$RC" -ne 0 ] && ok "no record: dispatch refused" || bad "no record: dispatch refused" "rc=0: $OUT"
want "the refusal names the missing record" "no acceptance-ad local-pass record for $SHA" "$OUT"
want "the refusal names the command"        "acceptance-local.sh"                          "$OUT"
[ ! -e "$TMP/ghq.calls" ] && ok "nothing was dispatched" || bad "nothing was dispatched" "ghq was called"

OUT="$(dispatch SPIRA_LOCAL_PASS_OVERRIDE=short)"; RC=$?
[ "$RC" -ne 0 ] && ok "a too-short override reason is refused" || bad "a too-short override reason is refused" "$OUT"

SPIRA_RUN="$TMP/run" spira-config local-pass record full-suite "$SHA" t
OUT="$(dispatch)"; RC=$?
[ "$RC" -ne 0 ] && ok "a full-suite record does not satisfy the acceptance check" || bad "a full-suite record does not satisfy the acceptance check" "$OUT"

OUT="$(dispatch SPIRA_LOCAL_PASS_OVERRIDE='box cannot run podman today')"; RC=$?
[ "$RC" -eq 0 ] && ok "a named override dispatches" || bad "a named override dispatches" "rc=$RC: $OUT"
want "the override is logged" "box cannot run podman today" "$(cat "$TMP/run/local-pass/overrides.log" 2>/dev/null)"
rm -f "$TMP/ghq.calls"

SPIRA_RUN="$TMP/run" spira-config local-pass record acceptance-ad "$SHA" acceptance-local.sh
OUT="$(dispatch)"; RC=$?
[ "$RC" -eq 0 ] && ok "a recorded A-D pass dispatches" || bad "a recorded A-D pass dispatches" "rc=$RC: $OUT"
want "gate.yml was dispatched" "cut=true" "$(cat "$TMP/ghq.calls" 2>/dev/null)"

tl_summary
