#!/usr/bin/env bash
#
# test-workflow-yaml-keys.sh — every .github/workflows/*.yml parses under a loader that
# rejects repeated mapping keys. GitHub refuses such a file and starts zero jobs, while
# PyYAML's safe_load accepts it silently.
#
# POSITIVE CONTROL: a planted duplicate-key document must be rejected before silence on
# the real workflows is believed.
#
# host-reason: parses checked-in YAML with python; no container or database dependency
#
# tier: T0
# covers: .github/workflows/*.yml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

checker='
import sys, yaml
class L(yaml.SafeLoader): pass
def ctor(loader, node, deep=False):
    seen = set()
    for k, _ in node.value:
        key = loader.construct_object(k, deep=True)
        if key in seen:
            raise yaml.constructor.ConstructorError(None, None, "duplicate key %r" % (key,), k.start_mark)
        seen.add(key)
    return yaml.SafeLoader.construct_mapping(loader, node, deep)
L.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, ctor)
rc = 0
for p in sys.argv[1:]:
    try:
        yaml.load(open(p), Loader=L)
    except Exception as e:
        print("%s: %s" % (p, e)); rc = 1
sys.exit(rc)
'

fail=0
plant="$(mktemp)"; trap 'rm -f "$plant"' EXIT
printf 'env:\n  A: 1\n  A: 2\n' >"$plant"
if python3 -I -c "$checker" "$plant" >/dev/null 2>&1; then
  echo "FAIL: positive control — duplicate key not detected"; exit 1
fi
echo "ok: positive control detects duplicate keys"

if out="$(python3 -I -c "$checker" "$ROOT"/.github/workflows/*.yml 2>&1)"; then
  echo "ok: no workflow has duplicate mapping keys"
else
  echo "FAIL: $out"; fail=1
fi
exit $fail
