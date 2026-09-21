#!/usr/bin/env bash
# acceptance-run.sh — release acceptance: fresh install from <tag>, land a bead end to end,
# uninstall; optionally also upgrade and rollback paths.
#
# Run this on a CLEAN machine (no existing Spira installation, no conflicting systemd units).
#
# Usage:
#   acceptance-run.sh <tag> --scratch-repo <path> [--prev-tag <tag>] [--record]
#                           [--file-defects] [--bd-db <path>] [--agent <path>]
#
# Arguments:
#   <tag>                  release tag to test (spira-release-spira-*)
#   --scratch-repo <path>  local checkout of a git repo in Spira's repo-map;
#                          a trivial bead is filed here and must land
#   --prev-tag <tag>       previous release tag; enables upgrade (phase B),
#                          rollback (phase C), and aged-install upgrade (phase D)
#   --record               write PASS/FAIL as a git note on <tag>
#                          under refs/notes/acceptance
#   --file-defects         file a builder bead per FAIL, linked discovered-from sp-ewwwq
#   --bd-db <path>         bd database path (default: ~/spira-acceptance-test-db).
#                          In CI, pass the instance's own db so phase D migration
#                          checks exercise the real store.
#   --agent <path>         stub agent replacing claude; set SPIRA_AGENT to this path
#                          so the sentinel's aeons complete deterministically without
#                          a model credential (enables single-checkout install mode)
#
# PHASES
#   A. Fresh install: check prerequisites, install from <tag>, land a bead by ancestry,
#      run uninstall, verify clean state.
#   B. Upgrade: install from <prev-tag>, capture unit set, deploy.sh <tag>, verify
#      no rollback occurred, verify .tag sidecar names <tag>.  [--prev-tag only]
#   C. Rollback: deploy.sh <prev-tag>, verify unit set matches pre-upgrade snapshot.
#      [--prev-tag only]
#   D. Aged-install upgrade: install <prev-tag> into surviving state (db, config from
#      prior phases), seed beads and statutes, start world, deploy.sh <tag>, assert
#      bead/memory counts preserved through migration, doctor no fatal, operator
#      override survived, no crash-loop units, world resumed, one bead lands after
#      upgrade. Force rollback and assert it either succeeds with a healthy world or
#      is refused and names the blocking migration.  [--prev-tag only]
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
#   Point the script at a tag known to be broken (e.g. one predating sp-jcb1) and
#   confirm it exits non-zero. A FAIL on the broken tag proves the PASS on the good
#   tag is meaningful — not vacuous.
#
# EXIT
#   0  all phases PASS
#   1  one or more phases FAIL (evidence printed on stdout)
#   2  usage error or missing prerequisite
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ---------------------------------------------------------------------------
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is0()     { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
not0()    { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is_same() { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# ---------------------------------------------------------------------------
# ARG PARSING
# ---------------------------------------------------------------------------
tag=""
scratch_repo=""
prev_tag=""
do_record=0
do_file_defects=0
bd_db="${HOME}/spira-acceptance-test-db"
_agent=""

while [ $# -gt 0 ]; do
    case "$1" in
        --scratch-repo)  scratch_repo="${2:-}"; shift 2 ;;
        --scratch-repo=*) scratch_repo="${1#--scratch-repo=}"; shift ;;
        --prev-tag)      prev_tag="${2:-}"; shift 2 ;;
        --prev-tag=*)    prev_tag="${1#--prev-tag=}"; shift ;;
        --bd-db)         bd_db="${2:-}"; shift 2 ;;
        --bd-db=*)       bd_db="${1#--bd-db=}"; shift ;;
        --agent)         _agent="${2:-}"; shift 2 ;;
        --agent=*)       _agent="${1#--agent=}"; shift ;;
        --record)        do_record=1; shift ;;
        --file-defects)  do_file_defects=1; shift ;;
        -*)              printf 'acceptance-run: unknown option: %s\n' "$1" >&2; exit 2 ;;
        *)
            if [ -z "$tag" ]; then tag="$1"; else
                printf 'acceptance-run: too many positional arguments\n' >&2; exit 2
            fi
            shift ;;
    esac
