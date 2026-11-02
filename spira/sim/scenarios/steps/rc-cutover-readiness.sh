#!/bin/sh
# rc-cutover-readiness.sh <bead> — once <bead> is SUBMITTED in sim, seed each round repo's lane at
# its landing ref and a green base verdict there through `spira-lc rc`, then run `round-controller
# cutover-check --readiness`. Exit 3 until <bead> is SUBMITTED, 0 when readiness exits 1 with
# exactly the expected rows, 9 otherwise, with its output on stderr.
set -u
bead="$1"
out=""
fail() {
    printf 'rc-cutover-readiness: %s\n%s\n' "$1" "$out" >&2
    exit 9
}
[ "$(spira-lc state "$bead" 2>/dev/null)" = SUBMITTED ] || exit 3

seed() {
    sha=$(git -C "$2" rev-parse --verify -q "local/main^{commit}") || fail "$1 has no local/main in $2"
    python3 - "$1" "$sha" <<'PY' | spira-lc rc txn --json - >/dev/null || fail "seeding $1's lane and base verdict was refused"
import hashlib, json, sys
repo, sha = sys.argv[1], sys.argv[2]
image, harness = "sha256:sim-image", "sim-harness"
ident = json.dumps({"repo": repo, "members": [], "base": sha, "image_digest": image,
                    "harness_version": harness, "selection": "all"}, separators=(",", ":"))
lane = "lanes/" + repo
ops = [
    {"put": [lane, {"data": {"landing_ref": sha, "image_digest": image, "harness_version": harness}}]},
    {"put": ["verdicts/%s/%s" % (repo, hashlib.sha256(ident.encode()).hexdigest()),
             {"metadata": {"ownerReferences": [lane]},
              "data": {"class": "green", "attempts": 1, "last_attempt": 0}}]},
]
print(json.dumps({"txn": {"compares": [], "ops": ops, "else_ops": []},
                  "request": {"id": "seed-%s-%s" % (repo, sha), "actor": "scenario", "kind": "seed"}}))
PY
}
seed sim "$SIM_WORLD/work"
seed other "$SIM_WORLD/repos/other/work"

out=$(round-controller cutover-check --readiness 2>&1)
rc=$?
[ "$rc" = 1 ] || fail "readiness exited $rc, not 1"
for row in \
    'sim green ci=green base=green submitted=green' \
    'other red ci=green base=green submitted=red' \
    'global green repos=green rounds=green queue-files=green' \
    '  submitted: no SUBMITTED bead' \
    'readiness=red'; do
    printf '%s\n' "$out" | grep -qxF -- "$row" || fail "no row: $row"
done
exit 0
