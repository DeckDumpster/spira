#!/usr/bin/env bash
#
# concierge.sh — the single Remote Control session the operator talks to from their phone.
#
#   concierge.sh start     launch it in tmux for the phone (idempotent; converges if running)
#   concierge.sh here      attach to the running session (start first if needed)
#   concierge.sh attach    attach locally
#   concierge.sh brief     render the system prompt and print its path; change nothing
#     overlay without cd:  claude ... --append-system-prompt-file "$(concierge.sh brief)"
#   concierge.sh status    is it up
#   concierge.sh wake "<text>"  type a prompt into the session; fails if it is not up
#   concierge.sh stop      kill it
#
# WHY ONE SESSION AND NOT SEVERAL
# -------------------------------
# The pattern is Yegge's Seneschal: "The mobile Claude app lets you see your
# /remote-control sessions, and I designated the Seneschal as my single remote control
# session. I talk to the Seneschal on the phone, who in turn talks to everyone else."
#
# One concierge, not a repo-by-repo list, because the phone is a narrow surface and the
# useful thing to reach from it is the session holding cross-repo context — the wiki,
# CLAUDE.md and the statute book. It reaches Spira through beads; it does not become a
# second control plane.
#
# WHY tmux AND NOT systemd
# ------------------------
# Remote Control needs an interactive session with a TTY. Every aeon on this box already
# runs this way, and tmux means the operator can attach to the same session locally and see
# exactly what the phone sees.
#
# WHY IT RUNS FROM THE HOME CHECKOUT
# ----------------------------------
# The cwd decides which CLAUDE.md, which hooks and which project memory it loads. From
# there it gets the operator's own conventions, their session-start list and their guards.
#
# THE COCKPIT SESSION PANE
# ------------------------
# The cockpit's session pane (top-left) shows the concierge via Option A: a pane running
# `concierge.sh here`, which does `start` then `exec tmux -L concierge attach`. The inner
# (concierge) tmux sits on its own socket; the outer (cockpit) tmux is the layout server.
# Both use the same Ctrl-b prefix — Ctrl-b Ctrl-b sends prefix to the outer session when
# the operator is interacting with the inner. This is standard nested-tmux and the pane
# is fully usable. The concierge keeping its own socket preserves the Remote Control name
# (`--remote-control concierge`) and the kill/restart lifecycle independently of the layout.
#
# WHY IT IS COMPOSED FROM A FAYTH
# -------------------------------
# It was not, and that was the defect. An aeon is summoned with a brief, a statute book and a
# model resolved from a file somebody maintains; this session got a working directory and the
# `claude` binary. The operator, 2026-09-12: "when we spin up aeons, they come with a system
# prompt and a set of overlays. you don't. i think that's the defect here."
#
# It was not a difference of degree. `render_memories` — the function that renders the `law-`
# memories in full — is called from `aeon.sh` and from nowhere else, so brain's CLAUDE.md,
# which says the harness "injects a `# Memories in force` section into every agent session at
# summon", described aeons only. The statute that the harness checkout is production had been
# in force for weeks and had never been delivered to the session that kept violating it.
#
# So `chamber/concierge.fayth` and `chamber/concierge.md` now define this session exactly as
# `builder.fayth` defines a Guardian, and what follows renders them. The one property that
# makes the concierge different is declared in the fayth rather than arranged: FAYTH_SUMMON is
# `operator`, and `spira_task_fayths` / `spira_lane_fayths` both refuse to hand it to the
# sentinel. Nothing but a human starts this.
set -uo pipefail

# lib.sh AND NOT conf.sh ALONE. conf.sh resolves configuration; the helpers this file needs —
# `fayth_get` to read the persona and `render_memories` to render the statute book — live in
# lib.sh, which sources conf.sh itself. Sourcing only conf.sh left both as "command not
# found", and the refusal below caught it on the first run, which is what the refusal is for.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/spira" && pwd -P)/lib.sh"
HARNESS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SESSION="${CONCIERGE_SESSION:-concierge}"
SOCKET="${CONCIERGE_SOCKET:-concierge}"
BRAIN="${SPIRA_WIKI:-$SPIRA_REPO}"
FAYTH="${CONCIERGE_FAYTH:-concierge}"
spira_require claude tmux || exit 1

TM="tmux -L $SOCKET"

