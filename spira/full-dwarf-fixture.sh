#!/usr/bin/env bash
# full-dwarf-fixture.sh — the one full-dwarf skeleton (bare origin, clone, runes, claude
# shim, bd store) that test-dwarf-decision-blocked.sh, test-dwarf-operator-wait.sh,
# test-requeue.sh, test-dwarf-ledger.sh, test-dwarf-presession-death.sh,
# test-dwarf-yield-headless.sh and test-dwarf-exit.sh each built for themselves — 20 suites
# in all did this (docs/test-plan/dwarf-execution.md D15). fa_setup builds it once; fa_reset
# resets only the bd store between rows, which is what let those seven pay for a fresh
# `git init --bare` and clone on every one of their combined ~20 dwarf runs.
#
# Sourced, never executed, after testlib.sh (for the ok/bad helpers a caller wants) and
# after testdb.sh (fa_setup calls testdb_require/testdb_up/testdb_reset itself, so a
# caller sources testdb.sh first only to pick a mode via SPIRA_TESTDB_MODE). Assumes the
# caller has already set HERE to this directory, the same convention testdb.sh itself uses.
#
#   HERE="$(cd "$(dirname "$0")" && pwd -P)"
#   . "$HERE/testdb.sh"
#   . "$HERE/full-dwarf-fixture.sh"
#   fa_setup mysuite
#   trap 'fa_teardown' EXIT INT TERM
#   fa_seed sp-x-1
#   cat > "$FA_BIN/claude" <<'SHIM' ... SHIM
#   fa_run_dwarf; is "..." "..." "$(fa_status sp-x-1)"
#
# covers: spira/lib.sh mail/src/*
set -uo pipefail