done

if [ -z "$tag" ]; then
    printf 'usage: acceptance-run.sh <tag> --scratch-repo <path> [--prev-tag <tag>]\n' >&2
    exit 2
fi
if [ -z "$scratch_repo" ]; then
    printf 'acceptance-run: --scratch-repo is required\n' >&2
    exit 2
fi

# Derive the repo root (this file is in spira/, one level below the repo root).
REPO_ROOT="$(cd "$HERE/.." && pwd -P)"

TMP="$(mktemp -d)"
trap '_acceptance_cleanup' EXIT INT TERM
_acceptance_cleanup() {
    # Do not delete the install: uninstall.sh is part of the test.
    # Clean up only our temp scratch (clones, etc.).
    rm -rf "$TMP"
}

printf 'acceptance-run.sh  tag=%s\n' "$tag"

# ===========================================================================
echo
echo "prerequisites — real tools on PATH"
# ===========================================================================
# Positive control: verify the prereq check would catch a missing tool.
# We confirm 'command -v' returns non-zero for a name that cannot exist.
if command -v "spira-acceptance-nonexistent-$$" >/dev/null 2>&1; then
    bad "positive-control: command -v catches missing tool" \
        "command -v returned 0 for a nonexistent name"
else
    ok "positive-control: command -v catches missing tool"
fi

for _tool in bd dolt git gh python3; do
    if command -v "$_tool" >/dev/null 2>&1; then
        ok "prereq: $_tool on PATH"
    else
        bad "prereq: $_tool on PATH" "not found; install before running acceptance"
    fi
done

# Check systemd --user is available (not just on PATH but functional).
# Uses is-system-running: the same probe doctor uses so a failure here blocks
# before doctor reaches the enabled-units check and produces one clear message.
_ar_mgr_out="$(systemctl --user is-system-running 2>/dev/null || true)"
if [ -n "$_ar_mgr_out" ]; then
    ok "prereq: systemd --user available"
else
    bad "prereq: systemd --user available" \
        "systemctl --user is-system-running produced no output; check XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS"
fi
unset _ar_mgr_out

# Scratch repo must exist.
if [ -d "$scratch_repo/.git" ] || git -C "$scratch_repo" rev-parse --git-dir >/dev/null 2>&1; then
    ok "prereq: scratch-repo is a git repository ($scratch_repo)"
else
    bad "prereq: scratch-repo is a git repository" "$scratch_repo is not a git repo"
fi

[ "$fail" -gt 0 ] && {
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    printf 'acceptance-run: prerequisite failures — cannot continue\n' >&2
    exit 2
}

# ===========================================================================
echo
echo "phase A — fresh install from $tag"
# ===========================================================================

# Locate the repo the tag lives in.
_tag_repo=""
if git -C "$REPO_ROOT" rev-parse --verify "refs/tags/$tag" >/dev/null 2>&1; then
    _tag_repo="$REPO_ROOT"
else
    bad "phase A: tag $tag exists in this repository" "tag not found"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "phase A: tag $tag found in repository"

# Clone the tag to a temp dir so we get a clean tree with no working-tree
# modifications. Use --no-local so git creates a true copy (not hardlinks)
# that mirrors what a fresh clone from the remote would produce.
_clone="$TMP/clone-$tag"
git clone --no-local --branch "$tag" --depth 1 "$_tag_repo" "$_clone" >/dev/null 2>&1
is0 "phase A: git clone --branch $tag" "$?"