# compose_brief -> path of the rendered system prompt, or non-zero with a reason on stderr.
#
# The same two-part shape aeon.sh uses: the persona's markdown with `{{...}}` placeholders
# substituted, then the statute book appended whole.
#
# THE LAUNCHERS PASS THE CONTENT VIA `--append-system-prompt`, not `--append-system-prompt-file`.
# `--append-system-prompt-file` was not honoured by earlier versions; the launchers keep the
# content form so they are not coupled to which version fixed it. For manual use, see usage.
#
# The statute book is tens of kilobytes. For `here`, the brief passes as "$(cat "$BRIEF")":
# bash expands it before exec, so the content arrives as a single argument without a shell
# layer in between. For `start`, the command goes through `tmux new-session` as a STRING —
# two rounds of shell quoting would corrupt a statute containing a quote or a backtick.
# Instead, `start` writes a launcher script using `printf '%q'` to embed the content, and
# tmux runs the script.
#
# A MISSING BRIEF IS A REFUSAL, NOT A DEGRADED START. A concierge launched without its brief
# is the exact session this file exists to stop shipping: it looks identical to a working one
# from outside, and the way anybody finds out is the next violated statute.
compose_brief() {
    local md="$SPIRA_HOME/chamber/$FAYTH.md" out statutes n
    local bead_tool="$SPIRA_HOME/bead.sh"
    local wiki_clause=""
    [ -n "${SPIRA_WIKI:-}" ] && wiki_clause=" It is the brain wiki — read its \`CLAUDE.md\` first; it governs over anything here that disagrees. Brain carries no copy of the harness."
    [ -f "$md" ] || { echo "concierge: no brief at $md" >&2; return 1; }

    # `law-` unless the fayth says otherwise — the same key builder and ops declare, read the
    # same way, so the concierge cannot end up reading a different book from the one its
    # persona file asks for.
    # THE CORE SET COMES FROM THE PERSONA, not from the box. An operator session and a builder
    # need different statutes in full text; the rest arrive as an index either way. See
    # FAYTH_STATUTE_CORE in the fayth for which ones and why.
    statutes="$(render_memories \
        "$(fayth_get "$FAYTH" FAYTH_MEMORY_PREFIXES law-)" "" \
        "$(fayth_get "$FAYTH" FAYTH_STATUTE_CORE "")")" || statutes=""
    if [ -z "$statutes" ]; then
        echo "concierge: the statute book rendered empty — refusing to start without it" >&2
        echo "  check: $HARNESS/rule.sh list" >&2
        return 1
    fi
    n="$(grep -c '^## ' <<<"$statutes" 2>/dev/null || printf 0)"

    out="$SPIRA_RUN/concierge-brief.md"
    mkdir -p "$SPIRA_RUN" 2>/dev/null
    {
        sed -e "s|{{CWD}}|$BRAIN|g" \
            -e "s|{{SPIRA_HOME}}|$SPIRA_HOME|g" \
            -e "s|{{COCKPIT}}|$SPIRA_COCKPIT|g" \
            -e "s|{{ASK}}|$SPIRA_HOME/mail.sh|g" \
            -e "s|{{RULE}}|$HARNESS/rule.sh|g" \
            -e "s|{{DB}}|$SPIRA_DB|g" \
            -e "s|{{STATUTE_COUNT}}|$n|g" \
            -e "s|{{DEADLINE}}||g" \
            -e "s|{{BEAD}}|$bead_tool|g" \
            -e "s|{{WIKI_CLAUSE}}|$wiki_clause|g" \
            "$md"
        printf '\n# Memories in force\n\n%s\n' "$statutes"
    } > "$out" || return 1

    # A DECLARED CORE SET THAT RENDERS NOTHING IN FULL IS A TYPO, NOT A CONFIGURATION.
    # render_memories matches core slugs EXACTLY and silently demotes anything it does not
    # recognise to the index tier, so one mistyped or retired slug costs that statute its full
    # text and says nothing. All of them mistyped costs the whole point of the persona, and the
    # brief still looks complete: 24KB, every placeholder filled, the law apparently present.
    if [ -n "$(fayth_get "$FAYTH" FAYTH_STATUTE_CORE "")" ] && ! grep -q '^## law-' "$out"; then
        echo "concierge: FAYTH_STATUTE_CORE is declared but no statute rendered in full" >&2
        echo "  every slug in it was demoted to the index — check them against:" >&2
        echo "  $HARNESS/rule.sh list" >&2
        return 1
    fi

    # THE PLACEHOLDER CHECK IS THE POINT OF DOING THIS IN A FUNCTION. An unsubstituted
    # `{{ASK}}` is not a cosmetic flaw — it is a command line the session will try to run,
    # and the failure arrives hours later as "the concierge does not escalate anything".
    if grep -q '{{[A-Z_]*}}' "$out"; then
        echo "concierge: brief still holds unsubstituted placeholders — refusing to start:" >&2
        grep -o '{{[A-Z_]*}}' "$out" | sort -u | sed 's/^/  /' >&2
        return 1
    fi
    printf '%s' "$out"
}

