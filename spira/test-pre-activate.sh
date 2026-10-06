#!/usr/bin/env bash
#
# test-pre-activate.sh — pre-activate.sh gates a release against six checks
# before releases/current moves onto it (release verify runs it and refuses the flip
# when the gate fails). Every check gets a fixture release dir with stubbed
# probes (a fake spira-config, a fake bd, a fake units-install --render, a fake
# self-test.sh) rather than the real dependency, since pre-activate exists
# specifically to run before those real dependencies are trusted.
#
# The lifecycle check also runs once against a real Dolt server with the real spira-lc
# (sp-p1z81): production's shape — root has a password, no admin credential configured —
# where a store carrying every migration must pass on the service user alone.
#
# defect: sp-p1z81
# tier: T1
# covers: spira/pre-activate.sh spira/self-test.sh spira-lc/src/migrate.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-pre-activate.sh"

TMP="$(mktemp -d)"
SERVER_PID=""
trap '[ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT
# No ambient admin credential: the lifecycle cases decide whether one is present.
unset SPIRA_LC_ADMIN_USER SPIRA_LC_ADMIN_PASSWORD SPIRA_LC_ADMIN_PASSWORD_FILE
mkdir -p "$TMP/home" "$TMP/emptyrepo" "$TMP/binstub"

# mkbd <exit-code> — stub `bd` on PATH; `migrate status` is all pre-activate calls.
mkbd() {
    cat > "$TMP/binstub/bd" <<EOF
#!/usr/bin/env bash
echo "stub bd: \$*"
exit $1
EOF
    chmod +x "$TMP/binstub/bd"
}
mkbd 0

# mkrel <dir> — a fixture release dir where every one of the five checks passes by
# default. Callers override one probe at a time to isolate that check's failure.
#   DEP_VMIN DEP_PROBE_OUT DEP_PROBE_RC   deps.toml's single runtime dep
#   CONFIG_RC                             spira-config stub's exit code
#   UNITS_OUT UNITS_RC                    units-install --render stub
#   SELFTEST_RC                           self-test.sh stub
mkrel() {
    local dir="$1"
    mkdir -p "$dir/spira" "$dir/bin" "$dir/systemd"
    cat > "$dir/probe-widget.sh" <<EOF
#!/usr/bin/env bash
echo "${DEP_PROBE_OUT:-9.9.9}"
exit ${DEP_PROBE_RC:-0}
EOF
    chmod +x "$dir/probe-widget.sh"
    cat > "$dir/spira/deps.toml" <<EOF
[[dep]]
name = "widget"
tier = "runtime"
purpose = "test fixture dependency"
version_min = "${DEP_VMIN:-}"
version_probe = "$dir/probe-widget.sh"

[[dep]]
name = "ignored"
tier = "optional"
purpose = "must not be checked — only runtime tier is"
version_min = "999.0.0"
version_probe = "false"
EOF
    cat > "$dir/bin/spira-config" <<EOF
#!/usr/bin/env bash
exit ${CONFIG_RC:-0}
EOF
    chmod +x "$dir/bin/spira-config"
    cat > "$dir/bin/units-install" <<EOF
#!/usr/bin/env bash
[ "\$1" = "--render" ] || exit 2
printf '%s\n' "${UNITS_OUT:-clean unit text}"
exit ${UNITS_RC:-0}
EOF
    chmod +x "$dir/bin/units-install"
    cat > "$dir/spira/self-test.sh" <<EOF
#!/usr/bin/env bash
exit ${SELFTEST_RC:-0}
EOF
    chmod +x "$dir/spira/self-test.sh"
}

# run <release-dir> -> sets $out and $rc from a pre-activate.sh invocation in an
# explicit, minimal environment: no ambient spira.toml, a stubbed bd on PATH.
#
# SPIRA_TOML IS PINNED TO A NONEXISTENT PATH, not merely left unset: check_config reads
# ${SPIRA_TOML:-} straight from the environment, so an operator who exports it for their
# own shell's convenience would otherwise leak a real, unrelated spira.toml into this
# fixture's "no config in force" case.
run() {
    out="$(
        HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" SPIRA_REPO="$TMP/emptyrepo" \
        SPIRA_TOML="$TMP/no-such.toml" \
        PATH="$TMP/binstub:$PATH" \
        pre-activate.sh "$1" 2>&1
    )"
    rc=$?
}

