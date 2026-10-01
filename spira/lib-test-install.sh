#!/usr/bin/env bash
# lib-test-install.sh — the install.sh test fixture every suite in this area hand-built:
# a symlinked systemd/+spira/ tree, and a cached `--render` so the templates are not
# recompiled by every suite (and every scenario within a suite) that needs a "nothing
# changed" DEST baseline (cluster 11, docs/test-plan/instance-lifecycle.md).
#
# Sourced, never executed.
#
# install_fixture_build <fixture-root>
#   Symlinks the real systemd/*.{service,timer,yaml}, install.sh and spira/{conf.sh,
#   lib.sh,units.sh,suite-covers.sh} into <fixture-root>/{systemd,spira}; writes an empty watchers manifest
#   and repo-map.example, a no-op `release` stub (install_fixture_release_stub below;
#   install.sh finds it by name, so a caller puts <fixture-root>/spira first on PATH), and
#   <fixture-root>/bin unit stubs.
#
# install_fixture_render <cache-key> <install.sh-invocation...>
#   Runs "$@" (an install.sh --render call) once and caches its stdout+rc, keyed on a hash
#   of the template files plus <cache-key>. A later call with the same templates and
#   cache-key returns the cached output without re-invoking python3/bash.
#   CORRECTNESS IS THE CALLER'S: render substitutes SPIRA_HOME, SPIRA_PROD, SPIRA_INSTANCE
#   and half a dozen other values into the templates, none of which this helper can see
#   inside "$@"'s own env assignment. <cache-key> must fold in every one of them — an
#   under-specified key serves a suite stale content for a value it changed.
#   CACHED UNDER THE CALLER'S OWN $TMP (read at call time, not at source time — every
#   suite here sets $TMP after sourcing this file), so the cache dies with the suite's own
#   `trap 'rm -rf "$TMP"' EXIT` rather than accumulating in a shared location. Set
#   SPIRA_TEST_INSTALL_CACHE to share it across suites in the same batch process instead.
#
# install_fixture_seed_dest <dest-dir> <rendered-text>
#   Splits <rendered-text> on "===== <unit> =====" markers into <dest-dir>/<unit> files —
#   the "nothing changed" baseline every suite starts from.
#
# mk_install_fixture <fixture-root> <tmp-root>
#   The root install.sh fixture (not systemd/install.sh's — see install_fixture_build above):
#   <fixture-root>/{systemd,spira,cockpit} symlinked to the real sources, spira/statutes/,
#   and a fake git origin+repo under <tmp-root> with origin/HEAD set, so the landref check
#   passes. Sets FAKE_ORIGIN and FAKE_REPO for the caller. Callers still write their own
#   doctor/configure.sh/build.sh/seed.sh/ready.sh/release (install_fixture_release_stub
#   below)/cockpit/layout.sh and mock systemctl/loginctl/tmux/bd — those differ suite to
#   suite (a passing doctor here, a DOCTOR_FAIL_FLAG-gated one there) and are each
#   suite's own.
#
# install_fixture_release_stub <spira-dir>
#   A no-op `release` binary stub at <spira-dir>/release answering `session-hook
#   install|status` and `intake install` the way a real success looks (sp-7jr34: install.sh
#   calls these by bare name now that spira/install-session-hook.sh and
#   spira/install-intake.sh are gone). For suites whose subject is something else entirely
#   (dolt mode, bd init cwd, refusal semantics, ...) and only need phase 5 to pass quietly.

_LIB_INSTALL_SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

_lib_install_hash() {  # stdin -> a short content hash; sha256sum where available
    if command -v sha256sum >/dev/null 2>&1; then sha256sum | awk '{print $1}'
    else cksum | awk '{print $1"-"$2}'
    fi
}

