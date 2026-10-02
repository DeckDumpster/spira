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
# prints nothing and reports the failure on stderr. Factored out of spira_toml_resolve so its
# two call sites (the conf-pinned case and the ambient one, below) share the exact same
# conversion, never a narrower one that drops the repo-map or a fayth and so, per
# `spira-config convert`'s own refusal, fails closed against a target that already has more
# (sp-zs04v.2) rather than by construction here.
#
# ONLY spira_toml_resolve CALLS THIS NOW (wave 4.7, sp-ksrss retired the other caller): a
# writer that must create ANOTHER root's spira.toml directly — install.sh's cross-checkout
# instance seed, historically — is `install::seed_instance` now, which calls
# `spira-config convert` itself and never sourced conf.sh's bash helpers to begin with (see
# that crate's own `write_target_for`). spira_toml_resolve, the one remaining caller,
# resolves an AMBIENT (not explicitly-given) target and applies spira_config_writeback
# itself before calling in — the redirect guards against a worktree regenerating some
# ambiguous default path.
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
# NO LONGER REGENERATES [persona.*] FROM A FRESHER FAYTH (sp-35ru0, wave 4.7: stopped here,
# not ported). Until this bead, a fayth file newer than $toml made this function — called
# merely to find out WHICH file to read, at the top of every `conf.sh` source — rewrite the
# operator's real spira.toml in place. Every release touches chamber/*.fayth, so every
# activation or landing rewrote it: content preserved, but the mtime churn alone caused
# test-literal-lint nondeterminism (sp-g9f3t) and this function's own comments already
# recorded a case that gutted a spira.toml from the read-side twin of this hazard. Runtime
# must never write operator config merely because it was asked to READ it — persona models
# live in spira.toml as the operator sets them (`spira-config set`), and
# `spira-config::chamber::persona_model` has read the toml table directly, with no fayth
# fallback, since wave 4.22 (sp-r5zd2), so nothing here depends on this file staying in sync
# with FAYTH_MODEL at launch. A fayth/toml disagreement is now silent rather than
# auto-resolved by rewriting — reporting it is doctor's job, not conf.sh's, and is not yet
# built (left for a follow-up: doctor has no check for it today).
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

    # A toml that already exists wins unconditionally, exactly as the comment above this
    # function's definition says (sp-usxfl) — no mtime comparison against the chamber, no
    # regenerate, whether or not a fayth is newer. Nothing to read yet is reported as
    # nothing, same as always; the caller's own fail-closed guard (conf.sh's "DERIVED
    # DEFAULTS" block, further down this file) is what notices.
    [ -n "$toml" ] && printf '%s' "$toml"
    return 0
}

# spira_toml_read AND spira_conf_defaults ARE RETIRED (sp-ubcgo, "wave 4.5: conf.sh becomes
# an eval of resolve"), not ported: between them they read spira.toml field-by-field and then
# hand-computed ~300 keys' worth of env-over-toml-over-default precedence, and that whole
# pipeline is now `spira_config::resolve()`, called once as a subprocess and `eval`'d into
# this shell below ("DERIVED DEFAULTS", past the config-write functions further down this
# file). `_spira_join` went with them — its three call sites were all inside the retired
# function and nothing else used it.

# --------------------------------------------------------------------------------------
# CONFIG WRITES (wave 4.7, sp-ksrss). Everything below writes the operator's spira.toml —
# never spira.conf — and resolves what it is actually about to write through
# `spira-config set`/`unset` (atomic temp-file-plus-rename, same as `convert`'s own writer).
#
# RETIRED RATHER THAN PORTED: `spira_single_checkout` (split- vs. single-checkout, once used
# only through doctor's promote section) and the cross-root pair `spira_toml_write_target_for`
# / `spira_config_set_at` (install.sh's cross-checkout instance seed) have zero remaining
# bash callers — grepped across the whole tree twice. doctor's own checkout-mode logic and
# the instance seed are both native Rust now (`install::seed_instance::write_target_for`),
# and neither ever called back into conf.sh's bash helpers to begin with.