# Run install.sh from the cloned tree.
# The install.sh at the repo root is the full installer (config, build, db, units, hooks).
# On a clean machine this will configure via configure.sh (interactive or via env vars).
# We pass SPIRA_INSTALL_CONFLICT_CONSIDERED=1 only if this is not the first run on this
# machine — on a genuinely clean machine, no conflict should exist.
_install_rc=0
_install_env=(SPIRA_HOME_REPO="$(basename "$scratch_repo")" SPIRA_OPERATED=0)
# --agent triggers single-checkout mode (CONFIGURE_PROD = clone path) so install.sh
# creates SPIRA_PROD inside the clone, bypassing the promote.sh requirement on a
# clean machine. The git-checkout guard is overridden because the clone IS the prod
# checkout in this mode — the test proves the path, not the release mechanism.
if [ -n "$_agent" ]; then
    _install_env+=(
        "CONFIGURE_PROD=$_clone/spira"
        "SPIRA_INSTALL_PROD_GIT_CONSIDERED=1"
    )
fi
env "${_install_env[@]}" bash "$_clone/install.sh" 2>&1 | tee "$TMP/install.log" || _install_rc=$?
is0 "phase A: install.sh exits 0" "$_install_rc"

if [ "$_install_rc" -eq 0 ]; then
    _conf="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
    [ -n "$_agent" ] && printf '\nSPIRA_AGENT = %s\n' "$_agent" >> "$_conf"
    printf 'SPIRA_OPERATED = 0\n' >> "$_conf"
fi

# After install, verify ready.sh exits 0.
_ready_rc=0
_ready_out="$(bash "$HERE/ready.sh" 2>&1)" || _ready_rc=$?
is0 "phase A: ready.sh exits 0 after install" "$_ready_rc"
[ "$_ready_rc" -eq 0 ] || printf '%s\n' "$_ready_out"

# ===========================================================================
echo
echo "phase A — end-to-end bead: file, sentinel summons aeon, land by ancestry"
# ===========================================================================

# Capture the scratch repo's land ref before filing the bead.
_land_ref="$(git -C "$scratch_repo" rev-parse --abbrev-ref HEAD 2>/dev/null)" || _land_ref="main"
_base_sha_before="$(git -C "$scratch_repo" rev-parse "origin/${_land_ref}" 2>/dev/null)" || \
    _base_sha_before="$(git -C "$scratch_repo" rev-parse "HEAD" 2>/dev/null)"

# File a trivial bead: "acceptance test: land this bead to prove the install works".
# Use a very short task description so an aeon can complete it in one pass without
# complex reasoning. The bead body says to commit an empty file.
_bead_title="acceptance-run: trivial land proof for $tag"
_bead_out=""
_bead_out="$(bd -C "$bd_db" create \
    --title "$_bead_title" \
    --description "Acceptance test: commit an empty file named acceptance-probe.txt to prove end-to-end landing works. Content: the tag under test is $tag." \
    --label "acceptance,repo:$(basename "$scratch_repo")" \
    --type task \
    2>&1)" || true
_bead_id="$(printf '%s\n' "$_bead_out" \
    | sed -n 's/.*Created issue: \([a-z0-9]*-[a-z0-9]*\).*/\1/p' | head -1)"

if [ -n "$_bead_id" ]; then
    ok "phase A: bead filed ($_bead_id)"
else
    bad "phase A: bead filed" "bd create output: $_bead_out"
fi

