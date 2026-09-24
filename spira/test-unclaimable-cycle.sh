#!/usr/bin/env bash
#
# test-unclaimable-cycle.sh — the unclaimable detector must not report on its OWN reports.
#
#   ./test-unclaimable-cycle.sh
#
# THE DEFECT. A report about an unclaimable bead is filed into the incident partition. When
# that partition is unservable — a narrowed roster, a missing persona — the report is itself
# unclaimable, so the detector reports it, and that report is unclaimable too. The dedup key
# is unclaimable:<subject>, and every link is a NEW subject, so dedup never fires.
#
# Measured 2026-09-12: one watchtower incident seeded a 128-deep chain and took the store from
# 58 beads to 310 in minutes during a single-persona focus period. Nothing was wrong except
# the missing base case — the detector was correctly reporting a condition it was itself
# creating more of.
#
# CASE 1 IS THE POSITIVE CONTROL and matters as much as the guard: a genuinely unclaimable
# bead must still be reported. A detector that reports nothing passes every other case here.
#
# covers: spira/lib.sh
# hermetic-ok: drives the detector's own python body against fixture JSON; no database,
# hermetic-ok: no systemd, no network
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok(){ pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad(){ fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }

echo "test-unclaimable-cycle.sh"
out="$(LIBSH="$HERE/lib.sh" python3 - <<'PY'
import json,subprocess,os,sys
src=open(os.environ["LIBSH"]).read()
seg=src.split("detect_unclaimable_ready() {",1)[1]
body=seg.split('| PARTS="$parts" ALL_PARTS="$all_parts" python3 -c \'',1)[1].split("\n' 2>/dev/null",1)[0]
body=body.replace("'\\''","'")
def run(beads):
    env=dict(os.environ); env["PARTS"]="builder|spira,plan|spira-poison\n"; env["ALL_PARTS"]="builder|spira,plan|spira-poison\n"; env["SPIRA_SCOPE_LABEL"]="spira"
    p=subprocess.run([sys.executable,"-c",body],input=json.dumps(beads),capture_output=True,text=True,env=env)
    if p.returncode!=0:
        print("HARNESS %s" % p.stderr.strip().splitlines()[-1] if p.stderr else "HARNESS unknown"); return None
    return [l for l in p.stdout.splitlines() if l.startswith("UNCLAIMABLE")]
genuine={"id":"sp-real","labels":["spira","incident","repo:spira"]}
report ={"id":"sp-rep","labels":["spira","groom","repo:spira"],"external_ref":"unclaimable:sp-real"}
a=run([genuine]); b=run([report]); c=run([genuine,report])
print("CONTROL %d" % (len(a) if a is not None else -1))
print("GUARD %d"   % (len(b) if b is not None else -1))
print("BOTH %d %s" % ((len(c) if c is not None else -1), ("sp-real" in (c[0] if c else "")) ))
PY
)"
case "$out" in *"HARNESS"*) bad "harness" "$out";; esac
[ "$(sed -n 's/^CONTROL //p' <<<"$out")" = 1 ] && ok "control — a genuinely unclaimable bead is still reported" \
    || bad "control" "expected 1 report, got [$(sed -n 's/^CONTROL //p' <<<"$out")]"
[ "$(sed -n 's/^GUARD //p' <<<"$out")" = 0 ] && ok "the detector does not report its own report — cycle has a base case" \
    || bad "guard" "expected 0, got [$(sed -n 's/^GUARD //p' <<<"$out")]"
[ "$(sed -n 's/^BOTH //p' <<<"$out")" = "1 True" ] && ok "both present — exactly one line, about the real bead" \
    || bad "both" "expected [1 True], got [$(sed -n 's/^BOTH //p' <<<"$out")]"
echo
printf 'test-unclaimable-cycle.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
