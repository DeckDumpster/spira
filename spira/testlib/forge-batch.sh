#!/usr/bin/env bash
# testlib/forge-batch.sh — seed a queue.forge repository's open batch the way production opens
# one: `queue batch open`, against a forge stub that opens the PR. The queue writes the record
# wherever it keeps it (the batch's own record on the control store, or the open file before the
# batch machine moved there), so a suite seeds the state its subject reads without ever naming
# the place, and `queue batch show` is how it reads the record back.
#
# Sourced, never executed, after testlib.sh and testlib/lc-fixture.sh, by a suite whose repo map
# names the repository in queue mode:
#   lcfix_up || exit 1                    # (or fb_store_up: the switch on)
#   rec="$(seed_forge_batch fixq sp-a,sp-b --pr 42)" || bad ...
#   end_forge_batch fixq "next case"
#
# seed_forge_batch <repo> <member>[,<member>...] [--pr N] [--owner A] [--opened-ago SECS]
#   A member is <id> or <id>:<tip>. Its spira/<id> branch is made when the repository has none:
#   at <tip> when given, else as an empty commit on the landing ref. Its lifecycle row is made
#   CERTIFIED at that tip. A repository with no remote for its landing ref gets a bare one
#   beside it. The PR is N (default 1); the owner is A (default: queue's own, operator); the
#   batch opened SECS ago (default now), on the queue's own virtual clock (SPIRA_NOW).
#   Prints the record `queue batch show` answers; non-zero, saying why on stderr, when no batch
#   opened.
#
# end_forge_batch <repo> [reason] — `queue abandon` the repo's open batch against the same stub;
#   nothing to do when the queue shows none open (a case's own eject or verdict ended it).
#
# fb_store_up — put the batch machine on the control store for the rest of the suite: a fresh
#   SQLite store, SPIRA_LC_STORE_PATH and SPIRA_LC_LIFECYCLE_ON_STORE=1 declared. Members are
#   then made CERTIFIED by the bead machine's own walk on the store.

# Made once, when sourced: a seeding call usually runs in a command substitution, whose own
# assignments never reach the suite.
_FB_DIR="$(mktemp -d "${_TL_CONF_DIR:-${TMPDIR:-/tmp}}/forge-batch.XXXXXX")" \
    || { echo "forge-batch: cannot make its working directory" >&2; return 1; }

_fb_dir() { printf '%s\n' "$_FB_DIR"; }

# _fb_run <cmd> [args...] — <cmd> as the staged release runs it: its own queue and spira-lc,
# whatever stub the suite has put ahead of them for its subject. Seeding is fixture setup, so
# nothing the suite stubs may intercept it.
_fb_run() {
    local rel="${_TL_STAGED_RELEASE:-${SPIRA_RELEASE:-}}"
    if [ -n "$rel" ] && [ -d "$rel/bin" ]; then
        SPIRA_RELEASE="$rel" PATH="$rel/bin:$rel/spira:$PATH" "$@"
    else
        "$@"
    fi
}

# The forge the seeding call opens its PR on: preflight passes, no PR is open, pr-create
# answers the PR asked for, auto-merge arms, everything else succeeds silently.
_fb_forge() {
    local dir pr="$1"
    dir="$(_fb_dir)" || return 1
    cat > "$dir/forge.t" <<FORGE
#!/usr/bin/env bash
case "\${1:-}" in
    pr-create) cat >/dev/null; printf '%s\n' "$pr" ;;
    pr-list-open|pr-list-queue) : ;;
esac
exit 0
FORGE
    chmod +x "$dir/forge.t" && mv "$dir/forge.t" "$dir/forge" && printf '%s\n' "$dir/forge"
}

# _fb_row <repo> — "<path>|<landref>" from the suite's declared repo map.
_fb_row() {
    local map
    map="$(_fb_run spira-config get spira.repo_map 2>/dev/null)" || return 1
    [ -f "$map" ] || { echo "seed_forge_batch: the repo map ($map) does not exist" >&2; return 1; }
    awk -F'|' -v r="$1" '{ n=$1; gsub(/[ \t]/, "", n); if (n == r) { p=$2; l=$4; gsub(/^[ \t]+|[ \t]+$/, "", p); gsub(/^[ \t]+|[ \t]+$/, "", l); print p "|" l; exit } }' "$map"
}