if [ -n "$_bead_id" ]; then
    # Wait for the bead to appear in sentinel's report (up to 6 minutes: 3 sentinel ticks).
    _sentinel_wait=0
    _bead_ready=0
    while [ "$_sentinel_wait" -lt 360 ]; do
        _report="$(bash "$HERE/sentinel.sh" --report 2>&1)" || true
        if printf '%s' "$_report" | grep -qF "$_bead_id"; then
            _bead_ready=1; break
        fi
        sleep 30
        _sentinel_wait=$((_sentinel_wait + 30))
    done
    [ "$_bead_ready" -eq 1 ] \
        && ok "phase A: sentinel --report names the bead within 6 minutes" \
        || bad "phase A: sentinel --report names the bead within 6 minutes" \
               "bead $_bead_id not seen after ${_sentinel_wait}s"

    # Wait for the aeon to land the bead (up to 15 minutes).
    _aeon_wait=0
    _landed=0
    while [ "$_aeon_wait" -lt 900 ]; do
        # Check landing by ancestry: did a new commit appear on the scratch repo's
        # land ref that descends from _base_sha_before?
        git -C "$scratch_repo" fetch origin >/dev/null 2>&1 || true
        _base_sha_now="$(git -C "$scratch_repo" rev-parse "origin/${_land_ref}" 2>/dev/null)" || \
            _base_sha_now="$_base_sha_before"
        if [ "$_base_sha_now" != "$_base_sha_before" ] \
            && git -C "$scratch_repo" log --format='%s' "${_base_sha_before}..${_base_sha_now}" \
               | grep -qF "$_bead_id" 2>/dev/null; then
            _landed=1; break
        fi
        sleep 30
        _aeon_wait=$((_aeon_wait + 30))
    done

    if [ "$_landed" -eq 1 ]; then
        ok "phase A: bead $_bead_id landed on $scratch_repo:$_land_ref by ancestry"
    else
        bad "phase A: bead $_bead_id landed on $scratch_repo:$_land_ref by ancestry" \
            "no commit containing bead id appeared on origin/$_land_ref after ${_aeon_wait}s"
    fi
fi

# ===========================================================================
echo
echo "phase A — uninstall and clean state"
# ===========================================================================
_uninstall_out="$(bash "$HERE/uninstall.sh" --yes 2>&1)"
is0 "phase A: uninstall.sh --yes exits 0" "$?"

# Verify no spira-* units remain.
_units_after="$(systemctl --user list-unit-files --no-legend 2>/dev/null \
    | awk '{print $1}' | grep '^spira-' || true)"
[ -z "$_units_after" ] \
    && ok "phase A: no spira-* units remain after uninstall" \
    || bad "phase A: no spira-* units remain after uninstall" "$_units_after"

# ===========================================================================
echo
echo "phase B — upgrade: install $prev_tag, deploy to $tag, verify no rollback"
# ===========================================================================
if [ -z "$prev_tag" ]; then
    printf '  skip  phase B+C: --prev-tag not given\n'
