#!/usr/bin/env bash
# tier: T0
# covers: spira/test-*.sh spira/testlib.sh
# hermetic-ok: reads suite source and filesystem; no database, no systemd
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# Suites still carrying their own ok()/bad()/is()/want()/nowant()/wantrc(), grouped by why:
#
# Never used the ok()/want()/nowant() family testlib.sh replaces -- testlib.sh's own
# header puts the mechanical migration at 459 of 466 suites for exactly this reason.
# These build their pass/fail counters a different way (pass()/fail(), or a hand-rolled
# is()/want() with an inline counter, some as multi-line functions) and need a real rewrite,
# not a rename. Migration filed as sp-u1a.
NEVER_OK_FAMILY=""
#
# Did use the ok()/want()/nowant() family but do something testlib.sh cannot yet do.
# Subshell-safe counters (test-canary.sh, test-gate-diag.sh --
# their assertions run inside `( )` subshells, whose variable writes never reach the
# parent, so testlib's own internal counters and TAP case numbering would silently
# corrupt across the subshell boundary): filed as sp-wpr.
# Custom assertion helpers beyond ok/bad/want/is (test-install-refusal.sh defines
# is1/absent/present): filed as sp-u1b.
NEEDS_TESTLIB_EXTENSION="test-canary.sh test-gate-diag.sh test-install-refusal.sh"
#
# Suites carrying unmigrated ok()/want()/nowant() awaiting migration strategy decision
# from sp-l2be6. These 358 suites were not in the scope of sp-29g55 (which handled
# 9 incident suites + test-install-refusal.sh extension). Mechanical migration (like
# sp-qvjzb's 459 suites) is blocked until sp-l2be6 decides whether to migrate or exempt.
# Track this in test-testlib-migrated.sh to prevent silent growth of unmigrated suites.
DEFERRED_MIGRATION="test-acceptance-ci.sh test-acceptance-prev-tag.sh test-aeon-base-ref-qualify.sh test-aeon-chamber-overlay.sh test-aeon-decision-blocked.sh test-aeon-elastic-concurrency.sh test-aeon-exit.sh test-aeon-gate-close-silent.sh test-aeon-launch-grammar.sh test-aeon-operator-wait.sh test-aeon-presession-death.sh test-aeon-prod-dirty.sh test-aeon-resume-collision.sh test-aeon-sweep.sh test-aeon-wiki-concurrent.sh test-aeon-worktree-collision.sh test-archivist.sh test-attempts.sh test-auron.sh test-batch-bisect-expiry.sh test-batch-cited-commit.sh test-batch-conflict.sh test-batch-conflicting-pr.sh test-batch-express.sh test-batch-idle-cut.sh test-batch-landed-false.sh test-batch-maxpar.sh test-batch-nocut-reason.sh test-batch-owner.sh test-batch-reconcile.sh test-batch-stuck.sh test-batch-timing.sh test-batcher-cut.sh test-bd-lock-retry.sh test-bd-resolve.sh test-bd-schema-stamp.sh test-bead-repo-guard.sh test-beads-push-commit.sh test-beads-push-verify.sh test-beads-sparklines.sh test-bin-manifest.sh test-boundary.sh test-broker-allowlist-lint.sh test-broker.sh test-budget-drift.sh test-build-bd-release.sh test-builder-qa-proposed.sh test-cadence-tool.sh test-capacity-probe.sh test-census-tz-boundary.sh test-cert-stale-gate-key.sh test-certified-withdraw.sh test-certify.sh test-chamber-repo-labels.sh test-check2-reaper.sh test-citations.sh test-claim-retry.sh test-closed-strand.sh test-cockpit-accept.sh test-cockpit-bd-contract.sh test-cockpit-collect-concurrency.sh test-cockpit-collect-probes.sh test-cockpit-collector-quota.sh test-cockpit-collector-watchdog.sh test-cockpit-down-marker.sh test-cockpit-dup-refs.sh test-cockpit-funnel.sh test-cockpit-fuse.sh test-cockpit-gate.sh test-cockpit-health-restart.sh test-cockpit-history-leak.sh test-cockpit-layout-conf.sh test-cockpit-layout-ensure-focus.sh test-cockpit-layout-identity.sh test-cockpit-layout-mail.sh test-cockpit-layout.sh test-cockpit-mouse.sh test-cockpit-probe-fault.sh test-cockpit-queue-section.sh test-cockpit-queue-wait.sh test-cockpit-reachable.sh test-cockpit-ready.sh test-cockpit-rebuild.sh test-cockpit-remote.sh test-cockpit-repo-labels.sh test-cockpit-runtime-path.sh test-cockpit-self.sh test-cockpit-snap-absent.sh test-cockpit-sop.sh test-cockpit-sphere.sh test-cockpit-strand-keys.sh test-cockpit-tiered-collector.sh test-cockpit-tmp.sh test-cockpit-unlanded.sh test-cockpit-unsent.sh test-cockpit-watcher-owner.sh test-cockpit.sh test-conf.sh test-configure.sh test-containment.sh test-content-landed-empty-branch.sh test-cross-repo.sh test-ctx-pointer.sh test-czar-partition.sh test-czar-pass.sh test-czar-shadow.sh test-dependents.sh test-deploy-preflight-new-unit.sh test-deploy.sh test-deps-lint.sh test-desired-state.sh test-destroy-branch.sh test-destructive-bead.sh test-doctor-concierge-singleton.sh test-doctor-events-probe.sh test-doctor-operator-channel.sh test-doctor-snap-fresh.sh test-drain-banner.sh test-event.sh test-express-cert-order.sh test-express-lane.sh test-fayth-ready-error.sh test-forge-batch-ci-status.sh test-forge-check-status.sh test-forge-orphan-runs.sh test-forge-run-metadata.sh test-forge-runs-active.sh test-freshclone.sh test-gate-check-flaky.sh test-gate-check-red-twice.sh test-gate-check-stderr.sh test-gate-check-stuck.sh test-gate-ci-diag.sh test-gate-discover-branch.sh test-gate-retry-structural.sh test-gate-retry.sh test-gate-vitals.sh test-gate-workflow.sh test-gh-intake.sh test-gh-issue-closeout.sh test-gh-run-gate.sh test-git-push-app.sh test-governor-deleted.sh test-groom-trigger.sh test-groomer-split-piece.sh test-groomer-sweep.sh test-groomer.sh test-held.sh test-hold.sh test-host-reason.sh test-id-prefix.sh test-incident-delivers-reopen-mismatch.sh test-install-bd-init-cwd.sh test-install-bootstrap-release.sh test-install-dolt-breaker.sh test-install-dolt-mode.sh test-install-dolt-port-wait.sh test-install-dolt.sh test-install-exec.sh test-install-hooks-artifact.sh test-install-instance.sh test-install-landref.sh test-install-migrate.sh test-install-paths.sh test-install-rehearsal.sh test-install-self-test.sh test-install-unit-ensure.sh test-install-unit-prune.sh test-inventory.sh test-land-build-ensure.sh test-landing-base-fail.sh test-landing-basefail-fix.sh test-landing-build.sh test-landing-gate-wait.sh test-landing-halt.sh test-landing-mode-map.sh test-landing-order.sh test-landing-pass.sh test-landing-pr.sh test-landing-queue-early.sh test-landing-race.sh test-landing-rebase.sh test-landing-red-recurring.sh test-landing-starvation.sh test-landing.sh test-layout-guard.sh test-limits.sh test-literal-lint.sh test-livelock.sh test-loom-rebuild.sh test-loop-readonly.sh test-mail-pane.sh test-model-switch-report.sh test-orphan-test.sh test-output-visibility.sh test-parallel-isolation.sh test-park-unmapped.sh test-pilgrimage.sh test-pr-stall.sh test-priority-bands.sh test-pve.sh test-queue-eject-mail.sh test-queue-flush.sh test-queue-ops.sh test-queue-protect.sh test-queue-sort-large.sh test-queue-submit.sh test-queue-sweep-orphan-runs.sh test-ready.sh test-rebase-branch.sh test-rebase-escalation.sh test-reclaim-slay-branch-guard.sh test-reconciler.sh test-release-workflow.sh test-release.sh test-released-defects.sh test-repo-label.sh test-repo-lanes.sh test-requeue.sh test-requires.sh test-resolve-output.sh test-review.sh test-roster-warn.sh test-runner-deps.sh test-schema-apply.sh test-schema.sh test-scope-label.sh test-scratch-fence.sh test-script-exec.sh test-select.sh test-sending-certified-guard.sh test-sending-closed-reap.sh test-sending-content-label.sh test-sending-empty-commit.sh test-sending-landstate-assert.sh test-sending-metrics.sh test-sending-squash-merged.sh test-sending-unlanded-guard.sh test-sentinel-pass.sh test-session-hook.sh test-skew-escalate.sh test-spike.sh test-statusline.sh test-strand-capacity.sh test-strand-deferred.sh test-strand-ghost-teardown.sh test-strand-lock.sh test-strand-pool-paused.sh test-strand-throttle.sh test-strand-truncated.sh test-suites-cluster.sh test-suites-containment.sh test-suites-hygiene.sh test-suites-red-dedup.sh test-suites-timer.sh test-suites-unreached.sh test-superseded.sh test-tarball-bins.sh test-testdb-concurrent.sh test-testdb-failsafe.sh test-testdb-mode-lint.sh test-testenv-batch-baseline.sh test-testenv-batch-branch.sh test-testenv-batch-with-bins.sh test-testenv-batch.sh test-testenv-doctor-check.sh test-testenv-image-heartbeat.sh test-testenv-image-tag.sh test-testenv-mode.sh test-testenv-owner-guard.sh test-testenv-registry.sh test-testenv-scratch.sh test-testenv-stdin.sh test-testenv-suites.sh test-testenv-systemctl.sh test-testenv-timeout.sh test-testenv-tmux-isolation.sh test-testenv.sh test-thrash-streak.sh test-timeout-lint.sh test-timeout.sh test-tmux-env.sh test-tokens.sh test-tsd.sh test-unclaimable-worktree.sh test-unit-binary.sh test-unit-name.sh test-verdict-replay.sh test-verdict.sh test-watchd-unit-name.sh test-watchtower-czar-outcome.sh test-watchtower-failed-units.sh test-watchtower-lapse.sh test-watchtower.sh test-wiki-add-fence.sh test-wire-token.sh test-work-services-exclusion.sh test-workspace-dist.sh test-worktree-absent.sh test-world-drain-deadline.sh test-world-timer-names.sh test-world-timer-service-result.sh test-world.sh"
#
# Shrink any list as a suite converts, and this check catches a name added to either
# list for any other reason. DEFERRED_MIGRATION should shrink as sp-l2be6 makes
# decisions and migrations proceed.
EXPECTED_OFFENDERS="$NEVER_OK_FAMILY $NEEDS_TESTLIB_EXTENSION $DEFERRED_MIGRATION"