# spira_config_writeback <candidate> — the ONE path any code that would REGENERATE a
# config file (spira.toml, a converted spira.conf, ...) resolves its write target through.
# The decision itself — same-checkout vs. this worktree's own SPIRA_REPO vs.
# XDG_CONFIG_HOME vs. a scratch file — now lives in `spira_config::writeback::writeback`;
# this is a one-line shim passing the ambient facts a subprocess cannot see for itself
# (SPIRA_HOME/SPIRA_PROD/SPIRA_REPO are deliberately unexported, like every other per-copy
# fact this file derives, so a subprocess sees none of them unless named on the call).
#
# scar: three cutover branches, each sourcing their own conf.sh with the operator's real
# HOME, regenerated the operator's real spira.toml from worktree state — three times in one
# day, each time blinding queue-watch until the file was restored by hand. A worktree has no
# business writing outside itself, however it got HOME. Full rationale — the single-checkout
# exemption, the sp-jv49c unwritable-SPIRA_REPO case, and why the scratch-file fallback must
# always succeed — is on the Rust side now; see that module's own comment.
spira_config_writeback() {
    SPIRA_CONFIG_WRITE="${SPIRA_CONFIG_WRITE:-0}" SPIRA_HOME="${SPIRA_HOME:-}" \
        SPIRA_PROD="${SPIRA_PROD:-}" SPIRA_REPO="${SPIRA_REPO:-}" HOME="$HOME" \
        XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-}" spira-config writeback "$1"
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
# `spira-config set`/`unset` are where the actual file mutation happens, atomically.
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

# spira_config_set <SPIRA_KEY> <value> — write one key into THIS process's own config file in
# force. deploy.sh's SPIRA_PROD writer is the live bash caller (aeons.sh's fleet-ceiling
# writer used to be the other one; the native `aeons` binary now calls `spira_config::set_path`
# directly and never sources conf.sh for this). Both used to hand-edit spira.conf directly,
# which is exactly the write spira_toml_resolve's auto-convert used to exist to survive. A
# spira.conf deleted out from under one of those one-key rewrites created a fresh, one-key
# spira.conf that the next read converted over a richer spira.toml — the "gutted spira.toml"
# failure this bead (sp-usxfl) retires.
spira_config_set() {
    _spira_config_write set "$(spira_toml_write_target)" "$1" "$2"
}

# spira_config_unset <SPIRA_KEY> — remove one key from THIS process's own config file in
# force, so it stops appearing rather than being left behind as an empty string. Symmetric
# with spira_config_set above; kept as the generic primitive even though its own original
# caller (aeons.sh's `unset` command) has since moved to the native `aeons` binary too.
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
# stderr before exiting non-zero.
#
# `return`, NOT `exit` (unlike spira_toml_read's old exit-127 case this replaces): a handful
# of callers — schema.sh, schema-apply.sh, configure.sh — source conf.sh as
# `. conf.sh || true` ON PURPOSE, because they have their own bash-level fallback for every
# value they read (schema_name's `${SPIRA_ASK_LABEL:-needs-operator}`, for one) and would
# rather degrade than die when the box's spira-config cannot be resolved. `exit` kills the
# whole process before that guard ever runs — a sourced script's `exit` is not catchable by
# `||`, only a `return` is — so it defeated every one of those callers' own explicit choice
# (sp-ubcgo: caught by the gate's own literal-lint step misreading schema.sh's fallback
# names as real ones). A caller that does NOT guard the `.`/`source` with `||` still sees
# this non-zero and, immediately after, every derived key unset — not a quieter failure,
# a differently-shaped one: this shell never holds a PARTIALLY resolved value either way.
if ! command -v spira-config >/dev/null 2>&1; then
    printf 'spira: spira-config not found on PATH — SPIRA_RELEASE is unset, or the launcher PATH omits the release, so configuration cannot be resolved from this box'"'"'s own tools\n' >&2
    return 1
fi
if [ -n "$SPIRA_TOML_FILE" ]; then
    _spira_resolved="$(SPIRA_HOME="$SPIRA_HOME" SPIRA_REPO="$SPIRA_REPO" spira-config resolve --sh-all --conf-d "$_spira_conf_real_dir/conf.d" "$SPIRA_TOML_FILE")"
else
    _spira_resolved="$(SPIRA_HOME="$SPIRA_HOME" SPIRA_REPO="$SPIRA_REPO" spira-config resolve --sh-all --conf-d "$_spira_conf_real_dir/conf.d")"
fi
_spira_resolved_rc=$?
if [ "$_spira_resolved_rc" -ne 0 ]; then
    unset _spira_resolved _spira_resolved_rc
    return 1
fi
eval "$_spira_resolved"
unset _spira_resolved _spira_resolved_rc

unset _spira_conf_here _spira_conf_home_env _spira_conf_real_dir

