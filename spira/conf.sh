# conf.sh — the ONE configuration surface. Sourced, never executed.
#
# WHAT THIS FILE IS FOR. Every path this harness touches used to be written into the script
# that touched it: the wiki checkout, the beads database, the directory two binaries happen
# to live in, the seven repositories one operator has. A colleague cloning it has none of
# those, and the failure is not a message — it is `bd: command not found` from a systemd
# timer, into a log nobody reads. So the paths are collected here, in one place, and every
# script asks this file instead of knowing the answer.
#
# THREE SOURCES, IN THIS ORDER, AND THE FIRST ONE THAT SPEAKS WINS:
#
#   1. the ENVIRONMENT — because that is the seam every test suite already drives a fixture
#      through, and a config file that could override a test's own SPIRA_DB would point the
#      suite at the operator's real database. Environment first is not a convenience here;
#      it is what keeps the suites isolated.
#   2. the CONFIG FILE — `spira.toml`, the operator's box.
#   3. a DERIVED DEFAULT — computed from where this file sits, so a clean clone with no
#      config at all still resolves to something coherent rather than to someone else's box.
#
# WHERE THE CONFIG FILE IS LOOKED FOR, first hit wins:
#
#   $SPIRA_TOML                                  an explicit path; set it to a nonexistent
#                                                one to read no file at all
#   $SPIRA_REPO/spira.toml                       beside the checkout the harness runs from
#   ${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml
#   /etc/spira/spira.toml
#
# NO SPIRA_TOML? A LEGACY spira.conf IS AUTO-CONVERTED, so a box that has only ever known
# `spira.conf` (KEY=value) does not silently lose every SPIRA_* setting the moment this file
# stops parsing that format itself. `spira_toml_resolve` runs `spira-config convert` and keeps
# regenerating the `.toml` from the `.conf` for as long as the `.conf` is the newer of the two
# — so an operator's existing tools (`aeons.sh`, `deploy.sh`, ...), which still read and write
# `spira.conf` unmodified, keep taking effect. Once the operator deletes `spira.conf`, the
# `.toml` already on disk becomes authoritative and nothing regenerates it again.
#
# IT IS NOT SOURCED. A config file that is shell — or a value inside one — can set PATH, run
# a command, or shadow a function this library defines, and it is read by a process that
# summons agents. `spira-config` parses TOML into a typed document with `deny_unknown_fields`,
# so a typo is a hard parse error naming the field, not a setting silently ignored.

[ -n "${SPIRA_CONF_LOADED:-}" ] && return 0
SPIRA_CONF_LOADED=1