# ── deps: positive and negative, both directions of A3 ───────────────────────────────
REL="$TMP/rel-deps-ok"; DEP_VMIN=1.0.0 DEP_PROBE_OUT=2.3.4 DEP_PROBE_RC=0 mkrel "$REL"
run "$REL"
is  "deps: positive: exit 0"        0 "$rc"
want "deps: positive: reports ok"   "ok   deps" "$out"

REL="$TMP/rel-deps-absent"; DEP_PROBE_RC=1 mkrel "$REL"
run "$REL"
is   "deps: absent probe: exit 1"          1 "$rc"
want "deps: absent probe: names dependency" "FAIL deps: widget" "$out"

REL="$TMP/rel-deps-old"; DEP_VMIN=5.0.0 DEP_PROBE_OUT=1.2.3 DEP_PROBE_RC=0 mkrel "$REL"
run "$REL"
is   "deps: below version_min: exit 1"    1 "$rc"
want "deps: below version_min: says why"  "below required 5.0.0" "$out"

REL="$TMP/rel-deps-optional-ignored"; DEP_VMIN=1.0.0 DEP_PROBE_OUT=2.0.0 mkrel "$REL"
run "$REL"
is "deps: optional-tier dep with a failing probe is not checked" 0 "$rc"

# ── config: skip when none is in force, then validate/reject against release schema ──
REL="$TMP/rel-config-skip"; mkrel "$REL"
run "$REL"
is   "config: no spira.toml: overall exit 0" 0 "$rc"
want "config: no spira.toml: names the skip" "nothing to validate" "$out"

# POSITIVE CONTROL (law-absence-needs-a-positive-control): export an ambient SPIRA_TOML
# pointing at a real, valid file in THIS suite's own shell — exactly what an operator's
# interactive environment does for convenience — and confirm run()'s explicit override to
# a nonexistent path is what produces the skip, not mere accidental absence. Without that
# override this assertion fails, since pre-activate.sh would find and report on the
# ambient file instead.
export SPIRA_TOML="$TMP/ambient-real.toml"
printf 'spira = {}\n' > "$SPIRA_TOML"
run "$REL"
is   "config: ambient SPIRA_TOML set: overall exit 0 despite it" 0 "$rc"
want "config: ambient SPIRA_TOML set: still names the skip" "nothing to validate" "$out"
unset SPIRA_TOML

REL="$TMP/rel-config-ok"; CONFIG_RC=0 mkrel "$REL"
printf 'spira = {}\n' > "$TMP/spira.toml"
out="$(
    HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" SPIRA_REPO="$TMP/emptyrepo" \
    SPIRA_TOML="$TMP/spira.toml" PATH="$TMP/binstub:$PATH" \
    pre-activate.sh "$REL" 2>&1
)"; rc=$?
is   "config: valid spira.toml: exit 0"  0 "$rc"
want "config: valid spira.toml: reports ok" "ok   config" "$out"

REL="$TMP/rel-config-bad"; CONFIG_RC=1 mkrel "$REL"
out="$(
    HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" SPIRA_REPO="$TMP/emptyrepo" \
    SPIRA_TOML="$TMP/spira.toml" PATH="$TMP/binstub:$PATH" \
    pre-activate.sh "$REL" 2>&1
)"; rc=$?
is   "config: invalid spira.toml: exit 1"   1 "$rc"
want "config: invalid spira.toml: FAILs"    "FAIL config" "$out"

# ── store: bead store reachability/schema, via the bd on PATH ────────────────────────
REL="$TMP/rel-store"; mkrel "$REL"
mkbd 0; run "$REL"
is   "store: bd migrate status ok: exit 0"   0 "$rc"
want "store: bd migrate status ok: reports ok" "ok   store" "$out"