# --------------------------------------------------------------------------------------
# ENVIRONMENT BOOTSTRAP (sp-kfimz, "wave 4.6: environment bootstrap into spira-config"):
# the PATH tail, SPIRA_BD resolution and the bd schema preflight — previously three blocks
# of bash here — are now one call each into spira-config, so this logic exists exactly
# once rather than in bash and in a Rust re-implementation that could drift from it.
#
# THE LAUNCHER'S PATH COMES FIRST AND IS NEVER REWRITTEN (sp-gypjk): every Spira tool is
# invoked by bare name, and the launcher put the release's bin/ and spira/ at the front of
# PATH. `env-bootstrap` only APPENDS the box's own tail — SPIRA_PATH (the config's
# business), then ~/.local/bin, ~/.cargo/bin (cargo, for the gate and testenv that build a
# tree under test — it holds no Spira tool) and the system directories — each segment
# once, so re-sourcing does not grow PATH. It then resolves SPIRA_BD from that PATH, unless
# the environment or spira.toml already gave it one (sp-s2zvn, scar from 2026-09-08: a bare
# ${SPIRA_BD:-bd} fallback in lib.sh could silently pick a different binary depending on
# whether the caller was a login shell, a systemd unit or an aeon's confined environment —
# all three disagree about PATH order).
#
# PATH, HOME and SPIRA_PATH are passed on THIS ONE CALL'S OWN ENVIRONMENT, not by exporting
# them first — SPIRA_PATH is resolved above but not yet `export`ed (that happens only in
# the explicit export list below), so a plain inherited environment would not carry it to
# a child process. Same reasoning as SPIRA_HOME/SPIRA_REPO's own per-call passing, above.
#
# FAIL CLOSED, matching the guard above: `spira-config` was already confirmed present on
# PATH before this file got this far, so only a crash or a truly empty result can leave
# PATH/SPIRA_BD unset — checking the exit code here, rather than trusting empty output to
# mean "nothing to do", is what keeps that failure loud instead of silent.
_spira_env_bootstrap="$(PATH="$PATH" HOME="$HOME" SPIRA_PATH="${SPIRA_PATH:-}" SPIRA_BD="${SPIRA_BD:-}" spira-config env-bootstrap --sh)"
_spira_env_bootstrap_rc=$?
if [ "$_spira_env_bootstrap_rc" -ne 0 ]; then
    unset _spira_env_bootstrap _spira_env_bootstrap_rc
    printf 'spira: spira-config env-bootstrap failed — PATH/SPIRA_BD could not be resolved\n' >&2
    exit 1
fi
eval "$_spira_env_bootstrap"
unset _spira_env_bootstrap _spira_env_bootstrap_rc

# BD SCHEMA REFUSAL. When the resolved bd's migration count disagrees with the database's,
# bd exits 0 with the complaint on stdout — callers that check exit status read success and
# parse the error as data. `spira-config check-bd` is the cached preflight that catches it
# once, here, before the mismatch can propagate to every bdq call; see its own doc
# (spira-config/src/env_bootstrap.rs) for the cache key, the locked-dolt tolerance and
# SPIRA_DOCTOR's exemption, all preserved exactly. `exit`, NOT `return` (unlike the
# "spira-config not found" guard above): a schema mismatch ends the WHOLE process that
# sourced conf.sh, even for the handful of callers that source it as `. conf.sh || true` —
# unchanged from this check's own behaviour before this bead.
SPIRA_BD="$SPIRA_BD" SPIRA_DB="${SPIRA_DB:-}" SPIRA_RUN="${SPIRA_RUN:-}" \
    SPIRA_DOCTOR="${SPIRA_DOCTOR:-}" SPIRA_CONF_FILE="${SPIRA_CONF_FILE:-spira.conf}" \
    spira-config check-bd || exit 1

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
# THE GATE OUTCOME PROTOCOL (wave4-decomposition.md row C5; bead sp-wqj3o, "wave 4.10: small
# conf.sh families") — SPIRA_GATE_NOVERDICT/SPIRA_GATE_BASEFAIL now come from the DERIVED
# DEFAULTS eval below (`spira_config::resolve()`, which computes them as FIXED values —
# never settable, by the same rule this comment used to state here: a configurable
# NO_VERDICT could be set to 0, turning every withheld verdict into a pass). See that
# module's doc for the four outcomes and why BASE_FAIL/NO_VERDICT may never reopen a bead or
# charge an attempt (sp-p4rl, sp-d21, sp-io5j, sp-snyj, sp-1aex).
#
# spira_gate_outcome/spira_gate_blames_branch ARE RETIRED, NOT PORTED: yield.sh was their
# only remaining bash caller (grepped across the whole tree twice) — every other caller
# ported its own copy of this same logic to Rust already (gate::engine::outcome,
# landing-pass::model::GateOutcome, queue::ops::simple::gate_outcome). yield.sh now carries
# these two functions itself.

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
    SPIRA_TESTENV_CPUS \
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
#
# WAVE 4.10 (sp-wqj3o): one-line shim onto `spira-config deps require`, which does the
# `command -v` search and prints the same diagnostic itself, in-process. SPIRA_HOME and
# SPIRA_CONF_FILE are passed on THIS ONE CALL'S OWN ENVIRONMENT, not by exporting them first
# — same reasoning as every other `spira-config` call this file already makes.
# --------------------------------------------------------------------------------------
spira_require() {        # spira_require <bin> [<bin>...] -> 0, or 1 having named each one
    SPIRA_HOME="$SPIRA_HOME" SPIRA_CONF_FILE="${SPIRA_CONF_FILE:-spira.conf}" \
        spira-config deps require "$@"
}

