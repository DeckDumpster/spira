#!/usr/bin/env bash
# lib-test-install.sh — the install.sh test fixture every suite in this area hand-built:
# a symlinked systemd/+spira/ tree, and a cached `--render` so the templates are not
# recompiled by every suite (and every scenario within a suite) that needs a "nothing
# changed" DEST baseline (cluster 11, docs/test-plan/instance-lifecycle.md).
#
# Sourced, never executed.
#
# install_fixture_build <fixture-root>
#   Symlinks the real systemd/*.{service,timer}, install.sh and spira/{conf.sh,watchd.sh,
#   lib.sh,units.sh} into <fixture-root>/{systemd,spira}; writes an empty watchers manifest
#   and repo-map.example, and a no-op install-session-hook.sh stub.
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

_LIB_INSTALL_SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

_lib_install_hash() {  # stdin -> a short content hash; sha256sum where available
    if command -v sha256sum >/dev/null 2>&1; then sha256sum | awk '{print $1}'
    else cksum | awk '{print $1"-"$2}'
    fi
}

install_fixture_build() {
    local fixture="$1" f
    mkdir -p "$fixture/systemd" "$fixture/spira"
    for f in "$_LIB_INSTALL_SELF/../systemd/"*.service "$_LIB_INSTALL_SELF/../systemd/"*.timer; do
        [ -e "$f" ] || continue
        ln -sf "$f" "$fixture/systemd/$(basename "$f")"
    done
    ln -sf "$_LIB_INSTALL_SELF/../systemd/install.sh" "$fixture/systemd/install.sh"
    for f in conf.sh watchd.sh lib.sh units.sh; do
        [ -e "$_LIB_INSTALL_SELF/$f" ] && ln -sf "$_LIB_INSTALL_SELF/$f" "$fixture/spira/$f"
    done
    printf '# empty — test fixture\n' > "$fixture/spira/watchers"
    printf '# empty\n' > "$fixture/spira/repo-map.example"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$fixture/spira/install-session-hook.sh"
    chmod +x "$fixture/spira/install-session-hook.sh"
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