install_fixture_build() {
    local fixture="$1" f
    mkdir -p "$fixture/systemd" "$fixture/spira"
    # *.yaml: install.sh renders dolt-server{,-test}.yaml whenever SPIRA_{DOLT,TESTDB}_DATA
    # resolve non-empty. suite-covers.sh: lib.sh sources it unconditionally.
    for f in "$_LIB_INSTALL_SELF/../systemd/"*.service "$_LIB_INSTALL_SELF/../systemd/"*.timer \
             "$_LIB_INSTALL_SELF/../systemd/"*.yaml; do
        [ -e "$f" ] || continue
        ln -sf "$f" "$fixture/systemd/$(basename "$f")"
    done
    ln -sf "$_LIB_INSTALL_SELF/../systemd/install.sh" "$fixture/systemd/install.sh"
    for f in conf.sh lib.sh units.sh suite-covers.sh; do
        [ -e "$_LIB_INSTALL_SELF/$f" ] && ln -sf "$_LIB_INSTALL_SELF/$f" "$fixture/spira/$f"
    done
    printf '# empty — test fixture\n' > "$fixture/spira/watchers"
    printf '# empty\n' > "$fixture/spira/repo-map.example"
    install_fixture_release_stub "$fixture/spira"
    install_fixture_release_bins "$fixture"
}

install_fixture_release_stub() {
    local dir="$1"
    cat > "$dir/release" <<'EOF'
#!/usr/bin/env bash
case "${1:-} ${2:-}" in
    "session-hook status")  printf 'ok      SessionStart\n' ;;
    "session-hook install") printf 'release: session-hook install: stub\n' ;;
    "intake install")       printf 'release: intake install: stub\n' ;;
    *) exit 0 ;;
esac
exit 0
EOF
    chmod +x "$dir/release"
}

# THE UNIT BINARIES (sp-gypjk). Unit templates ExecStart the release's bin/<tool>
# (@SPIRA_PROD_ROOT@/bin/<tool>, PROD_ROOT = dirname SPIRA_PROD), and install.sh refuses a unit
# whose ExecStart target is not executable. A fixture therefore stages a release-shaped bin/
# beside the spira/ its SPIRA_PROD names. Nothing here runs them — install only places and
# starts units against a mock systemctl.
# DERIVED, never hand-listed: every @SPIRA_PROD_ROOT@/bin/<name> a unit template in this tree
# execs (ExecStart, ExecStartPre, ExecCondition, ...). A hand list went stale the day auron moved
# into bin/ and turned three install suites red on the base.
_install_fixture_unit_bins() {
    local sysd
    sysd="$(cd "$(dirname "${BASH_SOURCE[0]}")/../systemd" 2>/dev/null && pwd)" || return 1
    grep -ho '@SPIRA_PROD_ROOT@/bin/[A-Za-z0-9_.-]*' "$sysd"/*.service "$sysd"/*.timer 2>/dev/null \
        | sed 's|.*/bin/||' | sort -u | tr '\n' ' '
}
INSTALL_FIXTURE_UNIT_BINS="$(_install_fixture_unit_bins)"

# install_fixture_release_bins <prod-root> -> no-op stubs at <prod-root>/bin/<tool> for every
# binary a unit template ExecStarts.
install_fixture_release_bins() {
    local dir="$1/bin" b
    if [ -z "${INSTALL_FIXTURE_UNIT_BINS// /}" ]; then
        echo "lib-test-install: found no unit binaries under systemd/ — refusing to stage an empty bin/" >&2
        return 1
    fi
    mkdir -p "$dir"
    for b in $INSTALL_FIXTURE_UNIT_BINS; do
        printf '#!/usr/bin/env bash\nexit 0\n' > "$dir/$b"
        chmod +x "$dir/$b"
    done
}