# --------------------------------------------------------------------------------------
# THE DEPENDENCY MANIFEST — four functions, now one-line shims onto `spira-config deps`
# (wave4-decomposition.md row C7; bead sp-wqj3o, "wave 4.10: small conf.sh families").
#
#   spira_bin_purpose <bin>   what it is for
#   spira_bin_tier    <bin>   runtime | optional | dev | operator
#   spira_bin_absent  <bin>   what actually happens on a box without it
#   spira_deps_list [tier]    emit all known program names, optionally filtered
#
# RETIRED RATHER THAN PORTED: the `python3 -c 'import tomllib'` subshell that used to parse
# deps.toml into `_spira_dep_*` shell variables at EVERY source of this file, guarded by
# `command -v python3` — a box without python3 silently lost this whole family. `spira-
# config deps` (`spira-config/src/deps.rs`) parses deps.toml itself (the `toml` crate,
# already a dependency of this binary) and reads it lazily, only when one of these four is
# actually called, so nothing here depends on python3 at all now.
# --------------------------------------------------------------------------------------
spira_deps_list() {      # spira_deps_list [tier] -> every declared program, optionally filtered
    SPIRA_HOME="$SPIRA_HOME" spira-config deps list "$@"
}

spira_bin_tier() {        # spira_bin_tier <bin> -> its deps.toml tier, or 'optional'
    SPIRA_HOME="$SPIRA_HOME" spira-config deps tier "$1"
}

spira_bin_purpose() {     # spira_bin_purpose <bin> -> its deps.toml purpose, or the fallback sentence
    SPIRA_HOME="$SPIRA_HOME" spira-config deps purpose "$1"
}

spira_bin_absent() {      # spira_bin_absent <bin> -> what happens on a box without it, or empty
    SPIRA_HOME="$SPIRA_HOME" spira-config deps absent "$1"
}

# watch_unit_name <watcher-name> -> the installed systemd unit name for a daemon watcher.
#
# MIRRORS inst_watch_name IN systemd/units.sh — one formula, two callers. A watcher named
# "answers" installs as spira-watch-answers-prod.service (not spira-watch@answers.service,
# which is the template form and is never instantiated). Querying the template form always
# returns 'inactive', so a stopped watcher and a running watcher are indistinguishable.
# Every caller that queries or restarts a watcher unit goes through this function so the two
# cannot drift.
#
# WAVE 4.10 (sp-wqj3o): one-line shim onto `spira-config unit --watch` (pure string
# formatting in Rust now; see spira-config/src/unit.rs — no systemctl call either side).
watch_unit_name() { SPIRA_INSTANCE="${SPIRA_INSTANCE:-}" spira-config unit --watch "$1"; }

# spira_unit <base> [service|timer] -> the unit name for this installation.
# Tries the instance-qualified form first (spira-<base>-<instance>.<type>); if that
# unit is not loaded (neither enabled nor active), falls back to the plain form.
# Returns '?' if neither form is known to systemd — a unit that cannot be found must
# not be queried for health, which would report 'inactive' about an unrelated subject
# (law-absence-needs-a-positive-control). Callers must treat '?' as unknown state.
#
# This is the same resolution the TIMER_BASES loop in world.sh uses, extracted so that
# every caller agrees on which name to address rather than each hard-coding one form.
#
# WAVE 4.10 (sp-wqj3o): one-line shim onto `spira-config unit`, which runs the same
# `systemctl --user is-enabled|is-active` queries in-process (spira-config/src/unit.rs).
# cockpit-collect, the one in-process Rust caller, calls that module directly rather than
# through this shim or a bash bridge (its own `unit_active_key`).
spira_unit() {
    SPIRA_INSTANCE="${SPIRA_INSTANCE:-}" SPIRA_SYSTEMCTL="${SPIRA_SYSTEMCTL:-}" \
        spira-config unit "$1" "${2:-service}"
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
