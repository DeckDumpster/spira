#!/usr/bin/env bash
# testlib/heimdall-fixture.sh — test-heimdall.sh's world: its own SPIRA_HOME and repositories, a
# fresh SQLite control store behind a shim that can fail or time out spira-lc on demand, a
# synthetic sentinel log, and the helpers that read heimdall's conditions back.
#
# Sourced, never executed, by a suite that already set HERE, TMP and REAL_LC (the real spira-lc).

# mklog — a synthetic sentinel log, in the format lib.sh's log() actually writes.
#   mklog <passes> <ready> <dwarves> <called:0|1> <complete:0|1> <last-pass-ends-at>
mklog() {
    N="$1" READY="$2" DWARVES="$3" SUMM="$4" DONE="$5" END="$6" EXTRA="${7:-}" python3 -c '
import datetime, os
n, end = int(os.environ["N"]), int(os.environ["END"])
out = []
for i in range(n):
    t = end - (n - 1 - i) * 120
    ts = datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    out.append("%s spira: state: open=20 plan_ready=%s in_progress=0 dwarves=%s runes=[builder ]"
               % (ts, os.environ["READY"], os.environ["DWARVES"]))
    if os.environ["EXTRA"]:
        out.append("%s spira: %s" % (ts, os.environ["EXTRA"]))
    if os.environ["SUMM"] == "1":
        out.append("%s spira: CHECK7 builder: %s ready, 1 free — calling" % (ts, os.environ["READY"]))
    if os.environ["DONE"] == "1":
        out.append("%s spira: pass complete — 0 action(s), 0 progress" % ts)
print("\n".join(out))'
}

SH="$TMP/spira"; RUN="$TMP/run"; mkdir -p "$SH" "$RUN"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"
# ITS OWN SPIRA_HOME AND ITS OWN repository map, so nothing reads the operator's real repositories.
tl_repo_map "brain | $TMP/repo | push | origin/main | |"

# A fresh control store: SQLite makes an empty file a new database. Direct mode, no service,
# and no lifecycle credential: SPIRA_LC_PASSWORD_FILE is a registered key, so undeclared it
# names the fixture home's absent credential, and spira-lc refuses before reaching the store.
STORE="$TMP/control.db"; : > "$STORE"
export SPIRA_LC_PASSWORD=unused
tl_config SPIRA_LC_SOCKET="" SPIRA_LC_PASSWORD_FILE="" SPIRA_LC_STORE_PATH="$STORE"

# The shim in front of the real spira-lc: `ok` passes through, `write-fail` refuses every
# write, `timeout` answers nothing as a timed-out call does.
mkdir -p "$TMP/shim"; printf ok > "$TMP/lc-mode"
cat > "$TMP/shim/spira-lc" <<SHIM
#!/usr/bin/env bash
case "\$(cat "$TMP/lc-mode")" in
    write-fail) [ "\${2:-}" = list ] || { echo "shim: write refused" >&2; exit 1; } ;;
    timeout) exit 124 ;;
esac
exec "$REAL_LC" "\$@"
SHIM
chmod +x "$TMP/shim/spira-lc"
export PATH="$TMP/shim:$PATH"; tl_declare_release

heimdall() {   # heimdall [--report] — one run against the fixture
    tl_config SPIRA_RUN="$RUN" SPIRA_EXPORTER="" SPIRA_WIKI="" SPIRA_LC_STORE_PATH="${HEIMDALL_STORE:-$STORE}"
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$TMP/repo" SPIRA_SYSTEMCTL=true \
    SPIRA_HEIMDALL_SENTINEL_LOG="$RUN/sentinel.log" \
        command heimdall --home "$SH" "$@" 2>&1
}
cond_field() {   # cond_field <key> <field...> -> the fields space-joined, or "-" with no condition
    "$REAL_LC" condition list --owner heimdall 2>/dev/null | KEY="$1" python3 -c '
import sys, os, json
fields = sys.argv[1:]
try: rows = json.load(sys.stdin)["conditions"]
except Exception: rows = []
for c in rows:
    if c["key"] == os.environ["KEY"]:
        print(" ".join(str(c.get(f)) for f in fields)); break
else:
    print("-")' "${@:2}"
}
cond_status() { cond_field "$1" status flaps; }
n_alerts() {
    "$REAL_LC" condition list --owner heimdall --type Alert 2>/dev/null \
        | python3 -c 'import sys, json; print(len(json.load(sys.stdin)["conditions"]))' 2>/dev/null || echo "?"
}
# A REALISTIC WEDGE: passes completed normally until an hour ago, then kept starting and
# stopped finishing. A log with no completed pass ANYWHERE cannot distinguish a wedged
# loop from a box where Heimdall came up first, and the floor deliberately reads it as the
# second — so a fixture without completions would test the floor, not the wedge.
wedge()  { { mklog 6 5 1 1 1 "$(( $(date +%s) - 3600 ))"
             mklog 10 5 0 0 0 "$(date +%s)"; } > "$RUN/sentinel.log"; }
heal()   { mklog 10 5 1 1 1 "$(date +%s)" > "$RUN/sentinel.log"; }
