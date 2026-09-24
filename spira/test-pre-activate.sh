#!/usr/bin/env bash
#
# test-pre-activate.sh — pre-activate.sh gates a release against five checks
# before releases/current moves onto it, and make install refuses the flip
# when the gate fails. Every check gets a fixture release dir with stubbed
# probes (a fake spira-config, a fake bd, a fake install.sh --render, a fake
# self-test.sh) rather than the real dependency, since pre-activate exists
# specifically to run before those real dependencies are trusted.
#
# covers: spira/pre-activate.sh spira/self-test.sh Makefile
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-pre-activate.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
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
#   UNITS_OUT UNITS_RC                    install.sh --render stub
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
    cat > "$dir/systemd/install.sh" <<EOF
#!/usr/bin/env bash
[ "\$1" = "--render" ] || exit 2
printf '%s\n' "${UNITS_OUT:-clean unit text}"
exit ${UNITS_RC:-0}
EOF
    chmod +x "$dir/systemd/install.sh"
    cat > "$dir/spira/self-test.sh" <<EOF
#!/usr/bin/env bash
exit ${SELFTEST_RC:-0}
EOF
    chmod +x "$dir/spira/self-test.sh"
}

# run <release-dir> -> sets $out and $rc from a pre-activate.sh invocation in an
# explicit, minimal environment: no ambient spira.toml, a stubbed bd on PATH.
run() {
    out="$(
        HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" SPIRA_REPO="$TMP/emptyrepo" \
        PATH="$TMP/binstub:$PATH" \
        bash "$HERE/pre-activate.sh" "$1" 2>&1
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

REL="$TMP/rel-config-ok"; CONFIG_RC=0 mkrel "$REL"
printf 'spira = {}\n' > "$TMP/spira.toml"
out="$(
    HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" SPIRA_REPO="$TMP/emptyrepo" \
    SPIRA_TOML="$TMP/spira.toml" PATH="$TMP/binstub:$PATH" \
    bash "$HERE/pre-activate.sh" "$REL" 2>&1
)"; rc=$?
is   "config: valid spira.toml: exit 0"  0 "$rc"
want "config: valid spira.toml: reports ok" "ok   config" "$out"

REL="$TMP/rel-config-bad"; CONFIG_RC=1 mkrel "$REL"
out="$(
    HOME="$TMP/home" XDG_CONFIG_HOME="$TMP/home/.config" SPIRA_REPO="$TMP/emptyrepo" \
    SPIRA_TOML="$TMP/spira.toml" PATH="$TMP/binstub:$PATH" \
    bash "$HERE/pre-activate.sh" "$REL" 2>&1
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

# ── units: install.sh --render must exit 0 with no placeholder left unfilled ─────────
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
is   "units: install.sh --render itself fails: exit 1" 1 "$rc"
want "units: install.sh --render itself fails: FAILs"  "FAIL units" "$out"

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

# ── self-test.sh itself: MANIFEST integrity, not just an exit-code stub ──────────────
REL="$TMP/rel-manifest-ok"
mkdir -p "$REL/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$REL/bin/thing"
chmod +x "$REL/bin/thing"
{
    printf 'commit deadbeef\n'
    printf 'bin/thing %s\n' "$(sha256sum "$REL/bin/thing" | awk '{print $1}')"
} > "$REL/MANIFEST"
"$HERE/self-test.sh" "$REL" >"$TMP/selftest_out" 2>&1
is "self-test.sh: matching sha256: exit 0" 0 "$?"

printf 'tampered\n' >> "$REL/bin/thing"
"$HERE/self-test.sh" "$REL" >"$TMP/selftest_out2" 2>&1
selftest_rc=$?
selftest_out="$(cat "$TMP/selftest_out2")"
is   "self-test.sh: tampered binary: exit 1"  1 "$selftest_rc"
want "self-test.sh: tampered binary: names it" "sha256" "$selftest_out"

# ── Makefile: pre-activate gates the real flip, forward and rollback alike ───────────
# A throwaway git repo, not the suite's own checkout: a testenv container's checkout
# can be a worktree whose git metadata points at a host path that does not exist in
# the container, and `git rev-parse` (which the install recipe needs for _root/_sha)
# fails there for reasons that have nothing to do with pre-activate.
MROOT="$TMP/makerepo"
git init -q "$MROOT"
git -C "$MROOT" config user.email "test@test"
git -C "$MROOT" config user.name "test"
printf 'x\n' > "$MROOT/x.txt"
git -C "$MROOT" add x.txt
git -C "$MROOT" commit -q -m init
SHA="$(git -C "$MROOT" rev-parse HEAD)"
MAKE="make -C $MROOT -f $ROOT/Makefile"

MREL="$TMP/makerels"
mkdir -p "$MREL"

# A pre-existing release dir means make install's cargo-build branch is skipped —
# only the gate and the flip run, so this exercises the wiring with no cargo, no
# git archive, and no network: exactly the rollback path the Makefile comment names.
mkdir -p "$MREL/$SHA/spira"
printf '#!/usr/bin/env bash\nexit 1\n' > "$MREL/$SHA/spira/pre-activate.sh"
chmod +x "$MREL/$SHA/spira/pre-activate.sh"
$MAKE install SPIRA_RELEASES="$MREL" COMMIT=HEAD >"$TMP/make_out" 2>&1
make_rc=$?
make_out="$(cat "$TMP/make_out")"
is   "make install: failing gate: exit nonzero"        1 $([ "$make_rc" -ne 0 ] && echo 1 || echo 0)
is   "make install: failing gate: current not created" 1 $([ -e "$MREL/current" ] || echo 1)
want "make install: failing gate: says so" "pre-activate failed" "$make_out"

printf '#!/usr/bin/env bash\nexit 0\n' > "$MREL/$SHA/spira/pre-activate.sh"
$MAKE install SPIRA_RELEASES="$MREL" COMMIT=HEAD >"$TMP/make_out2" 2>&1
make_rc=$?
make_out="$(cat "$TMP/make_out2")"
is "make install: passing gate: exit 0" 0 "$make_rc"
is "make install: passing gate: current flips" "$SHA" "$(basename "$(readlink "$MREL/current")")"

# A release with no pre-activate.sh at all is refused, not silently activated.
MREL2="$TMP/makerels-nopre"
mkdir -p "$MREL2/$SHA"
$MAKE install SPIRA_RELEASES="$MREL2" COMMIT=HEAD >"$TMP/make_out3" 2>&1
make_rc=$?
make_out="$(cat "$TMP/make_out3")"
is   "make install: no pre-activate.sh: exit nonzero" 1 $([ "$make_rc" -ne 0 ] && echo 1 || echo 0)
is   "make install: no pre-activate.sh: current not created" 1 $([ -e "$MREL2/current" ] || echo 1)
want "make install: no pre-activate.sh: names the reason" "unverifiable" "$make_out"

echo "-- $pass ok, $fail failed --"
[ "$fail" -eq 0 ]