else
    # Verify prev_tag exists.
    if ! git -C "$REPO_ROOT" rev-parse --verify "refs/tags/$prev_tag" >/dev/null 2>&1; then
        bad "phase B: prev tag $prev_tag exists" "tag not found"
    else
        ok "phase B: prev tag $prev_tag found"

        # Install from prev_tag.
        _prev_clone="$TMP/clone-$prev_tag"
        git clone --no-local --branch "$prev_tag" --depth 1 "$_tag_repo" "$_prev_clone" \
            >/dev/null 2>&1
        is0 "phase B: git clone --branch $prev_tag" "$?"

        _prev_install_rc=0
        SPIRA_OPERATED=0 bash "$_prev_clone/install.sh" >/dev/null 2>&1 || _prev_install_rc=$?
        is0 "phase B: install.sh ($prev_tag) exits 0" "$_prev_install_rc"

        # Capture unit set BEFORE upgrade.
        _units_pre_upgrade="$(systemctl --user list-unit-files --no-legend 2>/dev/null \
            | awk '{print $1, $2}' | grep '^spira-' | sort || true)"

        # Run deploy.sh to upgrade to newest tag.
        _deploy_rc=0
        bash "$HERE/deploy.sh" "$tag" 2>&1 | tee "$TMP/deploy-upgrade.log" || _deploy_rc=$?
        is0 "phase B: deploy.sh $tag exits 0 (no rollback)" "$_deploy_rc"

        # Verify .tag sidecar names the new tag (sp-cb0q1: sidecar written to releases dir).
        _releases_dir="${SPIRA_RELEASES:-${HOME}/spira-releases}"
        _tag_sidecar="${_releases_dir}/current/.tag"
        _sidecar_val="$(cat "$_tag_sidecar" 2>/dev/null || printf '')"
        is_same "phase B: .tag sidecar names $tag" "$tag" "$_sidecar_val"

        # Verify SPIRA_PROD points into the releases directory (not the raw checkout).
        _spira_prod="$(bash -c '. "$1/conf.sh" 2>/dev/null; printf "%s" "${SPIRA_PROD:-}"' \
            -- "$HERE" 2>/dev/null || true)"
        want "phase B: SPIRA_PROD updated to releases path" "${_releases_dir}" "$_spira_prod"

        # ===========================================================================
        echo
        echo "phase C — rollback: deploy $prev_tag, verify unit set restored"
        # ===========================================================================

        _rollback_rc=0
        bash "$HERE/deploy.sh" "$prev_tag" 2>&1 | tee "$TMP/deploy-rollback.log" \
            || _rollback_rc=$?
        is0 "phase C: deploy.sh $prev_tag (rollback) exits 0" "$_rollback_rc"

        # Capture unit set AFTER rollback.
        _units_post_rollback="$(systemctl --user list-unit-files --no-legend 2>/dev/null \
            | awk '{print $1, $2}' | grep '^spira-' | sort || true)"

        # Verify the unit set is identical to the pre-upgrade snapshot.
        # sp-x6ygl: rollback must restore the prior release's ExecStart paths AND unit set.
        _unit_diff="$(diff \
            <(printf '%s\n' "$_units_pre_upgrade") \
            <(printf '%s\n' "$_units_post_rollback") || true)"
        [ -z "$_unit_diff" ] \
            && ok "phase C: unit set after rollback matches pre-upgrade snapshot (no extra/dropped units)" \
            || bad "phase C: unit set after rollback matches pre-upgrade snapshot" \
                   "$(printf '%s\n' "$_unit_diff" | head -10)"

        # Final uninstall.
        bash "$HERE/uninstall.sh" --yes >/dev/null 2>&1
        is0 "phase C: uninstall.sh --yes after rollback exits 0" "$?"
    fi
fi

# ===========================================================================
echo
echo "phase D — aged-install upgrade: $prev_tag with real state → $tag"
# ===========================================================================
# Install prev_tag into surviving state (db and config persist from phases A-C;
# uninstall.sh --yes removes units but not the database or config file). Seed
# additional beads and memories to guarantee the store is populated, write an
# operator override, start the world, then upgrade via deploy.sh. Assert that
# migration preserved bead/memory counts, doctor reports no fatal, the operator
# override survived, no units crash-loop, and the world can land new work.
# Finally force a rollback and assert it either succeeds cleanly or is refused
# with the blocking migration named (law-pin-by-migration-count).
if [ -z "$prev_tag" ]; then
    printf '  skip  phase D: --prev-tag not given\n'