# brief_summary <path> [model] -> the one line both launchers print about what they composed.
#
# ONE FUNCTION BECAUSE TWO COPIES DISAGREED IMMEDIATELY. `here` grew its own inline `grep -c`
# and the quoting came out wrong, so it reported "0 statutes in full" about a brief holding
# twenty — a number that would have been believed, because a launcher reporting on itself is
# exactly the reading nobody goes behind.
brief_summary() {
    printf 'concierge: %s statutes in full, %s bytes%s\n' \
        "$(grep -c '^## law-' "$1" 2>/dev/null || printf '?')" \
        "$(wc -c < "$1" 2>/dev/null || printf '?')" \
        "${2:+, $2}"
}

# concierge_resume_id -> the recorded session id when BRAIN matches, else empty.
# Warns to stderr when a file exists but the cwd changed (orphaned conversation).
concierge_resume_id() {
    local sf="$SPIRA_RUN/concierge-session" stored_id stored_cwd
    [ -f "$sf" ] || return 0
    { IFS= read -r stored_id && IFS= read -r stored_cwd; } < "$sf" || return 0
    [ -n "$stored_id" ] || return 0
    if [ "$stored_cwd" != "$BRAIN" ]; then
        printf 'concierge: last session was in %s (now %s) — starting empty\n' \
            "$stored_cwd" "$BRAIN" >&2
        return 0
    fi
    printf '%s' "$stored_id"
}

# concierge_live_pid <session-id> -> pid holding that session id in --resume, or non-zero.
# Scans /proc/*/cmdline rather than pgrep -f: pgrep matches against a rendered string and
# can collide with process names; reading cmdline directly is exact and cannot be fooled by
# argv[0] manipulation (law-a-pattern-match-is-not-an-identity-check).
concierge_live_pid() {
    local sid="$1" f pid
    for f in /proc/*/cmdline; do
        pid="${f%/cmdline}"; pid="${pid##*/}"
        case "$pid" in *[!0-9]*) continue ;; esac
        [ "$pid" = "$$" ] && continue
        tr '\0' '\n' < "$f" 2>/dev/null | grep -qF -- "$sid" || continue
        printf '%s' "$pid"
        return 0
    done
    return 1
}

case "${1:-status}" in

start)
    if $TM has-session -t "$SESSION" 2>/dev/null; then
        echo "concierge: already running (tmux -L $SOCKET attach -t $SESSION)"
        exit 0
    fi
    # CONVERGENCE CHECK BEFORE BRIEF. A live process holding the recorded session id means
    # the concierge is already up (possibly on a different socket). Starting a second claude
    # on the same session causes a 4090 eviction race. Exit 0 — it is running, no second
    # is needed. Find by scanning /proc/*/cmdline, never pgrep -f.
    RESUME_ID="$(concierge_resume_id)"
    if [ -n "$RESUME_ID" ]; then
        _live="$(concierge_live_pid "$RESUME_ID")" && {
            printf 'concierge: session %s is already held by pid %s\n' "$RESUME_ID" "$_live"
            printf '  attach:  tmux -L %s attach -t %s\n' "$SOCKET" "$SESSION"
            exit 0
        }
    fi
    command -v claude >/dev/null || { echo "concierge: claude not on PATH" >&2; exit 1; }
    # THE SERVER BELOW INHERITS THIS PROCESS'S ENVIRONMENT AND KEEPS IT FOR LIFE. Started from
    # inside another Claude session — which is exactly how it gets started — it would hand that
    # session's identity to the concierge, and a client that thinks it is a child session does
    # not write a transcript. Clear them before the fork; scrub an existing server for the case
    # where this socket is already up. See cockpit/tmux-env.sh.
    _tmuxenv="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/cockpit/tmux-env.sh"
    bash "$_tmuxenv" scrub -L "$SOCKET" 2>/dev/null
    unset $(bash "$_tmuxenv" names) 2>/dev/null || true
    BRIEF="$(compose_brief)" || exit 1
    MODEL="$(fayth_get "$FAYTH" FAYTH_MODEL "")"
    brief_summary "$BRIEF" "$MODEL"

    # NO --allowedTools. For an interactive session under bypassed permissions that flag can
    # only SUBTRACT, and the persona's remit is unbounded — see concierge.fayth, where the
    # absence is the declaration. Every other persona names its tools because aeon.sh passes
    # them as an allow list that keeps a worker inside its job.
    #
    # LAUNCHER INSTEAD OF INLINE. The brief's text cannot survive two rounds of shell quoting
    # inside tmux's command string (see compose_brief above). printf '%q' writes it as a
    # bash-safe literal in the launcher, so the content reaches claude intact regardless of
    # what characters the statute book contains.
    #
    # SPIRA_CONCIERGE=1 IS EXPORTED SO THE SESSION HOOK CAN IDENTIFY THIS SESSION. The global
    # session hook fires for every Claude session on the box; only this one should update the
    # recorded session id. The hook reads the id from the hook payload rather than guessing
    # from the transcript directory, so a clear or compact inside the session updates the file.
    LAUNCHER="$SPIRA_RUN/concierge-launch.sh"
    {
        printf '#!/usr/bin/env bash\n'
        printf 'export SPIRA_CONCIERGE=1\n'
        printf 'exec claude --remote-control %q --dangerously-skip-permissions ' "$SESSION"
        [ -n "$MODEL" ] && printf -- '--model %q ' "$MODEL"
        [ -n "$RESUME_ID" ] && printf -- '--resume %q ' "$RESUME_ID"
        printf -- '--append-system-prompt %q\n' "$(cat "$BRIEF")"
    } > "$LAUNCHER"
    chmod +x "$LAUNCHER"
    # concierge.service is Type=oneshot/KillMode=control-group: when start exits, systemd
    # kills the whole cgroup — the tmux server with it. A transient unit gives the server
    # its own cgroup that outlives the oneshot. Same trap and fix as lib.sh (aeon summon).
    # --remain-after-exit keeps the unit active after tmux new-session daemonizes and exits,
    # preventing the KillMode cleanup until the server itself stops.
    # PATH and HOME are the minimum the launcher needs: PATH to find claude, HOME for config.
    systemd-run --user --collect --quiet --remain-after-exit \
        --setenv=PATH="$PATH" --setenv=HOME="$HOME" -- \
        tmux -L "$SOCKET" new-session -d -s "$SESSION" -c "$BRAIN" "$LAUNCHER"
    sleep 3
    if $TM has-session -t "$SESSION" 2>/dev/null; then
        echo "concierge: started as Remote Control session '$SESSION'"
        echo "  attach locally:  tmux -L $SOCKET attach -t $SESSION"
        echo "  on the phone:    Claude app -> Remote Control -> $SESSION"
    else
        echo "concierge: failed to stay up — run it in the foreground to see why:" >&2
        echo "  cd $BRAIN && claude --remote-control $SESSION" >&2
        exit 1
    fi
    ;;

