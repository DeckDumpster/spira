#!/usr/bin/env bash
#
# bead.sh — file a bead through the contract; never call bd create directly.
#
#   bead.sh file "<title>" --for <persona> --repo <name> [--priority N] [--body-file F] [--json] [--parent <id>]
#   bead.sh file "<title>" --kind <kind> [--repo <name>] [--priority N] [--body-file F] [--json] [--parent <id>]
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
# RE-EXPORT WHAT conf.sh DERIVES BUT DOES NOT EXPORT. conf.sh's own `export` list (its
# "THE ENVIRONMENT ALREADY OWNS" section) leaves several of the plain-shell-variable
# derivations it computes unexported — harmless for the bash bead.sh, which ran in the SAME
# process as conf.sh and so saw them regardless of export, but fatal to a binary this file
# `exec`s into: only the environment table crosses that boundary, and a plain variable does
# not. SPIRA_REPO_MAP is the one every fixture's repo-map depends on
# (`_spira_repo_map_candidate`'s derivation); the three lane labels are the same shape.
. "$(dirname "$0")/conf.sh" || exit 75
export SPIRA_HOME SPIRA_REPO_MAP SPIRA_GROOMER_LABEL SPIRA_MAECHEN_LABEL SPIRA_CZAR_LABEL
if ! command -v bead >/dev/null 2>&1; then
    printf 'bead: bead is not on PATH (the launcher sets PATH to a release) — refusing.\n' >&2
    exit 75
fi
exec -a "$0" bead --home "$(dirname "$0")" "$@"
