#!/usr/bin/env bash
#
# rule.sh — enact, amend, or retire a statute in one command.
#
#   rule.sh enact <slug> "<statute text>" [--dry-run]   write it, then synthesise it into the wiki
#   rule.sh retire <slug>                    remove it
#   rule.sh list                             what is in force
#   rule.sh show <slug>                      one statute's full text
#
# `<slug>` is written without the `law-` prefix; it is added for you.
#
# WHY THIS IS A COMMAND AND NOT A CHECKLIST
# -----------------------------------------
# Enacting a statute is two steps — write it to the statute book, regenerate the wiki
# page that is its only git-backed copy — and a rule that depends on remembering a second
# step is a resolution, not a mechanism. The standing lesson here is that when a rule is
# discovered the deliverable is a guard or a command, never a note.
#
# ONE DATABASE. Statutes live in the Spira beads database and nowhere else; that is the
# store every aeon reads its memories from at summon. There is no propagation step,
# because there is nothing to propagate to. `SPIRA_DB` overrides the path, as it does for
# every tool in this repo.
#
# WHEN TO REACH FOR IT
# --------------------
# When the operator answers an escalation, that answer is a verdict. Ask whether it
# generalises; if it does, enact it in the same session with the case that produced it
# as the trailing citation, so the rule carries its own history. The escalation queue should
# be producing law, not just draining — a class of question that keeps coming back is a
# missing statute.
#
# Statutes are read by every agent on every session, so write them to be read a thousand
# times: one paragraph, imperative, ~70 words, the scar as a single clause rather than a
# narrative of how we got here.
#
# THIS FILE IS NOW A SHIM (sp-g9mhe, rewrite wave 5f): the logic above lives in the Rust
# `rule` binary (rule/DESIGN.md). This stays the one entry point every caller names —
# concierge.sh, every chamber brief, the suites — the same shim pattern `gate/DESIGN.md`
# established for `gate.sh`: source `conf.sh` for its side effects (env, PATH), exactly as
# the original `rule.sh` did, then hand over to the binary by bare name.
#
# `exec -a "$0"` keeps this script's path in the process's argv.
#
# RE-EXPORT WHAT conf.sh DERIVES BUT DOES NOT EXPORT (bead.sh's shim carries the full
# explanation). SPIRA_MEMORIES_CACHE's derived default (`$SPIRA_RUN/memories-cache.json`)
# is a plain shell variable in conf.sh, harmless to the same-process bash `rule.sh` this
# replaces but invisible across `exec` into the binary unless re-exported here.
SPIRA_HOME_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/spira" && pwd -P)"
. "$SPIRA_HOME_DIR/conf.sh"
export SPIRA_DB SPIRA_MEMORIES_CACHE SPIRA_WIKI SPIRA_WIKI_HOOK
if ! command -v rule >/dev/null 2>&1; then
    printf 'rule: rule is not on PATH (the launcher sets PATH to a release) — refusing.\n' >&2
    exit 75
fi
exec -a "$0" rule --home "$SPIRA_HOME_DIR" "$@"