mkbd 1; run "$REL"
is   "store: bd migrate status fails: exit 1" 1 "$rc"
want "store: bd migrate status fails: FAILs"  "FAIL store" "$out"
mkbd 0

# ── units: units-install --render must exit 0 with no placeholder left unfilled ─────────
REL="$TMP/rel-units-ok"; UNITS_OUT="Description=fine" UNITS_RC=0 mkrel "$REL"
run "$REL"
is   "units: clean render: exit 0"        0 "$rc"
want "units: clean render: reports ok"    "ok   units" "$out"

REL="$TMP/rel-units-placeholder"; UNITS_OUT="ExecStart=@SPIRA_HOME@/loom" UNITS_RC=0 mkrel "$REL"
run "$REL"
is   "units: leftover placeholder: exit 1" 1 "$rc"
want "units: leftover placeholder: names it" "@SPIRA_HOME@" "$out"

REL="$TMP/rel-units-crash"; UNITS_RC=1 mkrel "$REL"
run "$REL"
is   "units: units-install --render itself fails: exit 1" 1 "$rc"
want "units: units-install --render itself fails: FAILs"  "FAIL units" "$out"

# ── self-test: the release's own smoke test gates activation too ─────────────────────
REL="$TMP/rel-selftest-ok"; SELFTEST_RC=0 mkrel "$REL"
run "$REL"
is   "self-test: passes: exit 0"       0 "$rc"
want "self-test: passes: reports ok"   "ok   self-test" "$out"

REL="$TMP/rel-selftest-bad"; SELFTEST_RC=1 mkrel "$REL"
run "$REL"
is   "self-test: fails: exit 1"      1 "$rc"
want "self-test: fails: FAILs"       "FAIL self-test" "$out"

# ── a clean release passes every check at once, independently reported ───────────────
REL="$TMP/rel-all-clean"; mkrel "$REL"
run "$REL"
is "all clean: exit 0" 0 "$rc"
for c in deps config store units self-test; do
    want "all clean: $c reported ok" "ok   $c" "$out"
done

# ── one bad check among five fails the whole gate without hiding the others ──────────
REL="$TMP/rel-one-bad"; SELFTEST_RC=1 mkrel "$REL"
run "$REL"
is   "one bad of five: overall exit 1"        1 "$rc"
want "one bad of five: names the failure"     "FAIL self-test" "$out"
want "one bad of five: the other four still ok" "ok   deps" "$out"
want "one bad of five: units unaffected"        "ok   units" "$out"

# ── lifecycle: pending migrations apply before the flip; a failing one refuses it ────
# The stub spira-lc records its argv and the identity pre-activate hands it.
REL="$TMP/rel-lc-ok"; mkrel "$REL"; mkdir -p "$REL/lifecycle/migrations"
cat > "$REL/bin/spira-lc" <<EOF2
#!/usr/bin/env bash
echo "user=\$SPIRA_LC_USER pwfile=\${SPIRA_LC_PASSWORD_FILE-unset} admin=\${SPIRA_LC_ADMIN_USER-unset} \$*" > "$TMP/lc-argv"
echo "admin-migrate: applied 0003-x.sql"
exit 0
EOF2
chmod +x "$REL/bin/spira-lc"
SPIRA_LC_USER=spira_lc SPIRA_LC_PASSWORD_FILE="$TMP/svc.cred" run "$REL"
is   "lifecycle: migrations applied: exit 0" 0 "$rc"
want "lifecycle: reports the check" "ok   lifecycle" "$out"
want "lifecycle: runs admin-migrate against the release's own migrations" "admin-migrate $REL/lifecycle/migrations" "$(cat "$TMP/lc-argv")"
nowant "lifecycle: passes no retired --if-enforced (sp-v62vn)" "--if-enforced" "$(cat "$TMP/lc-argv")"
# sp-p1z81: the service identity lc-serve uses, untouched — never swapped for a default root.
want "lifecycle: as the service user, its credential file kept" "user=spira_lc pwfile=$TMP/svc.cred admin=unset admin-migrate" "$(cat "$TMP/lc-argv")"

