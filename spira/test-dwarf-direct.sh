#!/usr/bin/env bash
#
# test-dwarf-direct.sh — `dwarf direct` starts its unit with the release environment, so the
# directed dwarf reaches its claim from a user manager that carries no SPIRA_*.
#
# The stub systemd-run runs the dwarf part of the call under `env -i` with only the --setenv
# pairs it was given: what a transient user unit gets. A call with no --setenv leaves the child
# on `neither SPIRA_HOME nor SPIRA_RELEASE is set`, and the caller waits out its whole wait.
#
# tier: T1
# covers: dwarf/src/direct.rs dwarf/src/main.rs runtime/src/units.rs spira-config/src/release_env.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'kill $(cat "$TMP"/child.pid 2>/dev/null) 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

STUBS="$TMP/stubs"; RUN="$TMP/run"; STATE="$TMP/state"
mkdir -p "$STUBS" "$RUN" "$STATE"
tl_config SPIRA_RUN="$RUN" SPIRA_CLAIM_RETRIES=1 SPIRA_CLAIM_RETRY_DELAY_S=0

cat > "$STUBS/spira-lc" <<STUB
#!/usr/bin/env bash
echo "\$*" >> "$STATE/lc.log"
case "\$1" in
    show) holder="\$(sed -n 's/^event bead sp-x .*--actor \([^ ]*\) .*"Claim".*/\1/p' "$STATE/lc.log" | head -1)"
          if [ -n "\$holder" ]; then
              echo '{"bead":{"bead_id":"sp-x","state":"WORKING","holder":"'"\$holder"'","lease_until":null,"holds":"","express":0,"stack":"{}"}}'
          else
              echo '{"bead":{"bead_id":"sp-x","state":"READY","holder":null,"lease_until":null,"holds":"","express":0,"stack":"{}"}}'
          fi ;;
    list) echo "[]" ;;
esac
exit 0
STUB

cat > "$STUBS/systemd-run" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$@" > "$STATE/systemd-run.argv"
envs=(); out=""; cmd=()
for a in "\$@"; do
    if [ "\${#cmd[@]}" -gt 0 ]; then cmd+=("\$a")
    else case "\$a" in
        --setenv=*) envs+=("\${a#--setenv=}") ;;
        --property=StandardOutput=append:*) out="\${a#*append:}" ;;
        --*) ;;
        *) cmd+=("\$a") ;;
    esac; fi
done
[ -n "\$out" ] || out="$STATE/unit.out"
env -i "\${envs[@]}" "\${cmd[@]}" >> "\$out" 2>&1 &
echo \$! >> "$TMP/child.pid"
exit 0
STUB
chmod +x "$STUBS/spira-lc" "$STUBS/systemd-run"

# A fixture release with no Cargo.toml at its root, so the rune-key check lets its home through;
# the stubs sit ahead of the real programs in its bin/.
REL="$TMP/release"; REAL="$(tl_release)"
mkdir -p "$REL/bin" "$REL/spira"
for f in "$REAL"/bin/* "$STUBS"/*; do ln -sf "$f" "$REL/bin/"; done
for f in "$REAL"/spira/* "$REAL"/spira/.[!.]*; do [ -e "$f" ] && ln -sfn "$f" "$REL/spira/"; done
[ -e "$REAL/model-bin" ] && ln -sfn "$REAL/model-bin" "$REL/model-bin"
export SPIRA_RELEASE="$REL" PATH="$REL/bin:$REL/spira:$PATH"

# A POSITIVE CONTROL for the matcher: this is what the unit prints without the environment.
want "the failure text this suite looks for is the dwarf's own" "neither SPIRA_HOME nor SPIRA_RELEASE" \
    "$(env -i "$REL/bin/dwarf" builder 2>&1)"

out="$(env -i HOME="$HOME" PATH="$PATH" SPIRA_RELEASE="$REL" SPIRA_TOML="$SPIRA_TOML" SPIRA_CALL="$STUBS/systemd-run" \
    "$REL/bin/dwarf" direct builder sp-x --by concierge --reason test 2>&1)"
rc=$?

is "dwarf direct exits 0" 0 "$rc"
want "the unit was called with the release" "--setenv=SPIRA_RELEASE=$REL" "$(cat "$STATE/systemd-run.argv" 2>/dev/null)"
want "the stub spira-lc recorded a claim of sp-x" "event bead sp-x" "$(grep '^event bead sp-x .*"Claim"' "$STATE/lc.log" 2>/dev/null)"
nowant "the directed dwarf's output does not carry the missing-environment failure" "neither SPIRA_HOME nor SPIRA_RELEASE" "$out$(cat "$RUN"/direct-out/sp-x.log 2>/dev/null)"

tl_summary
