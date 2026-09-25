#!/usr/bin/env bash
# lib-test-install.sh — the render->DEST fixture shared by suites that drive the REAL
# installer output, not a hand-modeled one (law-prefer-the-real-dependency).
#
# Sourced, never executed:
#   HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
#   . "$HERE/lib-test-install.sh"
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
# THIS IS THE MINIMAL FORM. The full seam — a render cached across suite PROCESSES, keyed
# on the templates' hash — is sp-yxwsz's; this file exists so sp-9gypc's suites are not
# blocked on that landing first, and is expected to be reconciled with it when it does.
set -u

declare -A _TINSTALL_RENDER_CACHE
declare -A _TINSTALL_RENDER_RC

tinstall_fixture() {   # tinstall_fixture <dir>
    local dir="$1" src="${BASH_SOURCE[1]:-$0}"
    src="$(cd "$(dirname "$src")" && pwd -P)"
    mkdir -p "$dir/systemd" "$dir/spira"
    local f
    for f in "$src/../systemd/"*.service "$src/../systemd/"*.timer; do
        [ -e "$f" ] || continue
        ln -sf "$f" "$dir/systemd/$(basename "$f")"
    done
    ln -sf "$src/../systemd/install.sh" "$dir/systemd/install.sh"
    for f in conf.sh watchd.sh lib.sh; do
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
