#!/usr/bin/env bash
#
# hermetic.sh — refuse a test suite that reaches the real box.
#
#   hermetic.sh                  scan every spira/test-*.sh; exit 1 naming each file and line
#   hermetic.sh --scan <file>    scan one file; print `<line>:<command>:<text>`, one per hit
#   hermetic.sh --commands       print the two command lists and exit
#
# WHAT THIS IS FOR. A suite must judge the code under test, and nothing else. When it reads
# the state of the machine it happens to be running on, its verdict is a fact about that
# machine: it is green for weeks, then turns red the day the box changes rather than the day
# the code does. That failure is worse than an ordinary red, because the branch it refuses is
# innocent and there is nothing in the output pointing anywhere but at the branch. Both
# failures the previous gate produced on the day it was deleted were this, and both were
# found by a human reading the output rather than by anything mechanical.
#
# The rule is already law (law-gates-run-in-a-clean-environment). It has been re-violated,
# and re-violation is what promotes a rule from something written down to something that
# refuses — so this is that rule as a program.
#
# WHAT IT LOOKS FOR. Two lists, because the two have different remedies.
#
#   UNROUTABLE — systemctl, systemd-run, journalctl, loginctl, gh, gt, crontab. There is no
#   argument that makes these local; they answer for the machine or for a network account.
#   A suite reaches them through a SEAM: the program under test reads `${SPIRA_SYSTEMCTL:-…}`,
#   `${SPIRA_LAUNCH:-…}` or `${SPIRA_GH:-…}`, and the suite points that at a shim it wrote in
#   its own scratch directory. Naming one of these as a command is therefore always a hit.
#
#   DIRECTABLE — bd, git, dolt. Each is hermetic when pointed somewhere disposable and reads
#   the operator's own state when it is not: `git` with no `-C` takes the working directory,
#   and `bd` with no `-C` resolves a database from it. So these are a hit only when the line
#   names nothing the suite created — see below.
#
# HOW A SCRATCH PATH IS RECOGNISED, without evaluating anything. A variable is scratch if it
# was assigned from `mktemp`, or from a value mentioning a variable already known to be
# scratch; the closure runs to a fixpoint, so `TMP=$(mktemp -d)` makes `REPO="$TMP/repo"`
# scratch and `git -C "$REPO"` clean. `SPIRA_DB` counts as scratch in a suite that calls
# `testdb_up`, which is what puts it on a throwaway database.
#
# WHAT IT DELIBERATELY DOES NOT SEE. Comment lines and heredoc bodies are skipped — a suite
# plants shims by writing them, and the shim's own text is not this suite's behaviour — and a
# command reached through a variable is invisible by construction. That is the right trade:
# a fence is a polite refusal aimed at the honest mistake, and every violation so far was
# somebody calling the obvious command in the obvious way. It is not a sandbox and cannot be.
#
# ITS ESCAPE, DOCUMENTED HERE BECAUSE A SUITE THAT MUST REACH THE BOX SHOULD SAY SO ALOUD.
# Put `# hermetic-ok: <why>` on the offending line or on the line directly above it, and this
# fence steps aside. It is deliberately a comment rather than a config file: the reason belongs
# beside the call, where the next reader of the failure will be, and a suite claiming the
# exemption is then greppable. Rewriting a call to hide from the matcher is the one response
# that is worse than the violation, because the next reader has neither the call nor the claim.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

UNROUTABLE="systemctl systemd-run journalctl loginctl gh gt crontab"
DIRECTABLE="bd git dolt"

