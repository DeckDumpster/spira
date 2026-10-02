#!/usr/bin/env bash
#
# bead.sh — file a bead through the contract; never call bd create directly.
#
#   bead.sh file "<title>" --for <persona> --repo <name> [--priority N] [--body-file F] [--submitted] [--json] [--parent <id>]
#   bead.sh file "<title>" --kind <kind> [--repo <name>] [--priority N] [--body-file F] [--submitted] [--json] [--parent <id>]
#   bead.sh lint [--all|<id>...]     check that beads in the store satisfy the contract
#   bead.sh contract                 legal personas, repos and kinds, read from source
#   bead.sh amend <id> [--note "<text>"] [--body-file F] [--express]
#   bead.sh dep add <id> <depends-on-id> [--type <type>]
#                                     wrap `bd dep add`, refusing a blocks edge onto an
#                                     incident-labelled bead (use `bd dep relate` for those)
#
# WHY THIS EXISTS AND NOT bd create DIRECTLY
# ------------------------------------------
# The partition labels a bead carries come from the persona's own predicate; writing them by
# hand means they can disagree, and a bead with wrong labels is either invisible (the persona
# cannot find it) or misrouted (a different persona claims it). `--for <persona>` reads the
# label set from the fayth file, so the filing tool and the claim predicate cannot disagree.
#
# For non-work kinds (event, escalation, proposal, insight) no persona claims the bead, so
# --for is not valid. The label set carries the scope label and, if --repo is given, repo:<name>;
# no partition label is added, making the bead deliberately unclaimable rather than accidentally
# invisible. insight beads are created closed at P4 per the schema declaration.
#
# `bead.sh contract` asks the live source — fayths for personas, schema.sh for kinds, the
# repo-map for repos — so the set it prints is exactly the set it can accept.
#
# THIS FILE IS NOW A SHIM (sp-g9mhe, rewrite wave 5f): the logic above lives in the Rust
# `bead` binary (bead/DESIGN.md). This stays the one entry point every caller names —
# batcher-cut, every chamber brief, concierge.sh, the suites — the same shim pattern
# `gate/DESIGN.md` established for `gate.sh`: source `conf.sh` for its side effects (PATH,
# and every derived default — SPIRA_REPO_MAP's own default is conf.sh's, not bead.sh's own
# "${VAR:-default}" — exactly as the original bead.sh got them, by sourcing `lib.sh`, which
# sources `conf.sh`, inline in the same process before its own logic ran). `: "${VAR:=...}"`
# is conf.sh's own idiom throughout, so an already-exported override (every suite's
# `env -i ... SPIRA_REPO_MAP=... bead.sh ...` fixture) is never replaced.
#
# `exec -a "$0"` keeps this script's path in the process's argv.
#
# THE RE-EXPORT THIS COMMENT ONCE DESCRIBED IS RETIRED (wave 4.9, sp-k80sa). This file used
# to `export SPIRA_HOME SPIRA_REPO_MAP SPIRA_GROOMER_LABEL SPIRA_MAECHEN_LABEL
# SPIRA_CZAR_LABEL` here, because conf.sh's own `export` list leaves those plain-shell-
# variable derivations unexported, and only the environment table crosses the `exec`
# boundary below. The `bead` binary now resolves all five itself: `SPIRA_HOME` is simply
# the `--home` argument already on its own argv (bead/src/main.rs's `chamber_dir`/
# `fayth_names` read that parameter, not `$SPIRA_HOME`); `SPIRA_REPO_MAP` and the three
# fayth labels (needed only for `fayth_get`'s bash subshell, which sources a `.fayth` file
# that references them by parameter expansion) are resolved in-process via
# `spira_config::resolve::resolve_for_process` (see `load_resolved`/`fayth_get` there).
. "$(dirname "$0")/conf.sh" || exit 75
if ! command -v bead >/dev/null 2>&1; then
    printf 'bead: bead is not on PATH (the launcher sets PATH to a release) — refusing.\n' >&2
    exit 75
fi
exec -a "$0" bead --home "$(dirname "$0")" "$@"
