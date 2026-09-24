#!/usr/bin/env bash
#
# gate-fences.sh — derive and copy the fence files gate-spira.sh declares.
# Sourced, never executed.
#
# Source this into suites that build a gate-spira.sh fixture. The fence list is
# read from the gate at call time, so a fence added to gate-spira.sh requires
# no edit in any suite.
#
# THE POSITIVE CONTROL (law-absence-needs-a-positive-control). Both copy
# functions fail if the gate declares no fences — an empty list and a bad path
# produce the same silence from outside, and neither may read as success.
#
# API
#   gate_fence_list  <gate-script>                — print one fence path per line
#   gate_fence_cp    <gate-script> <src> <dst>    — copy real fence files from src into dst
#   gate_fence_stubs <gate-script> <dst>          — write exit-0 stubs into dst

# gate_fence_list <gate-script>
# Print the relative fence paths the gate declares (e.g. spira/literal-lint.sh),
# one per line. Reads the `for fence in ...; do` loop that gate-spira.sh uses as
# its machine-readable fence list.
gate_fence_list() {
    local gate="$1"
    grep -oE 'for fence in [^;]+' "$gate" \
        | sed 's/for fence in //' \
        | tr ' ' '\n' \
        | grep '.'
}

# gate_fence_cp <gate-script> <src-dir> <dst-dir>
# Copy each fence file (by basename) from src-dir into dst-dir.
gate_fence_cp() {
    local gate="$1" src="$2" dst="$3"
    local list; list="$(gate_fence_list "$gate")"
    [ -n "$list" ] || {
        printf 'gate_fence_cp: gate declares no fences — refusing to copy nothing\n' >&2
        return 1
    }
    while IFS= read -r fence; do
        local name; name="${fence##*/}"
        cp "$src/$name" "$dst/$name"
    done <<< "$list"
}

# gate_fence_stubs <gate-script> <dst-dir>
# Write a minimal stub (#!/usr/bin/env bash\nexit 0\n) for each declared fence into
# dst-dir. Used by suites that control which fence "speaks" and want the rest silent.
gate_fence_stubs() {
    local gate="$1" dst="$2"
    local list; list="$(gate_fence_list "$gate")"
    [ -n "$list" ] || {
        printf 'gate_fence_stubs: gate declares no fences — refusing to stub nothing\n' >&2
        return 1
    }
    while IFS= read -r fence; do
        local name; name="${fence##*/}"
        printf '#!/usr/bin/env bash\nexit 0\n' > "$dst/$name"
    done <<< "$list"
}