# The awk is the whole matcher. It reads the file twice over an in-memory copy: once to learn
# which lines are code and which variables are scratch, once to judge. `-v` rather than a
# literal list, so the two lists above are the only place they are written.
scan() {                 # scan <file> -> `<line>:<command>:<text>` per hit; exit 0 either way
    awk -v UNR="$UNROUTABLE" -v DIR="$DIRECTABLE" '
    BEGIN {
        n = split(UNR, a, " "); for (i = 1; i <= n; i++) U[a[i]] = 1
        n = split(DIR, a, " "); for (i = 1; i <= n; i++) D[a[i]] = 1
        # Words that stand IN FRONT of a command without being one. Skipping them is what
        # makes `env -i PATH=… systemctl` and `timeout 30 bd …` visible; over-skipping only
        # ever looks further right, so it costs a miss and never a false accusation.
        n = split("env timeout sudo nohup xargs command exec time then else do elif if while until ! { ( ", a, " ")
        for (i = 1; i <= n; i++) W[a[i]] = 1
    }
    # decomment(s) -> s with a trailing comment removed. Quote state is tracked rather than
    # guessed, because a `#` inside a string is not a comment and stripping at the first one
    # would blind the matcher to the rest of a real command line.
    # decomment(s) -> s with a trailing comment removed. Quote state is tracked rather than
    # guessed, because a `#` inside a string is not a comment and stripping at the first one
    # would blind the matcher to the rest of a real command line. \047 is a single quote,
    # which cannot be written directly inside a single-quoted awk program.
    function decomment(s,   i, c, q, out) {
        q = ""
        for (i = 1; i <= length(s); i++) {
            c = substr(s, i, 1)
            # A backslash escapes the next character everywhere except inside single quotes,
            # where bash gives it no meaning at all.
            if (c == "\\" && q != "\047") { out = out c substr(s, i+1, 1); i++; continue }
            if (q == "") {
                if (c == "\"" || c == "\047") q = c
                else if (c == "#" && (i == 1 || substr(s, i-1, 1) ~ /[ \t]/)) return out
            } else if (c == q) q = ""
            out = out c
        }
        return out
    }
    { L[NR] = $0 }
    END {
        # ---- pass 1: which lines are code, and which variables point at scratch ----
        hd = ""
        for (i = 1; i <= NR; i++) {
            line = L[i]
            if (hd != "") {                      # inside a heredoc body
                SKIP[i] = 1
                t = line; sub(/^[ \t]+/, "", t); sub(/[ \t]+$/, "", t)
                if (t == hd) hd = ""
                continue
            }
            if (line ~ /^[ \t]*#/ || line ~ /^[ \t]*$/) { SKIP[i] = 1 }
            # A heredoc OPENS on a code line, so the opener itself is still judged. `<<<` is
            # a herestring and opens nothing; excluding it is why the third `<` is tested.
            if (line ~ /<<-?[ \t]*[\047"]?[A-Za-z_][A-Za-z0-9_]*/ && line !~ /<<</) {
                d = line
                sub(/^.*<<-?[ \t]*/, "", d)
                sub(/^[\047"]/, "", d)
                sub(/[^A-Za-z0-9_].*$/, "", d)
                if (d != "") hd = d
            }
            if (SKIP[i]) continue
            if (line ~ /testdb_up/) SCRATCH["SPIRA_DB"] = 1
            # Every `VAR=value` on the line, wherever it sits: bare, after `export`, or after
            # a separator. The value is taken to the next unquoted space, which is enough to
            # see which variables it mentions.
            rest = line
            while (match(rest, /(^|[ \t;&|(){}]|^export |[ \t]export )[A-Za-z_][A-Za-z0-9_]*=/)) {
                seg = substr(rest, RSTART, RLENGTH)
                rest = substr(rest, RSTART + RLENGTH)
                v = seg; sub(/=$/, "", v); sub(/^.*[^A-Za-z0-9_]/, "", v)
                val = rest; sub(/[ \t;].*$/, "", val)
                na++; AV[na] = v; AR[na] = val
                if (val ~ /mktemp/ || rest ~ /^"?\$\(mktemp/) SCRATCH[v] = 1
            }
        }
        # Fixpoint: a value mentioning a scratch variable makes its own variable scratch.
        # Bounded by the assignment count, because each round must promote at least one.
        for (round = 0; round <= na; round++) {
            moved = 0
            for (i = 1; i <= na; i++) {
                # `in`, never a subscript: referencing SCRATCH[x] would CREATE x as an
                # empty element, and the local_ok loop below iterates the keys — so every
                # assigned variable would silently count as scratch and the fence would
                # excuse every line that mentioned one.
                if (AV[i] in SCRATCH) continue
                for (v in SCRATCH) {
                    if (AR[i] ~ ("\\$\\{?" v "[^A-Za-z0-9_]") || AR[i] ~ ("\\$\\{?" v "$")) {
                        SCRATCH[AV[i]] = 1; moved = 1; break
                    }
                }
            }
            if (!moved) break
        }

        # ---- pass 2: judge each code line ----
        for (i = 1; i <= NR; i++) {
            if (SKIP[i]) continue
            if (L[i] ~ /hermetic-ok:/) continue
            if (i > 1 && L[i-1] ~ /hermetic-ok:/) continue
            # A TRAILING COMMENT IS NOT CODE. Prose after a `#` is full of the very words
            # this hunts, and it sits on the same line as real code, so it cannot be dropped
            # by the whole-line rule that handles comment-only lines.
            line = decomment(L[i])
            if (line ~ /^[ \t]*$/) continue

            # Does the line name anything disposable? Asked of the ORIGINAL text, before the
            # separators below chew it up, and of the whole line rather than the segment: a
            # `cd "$TMP/x" && git …` is honest and a fence that split them would not see it.
            local_ok = 0
            for (v in SCRATCH)
                if (line ~ ("\\$\\{?" v "[^A-Za-z0-9_]") || line ~ ("\\$\\{?" v "$")) { local_ok = 1; break }

            # Segments, so a command that FOLLOWS a separator is seen and a word sitting in
            # the middle of a string is not. `$(` first, or the `(` rule would eat its dollar.
            s = line
            gsub(/\$\(/, "\001", s); gsub(/&&|\|\|/, "\001", s); gsub(/[;|&`]/, "\001", s)
            ns = split(s, S, "\001")
            for (k = 1; k <= ns; k++) {
                nt = split(S[k], T, /[ \t]+/)
                sawwrap = 0
                for (j = 1; j <= nt; j++) {
                    t = T[j]
                    if (t == "") continue
                    if (t ~ /^[A-Za-z_][A-Za-z0-9_]*=/) continue        # inline environment
                    if (W[t]) { sawwrap = 1; continue }                 # a wrapper, not the command
                    if (sawwrap && t ~ /^(-|[0-9]|["\047$])/) continue  # that wrapper.s own arguments
                    if (U[t]) printf "%d:%s:%s\n", i, t, line
                    else if (D[t] && !local_ok) printf "%d:%s:%s\n", i, t, line
                    # UNCHECKED testdb_up. The scanner marks SPIRA_DB as scratch when it
                    # sees testdb_up, but only if the call SUCCEEDS. A return that goes
                    # unchecked leaves SPIRA_DB pointing at whatever the caller had —
                    # production — when testdb_up cannot build a fixture. Here `t` is the
                    # first non-wrapper token in a segment — the actual command word — so
                    # there is no word-boundary problem and no need for a separate scan.
                    # A call is checked if `||` appears on the same line (meaning a
                    # failure alternative exists), or if the call is in a conditional
                    # position: `if/while/until testdb_up …` at the start of the line.
                    else if (t == "testdb_up") {
                        s = line; sub(/^[ \t]*/, "", s)
                        checked = (s ~ /^(if |while |until )/) || (line ~ /testdb_up[^|]*\|\|/)
                        if (!checked) printf "%d:testdb_up:%s\n", i, L[i]
                    }
                    break                                               # the command word settles the segment
                }
            }
        }
    }' "$1"
}

# --------------------------------------------------------------------------
# HOST-REASON FENCE — every suite runs in a container, or says why it can't.
#
# WHAT THIS IS FOR. The container is the default execution context for a suite.
# A suite added with no annotation is expected to run inside testenv.sh; one that
# cannot says why at the top of its file:
#
#   # host-reason: drives the operator's own tmux server, which the container has no equivalent of
#
# The reason must be non-empty — it is the point of the declaration. This fence
# refuses a suite that has neither a container call nor a reason, and counts all
# undeclared suites so migration progress is visible on the sweep.
#
# It is a separate check from the main hermetic scan, exposed as --host-check and
# --count-undeclared, because the main scan is in the landing gate and the migration
# has not completed — adding it to the gate path now would refuse the 200+ suites
# that are not yet containerised. Migration itself is sp-vnxz and descendants.
#
# ITS ESCAPE. Add `# host-reason: <why>` anywhere in the suite file. The reason
# must be non-empty text. Do not leave the marker without a reason; that is refused
# as well, because the reason is what makes the declaration meaningful.
# --------------------------------------------------------------------------

# host_reason_of <file> -> prints the reason text from `# host-reason:`.
# Returns 0 and prints the reason (possibly empty if no text follows the colon)
# when the annotation is present; returns 1 if it is absent.
host_reason_of() {
    local line r
    while IFS= read -r line; do
        case "$line" in '# host-reason:'*)
            r="${line#'# host-reason:'}"
            # Strip leading whitespace between the colon and the reason text.
            while [ "${r:0:1}" = " " ] || [ "${r:0:1}" = "	" ]; do r="${r:1}"; done
            # Strip trailing whitespace so `# host-reason:   ` is treated as empty.
            while [ -n "$r" ] && { [ "${r: -1}" = " " ] || [ "${r: -1}" = "	" ]; }; do
                r="${r%?}"
            done
            printf '%s' "$r"
            return 0
        ;; esac
    done < "$1"
    return 1
}

# has_container <file> -> 0 if any non-comment code line calls testenv.sh with up or exec.
# `up` and `exec` are the subcommands that start or send work into a container; the others
# (scratch, shell, tag, probe) run on the host. Skips comment-only and blank lines.
has_container() {
    local line stripped
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        # Strip leading whitespace, then skip lines whose first non-blank character is #.
        stripped="$line"
        while [ "${stripped:0:1}" = " " ] || [ "${stripped:0:1}" = "	" ]; do
            stripped="${stripped:1}"
        done
        case "$stripped" in '#'*|'') continue ;; esac
        # Non-comment code line: match testenv.sh (or a variable that contains it) being
        # called with `up` or `exec`. The `up` subcommand starts the container; `exec` sends
        # a command into it. Both are the signals that assertions run inside a container.
        case "$line" in
            *testenv*" up "*|*testenv*" up"|\
            *testenv*" exec "*|*testenv*" exec"|\
            *TESTENV*" up "*|*TESTENV*" up"|\
            *TESTENV*" exec "*|*TESTENV*" exec") return 0 ;;
        esac
    done < "$1"
    return 1
}

# host_check_one <file> <relpath> -> 0 if ok, 1 if refused (prints the reason)
host_check_one() {
    local f="$1" rel="$2" r
    if r="$(host_reason_of "$f")"; then
        if [ -n "$r" ]; then return 0; fi
        # Declared but empty: the reason is the point of the declaration.
        printf '%s: # host-reason: with no reason text\n' "$rel"
        return 1
    fi
    if has_container "$f"; then return 0; fi
    printf '%s: host suite without a declaration\n' "$rel"
    return 1
}

case "${1:-}" in
--commands) printf 'unroutable: %s\ndirectable: %s\n' "$UNROUTABLE" "$DIRECTABLE"; exit 0 ;;
--scan)     scan "${2:?--scan needs a file}"; exit 0 ;;

# --host-check [<file>]: check one file, or all suites, for host-reason compliance.
# A suite passes if it has a non-empty `# host-reason:` declaration, or if any
# non-comment code line calls testenv.sh with `up` or `exec`.
--host-check)
    if [ -n "${2:-}" ]; then
        host_check_one "$2" "${2##*/}"
        rc=$?
        if [ "$rc" -ne 0 ]; then
            cat >&2 <<'WHY'

REFUSED by hermetic.sh --host-check — the suite above runs on the host without declaring why.

  The default is the container. A suite that cannot run in a container names the reason:

    # host-reason: drives the operator's own tmux server, which the container has no equivalent of

  The reason must be non-empty. A suite that runs in a container instead carries no annotation
  and calls testenv.sh (up or exec) so its assertions execute inside the container.
WHY
        fi
        exit "$rc"
    fi
    # Walk all suites in the tree, same glob and same exempt as the main check.
    shopt -s nullglob
    suites=("$ROOT"/spira/test-*.sh)
    if [ "${#suites[@]}" -eq 0 ]; then
        printf 'hermetic: no suites matched %s/spira/test-*.sh — refusing to report clean\n' "$ROOT" >&2
        exit 3
    fi
    bad=0
    for f in "${suites[@]}"; do
        case "${f##*/}" in test-hermetic.sh|test-host-reason.sh) continue ;; esac
        rel="${f#"$ROOT"/}"
        host_check_one "$f" "$rel" || bad=1
    done
    if [ "$bad" = 0 ]; then
        printf 'hermetic: host-check clean — all %d suite(s) have a container or # host-reason:\n' "${#suites[@]}"
        exit 0
    fi
    cat >&2 <<'WHY'

REFUSED by hermetic.sh --host-check — each suite above runs on the host without declaring why.

  The default is the container. A suite that cannot run in a container names the reason:

    # host-reason: drives the operator's own tmux server, which the container has no equivalent of

  The reason must be non-empty. A suite that runs in a container instead carries no annotation
  and calls testenv.sh (up or exec) so its assertions execute inside the container.
WHY
    exit 1
    ;;

# --count-undeclared: print the count of suites with neither # host-reason: nor container
# calls. Renders ? if the suite glob matches nothing (empty tree is indistinguishable from a
# wrong path). Exits 0 either way — this is an informational count, not a gate.
--count-undeclared)
    shopt -s nullglob
    suites=("$ROOT"/spira/test-*.sh)
    if [ "${#suites[@]}" -eq 0 ]; then printf '?\n'; exit 0; fi
    count=0
    for f in "${suites[@]}"; do
        host_reason_of "$f" > /dev/null && continue   # declared
        has_container "$f" && continue                # container suite
        count=$(( count + 1 ))
    done
    printf '%d\n' "$count"
    exit 0
    ;;

# --count-copying: print the count of suites that still copy harness files or create inline
# stubs — the wave 2 migration backlog. Falls monotonically as child beads of sp-841s close.
# Renders ? if the glob matches nothing. Host-reason suites are excluded: they are declared
# and intentional, not wave-2 candidates. Exits 0 either way — informational, not a gate.
--count-copying)
    shopt -s nullglob
    suites=("$ROOT"/spira/test-*.sh)
    if [ "${#suites[@]}" -eq 0 ]; then printf '?\n'; exit 0; fi
    count=0
    for f in "${suites[@]}"; do
        host_reason_of "$f" > /dev/null && continue   # declared host suite, not wave-2 work
        # Detect cp of harness files or inline stub creation. Three patterns cover the corpus:
        #   cp "$HERE/  — copies a named harness script to a scratch dir
        #   FAKE_SPIRA_HOME — creates a fake installation tree
        #   cat > "$...sh  — writes an inline stub script to a variable path
        if grep -qE 'cp "\$HERE/|FAKE_SPIRA_HOME=|cat > "\$[A-Za-z_]+/[^ ].*\.sh' "$f" 2>/dev/null; then
            count=$(( count + 1 ))
        fi
    done
    printf '%d\n' "$count"
    exit 0
    ;;
esac

# THE GLOB MUST MATCH SOMETHING (law-absence-needs-a-positive-control). An empty expansion
# and a clean tree are the same silence from outside, and the reassuring reading is the one a
# bad path would give — which is precisely how a check comes to pass for months on nothing.
shopt -s nullglob
suites=("$ROOT"/spira/test-*.sh)
if [ "${#suites[@]}" -eq 0 ]; then
    echo "hermetic: no suites matched $ROOT/spira/test-*.sh — refusing to report clean" >&2
    exit 3
fi

bad=0
for f in "${suites[@]}"; do
    # ONE SUITE IS EXEMPT, and for the same reason inventory.sh exempts its own: its content
    # IS the offender list. It has to plant a bare `systemctl`, a bare `bd` and a heredoc full
    # of both to prove this fence can go red, and a fence that flags itself is a fence
    # somebody deletes. The hole that leaves is real and named here rather than hidden — that
    # suite could reach the box unnoticed — which is why every program it runs, including this
    # one, it runs under `env -i`.
    case "${f##*/}" in test-hermetic.sh) continue ;; esac
    hits="$(scan "$f")"
    [ -n "$hits" ] || continue
    bad=1
    rel="${f#"$ROOT"/}"
    while IFS=: read -r ln cmd text; do
        if [ "$cmd" = testdb_up ]; then
            printf '%s:%s: %s exit status unchecked\n' "$rel" "$ln" "$cmd"
        else
            printf '%s:%s: %s reaches the box\n' "$rel" "$ln" "$cmd"
        fi
        printf '    %s\n' "$(printf '%s' "$text" | sed 's/^[[:space:]]*//')"
    done <<< "$hits"
done

if [ "$bad" = 0 ]; then
    printf 'hermetic: clean — %d suite(s) reach nothing outside their own scratch\n' "${#suites[@]}"
    exit 0
fi
cat >&2 <<'WHY'

REFUSED by hermetic.sh — the lines above have one of two problems:

  reaches the box — a command whose verdict depends on this machine. A suite that reads the
  real box is green until the box changes, and then it refuses work that is correct with
  nothing in the output pointing anywhere but at the branch. Route the call through the seam
  the program already has — SPIRA_SYSTEMCTL, SPIRA_LAUNCH, SPIRA_GH, SPIRA_BD — and point
  it at a shim in the suite's own scratch directory, or give the command a path under that
  directory.

  exit status unchecked — a testdb_up call whose return is not inspected. testdb_up unsets
  SPIRA_DB on failure, so a suite that ignores the return dies loudly rather than writing to
  production, but the failure message is cleaner and earlier when the suite checks itself.
  Add `|| exit 1` (or a conditional) so the failure names the suite.

If the suite genuinely must reach the box, say so where the call is:

    # hermetic-ok: <why this one has to>

Do not reword the call to slip past the matcher; that leaves the next reader neither the
call nor the reason.
WHY
exit 1
