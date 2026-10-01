#!/usr/bin/env bash
# maechen-trigger.sh — thin shim; all logic is in the maechen-trigger binary (sp-0ekp7,
# wave 7b).
#
# RESOLVED VIA `command -v`, NEVER A BARE `exec maechen-trigger` (scar, sp-0ekp7). bash's
# `exec` builtin falls back to a CWD-relative lookup when its PATH search finds nothing —
# even with "." nowhere in PATH — and this checkout's own crate source directory,
# maechen-trigger/ at the repo root, collides with the binary's bare name whenever a
# caller's working directory is the checkout root (a systemd unit's WorkingDirectory=, a
# test harness invoked from the repo root, …). `command -v` does not have that fallback and
# correctly skips a same-named directory to keep searching PATH, so resolving through it
# first and exec'ing the absolute result — a path with a "/" in it — is exec-safe.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
bin="$(command -v maechen-trigger)" || {
    printf 'maechen-trigger.sh: maechen-trigger is not on PATH\n' >&2
    exit 1
}
# conf.sh sets SPIRA_HOME as a plain shell variable, never exported (it is read within
# THIS process, by scripts that source it directly) — but the binary below is a fresh
# process image after exec, so it needs SPIRA_HOME another way. THE EXPORT THIS COMMENT
# ONCE DESCRIBED IS RETIRED (wave 4.9, sp-k80sa): `export SPIRA_HOME="$HERE"` is replaced
# with `--home "$HERE"` on the binary's own argv, the same convention bead.sh/rule.sh
# already use — SPIRA_HOME is a per-copy fact `spira_config::resolve` deliberately never
# derives, so it has to be told, not read back out of something that itself depends on it.
# `main.rs` parses `--home` once, at the top, and every later `spira_config::resolve_for_
# process` call (including the one behind its own lib.sh-sourcing seam, `real.rs`'s `seam`/
# `seam_ok`, which still `.env("SPIRA_HOME", ...)`s that subprocess explicitly) now reads
# it from there. Found originally via every seam call in test-maechen-trigger.sh failing
# uniformly (source-lib.sh rc=96, SPIRA_HOME seen as ".") — this fix keeps that path closed.
exec "$bin" --home "$HERE" "$@"