else
    _aged_clone="$TMP/clone-aged-$prev_tag"
    git clone --no-local --branch "$prev_tag" --depth 1 "$_tag_repo" "$_aged_clone" \
        >/dev/null 2>&1
    is0 "phase D: git clone $prev_tag (aged base)" "$?"

    _aged_install_rc=0
    _aged_env=(SPIRA_HOME_REPO="$(basename "$scratch_repo")" SPIRA_OPERATED=0)
    if [ -n "$_agent" ]; then
        _aged_env+=(
            "CONFIGURE_PROD=$_aged_clone/spira"
            "SPIRA_INSTALL_PROD_GIT_CONSIDERED=1"
        )
    fi
    env "${_aged_env[@]}" bash "$_aged_clone/install.sh" 2>&1 \
        | tee "$TMP/aged-install.log" || _aged_install_rc=$?
    is0 "phase D: install.sh ($prev_tag, aged) exits 0" "$_aged_install_rc"

    if [ "$_aged_install_rc" -eq 0 ]; then
        _aged_conf="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"

        [ -n "$_agent" ] && printf '\nSPIRA_AGENT = %s\n' "$_agent" >> "$_aged_conf"
        printf 'SPIRA_OPERATED = 0\n' >> "$_aged_conf"

        # Seed beads into $bd_db (the instance's db in CI).
        bd -C "$bd_db" create \
            --title "aged-install: open seed bead (pre-upgrade)" \
            --label "acceptance-seed" --type task >/dev/null 2>&1 || true
        _aged_seed2_out=""
        _aged_seed2_out="$(bd -C "$bd_db" create \
            --title "aged-install: closed seed bead (pre-upgrade)" \
            --label "acceptance-seed" --type task 2>&1)" || true
        _aged_seed2="$(printf '%s\n' "$_aged_seed2_out" \
            | sed -n 's/.*Created issue: \([a-z0-9]*-[a-z0-9]*\).*/\1/p' | head -1)"
        [ -n "$_aged_seed2" ] && \
            bd -C "$bd_db" close "$_aged_seed2" \
                --reason "acceptance: closed for aged-install migration test" \
                >/dev/null 2>&1 || true

        # Seed statute-style memories.
        bd -C "$bd_db" remember \
            "aged-install acceptance seed: statute 1 for migration verification" \
            --key "law-acceptance-aged-seed-1" >/dev/null 2>&1 || true
        bd -C "$bd_db" remember \
            "aged-install acceptance seed: statute 2 for migration verification" \
            --key "law-acceptance-aged-seed-2" >/dev/null 2>&1 || true

        # Capture pre-upgrade counts.
        _aged_pre_beads="$(bd -C "$bd_db" list --all --json 2>/dev/null \
            | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null \
            || printf 0)"
        _aged_pre_mems="$(bd -C "$bd_db" memories 2>/dev/null | grep -c . 2>/dev/null \
            || printf 0)"

        # Write an operator override into spira.conf to verify it survives the upgrade.
        _aged_override_val="aged-install-override-$$"
        printf '\nACCEPTANCE_AGED_OVERRIDE = %s\n' "$_aged_override_val" >> "$_aged_conf"

        # Start world and confirm the sentinel timer is active.
        bash "$HERE/world.sh" start 2>&1 | tee "$TMP/aged-world-start.log" || true
        _aged_sentinel="$(systemctl --user list-units --state=active --no-legend 2>/dev/null \
            | awk '{print $1}' | grep 'spira-sentinel' | head -1)"
        [ -n "$_aged_sentinel" ] \
            && ok "phase D: sentinel timer active (world live before upgrade)" \
            || bad "phase D: sentinel timer active (world live before upgrade)" \
                   "no spira-sentinel* in active units"

        # Brief window: prove the world is live before upgrading.
        sleep 30

        # Upgrade to tag from the aged, populated state.
        _aged_deploy_rc=0
        bash "$HERE/deploy.sh" "$tag" 2>&1 | tee "$TMP/aged-deploy.log" \
            || _aged_deploy_rc=$?
        is0 "phase D: deploy.sh $tag (aged upgrade) exits 0 — no rollback" "$_aged_deploy_rc"

        if [ "$_aged_deploy_rc" -eq 0 ]; then

            # Assert: bead count preserved through migration (no rows dropped).
            _aged_post_beads="$(bd -C "$bd_db" list --all --json 2>/dev/null \
                | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null \
                || printf 0)"
            [ "$_aged_post_beads" -ge "$_aged_pre_beads" ] \
                && ok "phase D: bead count preserved through migration ($_aged_pre_beads → $_aged_post_beads)" \
                || bad "phase D: bead count preserved through migration" \
                       "before=$_aged_pre_beads after=$_aged_post_beads — rows lost"

            # Assert: memory count preserved through migration.
            _aged_post_mems="$(bd -C "$bd_db" memories 2>/dev/null | grep -c . 2>/dev/null \
                || printf 0)"
            [ "$_aged_post_mems" -ge "$_aged_pre_mems" ] \
                && ok "phase D: memory count preserved through migration ($_aged_pre_mems → $_aged_post_mems)" \
                || bad "phase D: memory count preserved through migration" \
                       "before=$_aged_pre_mems after=$_aged_post_mems — rows lost"

            # Assert: doctor.sh exits 0 (no fatal).
            _aged_doctor_rc=0
            bash "$HERE/doctor.sh" 2>&1 | tee "$TMP/aged-doctor.log" \
                || _aged_doctor_rc=$?
            is0 "phase D: doctor.sh no fatal after aged upgrade" "$_aged_doctor_rc"

            # Assert: operator override survived in spira.conf.
            _aged_override_got="$(grep -E '^\s*ACCEPTANCE_AGED_OVERRIDE\s*=' \
                "$_aged_conf" 2>/dev/null \
                | sed 's/[^=]*=\s*//' | head -1 || true)"
            is_same "phase D: operator override survived aged upgrade" \
                "$_aged_override_val" "$_aged_override_got"

            # Assert: no failed spira units 2 min after upgrade (crash-loop check).
            sleep 120
            _aged_failed="$(systemctl --user list-units --state=failed --no-legend \
                2>/dev/null | awk '{print $1}' | grep '^spira-' || true)"
            [ -z "$_aged_failed" ] \
                && ok "phase D: no failed spira units 2 min after aged upgrade" \
                || bad "phase D: no failed spira units 2 min after aged upgrade" \
                       "$_aged_failed"

            # Assert: world resumed after upgrade (no HALTED/STOPPED state).
            _aged_world_out="$(bash "$HERE/world.sh" status 2>&1)"
            if printf '%s' "$_aged_world_out" | grep -qE 'HALTED|STOPPED'; then
                bad "phase D: world running after aged upgrade" \
                    "$(printf '%s' "$_aged_world_out" \
                       | grep -E 'HALTED|STOPPED' | head -1)"
            else
                ok "phase D: world running after aged upgrade"
            fi

            # Assert: file a bead post-upgrade; verify it lands by ancestry.
            _aged_land_base="$(git -C "$scratch_repo" \
                rev-parse "origin/${_land_ref:-main}" 2>/dev/null)" || _aged_land_base=""
            _aged_probe_out=""
            _aged_probe_out="$(bd -C "$bd_db" create \
                --title "aged-install: post-upgrade land proof ($prev_tag → $tag)" \
                --description "Prove world resumed and can land work after aged upgrade from $prev_tag to $tag." \
                --label "acceptance,repo:$(basename "$scratch_repo")" \
                --type task 2>&1)" || true
            _aged_probe_id="$(printf '%s\n' "$_aged_probe_out" \
                | sed -n 's/.*Created issue: \([a-z0-9]*-[a-z0-9]*\).*/\1/p' | head -1)"
            if [ -n "$_aged_probe_id" ]; then
                ok "phase D: post-upgrade bead filed ($_aged_probe_id)"
                _aged_land_wait=0; _aged_landed=0
                while [ "$_aged_land_wait" -lt 600 ]; do
                    git -C "$scratch_repo" fetch origin >/dev/null 2>&1 || true
                    _aged_sha_now="$(git -C "$scratch_repo" \
                        rev-parse "origin/${_land_ref:-main}" 2>/dev/null)" \
                        || _aged_sha_now="${_aged_land_base:-}"
                    if [ -n "${_aged_land_base:-}" ] \
                        && [ "$_aged_sha_now" != "$_aged_land_base" ] \
                        && git -C "$scratch_repo" log --format='%s' \
                               "${_aged_land_base}..${_aged_sha_now}" 2>/dev/null \
                           | grep -qF "$_aged_probe_id"; then
                        _aged_landed=1; break
                    fi
                    sleep 30
                    _aged_land_wait=$((_aged_land_wait + 30))
                done
                [ "$_aged_landed" -eq 1 ] \
                    && ok "phase D: post-upgrade bead landed by ancestry" \
                    || bad "phase D: post-upgrade bead landed by ancestry" \
                           "no commit with $_aged_probe_id on origin/${_land_ref:-main} after ${_aged_land_wait}s"
            else
                bad "phase D: post-upgrade bead filed" "output: $_aged_probe_out"
            fi
        fi

        # Rollback: deploy prev_tag. A schema migration that prevents downgrade must
        # cause deploy to REFUSE and name the migration (law-pin-by-migration-count).
        echo
        echo "phase D — aged rollback: deploy $prev_tag (refuse-or-succeed)"
        _aged_rollback_rc=0
        _aged_rollback_out="$(bash "$HERE/deploy.sh" "$prev_tag" 2>&1)" \
            || _aged_rollback_rc=$?
        if [ "$_aged_rollback_rc" -ne 0 ]; then
            if printf '%s' "$_aged_rollback_out" | grep -qi 'migrat'; then
                ok "phase D: rollback refused — names migration (law-pin-by-migration-count)"
            else
                bad "phase D: rollback refused but output does not name migration" \
                    "$(printf '%s' "$_aged_rollback_out" | tail -5)"
            fi
        else
            ok "phase D: rollback to $prev_tag succeeded"
            _aged_rollback_world="$(bash "$HERE/world.sh" status 2>&1)"
            if printf '%s' "$_aged_rollback_world" | grep -qE 'HALTED|STOPPED'; then
                bad "phase D: world running after aged rollback" \
                    "$(printf '%s' "$_aged_rollback_world" \
                       | grep -E 'HALTED|STOPPED' | head -1)"
            else
                ok "phase D: world running after aged rollback"
            fi
        fi

        bash "$HERE/uninstall.sh" --yes >/dev/null 2>&1
        is0 "phase D: uninstall.sh exits 0" "$?"
    fi
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"

