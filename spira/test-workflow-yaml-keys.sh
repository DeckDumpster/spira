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
. "$HERE/testlib.sh"

python3 -I -c "import yaml" 2>/dev/null || skip "python3 has no yaml module on this box"

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

plant="$(mktemp)"; trap 'rm -f "$plant"' EXIT
printf 'env:\n  A: 1\n  A: 2\n' >"$plant"
python3 -I -c "$checker" "$plant" >/dev/null 2>&1
isnz=$?
[ "$isnz" != 0 ] && ok "positive control detects duplicate keys" || { bad "positive control" "duplicate key not detected"; exit 1; }

out="$(python3 -I -c "$checker" "$ROOT"/.github/workflows/*.yml 2>&1)"
wantrc "no workflow has duplicate mapping keys: $out" 0 $?