fa_setup() {   # fa_setup <tag> — build the fixture once
    local tag="${1:?fa_setup: tag required}"
    testdb_require "$tag"
    FA_TMP="$(mktemp -d)"
    # A caller that requested server mode and got none (no server-mode fixture available) decides for itself whether that is a skip or a failure — return, not exit.
    testdb_up "$tag" || { rm -rf "$FA_TMP"; return 1; }
    export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

    FA_ORIGIN="$FA_TMP/origin.git"; git init -q --bare -b main "$FA_ORIGIN"
    FA_REPO="$FA_TMP/repo"; timeout 5 git clone -q "$FA_ORIGIN" "$FA_REPO" 2>/dev/null
    git -C "$FA_REPO" config user.email t@t; git -C "$FA_REPO" config user.name t
    printf 'seed\n' > "$FA_REPO/f"
    git -C "$FA_REPO" add f; git -C "$FA_REPO" commit -qm seed
    timeout 5 git -C "$FA_REPO" push -q origin main 2>/dev/null

    FA_HOME="$FA_TMP/home"; mkdir -p "$FA_HOME/runes"
    printf '. "%s/lib.sh"\n' "$HERE" > "$FA_HOME/lib.sh"   # the dwarf binary sources <home>/lib.sh; the real one, as dwarf.sh did
    # conf.d IS COPIED IN (matching test-dwarf-sweep.sh, test-dwarf-world-stop.sh, ...): dwarf's
    # own in-process config registry derives conf.d from THIS --home and now REFUSES to
    # start if it is missing (sp-1cdgq) — every suite built on this shared fixture needs it.
    cp -r "$HERE/conf.d" "$FA_HOME/"
    cp -r "$HERE/conf.toml.d" "$FA_HOME/"
    FA_RUN="$FA_TMP/run"; mkdir -p "$FA_RUN"
    FA_REPO_MAP="$FA_TMP/repo-map"
    export SPIRA_HOME="$FA_HOME"
    # ONE SOURCE OF CONFIG (per Ryan 2026-10-05): these are registered keys (spira/conf.d) —
    # no process reads them from the environment any more, and SPIRA_RUNES_DIR no longer
    # derives from SPIRA_HOME (the complete fixture declares its own path), so this fixture
    # must declare all of them through tl_config, not export. Plain (non-exported) shell
    # copies of SPIRA_RUN/SPIRA_REPO_MAP are kept too — this file's own code still reads
    # them directly (fa_run_dwarf's "$SPIRA_RUN/worktree", the repo-map write below).
    SPIRA_RUN="$FA_RUN"; SPIRA_REPO_MAP="$FA_REPO_MAP"; SPIRA_MAIL="$FA_TMP/mail"
    # SPIRA_ASK_LABEL: the complete fixture declares "needs-ryan"; every caller in this
    # fixture and its suite (the claude shims' own `${SPIRA_ASK_LABEL:-needs-operator}`
    # fallback — never reached anyway, since the model session's restricted env carries no
    # SPIRA_ASK_LABEL at all — and builder.rune's RUNE_EXCLUDE_LABELS below) was written
    # against "needs-operator". dwarf's own ask_label() now resolves the registered key from
    # the one source with no code-level default (per Ryan 2026-10-05), so it would otherwise
    # disagree with every literal here and open_ask_blocker would never match the shim's
    # decision bead. Declare the suite's own value so both sides agree.
    # literal-ok: the fixture declares the ask label its shims and beads agree on
    SPIRA_ASK_LABEL="needs-operator"
    tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_REPO_MAP="$SPIRA_REPO_MAP" SPIRA_RUNES_DIR="$FA_HOME/runes" \
        SPIRA_MAIL="$SPIRA_MAIL" SPIRA_MAIL_KINDS="$HERE/mail/kinds" SPIRA_ASK_LABEL="$SPIRA_ASK_LABEL"
    printf 'fixture | %s | push | origin/main | |\n' "$FA_REPO" > "$SPIRA_REPO_MAP"
    cat > "$FA_HOME/runes/builder.rune" <<RUNE
RUNE_NAME=builder
RUNE_WORKING_EXITS="SUBMITTED ask park hold"
RUNE_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
RUNE_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=600
RUNE
    printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$FA_HOME/runes/builder.md"

    FA_BIN="$FA_TMP/bin"; mkdir -p "$FA_BIN"
    # sp-mve9i: the dwarf reads its bead's state from the lifecycle row, never bd status; a
    # shim's bd close is told to it in lifecycle terms (testlib.sh lc_dwarf_mirror).
    lc_dwarf_mirror "$FA_TMP/lcm"; export PATH="$FA_TMP/lcm:$PATH"
    export TMP="$FA_TMP"
    # The model session is restricted (sp-v62vn); the shim is a fixture — testlib dwarf_fixture_agent.
    # It only `export`s SPIRA_AGENT (testlib.sh, not edited here) — a registered key, so the
    # dwarf binary also needs it declared through tl_config, read back from what it set.
    dwarf_fixture_agent "$FA_BIN/claude"
    tl_config SPIRA_AGENT="$SPIRA_AGENT"
    command -v dwarf >/dev/null 2>&1 \
        || { printf 'full-dwarf-fixture: dwarf is not on PATH — refusing to run\n' >&2; exit 1; }
}

# fa_teardown — call from the caller's own `trap ... EXIT INT TERM`; not registered here
# so a caller that also needs its own cleanup keeps one trap, not two competing ones.
fa_teardown() { testdb_drop; rm -rf "$FA_TMP"; }

fa_reset() { testdb_reset; }

fa_seed() {   # fa_seed <id> [updated_at]
    local id="$1" updated="${2:-2026-09-04T00:00:00Z}"
    local lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"%s"}\n' \
        "$id" "$lbl" "$updated" | testdb_seed
}

fa_run_dwarf() {   # fa_run_dwarf [rune] -> prints the dwarf binary's rc; output captured to $FA_TMP/out
    rm -rf "$SPIRA_RUN/worktree"
    dwarf --home "$FA_HOME" "${1:-builder}" > "$FA_TMP/out" 2>&1
    printf '%s' "$?"
}

fa_out() { cat "$FA_TMP/out" 2>/dev/null; }

fa_field() {   # fa_field <id> <json-field>
    BD_IGNORE_SCHEMA_SKEW=1 timeout 5 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get(sys.argv[1], "") or "")' "$2" 2>/dev/null
}

fa_status() { fa_field "$1" status; }
fa_notes()  { fa_field "$1" notes; }

fa_labels() { timeout 5 bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }

fa_ledger_lines() {   # fa_ledger_lines <id> -> every "done builder <id>" line, in order
    grep " done builder $1 " "$SPIRA_RUN/dwarf-ledger.log" 2>/dev/null
}

fa_ledger_line() { fa_ledger_lines "$1" | tail -1; }   # the most recent one