# install_fixture_prod <prod-root> <spira-dir> -> prints <prod-root>/spira: a release-shaped
# root whose spira/ is <spira-dir>, whose other top-level entries (cockpit/, systemd/, ...)
# are the ones beside <spira-dir>, and whose bin/ holds the unit-binary stubs above. For a
# suite that used to pass SPIRA_PROD=<a real tree's spira/> plus SPIRA_<TOOL>_BIN stubs.
install_fixture_prod() {
    local root="$1" sp f
    sp="$(cd "$2" && pwd -P)"
    mkdir -p "$root"
    for f in "$sp/.."/*; do
        case "$(basename "$f")" in spira|bin) continue ;; esac
        ln -sfn "$f" "$root/$(basename "$f")"
    done
    ln -sfn "$sp" "$root/spira"
    install_fixture_release_bins "$root"
    printf '%s/spira' "$root"
}

install_fixture_render() {
    local cache_key="$1" tmpl_hash entry rc out cache_root
    shift
    cache_root="${SPIRA_TEST_INSTALL_CACHE:-${TMP:-${TMPDIR:-/tmp}}/spira-install-render-cache}"
    tmpl_hash="$(cat "$_LIB_INSTALL_SELF/../systemd/"*.service \
                     "$_LIB_INSTALL_SELF/../systemd/"*.timer \
                     "$_LIB_INSTALL_SELF/../systemd/install.sh" \
                     "$_LIB_INSTALL_SELF/units.sh" 2>/dev/null | _lib_install_hash)"
    mkdir -p "$cache_root/$tmpl_hash"
    entry="$cache_root/$tmpl_hash/$(printf '%s' "$cache_key" | _lib_install_hash)"
    if [ -f "$entry.rc" ]; then
        cat "$entry.out"
        return "$(cat "$entry.rc")"
    fi
    out="$("$@" 2>&1)"; rc=$?
    printf '%s' "$out" > "$entry.out.tmp" && mv "$entry.out.tmp" "$entry.out"
    printf '%s' "$rc" > "$entry.rc.tmp" && mv "$entry.rc.tmp" "$entry.rc"
    printf '%s' "$out"
    return "$rc"
}

mk_install_fixture() {
    local fixture="$1" tmp="$2" spira="$1/spira" systemd="$1/systemd" cockpit="$1/cockpit" f
    mkdir -p "$spira" "$systemd" "$cockpit" "$spira/statutes"
    for f in "$_LIB_INSTALL_SELF/../systemd/"*.service "$_LIB_INSTALL_SELF/../systemd/"*.timer \
             "$_LIB_INSTALL_SELF/../systemd/"*.yaml; do
        [ -e "$f" ] || continue
        ln -sf "$f" "$systemd/$(basename "$f")"
    done
    ln -sf "$_LIB_INSTALL_SELF/../systemd/install.sh" "$systemd/install.sh"
    ln -sf "$_LIB_INSTALL_SELF/../systemd/units.sh"   "$systemd/units.sh"
    for f in conf.sh lib.sh suite-covers.sh; do
        [ -e "$_LIB_INSTALL_SELF/$f" ] && ln -sf "$_LIB_INSTALL_SELF/$f" "$spira/$f"
    done
    printf '# empty\n' > "$spira/watchers"
    printf '# empty\n' > "$spira/repo-map.example"

    FAKE_ORIGIN="$tmp/origin.git"
    FAKE_REPO="$tmp/fakerepo"
    git init -q --bare -b main "$FAKE_ORIGIN" 2>/dev/null
    git init -q -b main "$FAKE_REPO" 2>/dev/null
    git -C "$FAKE_REPO" config user.email t@t
    git -C "$FAKE_REPO" config user.name test
    printf 'seed\n' > "$FAKE_REPO/f"
    git -C "$FAKE_REPO" add f
    git -C "$FAKE_REPO" commit -qm "seed" 2>/dev/null
    git -C "$FAKE_REPO" remote add origin "$FAKE_ORIGIN"
    git -C "$FAKE_REPO" push -q origin main 2>/dev/null
    git -C "$FAKE_REPO" fetch -q origin 2>/dev/null
    git -C "$FAKE_REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main
}

install_fixture_seed_dest() {
    local dest="$1" rendered="$2" current_unit=""
    mkdir -p "$dest"
    while IFS= read -r line; do
        if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
            current_unit="${BASH_REMATCH[1]}"; > "$dest/$current_unit"
        elif [ -n "$current_unit" ]; then
            printf '%s\n' "$line" >> "$dest/$current_unit"
        fi
    done <<< "$rendered"
}

# =======================================================================================
# THE MINIMAL FORM (sp-9gypc), kept alongside the cached seam above: test-unit-drift.sh
# drives tinstall_*; install_fixture_* is sp-yxwsz's cross-process cache. Migrating the
# tinstall_* callers onto install_fixture_* and deleting this section is the reconciliation
# its own header anticipated.
# =======================================================================================
# tinstall_* — the render->DEST fixture for suites that drive the REAL installer output,
# not a hand-modeled one (law-prefer-the-real-dependency).
#
#   tinstall_fixture <dir>                  build a harness tree at <dir> (symlinks to the
#                                            real systemd/ + spira/ sources) install.sh can
#                                            render against.
#   tinstall_render <fixture> <home>        run install.sh --render in a controlled env;
#                                            memoized per (fixture, home) pair for the life
#                                            of the process, so two call sites asking for
#                                            the same render in one suite pay for it once.
#   tinstall_write_dest <dest> <rendered>   parse `===== <name> =====` blocks out of a
#                                            render and write each unit into <dest>, fresh.
#
set -u

declare -A _TINSTALL_RENDER_CACHE
declare -A _TINSTALL_RENDER_RC

tinstall_fixture() {   # tinstall_fixture <dir>
    local dir="$1" src="${BASH_SOURCE[1]:-$0}"
    src="$(cd "$(dirname "$src")" && pwd -P)"
    mkdir -p "$dir/systemd" "$dir/spira"
    local f
    for f in "$src/../systemd/"*.service "$src/../systemd/"*.timer "$src/../systemd/"*.yaml; do
        [ -e "$f" ] || continue
        ln -sf "$f" "$dir/systemd/$(basename "$f")"
    done
    ln -sf "$src/../systemd/install.sh" "$dir/systemd/install.sh"
    for f in conf.sh lib.sh suite-covers.sh; do
        [ -e "$src/$f" ] && ln -sf "$src/$f" "$dir/spira/$f"
    done
    printf '# empty — test fixture\n' > "$dir/spira/watchers"
    printf '# empty\n' > "$dir/spira/repo-map.example"
}

tinstall_render() {    # tinstall_render <fixture> <home> -> rendered text (memoized); $? is
                        # install.sh's own exit code, cached alongside the text.
    local fixture="$1" home="$2" key
    key="$(printf '%s\x1e%s' "$fixture" "$home" | cksum | cut -d' ' -f1)"
    if [ -z "${_TINSTALL_RENDER_CACHE[$key]+x}" ]; then
        local out rc
        out="$(env -i PATH="$PATH" HOME="$home" \
            SPIRA_CONF=/nonexistent \
            SPIRA_WATCHERS="$fixture/spira/watchers" \
            SPIRA_DOLT_DATA="" \
            SPIRA_TESTDB_DATA="" \
            bash "$fixture/systemd/install.sh" --render 2>&1)"; rc=$?
        _TINSTALL_RENDER_CACHE[$key]="$out"
        _TINSTALL_RENDER_RC[$key]="$rc"
    fi
    printf '%s' "${_TINSTALL_RENDER_CACHE[$key]}"
    return "${_TINSTALL_RENDER_RC[$key]}"
}

tinstall_write_dest() {   # tinstall_write_dest <dest-dir> <rendered-text>
    local dest="$1" rendered="$2" current_unit=""
    mkdir -p "$dest"
    rm -f "$dest"/*
    while IFS= read -r line; do
        if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
            current_unit="${BASH_REMATCH[1]}"
            > "$dest/$current_unit"
        elif [ -n "$current_unit" ]; then
            printf '%s\n' "$line" >> "$dest/$current_unit"
        fi
    done <<< "$rendered"
}
