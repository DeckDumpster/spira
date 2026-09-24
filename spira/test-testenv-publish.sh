#!/usr/bin/env bash
# test-testenv-publish.sh — packages:write is declared in the suites job,
# publish pushes under the closure tag, and a prior publish turns a subsequent
# acquire from a build into a pull.
#
# WHAT IS TESTED
#   1. The suites job in gate.yml declares packages:write, not packages:read.
#      With only read, the publish step runs, fails, and continue-on-error
#      swallows it — the registry stays empty forever
#      (law-a-control-that-cannot-check-must-refuse).
#   2. testenv.sh publish pushes under the exact tag testenv.sh tag prints.
#      No floating :latest is ever pushed.
#   3. Miss → publish → hit. After a publish, an acquire pulls the image rather
#      than rebuilding it. The asserted property is the closure tag in the
#      returned ref, not the prose line naming which path ran
#      (law-a-matcher-reads-code-not-prose).
#
# POSITIVE CONTROLS
#   1. A mock gate.yml with packages:read in the suites job is shown to fail the
#      check before the real gate.yml is asserted to pass it.
#   3. The miss path (no prior publish) is shown to build before the hit path
#      (after publish) is shown to pull.
#
# FIXTURES. podman is stubbed on PATH. No real container is started or image
# transferred. The stub records its argv; assertions read from that log.
#
# covers: .github/workflows/gate.yml spira/testenv.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
saw()    { grep -q -- "$2" "$3" 2>/dev/null && ok "$1" \
               || bad "$1" "no [$2] in podman calls"; }
notsaw() { grep -q -- "$2" "$3" 2>/dev/null \
               && bad "$1" "must not have called podman with [$2]" || ok "$1"; }

echo "test-testenv-publish.sh"

GATE_YML="$ROOT/.github/workflows/gate.yml"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

FIXTURE="$TMP/spira"
mkdir -p "$FIXTURE/testenv" "$TMP/bin"
cp "$HERE/testenv.sh"            "$FIXTURE/testenv.sh"
cp "$HERE/conf.sh"               "$FIXTURE/conf.sh"
cp "$HERE/testenv/Containerfile" "$FIXTURE/testenv/Containerfile"
PIN="$TMP/bd-pin"; printf 'BD_PIN_MIGRATIONS=42\n' > "$PIN"

# podman stub: STUB_LOCAL=1 means image already exists locally;
# STUB_PULL=0 means pull fails (empty or unreachable registry).
cat > "$TMP/bin/podman" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PODMAN_LOG"
case "$1" in
    image)  [ "${STUB_LOCAL:-0}" = "1" ] && exit 0; exit 1 ;;
    pull)   [ "${STUB_PULL:-0}"  = "1" ] && exit 0; exit 1 ;;
    build|tag|push) exit 0 ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/podman"

LOG="$TMP/podman.log"
run() {
    local reg="$1" loc="$2" pul="$3"; shift 3
    : > "$LOG"
    PATH="$TMP/bin:$PATH" PODMAN_LOG="$LOG" STUB_LOCAL="$loc" STUB_PULL="$pul" \
    SPIRA_BD_PIN="$PIN" SPIRA_TESTENV_REGISTRY="$reg" \
        bash "$FIXTURE/testenv.sh" "$@" 2>/dev/null
}

TAG="$(SPIRA_BD_PIN="$PIN" bash "$FIXTURE/testenv.sh" tag 2>/dev/null)"

# ==========================================================================
echo
echo "1. the suites job in gate.yml declares packages:write:"

# POSITIVE CONTROL. A mock gate.yml with packages:read in the suites job must
# fail the check before trusting it when the real gate.yml passes.
cat > "$TMP/mock-gate.yml" <<'MOCK'
jobs:
  suites:
    permissions:
      contents: read
      packages: read
  gate:
    runs-on: ubuntu-latest
MOCK
_mock_block="$(awk '/^  suites:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' \
    "$TMP/mock-gate.yml")"
if printf '%s' "$_mock_block" | grep -q 'packages: write'; then
    bad "positive control: packages:read does not satisfy the write check" \
        "mock read block was wrongly accepted as write"
else
    ok "positive control: packages:read is not packages:write"
fi

_suites_block="$(awk '/^  suites:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' \
    "$GATE_YML")"
if [ -z "$_suites_block" ]; then
    bad "suites job block found (positive control)" \
        "awk extracted nothing; assertion below would be vacuous"
else
    ok "suites job block found (positive control)"
fi
if printf '%s' "$_suites_block" | grep -q 'packages: write'; then
    ok "suites job declares packages:write"
else
    bad "suites job declares packages:write" \
        "only packages:read — publish runs, fails, continue-on-error swallows it, registry stays empty"
fi

# ==========================================================================
echo
echo "2. publish pushes the closure tag testenv.sh tag prints, not :latest:"

[ -n "$TAG" ] && ok "closure tag resolves ($TAG)" \
              || bad "closure tag resolves" "empty — cannot verify identity"

out="$(run "example.invalid/spira" 1 0 publish)"; rc=$?
[ "$rc" -eq 0 ] && ok "publish exits 0" || bad "publish exits 0" "rc=$rc"
saw    "push is invoked"              "push"    "$LOG"
saw    "closure tag is what is pushed" "$TAG"   "$LOG"
notsaw "no :latest is pushed"        ":latest"  "$LOG"
case "$out" in
    *"$TAG"*) ok "publish returns a ref containing the closure tag" ;;
    *)        bad "publish returns a ref containing the closure tag" "got [$out]" ;;
esac

# ==========================================================================
echo
echo "3. miss → publish → hit: a published image is pulled, not rebuilt:"

# MISS (positive control for seen-to-fail): registry image absent → build runs.
# Without the publish step a gate run would always land here. A configured
# registry is still consulted (pull is attempted, fails), and then the build runs.
run "example.invalid/spira" 0 0 image >/dev/null
saw "miss drives a build (positive control)" "build" "$LOG"

# PUBLISH under the closure tag.
pushed_ref="$(run "example.invalid/spira" 1 0 publish)"
pushed_tag="$(printf '%s' "$pushed_ref" | awk -F: '{print $NF}')"
[ -n "$pushed_tag" ] && ok "publish produced a ref ($pushed_ref)" \
                      || bad "publish produced a ref" "empty"

# HIT: registry now has the image → pull runs, build does not.
hit_ref="$(run "example.invalid/spira" 0 1 image)"
saw    "hit drives a pull"    "pull"  "$LOG"
notsaw "hit does not rebuild" "build" "$LOG"

# Identity: the pulled image carries the same closure tag as what was published.
hit_tag="$(printf '%s' "$hit_ref" | awk -F: '{print $NF}')"
if [ -n "$hit_tag" ] && [ "$hit_tag" = "$pushed_tag" ] && [ "$hit_tag" = "$TAG" ]; then
    ok "pulled image carries the same closure tag as what was published ($hit_tag)"
else
    bad "pulled image carries the same closure tag as what was published" \
        "published=[$pushed_tag] pulled=[$hit_tag] expected=[$TAG]"
fi

# ==========================================================================
echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
