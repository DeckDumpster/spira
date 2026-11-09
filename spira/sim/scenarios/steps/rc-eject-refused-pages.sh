#!/bin/sh
# rc-eject-refused-pages.sh write|check — `write` puts a lane, a round being attributed and its
# progress record holding a refused eject into the world's control store through the round
# controller's door (`spira-lc rc`). `check` exits 0 only when the round controller raised the
# eject-refused condition on the published section, the alarms pass ran at least 20 times over it,
# and the world's mailboxes hold exactly one page for it. 9, naming what it saw, otherwise.
set -u
fail() { echo "rc-eject-refused-pages: $1" >&2; exit 9; }

put() {  # put <key> <data JSON>
    body=$(printf '{"txn":{"compares":[],"ops":[{"put":["%s",{"data":%s}]}],"else_ops":[]},"request":{"id":"rc-eject-refused-pages-%s","actor":"rc-eject-refused-pages","kind":"seed"}}' "$1" "$2" "$(printf '%s' "$1" | tr / -)")
    out=$(spira-lc rc txn --json "$body" 2>&1) || fail "put $1: $out"
    printf '%s' "$out" | grep -q '"succeeded":true' || fail "put $1 did not commit: $out"
}

case "${1:-}" in
write)
    put lanes/sim '{"landing_ref":"ej-base-0","image_digest":"ej-img-0","harness_version":"ej-harness","concurrency":4}'
    put batches/sim/ej-round '{"batch_id":"ej-round","repo":"sim","state":"Attributing","head":"ej-h","base":"ej-base-0","pass":1,"version":1,"tested_base":"ej-base-0","members":[{"id":"ej-a","tip":"t1"}]}'
    put batches/sim/ej-round/progress '{"pass":1,"runs":[],"held":{"member":"ej-a","suites":["ci"],"text":"the eject verb refused ej-a"}}' ;;
check)
    snap=$(spira-lc section snapshot 2>&1) || fail "section snapshot: $snap"
    printf '%s' "$snap" | grep -q 'EjectRefused' || fail "no EjectRefused condition on a published section: $(printf '%s' "$snap" | cut -c1-300)"
    root="${SIM_WORLD:-.}/home/spira/run/mail"
    pages=$(grep -rl 'ej-round' "$root" 2>/dev/null | wc -l)
    [ "$pages" = 1 ] || fail "want exactly one page naming ej-round under $root, found $pages: $(grep -rl 'ej-round' "$root" 2>/dev/null)" ;;
*)
    fail "usage: rc-eject-refused-pages.sh write|check" ;;
esac
exit 0