_fb_on_store() {
    [ -n "$(_fb_run spira-config get spira.lc_store_path 2>/dev/null)" ] \
        && [ "$(_fb_run spira-config get spira.lc_lifecycle_on_store 2>/dev/null)" = 1 ]
}

# _fb_remote <path> <landref> — the landing ref resolves and its remote exists; a repository
# with neither gets a bare remote beside it, holding the landing branch.
_fb_remote() {
    local path="$1" landref="$2" remote=origin branch="${2##*/}" r
    git -C "$path" rev-parse -q --verify "$landref^{commit}" >/dev/null 2>&1 \
        && [ -n "$(git -C "$path" remote)" ] && return 0
    case "$landref" in
        refs/remotes/*/*) r="${landref#refs/remotes/}"; remote="${r%%/*}" ;;
        */*) remote="${landref%%/*}" ;;
    esac
    if ! git -C "$path" remote get-url "$remote" >/dev/null 2>&1; then
        git init -q --bare -b main "$path.forge.git" >/dev/null 2>&1 || return 1
        git -C "$path" remote add "$remote" "$path.forge.git" || return 1
    fi
    case "$landref" in
        */*) timeout 5 git -C "$path" push -q "$remote" "HEAD:refs/heads/$branch" >/dev/null 2>&1 \
                 && timeout 5 git -C "$path" fetch -q "$remote" >/dev/null 2>&1 ;;
    esac
    git -C "$path" rev-parse -q --verify "$landref^{commit}" >/dev/null 2>&1 \
        || { echo "seed_forge_batch: $path cannot resolve its landing ref $landref" >&2; return 1; }
}

# _fb_certify <id> <tip> — the member's lifecycle row, CERTIFIED at <tip>.
_fb_certify() {
    local id="$1" tip="$2" v
    if ! _fb_on_store; then
        lcfix_seed "$id" CERTIFIED "$tip"
        return
    fi
    _fb_version() { _fb_run spira-lc show "$1" 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d.get("bead", d) or {}
print(d.get("version", ""))' 2>/dev/null; }
    _fb_run spira-lc create-bead "$id" --ready >/dev/null || return 1
    v="$(_fb_version "$id")"
    _fb_run spira-lc event bead "$id" --expect READY --version "$v" --actor fixture \
        --kind '{"Claim":{"holder":"fixture","lease_until":1}}' >/dev/null || return 1
    v="$(_fb_version "$id")"
    _fb_run spira-lc event bead "$id" --expect WORKING --version "$v" --actor fixture \
        --kind "{\"Submit\":{\"tip\":\"$tip\"}}" >/dev/null || return 1
    v="$(_fb_version "$id")"
    _fb_run spira-lc event bead "$id" --expect SUBMITTED --version "$v" --actor fixture \
        --kind "{\"GatePass\":{\"tip\":\"$tip\",\"gate_key\":\"fixture-gate-key\"}}" >/dev/null
}

seed_forge_batch() {
    local repo="${1:?seed_forge_batch: <repo> required}" members="${2:?seed_forge_batch: <members> required}"
    shift 2
    local pr=1 owner="" now row path landref base spec id tip tips=() csv="" head forge out rc
    while [ $# -gt 0 ]; do
        case "$1" in
            --pr) pr="$2"; shift 2 ;;
            --owner) owner="$2"; shift 2 ;;
            --opened-ago) now="$(( $(date -u +%s) - $2 ))"; shift 2 ;;
            *) echo "seed_forge_batch: unknown option $1" >&2; return 2 ;;
        esac
    done
    row="$(_fb_row "$repo")"
    [ -n "$row" ] || { echo "seed_forge_batch: the repo map names no $repo" >&2; return 1; }
    path="${row%%|*}"; landref="${row#*|}"
    git -C "$path" rev-parse --git-dir >/dev/null 2>&1 || { echo "seed_forge_batch: $path is not a git repository" >&2; return 1; }
    _fb_remote "$path" "$landref" || return 1
    base="$(git -C "$path" rev-parse "$landref^{commit}")"
    for spec in ${members//,/ }; do
        id="${spec%%:*}"; tip=""; [ "$spec" != "$id" ] && tip="${spec#*:}"
        if git -C "$path" rev-parse -q --verify "refs/heads/spira/$id" >/dev/null; then
            [ -n "$tip" ] || tip="$(git -C "$path" rev-parse "refs/heads/spira/$id")"
        else
            [ -n "$tip" ] || tip="$(git -C "$path" -c user.name=fixture -c user.email=fixture@test \
                commit-tree "$base^{tree}" -p "$base" -m "$id: seeded member")" || return 1
            git -C "$path" branch "spira/$id" "$tip" || return 1
        fi
        _fb_certify "$id" "$tip" || { echo "seed_forge_batch: cannot make $id CERTIFIED at $tip" >&2; return 1; }
        tips+=("$tip"); csv="${csv:+$csv,}$id:$tip"
    done
    if [ "${#tips[@]}" -eq 1 ]; then
        head="${tips[0]}"
    else
        head="$(git -C "$path" -c user.name=fixture -c user.email=fixture@test \
            commit-tree "$base^{tree}" $(printf -- '-p %s ' "${tips[@]}") -m "queue: seeded batch")" || return 1
    fi
    forge="$(_fb_forge "$pr")" || return 1
    # A batch is named <repo>-<second> and its branch spira/queue/<second>: a batch seeded in a
    # second another already took would reuse that one's name, or its branch in another repo.
    while [ -f "$(_fb_dir)/stamps" ] && grep -qx "${now:-$(date -u +%s)}" "$(_fb_dir)/stamps"; do
        if [ -n "${now:-}" ]; then now=$((now + 1)); else sleep 0.2; fi
    done
    printf '%s\n' "${now:-$(date -u +%s)}" >> "$(_fb_dir)/stamps"
    printf 'Seeded batch for %s.\n' "$repo" > "$(_fb_dir)/body"
    out="$(SPIRA_TOML="$(tl_layer SPIRA_FORGE="$forge")" _fb_run env ${now:+SPIRA_NOW="$now"} queue batch open --repo "$repo" \
        --members "$csv" --head "$head" --skip-pregate --body-file "$(_fb_dir)/body" \
        ${owner:+--owner "$owner"} 2>&1)"; rc=$?
    [ -n "${now:-}" ] || date -u +%s >> "$(_fb_dir)/stamps"
    [ "$rc" -eq 0 ] || { printf 'seed_forge_batch: queue batch open %s exited %s:\n%s\n' "$repo" "$rc" "$out" >&2; return 1; }
    out="$(_fb_run queue batch show --repo "$repo")" && [ -n "$out" ] \
        || { printf 'seed_forge_batch: queue batch show %s answers no batch after the open: %s\n' "$repo" "$out" >&2; return 1; }
    printf '%s\n' "$out"
}

end_forge_batch() {
    local repo="${1:?end_forge_batch: <repo> required}" reason="${2:-the suite ends this fixture batch}" forge out
    out="$(_fb_run queue batch show --repo "$repo")" || { printf 'end_forge_batch: queue batch show %s: %s\n' "$repo" "$out" >&2; return 1; }
    [ -n "$out" ] || return 0
    forge="$(_fb_forge 0)" || return 1
    out="$(SPIRA_TOML="$(tl_layer SPIRA_FORGE="$forge")" _fb_run queue abandon "$repo" --reason "$reason" 2>&1)" \
        || { printf 'end_forge_batch: queue abandon %s: %s\n' "$repo" "$out" >&2; return 1; }
}

fb_store_up() {
    local dir
    dir="$(_fb_dir)" || return 1
    # With no dolt fixture up there is no lifecycle credential: an undeclared password file names
    # the fixture home's absent one, and spira-lc refuses before it reaches the store.
    if [ -z "${LCFIX_DIR:-}" ]; then
        export SPIRA_LC_PASSWORD=unused
        tl_config SPIRA_LC_SOCKET="" SPIRA_LC_PASSWORD_FILE="" || return 1
    fi
    tl_config SPIRA_LC_STORE_PATH="$dir/control.db" SPIRA_LC_LIFECYCLE_ON_STORE=1 || return 1
    rm -f "$dir/control.db"
    _fb_run spira-lc admin-create-store >/dev/null || { echo "fb_store_up: cannot create the control store" >&2; return 1; }
}