REL="$TMP/rel-lc-bad"; mkrel "$REL"; mkdir -p "$REL/lifecycle/migrations"
printf '#!/usr/bin/env bash\necho "admin-migrate: 0003-x.sql FAILED" >&2\nexit 2\n' > "$REL/bin/spira-lc"
chmod +x "$REL/bin/spira-lc"
run "$REL"
is   "lifecycle: failing migration refuses the release" 1 "$rc"
want "lifecycle: names the failure" "FAIL lifecycle: admin-migrate: 0003-x.sql FAILED" "$out"

# A release refused for another reason must not change the live schema.
REL="$TMP/rel-lc-other-bad"; SELFTEST_RC=1 mkrel "$REL"; mkdir -p "$REL/lifecycle/migrations"
rm -f "$TMP/lc-argv"
cat > "$REL/bin/spira-lc" <<EOF2
#!/usr/bin/env bash
echo "\$*" > "$TMP/lc-argv"
exit 0
EOF2
chmod +x "$REL/bin/spira-lc"
run "$REL"
is   "lifecycle: another check failed: exit 1" 1 "$rc"
want "lifecycle: another check failed: reported skipped" "skip lifecycle" "$out"
is   "lifecycle: another check failed: admin-migrate never ran" "absent" "$([ -e "$TMP/lc-argv" ] && echo present || echo absent)"

# ── lifecycle, real spira-lc against a real store: root has a password, no admin set ──
# (sp-p1z81) Every migration applied: pre-activate passes on the service user alone. One
# pending: it refuses naming SPIRA_LC_ADMIN_USER/SPIRA_LC_ADMIN_PASSWORD and the migration.
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
LC_BIN="$(command -v spira-lc 2>/dev/null || true)"
REPO="$(cd "$HERE/.." && pwd -P)"
if [ -z "$DOLT_BIN" ] || [ -z "$LC_BIN" ]; then
    bad "lifecycle (real store): needs dolt and the tree's spira-lc on PATH" "dolt=[$DOLT_BIN] spira-lc=[$LC_BIN]"
else
    PORT=$(( ${SPIRA_LC_TESTDB_PORT:-3308} + 1300 + RANDOM % 300 ))
    mkdir -p "$TMP/dolt/data"
    cat > "$TMP/dolt/server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
