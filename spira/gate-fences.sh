#!/usr/bin/env bash
#
# gate-fences.sh — the fences gate-spira.sh runs, as data gate-spira.sh itself reads.
#
# Before this, three suites each grepped gate-spira.sh's source for their own fence's
# name (D7) — a fence renamed in the gate but not in the grep would pass silently, since
# the assertion and the thing it asserts about were two independent copies of the same
# list. One list, read by the gate's own fence loop and asserted against by a suite,
# cannot drift out of step with itself (law-a-matcher-reads-code-not-prose).
#
# Replaces this file's earlier contents (sp-i7u): a `gate_fence_list <gate-script>`
# that grep-parsed the fence names back out of gate-spira.sh's `for fence in ...` line for
# fixture-building suites, all four of which are gone. That direction — deriving the list
# FROM the gate's own literal — could never let the gate read the list itself without
# circularity; here the list is the source and the gate reads it.
#
# The fences ported to spira-lint (binary-path-fence, payload-argv-lint, config-fence,
# fence-scripts, testdb-mode-lint, bd-stdin-lint, incident-cause-lint, and — sp-ekkak —
# inventory, literal-lint, scratch-fence, wiki-add-fence, tmux-scope-fence, gh-intake-lint)
# are no longer files here: gate-spira.sh runs spira-lint --only <rule> for each, and
# refuses to land when the binary is not built.
#
# Sourced, never executed.
#
# covers: spira/gate-spira.sh
set -u

gate_fence_list() {    # gate_fence_list -> one repo-relative fence path per line, in the
                        # order gate-spira.sh runs them
    printf '%s\n' \
        spira/exclude.sh \
        spira/sop.sh \
        spira/suite-state-fence.sh \
        spira/orphan-test.sh
}