# --------------------------------------------------------------------------------------
# Every key this harness honours, with the default the CODE carries. A key absent from this
# list is refused when it appears in a config file.
#
# `spira.toml` itself refuses a misspelled key: it is typed,
# with `deny_unknown_fields`, so a typo is a parse error rather than a setting silently
# ignored — but its `[spira]` table only covers the keys a real install actually sets, a
# narrower set than this list, which runs past 200 (most of them derived defaults nothing
# overrides).
# --------------------------------------------------------------------------------------
# SPIRA_HOME AND SPIRA_REPO ARE ABSENT FROM THIS LIST DELIBERATELY, and that is a fence.
# Where the harness IS is not a configuration question — it is a fact about where this file
# sits — and letting a config file answer it breaks the landing gate, which extracts a branch
# to a scratch tree and runs that tree's own suites. The config would point them back at the
# installed copy, so the gate would test the code already in force instead of the code being
# judged, and pass. The environment may still override both: that is the seam every suite
# drives a fixture through, and it is explicit rather than ambient.
# --------------------------------------------------------------------------------------
# GENERATED (sp-g3uwp). This used to be ~250 keys hand-grouped across dozens of lines plus
# a second, alphabetized one-per-line copy appended by sp-wjkj6 — and by the time this bead
# started, three keys (SPIRA_EXPRESS_LABEL among them) were declared twice across the two
# copies, and eleven keys with a real default elsewhere in this file were missing from this
# list entirely (SPIRA_LC_SOCKET, SPIRA_REBASE_DECOMPOSE_FILES, ...). Both failure modes are
# now impossible by construction: spira/conf.d/ is one file per key — adding a key adds a
# file, which cannot conflict with another branch's own new file — and conf-gen.sh derives
# this allowlist from exactly that directory listing, so the registry and the allowlist
# cannot disagree about which keys exist.
#
# NEVER EDIT spira/conf.d.keys.generated.sh BY HAND. Add, rename, or remove a key by adding,
# renaming, or removing its file under spira/conf.d/, then let _spira_conf_gen_ensure (below)
# or `bash spira/conf-gen.sh` regenerate it.
#
# SELF-CONTAINED rather than trusting a global set once and read later, even though this is
# now called only once (for "keys"): `spira_conf_defaults`, the other caller that used to
# source "defaults" from here, is retired (sp-ubcgo) in favour of `spira_config::resolve()`.
# conf-gen.sh still regenerates conf.d.defaults.generated.sh (nothing has removed that
# generator), but conf.sh no longer sources it.
#
# RESOLVES THE SYMLINK, unlike SPIRA_HOME's own derivation below. Dozens of suites
# (test-conf.sh among them) `ln -s "$HERE/conf.sh" "$FIXTURE/spira/conf.sh"` to make conf.sh
# believe it is running from a synthetic tree — that is SPIRA_HOME's whole point, and this
# function must not disturb it. But conf.d/ and conf-gen.sh live beside conf.sh's REAL file,
# not beside wherever a test symlinked it, so this one path is read with the link followed;
# `dirname "${BASH_SOURCE[0]}"` alone resolved to the fixture's empty spira/ directory and
# refused every such suite with "conf-gen.sh failed to regenerate" (caught by test-conf.sh,
# test-conf-toml.sh and test-conf-writeback.sh — 76 assertions, first attempt at this fix).
_spira_conf_gen_ensure() {   # _spira_conf_gen_ensure <keys|defaults> -> sources the named
                             # generated fragment, regenerating first if it is stale
    local _dir _which="${1:?_spira_conf_gen_ensure needs keys or defaults}" _gen _src _stale="" _real
    _real="$(readlink -f "${BASH_SOURCE[0]}" 2>/dev/null)" || _real="${BASH_SOURCE[0]}"
    _dir="$(cd "$(dirname "$_real")" && pwd -P)"
    # NOT `local`: spira_config::resolve() (the "DERIVED DEFAULTS" eval, far below) needs
    # this SAME symlink-resolved directory to find conf.d/ — SPIRA_HOME itself stays
    # unresolved (see this function's own comment above), so spira-config cannot derive it
    # from SPIRA_HOME the way this function derives it from BASH_SOURCE (sp-ubcgo).
    _spira_conf_real_dir="$_dir"
    _gen="$_dir/conf.d.$_which.generated.sh"
    [ -f "$_gen" ] || _stale=1
    if [ -z "$_stale" ]; then
        for _src in "$_dir"/conf.d/* "$_dir/conf-gen.sh"; do
            [ -e "$_src" ] || continue
            [ "$_src" -nt "$_gen" ] && { _stale=1; break; }
        done
    fi
    if [ -n "$_stale" ]; then
        # To stderr: sourcing conf.sh must never write to the caller's stdout, which a script
        # capturing its own output (release.sh's tag name) would otherwise swallow (sp-gt0ta).
        bash "$_dir/conf-gen.sh" >&2 \
            || { echo "spira: conf-gen.sh failed to regenerate conf.d.*.generated.sh — refusing to run with a stale or missing generated file" >&2; return 1; }
    fi
    # shellcheck disable=SC1090
    . "$_gen"
}
_spira_conf_gen_ensure keys || return 1

# --------------------------------------------------------------------------------------
# Where we are. SPIRA_HOME is the directory holding this file; everything else can be
# derived from it, which is what makes a clean clone runnable.
#
# BASH_SOURCE, not $0: this file is sourced, so $0 is whatever script sourced it, and
# addressing SPIRA_HOME as the caller's directory broke the moment a cockpit tool two
# directories away sourced lib.sh.
# --------------------------------------------------------------------------------------
# WHICH KEYS THE ENVIRONMENT ALREADY OWNS no longer needs recording here (sp-ubcgo, "wave
# 4.5"): `spira_config::resolve()` applies the env-over-toml-over-default precedence itself,
# inside the subprocess called below, so there is nothing left in THIS file that needs to
# ask "is it set?" before deriving. (The one thing bash still derives ahead of that call,
# SPIRA_HOME/SPIRA_REPO, is handled by the explicit-vs-ambient distinction immediately below.)
_spira_conf_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# Whether SPIRA_HOME was ANSWERED BY THE CALLER or derived is remembered, because it changes
# which repo-map wins below. An explicit SPIRA_HOME is a caller saying "this tree is the
# harness in force" — a fixture, or the gate's scratch checkout — and that tree's map is the
# one it means.
_spira_conf_home_env=""; [ -n "${SPIRA_HOME:-}" ] && _spira_conf_home_env=1
SPIRA_HOME="${SPIRA_HOME:-$_spira_conf_here}"

# The checkout the harness is installed in. `git -C ... --show-toplevel` rather than
# "two directories up", because the harness may be installed anywhere, and because a
# worktree must resolve to ITSELF — an aeon running from a worktree whose helpers resolve
# to the installed copy on main is a version skew that shows up as nothing at all.
# WITHOUT GIT'S HOOK ENVIRONMENT: a git hook runs with GIT_DIR exported, and with it set
# --show-toplevel answers the -C directory itself — <repo>/spira — so a hook sourcing this
# file resolved SPIRA_REPO one level too deep.
SPIRA_REPO_DERIVED="$(env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_PREFIX \
    git -C "$SPIRA_HOME" rev-parse --show-toplevel 2>/dev/null)" || SPIRA_REPO_DERIVED=""
# THE FALLBACK IS THE PARENT, not a fixed number of levels up. Outside a git checkout there
# is nothing to ask, and "two directories up" was an assumption about one layout that became
# silently wrong the moment the harness moved from `<repo>/.claude/spira` to `<repo>/spira` —
# resolving SPIRA_REPO to the parent of the repository, where nothing it looks for exists.
[ -n "$SPIRA_REPO_DERIVED" ] || SPIRA_REPO_DERIVED="$(cd "$SPIRA_HOME/.." 2>/dev/null && pwd -P)"
SPIRA_REPO="${SPIRA_REPO:-$SPIRA_REPO_DERIVED}"

# THE DERIVED VALUE IS KEPT, because "SPIRA_REPO was set deliberately" and "SPIRA_REPO fell
# out of where this file sits" mean different things to repo_root: the first is an override
# of the repository map, the second is not. Before it was kept, deriving a value made every
# home-repository lookup bypass the map — invisible here, where the two agree, and wrong
# everywhere else. NOT exported, for the same reason SPIRA_HOME is not: it is a fact about
# this copy of the harness, and whatever sources its own conf.sh resolves its own.

# NO BINARY RESOLVER (sp-gypjk, design runtime-is-a-release): every Spira tool is invoked by
# its bare name and found on the PATH the launcher set ($SPIRA_RELEASE/bin and
# $SPIRA_RELEASE/spira first). There is no spira_bin, no SPIRA_ARTIFACTS, no per-tool
# SPIRA_*_BIN variable and no fallback: a tool missing from PATH fails at its call, naming
# itself.

# One line, single-spaced, padded at both ends — because the membership test below is a
# `case` on " $key ", and a key that happened to sit at the end of a line in the list above
# was followed by a newline rather than a space and was refused as unknown. Six of the
# twenty-two keys, silently ignored, which is precisely the failure the allowlist exists to
# prevent happening to a typo.
SPIRA_CONF_KEYS=" $(echo $SPIRA_CONF_KEYS) "

# spira_conf_file -> the path of the LEGACY spira.conf in force, or empty. Nothing writes
# this format anymore (every writer targets spira.toml through spira_config_set/_at, below;
# sp-usxfl) — this is read-only, kept so a box that has only ever known spira.conf still
# resolves it as the conversion SOURCE `spira_toml_resolve` reads from below.
#
# NO $SPIRA_REPO CANDIDATE (sp-9hwim, design runtime-is-a-release #5): config is read from
# an explicit SPIRA_CONF, from XDG, or from /etc/spira only — never from beside the
# checkout, which the running system must not read at all. A caller that means "the config
# beside THIS tree" (a fixture, the gate's scratch checkout) says so with an explicit
# SPIRA_CONF, exactly the same seam the environment-wins rule above already relies on.
spira_conf_file() {
    local c
    if [ -n "${SPIRA_CONF+set}" ]; then
        [ -f "$SPIRA_CONF" ] && printf '%s' "$SPIRA_CONF"
        return 0
    fi
    for c in "${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf" \
             /etc/spira/spira.conf; do
        [ -f "$c" ] && { printf '%s' "$c"; return 0; }
    done
    return 0
}

# spira_toml_file -> the path of spira.toml in force, or empty. Same search spira_conf_file
# uses, and for the same reason: an explicit path, then XDG, then /etc/spira — never beside
# the checkout (sp-9hwim).
spira_toml_file() {
    local c
    if [ -n "${SPIRA_TOML+set}" ]; then
        [ -f "$SPIRA_TOML" ] && printf '%s' "$SPIRA_TOML"
        return 0
    fi
    for c in "${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml" \
             /etc/spira/spira.toml; do
        [ -f "$c" ] && { printf '%s' "$c"; return 0; }
    done
    return 0
}

# _spira_repo_map_candidate -> the repo-map path SPIRA_REPO_MAP would default to (its own
# fallback now lives in spira_config::resolve()'s Rust port of this same search, run below),
# or empty if none of its candidates exist yet. Used here so spira_toml_resolve's
# auto-convert — which runs before SPIRA_REPO_MAP is resolved, to build the `spira-config
# convert` call — finds the SAME file rather than converting with no map at
# all (sp-zs04v.2: an auto-convert with no --repo-map produced a spira.toml with an empty
# [repo] table, and overwrote a real one).
#
# AMBIENT NEVER READS $SPIRA_HOME/repo-map (sp-9hwim, design runtime-is-a-release #5): when
# SPIRA_HOME was derived (nobody named a tree on purpose), the map comes from beside the
# resolved config file — now always XDG or /etc, never the checkout — or from the tracked
# `repo-map.example` a clean clone ships. AN EXPLICIT SPIRA_HOME is unaffected: a caller that
# names its own tree on purpose (a fixture, the gate's scratch checkout) still gets that
# tree's own repo-map first, because that tree is what it means.
_spira_repo_map_candidate() {
    local cf d c
    cf="$(spira_conf_file)"
    [ -n "$cf" ] && d="$(dirname "$cf")" || d=""
    if [ -n "$_spira_conf_home_env" ]; then
        set -- "$SPIRA_HOME/repo-map" ${d:+"$d/repo-map"} "$SPIRA_HOME/repo-map.example"
    else
        set -- ${d:+"$d/repo-map"} "$SPIRA_HOME/repo-map.example"
    fi
    for c in "$@"; do
        [ -f "$c" ] && { printf '%s' "$c"; return 0; }
    done
    # EXPLICIT, not left to the loop's own exit status: this function is called as
    # `X="${X:-$(_spira_repo_map_candidate)}"`, and when no candidate exists the loop's
    # last executed command is the final failing `[ -f "$c" ]` — under errexit (every
    # `set -e` caller conf.sh is sourced by, e.g. aerc/accept-default.sh) that aborts the
    # caller right at the assignment instead of yielding "".
    return 0
}

# _spira_fayth_paths -> every "*.fayth" file under the chamber in force, one per line.
# SPIRA_CHAMBER_OVERLAY only replaces prompt text (aeon.sh's *.md overlay); fayth files
# themselves are never overlaid, so only SPIRA_CHAMBER (or its default) is read here.
_spira_fayth_paths() {
    local dir="${SPIRA_CHAMBER:-$SPIRA_HOME/chamber}" f
    for f in "$dir"/*.fayth; do
        [ -f "$f" ] && printf '%s\n' "$f"
    done
}

# spira_toml_resolve -> the spira.toml path to read, or empty.
#
# ONCE A spira.toml EXISTS IT WINS UNCONDITIONALLY — no mtime comparison against spira.conf.
# Every writer this harness ships now writes spira.toml directly (spira_config_set, below;
# sp-usxfl), so a spira.conf newer than the toml is never a newer answer, only a STALE one
# some other tool or a hand edit left behind. Regenerating from it would silently discard
# whatever the toml alone has gained since — the "gutted spira.toml" failure this bead
# retires. `doctor` warns when both files are present so that stale spira.conf gets
# noticed and removed.
#
# AUTO-CONVERTS FROM A LEGACY spira.conf ONLY WHEN NO spira.toml EXISTS AT ALL, so a box that
# has only ever known `spira.conf` does not silently lose every SPIRA_* setting the moment
# this file stops parsing that format itself. Once the conversion has produced a spira.toml,
# this function never looks at spira.conf again.
#
# THE REPO-MAP AND EVERY FAYTH ARE PASSED, not just --conf: a conversion missing them is
# not incomplete, it is DESTRUCTIVE — `spira-config convert`'s own writer (below) refuses
# to let a document with fewer [repo]/[persona] tables replace one that has more, so an
# auto-convert that omitted them would simply fail closed against a box with a real
# spira.toml already on disk instead of quietly gutting it (sp-zs04v.2).
#
# _spira_toml_convert_from_conf <conf> <target> -> writes <target> exactly (no writeback
# redirect — the caller has already decided this IS the right place) from <conf> plus this
# box's repo-map and every fayth, via a full `spira-config convert`, and prints <target> — or
# prints nothing and reports the failure on stderr. Factored out of spira_toml_resolve so any
# writer that must create ANOTHER root's spira.toml directly (install.sh seeding a separate
# SPIRA_PROD checkout's instance) shares the exact same conversion, never a narrower one that
# drops the repo-map or a fayth and so, per `spira-config convert`'s own refusal, fails closed
# against a target that already has more (sp-zs04v.2) rather than by construction here.
# spira_toml_resolve, the one caller resolving an AMBIENT (not explicitly-given) target,
# applies spira_config_writeback itself before calling in — the redirect guards against a
# worktree regenerating some ambiguous default path, which does not apply to a target a
# caller (install.sh, deploy.sh) already named on purpose.
_spira_toml_convert_from_conf() {
    local conf="$1" target="$2" out rmap f
    local -a conv_args
    conv_args=(--conf "$conf" --home "$HOME" --out "$target")
    rmap="${SPIRA_REPO_MAP:-$(_spira_repo_map_candidate)}"
    [ -n "$rmap" ] && conv_args+=(--repo-map "$rmap")
    while IFS= read -r f; do
        conv_args+=(--fayth "$f")
    done < <(_spira_fayth_paths)
    if out="$(spira-config convert "${conv_args[@]}" 2>&1)"; then
        [ -n "$out" ] && printf '%s\n' "$out" >&2
        printf '%s' "$target"
    else
        printf 'spira.conf: auto-convert to spira.toml failed: %s\n' "$out" >&2
        # EMPTY, NOT A BARE (no-op) STATEMENT: this function is called as
        # `X="$(spira_toml_resolve)"`, a subshell that inherits errexit from every `set -e`
        # caller (aerc/accept-default.sh), and a subshell's last command failing exits the
        # substitution non-zero right here instead of yielding "". `printf` with an empty
        # argument always succeeds, so this is the guard, spelled to survive errexit.
        printf '%s' ""
    fi
}

# WHEN SPIRA_CONF IS PINNED EXPLICITLY BUT SPIRA_TOML IS NOT, the toml candidate is looked
# for ONLY beside that conf — never through the ordinary $SPIRA_REPO/XDG/etc tiers. Those
# tiers answer "where does THIS host's config live", a question the caller already answered
# by naming a conf file directly; consulting them anyway can find a toml that has nothing to
# do with the pinned conf (sp-zs04v.2: a test fixture's spira.conf gutted an unrelated, real
# spira.toml this way).
#
# ALSO regenerates [persona.*] from chamber/*.fayth (sp-zs04v.4), on top of whatever toml was
# just resolved, whenever a fayth is newer than it or it has no [persona.*] table yet — a box
# whose only record of a persona's model is FAYTH_MODEL must not silently lose it the moment
# fayth_get stops reading that field at launch (persona_model, lib.sh). This is narrower than
# the full spira.conf conversion above: it passes only --fayth, never --conf/--repo-map, so it
# must never run in place of that conversion — only after a toml already exists, or when there
# is no spira.conf to convert from at all.
spira_toml_resolve() {
    local toml conf conf_pinned=0
    conf="$(spira_conf_file)"
    if [ -n "${SPIRA_CONF+set}" ] && [ -z "${SPIRA_TOML+set}" ]; then
        conf_pinned=1
        toml=""
        [ -n "$conf" ] && [ -f "$(dirname "$conf")/spira.toml" ] && toml="$(dirname "$conf")/spira.toml"
    else
        toml="$(spira_toml_file)"
    fi

    if [ -z "$toml" ] && [ -n "$conf" ]; then
        # A PINNED SPIRA_CONF NAMES ITS OWN WRITE TARGET, same as the search restriction
        # above: spira_config_writeback's SPIRA_REPO redirect exists for the AMBIENT case,
        # where nothing named a location on purpose. Applying it here sends the conversion
        # to this checkout's own spira.toml instead of beside the pinned conf — which, when
        # the checkout already has a real one, made `spira-config convert` refuse to shrink
        # it (the write-side twin of the read-side sp-zs04v.2 scar cited above).
        if [ "$conf_pinned" -eq 1 ]; then
            _spira_toml_convert_from_conf "$conf" "$(dirname "$conf")/spira.toml"
        else
            _spira_toml_convert_from_conf "$conf" \
                "$(spira_config_writeback "${SPIRA_TOML:-$(dirname "$conf")/spira.toml}")"
        fi
        return 0
    fi

    local fayth_dir f stale=0
    local -a fayth_files=()
    fayth_dir="${SPIRA_CHAMBER:-$SPIRA_HOME/chamber}"
    if [ -d "$fayth_dir" ]; then
        for f in "$fayth_dir"/*.fayth; do
            [ -f "$f" ] && fayth_files+=("$f")
        done
    fi
    if [ "${#fayth_files[@]}" -eq 0 ]; then
        [ -n "$toml" ] && printf '%s' "$toml"
        return 0
    fi
    if [ -z "$toml" ]; then
        stale=1
    else
        grep -q '^\[persona\.' "$toml" 2>/dev/null || stale=1
        for f in "${fayth_files[@]}"; do
            [ "$f" -nt "$toml" ] && stale=1
        done
    fi
    [ "$stale" -eq 0 ] && { printf '%s' "$toml"; return 0; }

    local target out
    if [ -n "${SPIRA_TOML+set}" ]; then
        # SPIRA_TOML NAMES ITS OWN WRITE TARGET, same as the pinned-SPIRA_CONF case above:
        # a caller (deploy.sh, a test fixture) that pinned this path on purpose must have the
        # regenerated [persona.*] table land there, not redirected by spira_config_writeback's
        # SPIRA_REPO fallback (the write-side twin of the pinned-conf fix, sp-zs04v.4).
        target="$SPIRA_TOML"
    else
        # spira_config_writeback: $toml may resolve to the operator's real spira.toml
        # (spira_toml_file checks $HOME before regenerating anything from this worktree's own
        # fayths).
        target="$(spira_config_writeback "${toml:-$SPIRA_REPO/spira.toml}")"
    fi
    local -a args=(--home "$HOME" --out "$target")
    for f in "${fayth_files[@]}"; do args+=(--fayth "$f"); done
    if out="$(spira-config convert "${args[@]}" 2>&1)"; then
        [ -n "$out" ] && printf '%s\n' "$out" >&2
        printf '%s' "$target"
    else
        printf '%s\n' "$out" >&2
        [ -n "$toml" ] && printf '%s' "$toml"
    fi
}

# spira_toml_read AND spira_conf_defaults ARE RETIRED (sp-ubcgo, "wave 4.5: conf.sh becomes
# an eval of resolve"), not ported: between them they read spira.toml field-by-field and then
# hand-computed ~300 keys' worth of env-over-toml-over-default precedence, and that whole
# pipeline is now `spira_config::resolve()`, called once as a subprocess and `eval`'d into
# this shell below ("DERIVED DEFAULTS", past the config-write functions further down this
# file). `_spira_join` went with them — its three call sites were all inside the retired
# function and nothing else used it.

# --------------------------------------------------------------------------------------
# CHECKOUT MODE. SPIRA_PROD is the tree systemd executes; SPIRA_REPO is the tree being
# developed. Their relation decides what several programs may assume, and they must all
# decide it the same way — doctor and the unit manifest disagreeing about this is how a
# box ends up reporting a fatal for a unit that is correctly absent.
#
#   split-checkout   SPIRA_PROD outside SPIRA_REPO. promote.sh carries commits from one
#                    to the other, so a dirty or mid-landing dev tree never executes.
#   single-checkout  SPIRA_PROD inside SPIRA_REPO. One tree, developed and executed.
#                    Supported, and the only sane shape on a box whose production lives
#                    on another machine and is reached through a tagged release. It costs
#                    what it obviously costs: an edit is live the moment it is saved.
#
# Returns 0 for single-checkout, 1 for split-checkout AND for an empty SPIRA_PROD — an
# unset production directory is its own error, reported where it is diagnosed rather than
# folded into this answer.
spira_single_checkout() {
    [ -n "${SPIRA_PROD:-}" ] || return 1
    local _p _r
    _p="$(cd "$SPIRA_PROD" 2>/dev/null && pwd -P)" || _p=""
    [ -n "$_p" ] || _p="$SPIRA_PROD"
    _r="$(cd "${SPIRA_REPO:-}" 2>/dev/null && pwd -P)" || _r=""
    [ -n "$_r" ] || _r="${SPIRA_REPO:-}"
    [ -n "$_r" ] || return 1
    # Strip the trailing slash before interpolating: when _r is "/" the unstripped form
    # produces "//*", which a case statement never matches.
    case "$_p/" in "${_r%/}/"*) return 0 ;; *) return 1 ;; esac
}

# spira_config_writeback <candidate> — the ONE path any code that would REGENERATE a
# config file (spira.toml, a converted spira.conf, ...) resolves its write target through.
# <candidate> is returned unchanged only when this checkout IS the installed release
# (SPIRA_HOME resolves to the same directory as SPIRA_PROD) or SPIRA_CONFIG_WRITE=1 is set
# explicitly; otherwise the write is redirected to $SPIRA_REPO, then further to
# XDG_CONFIG_HOME, then to a private scratch file, at the first of those that is actually
# writable.
#
# scar: three cutover branches, each sourcing their own conf.sh with the operator's real
# HOME, regenerated the operator's real spira.toml from worktree state — three times in one
# day, each time blinding queue-watch until the file was restored by hand. A worktree has no
# business writing outside itself, however it got HOME; an unresolved SPIRA_PROD (default
# not yet derived) compares unequal to SPIRA_HOME and so fails closed into the redirect,
# which is the safe side of this check.
#
# $SPIRA_REPO IS CHECKED, NOT ASSUMED, WRITABLE (sp-jv49c): a testenv container bind-mounts
# it read-write for its host owner but read-only (or foreign-UID-owned) for the user conf.sh
# runs as, so a redirect that lands there unconditionally hands `spira-config convert` a
# target it cannot write and the whole auto-convert fails — silently reverting every
# SPIRA_* key to its computed default instead of what spira.conf actually says. The final
# fallback, a private scratch file, is chosen precisely because `mktemp` always succeeds:
# the auto-convert this run's values depend on must not fail for want of a place to land.
spira_config_writeback() {
    local candidate="$1"
    if [ "${SPIRA_CONFIG_WRITE:-0}" = "1" ]; then
        printf '%s' "$candidate"
        return 0
    fi
    local home_p prod_p
    home_p="$(cd "${SPIRA_HOME:-}" 2>/dev/null && pwd -P)" || home_p="${SPIRA_HOME:-}"
    prod_p="$(cd "${SPIRA_PROD:-}" 2>/dev/null && pwd -P)" || prod_p="${SPIRA_PROD:-}"
    if [ -n "$prod_p" ] && [ "$home_p" = "$prod_p" ]; then
        printf '%s' "$candidate"
        return 0
    fi
    if [ -w "$SPIRA_REPO" ]; then
        printf '%s/%s' "$SPIRA_REPO" "$(basename "$candidate")"
        return 0
    fi
    local xdg_dir="${XDG_CONFIG_HOME:-$HOME/.config}/spira"
    if mkdir -p "$xdg_dir" 2>/dev/null && [ -w "$xdg_dir" ]; then
        printf '%s/%s' "$xdg_dir" "$(basename "$candidate")"
        return 0
    fi
    mktemp "${TMPDIR:-/tmp}/spira-toml.XXXXXX"
}

# spira_toml_write_target_for <conf> <toml> -> the spira.toml path a writer targeting a
# GIVEN root (not the ambient one this process itself runs under) should write to, creating
# it from <conf> first via _spira_toml_convert_from_conf if <toml> does not exist yet.
# install.sh's cross-checkout instance seed is the caller this exists for: it already knows
# both candidate paths beside a separate $SPIRA_PROD checkout, so it has no ambient
# SPIRA_CONF/SPIRA_TOML env pin of its own to resolve through.
spira_toml_write_target_for() {
    local conf="$1" toml="$2"
    if [ -f "$toml" ]; then
        printf '%s' "$toml"
        return 0
    fi
    if [ -f "$conf" ]; then
        _spira_toml_convert_from_conf "$conf" "$toml"
    else
        printf '%s' "$toml"
    fi
}

# spira_toml_write_target -> the spira.toml path any WRITER of THIS process's own config
# should target, creating it (via a full auto-convert of any legacy spira.conf, this box's
# repo-map and every fayth) first if it does not exist yet. Never spira_toml_resolve alone
# for this purpose: that returns "" when nothing exists yet, and `spira-config set` starting
# from nothing there would produce a document holding only the one key just written — the
# same "gutted spira.toml" failure a bare spira.conf write causes (sp-usxfl).
spira_toml_write_target() {
    local t; t="$(spira_toml_resolve)"
    if [ -n "$t" ]; then
        printf '%s' "$t"
        return 0
    fi
    # Neither spira.conf nor spira.toml exists — a from-scratch box with nothing yet to
    # protect against shrinking. Its first spira.toml lands at the same first-tier path
    # spira.conf itself would have used.
    spira_config_writeback "${SPIRA_TOML:-$SPIRA_REPO/spira.toml}"
}

# _spira_config_write set <target> <SPIRA_KEY> <value> | unset <target> <SPIRA_KEY> — the one
# place a [spira] key's dotted path is derived and `spira-config <verb>` is invoked, shared by
# every writer below so the SPIRA_KEY -> dotted-path mapping cannot drift between them.
_spira_config_write() {
    local verb="$1" target="$2" key="$3" dotted
    [ -n "$target" ] || { printf 'spira_config_%s: no config path resolved for %s\n' "$verb" "$key" >&2; return 1; }
    dotted="spira.$(printf '%s' "${key#SPIRA_}" | tr '[:upper:]' '[:lower:]')"
    case "$verb" in
        set)   spira-config set "$dotted" "$4" "$target" ;;
        unset) spira-config unset "$dotted" "$target" ;;
        *)     printf '_spira_config_write: unknown verb %s\n' "$verb" >&2; return 1 ;;
    esac
}

# spira_config_set_at <conf> <toml> <SPIRA_KEY> <value> — write one [spira] key into the
# config file at a GIVEN root; install.sh's cross-checkout instance seed uses this directly.
# ALWAYS spira.toml, THROUGH `spira-config set`, NEVER a hand-rolled writer of either format —
# see spira_toml_write_target_for above for why the target is resolved (and, if needed,
# created) before anything is written.
spira_config_set_at() {
    local conf="$1" toml="$2" key="$3" val="$4"
    _spira_config_write set "$(spira_toml_write_target_for "$conf" "$toml")" "$key" "$val"
}

# spira_config_set <SPIRA_KEY> <value> — write one key into THIS process's own config file in
# force. The ONE way any harness tool changes a persisted [spira] setting on its own root:
# aeons.sh's fleet-ceiling writer and deploy.sh's SPIRA_PROD writer used to hand-edit
# spira.conf directly, which is exactly the write spira_toml_resolve's auto-convert used to
# exist to survive. A spira.conf deleted out from under one of those one-key rewrites created
# a fresh, one-key spira.conf that the next read converted over a richer spira.toml — the
# "gutted spira.toml" failure this bead (sp-usxfl) retires.
spira_config_set() {
    _spira_config_write set "$(spira_toml_write_target)" "$1" "$2"
}

# spira_config_unset <SPIRA_KEY> — remove one key from THIS process's own config file in
# force, so it stops appearing rather than being left behind as an empty string (aeons.sh's
# `unset` command: the fleet ceiling reverts to "no cap" only when the key is truly gone).
spira_config_unset() {
    _spira_config_write unset "$(spira_toml_write_target)" "$1"
}

SPIRA_CONF_FILE="$(spira_conf_file)"
SPIRA_TOML_FILE="$(spira_toml_resolve)"

# --------------------------------------------------------------------------------------
# DERIVED DEFAULTS. spira_toml_read and spira_conf_defaults are retired (see the comment
# where they used to live, above this file's config-write functions): the whole
# env-over-toml-over-derived-default pipeline, ~300 keys deep, is now
# `spira_config::resolve()`, called here as a subprocess and `eval`'d into this shell
# (sp-ubcgo, "wave 4.5: conf.sh becomes an eval of resolve").
#
# `--sh-all`, NOT `--sh`: this shell SOURCES the result rather than inheriting it across an
# exec boundary, so every resolved key belongs here — including SPIRA_REPO_MAP, SPIRA_FAYTHS
# and SPIRA_MAX_AEONS, which `--sh`'s narrower, typed export set deliberately omits because
# those three must never reach a CHILD process (see "WHAT IS EXPORTED, AND WHAT MUST NEVER
# BE" below, which re-exports only that narrower set to this process's own children — the
# export list itself is unchanged by this bead).
#
# SPIRA_HOME/SPIRA_REPO ARE PASSED AS THIS ONE CALL'S OWN ENVIRONMENT, not by exporting them
# from this shell — they stay unexported here for the same per-copy reason they are derived,
# rather than configured, above. `spira-config` has no BASH_SOURCE to stand in for "where
# conf.sh sits"; only the caller that already derived both — this file — can tell it.
#
# FAIL CLOSED (sp-c7b85's rule, now covering every derived default, not only the toml ones):
# a missing or failing `spira-config` refuses outright, loudly, and this shell never ends up
# holding some keys resolved and the rest silently empty — the one failure mode this cutover
# must not reintroduce. `command -v` first distinguishes "not on PATH" (name SPIRA_RELEASE,
# the usual cause) from any other refusal, which `spira-config` already explains on its own
# stderr before exiting non-zero. `exit`, not `return`: a `return 1` from the last command of
# the `&&` that would otherwise call this only aborts a `set -e` CALLER that happens to check
# it (aerc/accept-default.sh) — `exit` holds whether or not the sourcing script opted into
# errexit.
if ! command -v spira-config >/dev/null 2>&1; then
    printf 'spira: spira-config not found on PATH — SPIRA_RELEASE is unset, or the launcher PATH omits the release, so configuration cannot be resolved from this box'"'"'s own tools\n' >&2
    exit 1
fi
if [ -n "$SPIRA_TOML_FILE" ]; then
    _spira_resolved="$(SPIRA_HOME="$SPIRA_HOME" SPIRA_REPO="$SPIRA_REPO" spira-config resolve --sh-all --conf-d "$_spira_conf_real_dir/conf.d" "$SPIRA_TOML_FILE")"
else
    _spira_resolved="$(SPIRA_HOME="$SPIRA_HOME" SPIRA_REPO="$SPIRA_REPO" spira-config resolve --sh-all --conf-d "$_spira_conf_real_dir/conf.d")"
fi
_spira_resolved_rc=$?
if [ "$_spira_resolved_rc" -ne 0 ]; then
    unset _spira_resolved _spira_resolved_rc
    exit 1
fi
eval "$_spira_resolved"
unset _spira_resolved _spira_resolved_rc

unset _spira_conf_here _spira_conf_home_env _spira_conf_real_dir

# --------------------------------------------------------------------------------------
# PATH. `bd`, `git`, `gh` and the configured agent CLI live wherever the operator put them,
# and everything here is invoked from systemd, where a login shell's PATH does not exist.
# Bootstrapping in one place is the difference between working and failing silently.
#
# THE LAUNCHER'S PATH COMES FIRST AND IS NEVER REWRITTEN (sp-gypjk): every Spira tool is
# invoked by bare name, and the launcher put the release's bin/ and spira/ at the front of
# PATH, so nothing this file adds can shadow a release tool. This file only APPENDS the
# box's own tail — SPIRA_PATH (the config's business), then ~/.local/bin, ~/.cargo/bin
# (cargo, for the gate and testenv that build a tree under test — it holds no Spira tool)
# and the system directories — each segment once, so re-sourcing does not grow PATH.
# --------------------------------------------------------------------------------------
_spira_path_seg=""
for _spira_path_seg in ${SPIRA_PATH//:/ } "$HOME/.local/bin" "$HOME/.cargo/bin" /usr/local/bin /usr/bin /bin; do
    case ":${PATH:-}:" in
        *":$_spira_path_seg:"*) ;;
        *) PATH="${PATH:+$PATH:}$_spira_path_seg" ;;
    esac
done
unset _spira_path_seg
export PATH

# SPIRA_BD — resolved once, deterministically, after SPIRA_PATH is applied. When the
# environment or a config file already set it (both captured before this point), the value
# survives unchanged. When neither did, it resolves to the first bd on the PATH this
# harness just assembled — rather than whatever PATH the calling context carries. PATH
# order differs between a login shell, a systemd unit and an aeon's confined environment,
# so a caller that fell through to ${SPIRA_BD:-bd} in lib.sh could silently pick a
# different binary in each context (sp-s2zvn, scar from 2026-09-08).
if [ -z "${SPIRA_BD:-}" ]; then
    SPIRA_BD="$(command -v bd 2>/dev/null || echo bd)"
fi
export SPIRA_BD

# BD SCHEMA REFUSAL. When the resolved bd's migration count disagrees with the database's,
# bd exits 0 with the complaint on stdout — callers that check exit status read success and
# parse the error as data. Catching it here, once, stops the mismatch from propagating to
# every bdq call. Only runs when the database is present; on a fresh install or in a test
# fixture that has not yet called testdb_up, $SPIRA_DB/.beads does not exist and the check
# is skipped entirely. bd's version string does not order against release tags — a dev build
# knows MORE migrations than a tagged release — so pin by migration count, not version string.
if [ -d "${SPIRA_DB:-}/.beads" ]; then
    # SCHEMA CHECK CACHE. Running `bd migrate schema` on every conf.sh source opens the
    # store once before any actual work — doubling the load. The check is cached by the
    # bd binary's mtime and size: if neither has changed since the last successful pass,
    # the cursor cannot have moved (migrations are applied by bd, not by the database alone).
    # Both fields are required: mtime alone has second-level resolution, so two distinct
    # binaries built in the same second but with different content share a mtime but differ
    # in size. The stamp file lives in SPIRA_RUN so it is instance-qualified.
    _spira_schema_stamp="${SPIRA_RUN}/bd-schema-stamp"
    _spira_bd_mtime="$(stat -c '%Y %s' "$SPIRA_BD" 2>/dev/null || true)"
    _spira_schema_cached=0
    if [ -n "$_spira_bd_mtime" ] && [ -f "$_spira_schema_stamp" ]; then
        _spira_stamp_val="$(cat "$_spira_schema_stamp" 2>/dev/null || true)"
        [ "$_spira_stamp_val" = "$_spira_bd_mtime" ] && _spira_schema_cached=1
    fi
    if [ "$_spira_schema_cached" = 0 ]; then
        _spira_bd_out="$(timeout 30 "$SPIRA_BD" -C "$SPIRA_DB" migrate schema 2>&1)"
        _spira_bd_rc=$?
        _spira_schema_ok=0
        if [ "$_spira_bd_rc" -eq 0 ]; then
            _spira_schema_ok=1
        elif grep -q 'dolt_server_port.*deprecated' <<< "$_spira_bd_out"; then
            # A deprecated field in metadata.json; schema is unaffected (sp-lh8r).
            _spira_schema_ok=1
        elif grep -q 'locked by another dolt process' <<< "$_spira_bd_out"; then
            # Lock contention means the store is in embedded mode — one exclusive lock,
            # many waiters. Embedded mode is refused by doctor; run it to diagnose.
            printf 'spira: bd locked — store is in embedded mode (dolt_mode); run doctor\n' >&2
            [ -z "${SPIRA_DOCTOR:-}" ] && { unset _spira_bd_out _spira_bd_rc _spira_schema_ok; exit 1; }
        else
            _spira_bd_db="$(printf '%s\n' "$_spira_bd_out" | grep -oE 'database is at v[0-9]+' | grep -oE '[0-9]+')"
            _spira_bd_bin="$(printf '%s\n' "$_spira_bd_out" | grep -oE 'binary knows up to v[0-9]+' | grep -oE '[0-9]+')"
            # Report both versions, rendering ? when one cannot be read. (sp-1khst)
            if [ -n "${_spira_bd_db:-}" ] || [ -n "${_spira_bd_bin:-}" ]; then
                printf 'spira: bd schema mismatch — database is at v%s, %s knows up to v%s\n' \
                    "${_spira_bd_db:-?}" "$SPIRA_BD" "${_spira_bd_bin:-?}" >&2
                printf 'spira: rebuild bd at v%s or set SPIRA_BD in %s\n' \
                    "${_spira_bd_db:-?}" "${SPIRA_CONF_FILE:-spira.conf}" >&2
            else
                printf 'spira: bd migrate schema failed — %s\n' \
                    "$(printf '%s\n' "$_spira_bd_out" | head -1)" >&2
                printf 'spira: bd is %s\n' "$SPIRA_BD" >&2
            fi
            unset _spira_bd_db _spira_bd_bin
            # SPIRA_DOCTOR=1 means doctor is the caller; it runs its own schema check.
            [ -z "${SPIRA_DOCTOR:-}" ] && { unset _spira_bd_out _spira_bd_rc _spira_schema_ok; exit 1; }
        fi
        unset _spira_bd_out _spira_bd_rc
        # Write the stamp only after a successful check (best-effort; failure is silent).
        if [ "${_spira_schema_ok:-0}" = 1 ] && [ -n "$_spira_bd_mtime" ] && mkdir -p "$SPIRA_RUN" 2>/dev/null; then
            printf '%s\n' "$_spira_bd_mtime" > "$_spira_schema_stamp" 2>/dev/null || true
        fi
        unset _spira_schema_ok
    fi
    unset _spira_schema_stamp _spira_bd_mtime _spira_schema_cached _spira_stamp_val
fi

# BD_IGNORE_SCHEMA_SKEW WAS EXPORTED HERE AND IS GONE, because the recovery it was waiting on
# has run. The database was at schema v61, migrated by the accidental v1.2.0/v1.2.1 release,
# and every bd newer than v1.1.0 refused it outright — a refusal that EXITS 0 with the
# complaint on stdout, so a caller checking status read success and parsed an error as data.
# On 2026-09-08 a CGO rebuild of bd landed in ~/.local/bin, fayth_ready returned nothing, the
# sentinel logged "no fayth in the chamber" for every persona, and Spira summoned nothing for
# six minutes while the panes showed 0 ready rather than a fault.
#
# The rollback (sp-6ylz, 2026-09-08 18:14) ran and was reverted three minutes later at 18:17:
# at v53 every write to the production database failed. The cursor has been at v61 since 18:17.
# Reads and writes work without BD_IGNORE_SCHEMA_SKEW because this bd is a CGO_ENABLED=0
# (server-mode) dev build from main that knows all 61 migrations — NOT because the cursor
# moved. The rule the scar teaches: bd's version string does not order against release tags.
# A dev build from main knows MORE migrations than a tagged release (v1.2.2 knows only 53),
# so pin by migration count, never by version string. See spira/bd-pin.sh and SPIRA_BD_PIN.
# The revert is in the dolt log of database spira: commit piud5v03ri22ktnshufhl5dk09m24f5b,
# 'revert: restore schema cursor to v61 — rollback broke writes (bd 1.1.0 knows 61, not 53)'.

# --------------------------------------------------------------------------------------
# WHAT IS EXPORTED, AND WHAT MUST NEVER BE.
#
# EXPORTED, because the readers are not all shell: a python one-liner, an aeon's session and
# the Rust attention panel are all children of a process that sourced this file, and the
# alternative — threading each value through argv at every call site — is where one gets
# dropped and the tool silently reads a different database.
#
# NOT EXPORTED: EVERYTHING DERIVED FROM WHERE THIS FILE SITS. SPIRA_HOME, SPIRA_REPO, the
# maps, the chamber, the cockpit, the runtime directory. Those differ per COPY of the
# harness, and anything that sources its own conf.sh must resolve them from its own location.
# Exporting them cost eleven tests in one go: a fixture library, sourced by a suite for its
# database helpers, published the live installation's SPIRA_HOME into the environment of the
# sentinel that suite then ran under a temporary one — so the sentinel read the operator's
# REAL repo-map, found none of the fixture's repositories, and landed nothing, while every
# log line it printed looked ordinary (law-gates-run-in-a-clean-environment).
#
# It is safe to export the rest precisely because the landing gate runs its trial under
# `env -i`: configuration reaches the harness's own children and stops at the boundary of
# anything being judged.
#
# SPIRA_FAYTHS and SPIRA_MAX_AEONS are also not exported. They are policy for THIS host, read
# in-process, and exporting SPIRA_FAYTHS is the scar that statute is named for — it reached a
# test suite through systemd and made the suite assert the host's roster instead of the
# defaults it was written to check.

# --------------------------------------------------------------------------------------
# THE EXIT STATUS THAT MEANS "NO VERDICT", as opposed to "failed". The landing gate withholds
# a verdict when it cannot obtain the tree it judges in, and a caller that reads that as a red
# gate charges a queue to the branch — reopening a finished bead as having "failed the landing
# gate", three of which poison it and escalate to the operator over a lock it never contended
# for. It is a shared constant rather than a literal in the gate and again in its caller,
# because two programs that disagree about this number fail in exactly that direction.
#
# DELIBERATELY NOT SETTABLE, and so not in SPIRA_CONF_KEYS: it is the protocol between the
# gate and whoever runs it, not a fact about a host. A configurable one could be set to 0,
# which would turn every withheld verdict into a pass. 75 is EX_TEMPFAIL — "try again" —
# and is outside the range a gate command of its own would return.
SPIRA_GATE_NOVERDICT=75

# FOUR OUTCOMES, AND WHOSE FAULT EACH ONE IS. Every judgement in this harness returns exactly
# one of these, and the caller's whole decision follows from which. Before this there were
# two — 0 and "non-zero" — so a gate that timed out, a gate that could not find its own
# worktree, and a repository whose suites fail on its own base were all delivered to the
# landing pass as "this branch is broken". Each was then fixed by adding one more special
# case at the caller, and the shape recurred five times in two days (sp-p4rl, sp-d21,
# sp-io5j, sp-snyj, sp-1aex).
#
#   PASS       0   judged, and good                     -> land it
#   FAIL       1   judged, and bad                      -> the BRANCH is at fault
#   BASE_FAIL 76   the same suites fail on the base     -> the BASE is at fault
#   NO_VERDICT 75  not judged at all                    -> the MACHINERY is at fault
#
# THE RULE THAT MATTERS: BASE_FAIL and NO_VERDICT may never reopen a bead and never charge an
# attempt. Three charged attempts poison a bead and escalate to the operator, so a lock, a
# timeout or somebody else's red main could — and repeatedly did — walk finished work to
# poison and then report it as the branch's failure.
#
# A JUDGEMENT WITH NO EVIDENCE IS NO_VERDICT BY CONSTRUCTION. A gate killed at its deadline
# printed an empty `tail -20`, which arrived as a bare "failed" with nothing in it to act on
# (sp-p4rl); the one refusal path that printed nothing at all did the same (sp-io5j). FAIL
# has to be able to show its work or it is not a FAIL.
#
# 76 is EX_UNAVAILABLE — "the service is not available" — which is precisely the claim: the
# base this branch must merge into is not in a fit state to judge against.
SPIRA_GATE_BASEFAIL=76

# The names, for logs and for the bead notes a human reads. Keyed by status so that a caller
# that has a number can always render the word, and one place decides the wording.
spira_gate_outcome() {   # spira_gate_outcome <status> -> PASS|FAIL|BASE_FAIL|NO_VERDICT
    case "${1:-}" in
        0)  echo PASS ;;
        75) echo NO_VERDICT ;;
        76) echo BASE_FAIL ;;
        *)  echo FAIL ;;
    esac
}

# Does this outcome say the BRANCH is at fault? The one question every caller actually asks,
# in one place, so that "may I reopen the bead and charge an attempt?" cannot drift between
# the landing pass, the sentinel and any future caller.
spira_gate_blames_branch() {   # spira_gate_blames_branch <status> -> 0 if the branch is at fault
    case "${1:-}" in
        0|75|76) return 1 ;;
        *)       return 0 ;;
    esac
}

# --------------------------------------------------------------------------------------
# SPIRA_ID_PREFIX and every SPIRA_MAIL* key joined this list for sp-ooh1k: `mail` is a
# compiled binary now and cannot re-derive them the way mail.sh did, by sourcing this file
# itself in its own process. A bash caller that pins a non-default value (a test fixture,
# `SPIRA_MAIL_LOCAL_TIDY_FRESH=...`) must still see `mail` agree — this is not the
# SPIRA_HOME/SPIRA_REPO fence above SPIRA_CONF_KEYS: those are facts about where the caller
# itself lives, never configuration: these are ordinary operational config, exactly like
# SPIRA_RUN and SPIRA_DB already exported below.
export COCKPIT_BOTTOM_PCT \
    COCKPIT_CLIENT_IDLE_SECS \
    COCKPIT_CLIPBOARD \
    COCKPIT_CWD \
    COCKPIT_DB \
    COCKPIT_MAIL \
    COCKPIT_MOUSE \
    COCKPIT_RIGHT_PCT \
    SPIRA_ALERT_GLOB \
    SPIRA_ASK_LABEL \
    SPIRA_BD \
    SPIRA_CI_LABEL \
    SPIRA_CI_PARK_MAX \
    SPIRA_CONF_FILE \
    SPIRA_CTRL \
    SPIRA_CUTOVER_ROUND_LABEL \
    SPIRA_DB \
    SPIRA_DESIGN \
    SPIRA_DOLT_DATA \
    SPIRA_EXPORTER \
    SPIRA_EXPRESS_LABEL \
    SPIRA_FLAKY_GH_REPO \
    SPIRA_GATE_BASEFAIL \
    SPIRA_GATE_NOVERDICT \
    SPIRA_GH_INTAKE_BEAD_REPO \
    SPIRA_GH_INTAKE_PRIORITY \
    SPIRA_GH_INTAKE_REPO \
    SPIRA_GROOM_ASK_LABEL \
    SPIRA_ID_PREFIX \
    SPIRA_INCIDENT_LABEL \
    SPIRA_INCIDENT_PRIORITY \
    SPIRA_INSTANCE \
    SPIRA_LAND_GATE_RESERVE \
    SPIRA_LAND_MAXSEC \
    SPIRA_LC_SOCKET \
    SPIRA_LC_TESTDB_DATA \
    SPIRA_LC_TESTDB_PORT \
    SPIRA_LC_UNIX_GROUP \
    SPIRA_LC_UNIX_USER \
    SPIRA_LIFECYCLE_ENFORCE \
    SPIRA_LOOM_ADDR \
    SPIRA_LOOM_BUDGET_MS \
    SPIRA_LOOM_CACHE_S \
    SPIRA_LOOM_READY_GRACE \
    SPIRA_MAIL \
    SPIRA_MAIL_INDEX \
    SPIRA_MAIL_KINDS \
    SPIRA_MAIL_MUTE \
    SPIRA_MAIL_REPEAT_WINDOW \
    SPIRA_MAIL_SESSION_MAILBOX \
    SPIRA_MAIL_TIDY_FRESH \
    SPIRA_MIRROR \
    SPIRA_NO_LOOP_LABEL \
    SPIRA_OPERATOR \
    SPIRA_OPERATOR_ACTOR \
    SPIRA_PATH \
    SPIRA_PLAN_LABEL \
    SPIRA_PROD \
    SPIRA_RECLAIM_GRACE_SECS \
    SPIRA_RELEASES \
    SPIRA_RELEASE_REPO \
    SPIRA_REVIEWER_MODEL \
    SPIRA_REVIEWER_VERDICTS \
    SPIRA_REVIEW_LABEL \
    SPIRA_RUN \
    SPIRA_SCOPE_LABEL \
    SPIRA_SPIKE_DIR \
    SPIRA_SPIKE_LABEL \
    SPIRA_SPIKE_PATHS \
    SPIRA_SUBMITTED_LABEL \
    SPIRA_SYSTEMCTL \
    SPIRA_TESTDB_BD \
    SPIRA_TESTDB_DATA \
    SPIRA_TESTDB_PORT \
    SPIRA_TESTENV_MAX_CONCURRENT \
    SPIRA_TESTENV_QUEUE_POLL \
    SPIRA_TESTENV_QUEUE_TIMEOUT \
    SPIRA_TESTENV_REGISTRY \
    SPIRA_TOML_FILE \
    SPIRA_TOWN \
    SPIRA_TZ \
    SPIRA_WIKI \
    SPIRA_WIKI_HOOK \
    SPIRA_WORKSPACES \
    SPIRA_WORK_CLOSE_TYPES

# --------------------------------------------------------------------------------------
# NAME WHAT IS MISSING. A harness that dies with `bd: command not found` from a timer has
# told the operator nothing: not which program, not what it is for, not where to get it.
#
# `spira_require` is cheap enough to call at the top of anything — it is a `command -v` —
# and it is the only reason a fresh box gets a sentence instead of a shell error.
# --------------------------------------------------------------------------------------
spira_require() {        # spira_require <bin> [<bin>...] -> 0, or 1 having named each one
    local b missing=""
    for b in "$@"; do command -v "$b" >/dev/null 2>&1 || missing="$missing $b"; done
    [ -z "$missing" ] && return 0
    for b in $missing; do
        printf 'spira: required program not found on PATH: %s — %s\n' \
            "$b" "$(spira_bin_purpose "$b")" >&2
    done
    printf 'spira: PATH is %s\n' "$PATH" >&2
    printf 'spira: if it is installed elsewhere, set SPIRA_PATH in %s\n' \
        "${SPIRA_CONF_FILE:-spira.conf}" >&2
    return 1
}

# --------------------------------------------------------------------------------------
# THE DEPENDENCY MANIFEST — three functions backed by deps.toml.
#
#   spira_bin_purpose <bin>   what it is for
#   spira_bin_tier    <bin>   runtime | optional | dev | operator
#   spira_bin_absent  <bin>   what actually happens on a box without it
#   spira_deps_list [tier]    emit all known program names, optionally filtered
#
# The data lives in deps.toml (same directory as this file), loaded once at
# source time into per-program shell variables. doctor reads from these
# rather than carrying its own hardcoded lists.
# --------------------------------------------------------------------------------------

_SPIRA_DEPS="$SPIRA_HOME/deps.toml"
_spira_dep_names=""

# Load all dep fields from deps.toml with one python3 call.
if [ -f "$_SPIRA_DEPS" ] && command -v python3 >/dev/null 2>&1; then
    _spira_deps_raw="$(python3 - "$_SPIRA_DEPS" 2>/dev/null <<'_DEPS_PY'
import sys, tomllib, shlex
with open(sys.argv[1], "rb") as f:
    data = tomllib.load(f)
names = []
for d in data.get("dep", []):
    n = d["name"].replace("-", "_")
    names.append(d["name"])
    print(f"_spira_dep_tier_{n}={shlex.quote(d.get('tier', 'optional'))}")
    print(f"_spira_dep_purpose_{n}={shlex.quote(d.get('purpose', 'required by the harness'))}")
    print(f"_spira_dep_absent_{n}={shlex.quote(d.get('absent', ''))}")
print(f"_spira_dep_names={shlex.quote(' '.join(names))}")
_DEPS_PY
)"
    while IFS= read -r _spira_deps_line; do
        eval "$_spira_deps_line"
    done <<< "$_spira_deps_raw"
    unset _spira_deps_raw _spira_deps_line
fi

spira_deps_list() {
    local _tier="${1:-}" _b
    for _b in $_spira_dep_names; do
        if [ -z "$_tier" ]; then
            printf '%s\n' "$_b"
        else
            local _k="_spira_dep_tier_${_b//-/_}"
            [ "${!_k:-optional}" = "$_tier" ] && printf '%s\n' "$_b"
        fi
    done
}

spira_bin_tier() {
    local _k="_spira_dep_tier_${1//-/_}"; echo "${!_k:-optional}"
}

spira_bin_purpose() {
    local _k="_spira_dep_purpose_${1//-/_}"; echo "${!_k:-required by the harness}"
}

spira_bin_absent() {
    local _k="_spira_dep_absent_${1//-/_}"; echo "${!_k:-}"
}

# watch_unit_name <watcher-name> -> the installed systemd unit name for a daemon watcher.
#
# MIRRORS inst_watch_name IN systemd/units.sh — one formula, two callers. A watcher named
# "answers" installs as spira-watch-answers-prod.service (not spira-watch@answers.service,
# which is the template form and is never instantiated). Querying the template form always
# returns 'inactive', so a stopped watcher and a running watcher are indistinguishable.
# Every caller that queries or restarts a watcher unit goes through this function so the two
# cannot drift.
watch_unit_name() { printf 'spira-watch-%s-%s.service' "$1" "${SPIRA_INSTANCE:-prod}"; }

# spira_unit <base> [service|timer] -> the unit name for this installation.
# Tries the instance-qualified form first (spira-<base>-<instance>.<type>); if that
# unit is not loaded (neither enabled nor active), falls back to the plain form.
# Returns '?' if neither form is known to systemd — a unit that cannot be found must
# not be queried for health, which would report 'inactive' about an unrelated subject
# (law-absence-needs-a-positive-control). Callers must treat '?' as unknown state.
#
# This is the same resolution the TIMER_BASES loop in world.sh uses, extracted so that
# every caller agrees on which name to address rather than each hard-coding one form.
spira_unit() {
    local base="$1" t="${2:-service}"
    local SC="${SPIRA_SYSTEMCTL:-systemctl}"
    local inst="spira-${base}${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.${t}"
    local plain="spira-${base}.${t}"
    if "$SC" --user is-enabled "$inst" >/dev/null 2>&1 ||
       "$SC" --user is-active  "$inst" >/dev/null 2>&1; then
        printf '%s' "$inst"
    elif "$SC" --user is-enabled "$plain" >/dev/null 2>&1 ||
         "$SC" --user is-active  "$plain" >/dev/null 2>&1; then
        printf '%s' "$plain"
    else
        printf '?'
    fi
}

# conf_changed <path> <mtime0> -> 0 if <path>'s mtime now differs from <mtime0>, 1 otherwise.
# The one shared implementation of law-long-lived-processes-pin-their-config's watch loop:
# health.sh, loom.sh and collect.sh each capture their own mtime0 at startup and poll this
# on their own tick, rather than each repeating the same `stat` comparison.
conf_changed() {   # conf_changed <path> <mtime0>
    local path="$1" mtime0="$2"
    [ -n "$path" ] || return 1
    [ "$(stat --format='%Y' "$path" 2>/dev/null || echo 0)" != "$mtime0" ]
}