attach)  exec $TM attach -t "$SESSION" ;;

wake)
    [ -n "${2:-}" ] || { echo "usage: concierge.sh wake \"<text>\"" >&2; exit 2; }
    $TM has-session -t "$SESSION" 2>/dev/null || { echo "concierge: not running" >&2; exit 1; }
    # -l sends the text literally; Enter is a separate key so a prompt mid-turn is queued, not split.
    $TM send-keys -t "$SESSION" -l -- "$2" && $TM send-keys -t "$SESSION" Enter
    ;;

# CONVERGENCE ENTRY POINT. `here` is how the operator and the cockpit session pane join the
# one concierge process. It starts the concierge if it is not running, then attaches. A
# second standalone foreground claude is the defect, not a mode.
here)
    if $TM has-session -t "$SESSION" 2>/dev/null; then
        exec $TM attach -t "$SESSION"
    fi
    bash "$0" start || exit 1
    exec $TM attach -t "$SESSION"
    ;;

# RENDER IT AND PRINT THE PATH, CHANGING NOTHING. The brief is the part of this session that
# is easy to get wrong and impossible to see from outside once it has started, so it has a
# seam of its own: `brief` is what the suite drives and what the operator reads before
# deciding the persona says what he meant.
brief)
    [ $# -gt 1 ] && {
        printf 'concierge: brief takes no arguments\n' >&2
        printf '  to use the overlay without cd: claude ... --append-system-prompt-file "$(concierge.sh brief)"\n' >&2
        exit 2
    }
    b="$(compose_brief)" || exit 1; echo "$b"; ;;

status)
    if $TM has-session -t "$SESSION" 2>/dev/null; then
        echo "concierge: running"
        $TM list-panes -t "$SESSION" -F '  pane #{pane_id} pid=#{pane_pid} #{pane_current_command}'
        echo "  last 15 lines:"
        $TM capture-pane -p -t "$SESSION" 2>/dev/null | grep -v '^$' | tail -15 | sed 's/^/    /'
    else
        echo "concierge: not running  (start with: $0 start)"
        exit 1
    fi
    ;;

stop)    $TM kill-session -t "$SESSION" 2>/dev/null && echo "concierge: stopped" ;;

_resume-id)  concierge_resume_id ;;   # internal: used by test suite
_live-pid)   concierge_live_pid "${2:-}" ;;  # internal: used by test suite

*)       sed -n '3,9p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