verdict="FAIL"
[ "$fail" -eq 0 ] && verdict="PASS"

printf 'verdict: %s  tag=%s  date=%s\n' "$verdict" "$tag" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

# ---------------------------------------------------------------------------
# Record verdict as a git note on the tag so it is retrievable later.
# ---------------------------------------------------------------------------
if [ "$do_record" -eq 1 ]; then
    _note="$(printf '%s\n%s %s  %d passed, %d failed\n' \
        "$verdict" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$tag" "$pass" "$fail")"
    # Include (from, to) pair so phase D results are queryable per upgrade path.
    if [ -n "$prev_tag" ]; then
        _note="$(printf '%s\naged-install from=%s: %s\n' "$_note" "$prev_tag" "$verdict")"
    fi
    git -C "$REPO_ROOT" notes --ref=acceptance add -f -m "$_note" "refs/tags/$tag" \
        && printf 'recorded: git notes --ref=acceptance show refs/tags/%s\n' "$tag" \
        || printf 'warning: could not write git note (verdict still printed above)\n'
fi

# ---------------------------------------------------------------------------
# File defect beads for each failure if requested.
# ---------------------------------------------------------------------------
if [ "$do_file_defects" -eq 1 ] && [ "$fail" -gt 0 ]; then
    bd -C "$bd_db" create \
        --title "acceptance-run $tag: $fail phase(s) failed" \
        --label "defect,repo:spira,discovered-from:sp-ewwwq" \
        --type bug \
        --description "acceptance-run.sh found $fail failure(s) against tag $tag. See the acceptance-run log for detail." \
        >/dev/null 2>&1 || true
fi

[ "$fail" -eq 0 ]