printf 'test-testlib-migrated.sh\n'

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

offenders_in() {  # offenders_in <dir> -> newline-separated basenames redefining a testlib primitive
    grep -lE '^(ok|bad|fail|is|want|nowant|notwant|wantrc)\(\)' "$1"/test-*.sh 2>/dev/null \
        | xargs -n1 basename 2>/dev/null | LC_ALL=C sort
}

# POSITIVE CONTROL: a suite that still defines ok() must be caught.
cp "$HERE/test-dummy.sh" "$TMP/test-dummy.sh"
printf 'ok() { :; }\n' >> "$TMP/test-dummy.sh"
_planted="$(offenders_in "$TMP")"
want "positive control: planted ok() is detected" "test-dummy.sh" "$_planted"
rm -f "$TMP/test-dummy.sh"

_found="$(offenders_in "$HERE")"
_expected="$(printf '%s\n' $EXPECTED_OFFENDERS | LC_ALL=C sort)"

_unexpected="$(LC_ALL=C comm -23 <(printf '%s\n' "$_found") <(printf '%s\n' "$_expected"))"
is "no suite defines ok()/want()/nowant() outside the declared exceptions" "" "$_unexpected"

_stale="$(LC_ALL=C comm -13 <(printf '%s\n' "$_found") <(printf '%s\n' "$_expected"))"
is "every declared exception still actually needs one" "" "$_stale"

tl_summary