data_dir: "$TMP/dolt/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
    "$DOLT_BIN" sql-server --config "$TMP/dolt/server.yaml" > "$TMP/dolt/server.log" 2>&1 & # batch-job: the suite's own disposable sql-server, killed by the EXIT trap
    SERVER_PID=$!
    dsql() { "$DOLT_BIN" --data-dir "$TMP/dolt" --host 127.0.0.1 --port "$PORT" --no-tls "$@"; } # batch-job: fixture SQL against the suite's private server
    up=0
    for _ in $(seq 1 50); do
        if timeout 5 "$DOLT_BIN" --data-dir "$TMP/dolt" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then up=1; break; fi
        sleep 0.2
    done
    [ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/dolt/server.log")"
    ROOTPW="root-pw-$RANDOM$RANDOM"; SVCPW="svc-pw-$RANDOM$RANDOM"
    printf '%s\n' "$SVCPW" > "$TMP/svc.cred"; chmod 600 "$TMP/svc.cred"
    sed -e "s/@SPIRA_LC_PASSWORD@/$SVCPW/" -e "s/@SPIRA_LC_RO_PASSWORD@/ro-$SVCPW/" "$REPO/lifecycle/grants.sql" > "$TMP/grants.sql"
    lc_env() { env SPIRA_LC_SOCKET=/nonexistent/test-pre-activate SPIRA_LC_HOST=127.0.0.1 SPIRA_LC_PORT="$PORT" SPIRA_LC_DB=spira_lifecycle \
        XDG_CONFIG_HOME="$TMP/home/.config" "$@"; }
    # SPIRA_LC_PASSWORD_FILE is declared config now (spira/conf.d), read only from $SPIRA_TOML
    # (spira-lc/src/db.rs password_from), never this literal env assignment — the complete
    # fixture's own declared path does not exist for this suite's throwaway server. Empty it
    # via tl_config so SPIRA_LC_PASSWORD="" (root, no password yet) is actually used below.
    tl_config SPIRA_LC_PASSWORD_FILE=""
    lc_env SPIRA_LC_USER=root SPIRA_LC_PASSWORD="" "$LC_BIN" admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/dolt/ddl.log" 2>&1 \
        && lc_env SPIRA_LC_USER=root SPIRA_LC_PASSWORD="" "$LC_BIN" admin-apply-ddl "$TMP/grants.sql" >>"$TMP/dolt/ddl.log" 2>&1 \
        && dsql -u root -p "" sql -q "ALTER USER 'root'@'localhost' IDENTIFIED BY '$ROOTPW'" >>"$TMP/dolt/ddl.log" 2>&1 \
        || bail "lifecycle fixture did not build: $(cat "$TMP/dolt/ddl.log")"
    is "lifecycle (real store): positive control: root with an empty password is refused, as in production" 1 \
        "$(dsql -u root -p "" sql -q "SELECT 1" >/dev/null 2>&1; echo $?)"

    REL="$TMP/rel-lc-real"; mkrel "$REL"; mkdir -p "$REL/lifecycle"
    ln -sf "$LC_BIN" "$REL/bin/spira-lc"
    cp -r "$REPO/lifecycle/migrations" "$REL/lifecycle/migrations"
    run_lc() { out="$(lc_env SPIRA_LC_USER=spira_lc SPIRA_LC_PASSWORD_FILE="$TMP/svc.cred" \
        HOME="$TMP/home" SPIRA_REPO="$TMP/emptyrepo" SPIRA_TOML="$TMP/no-such.toml" PATH="$TMP/binstub:$PATH" \
        pre-activate.sh "$1" 2>&1)"; rc=$?; }

    run_lc "$REL"
    is   "lifecycle (real store): every migration applied, service user only: exit 0" 0 "$rc"
    want "lifecycle (real store): passes without an admin" "ok   lifecycle (admin-migrate: every migration already applied" "$out"

    dsql -u root -p "$ROOTPW" --use-db spira_lifecycle sql -q "ALTER TABLE bead DROP COLUMN since" >/dev/null 2>&1
    run_lc "$REL"
    is   "lifecycle (real store): a pending migration, no admin: refused" 1 "$rc"
    want "lifecycle (real store): names the pending migration" "0002-since.sql is pending" "$out"
    want "lifecycle (real store): names the exit" "set SPIRA_LC_ADMIN_USER and SPIRA_LC_ADMIN_PASSWORD" "$out"
    nowant "lifecycle (real store): never a password" "$SVCPW" "$out"
fi

# ── self-test.sh itself: MANIFEST integrity, not just an exit-code stub ──────────────
REL="$TMP/rel-manifest-ok"
mkdir -p "$REL/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$REL/bin/thing"
chmod +x "$REL/bin/thing"
{
    printf 'commit deadbeef\n'
    printf 'bin/thing %s\n' "$(sha256sum "$REL/bin/thing" | awk '{print $1}')"
} > "$REL/MANIFEST"
self-test.sh "$REL" >"$TMP/selftest_out" 2>&1
is "self-test.sh: matching sha256: exit 0" 0 "$?"

printf 'tampered\n' >> "$REL/bin/thing"
self-test.sh "$REL" >"$TMP/selftest_out2" 2>&1
selftest_rc=$?
selftest_out="$(cat "$TMP/selftest_out2")"
is   "self-test.sh: tampered binary: exit 1"  1 "$selftest_rc"
want "self-test.sh: tampered binary: names it" "sha256" "$selftest_out"

# The gate on the real flip (a failing or missing pre-activate.sh refuses activation) is
# the release crate's `release verify` now (release/src/tests.rs); make install is gone (sp-gkfg1).

tl_summary
