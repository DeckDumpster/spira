#!/usr/bin/env bash
#
# test-gh-intake.sh — gh-intake: wiring smoke test over the real binary.
#
# gh-intake.sh (bash) is retired (sp-8fsql); its triage/dedup decision tree — trust by
# author_association, spira:accept promotion checked against current access, idempotent
# re-run, the post-check refusing an unlabelled work bead — is now `logic::run()` in the
# `gh-intake` crate and is covered by `cargo test -p gh-intake` (gh-intake/DESIGN.md §5).
# What THAT suite cannot cover — because it never touches a real subprocess or a real
# socket — is what this suite checks instead:
#
#   1. the real binary reads its env vars and really calls out over HTTP to
#      SPIRA_GH_INTAKE_API (law-probe-a-fixture-not-production): a local stub HTTP server,
#      not `curl`, since this binary makes its own HTTP calls (ureq) rather than shelling
#      out to curl the way the bash did.
#   2. it really execs `bd` (via SPIRA_BD) and `mail.sh` (via SPIRA_MAIL_BIN) as
#      subprocesses, with the argv/stdin shape those stubs expect.
#   3. --dry-run really makes no bd-create or mail calls.
#   4. no credential ever reaches the wire: every request the stub server receives is
#      checked for an Authorization header and for the canary token value
#      (law-beads-is-never-public) — a real behavioural check, not a source grep.
#
# tier: T1
# covers: gh-intake/src/*.rs systemd/spira-gh-intake.service systemd/spira-gh-intake.timer
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-gh-intake.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-gh-intake: cargo not found — gh-intake binary cannot be built"
    exit 77
fi

T="$(mktemp -d)"; trap 'rm -rf "$T"; [ -n "${STUB_PID:-}" ] && kill "$STUB_PID" 2>/dev/null || true' EXIT INT TERM

CRATE_ROOT="$HERE/../gh-intake"
BUILD_LOG="$T/cargo-build.log"
if ! CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/target" \
    "$CARGO_BIN" build --manifest-path "$CRATE_ROOT/Cargo.toml" -p gh-intake >"$BUILD_LOG" 2>&1
then
    bad "cargo build -p gh-intake" "see $BUILD_LOG"
    tail -60 "$BUILD_LOG" >&2
    tl_summary
fi
ok "cargo build -p gh-intake"
BIN="$T/target/debug/gh-intake"
[ -x "$BIN" ] || bail "gh-intake binary not found at $BIN"

# ---- stub HTTP server: serves canned JSON by exact path, logs every request ---------------
STUB_DIR="$T/stub"; mkdir -p "$STUB_DIR"
REQLOG="$T/requests.log"; : > "$REQLOG"
STUB_PID=""
start_stub() {
    STUB_DIR="$STUB_DIR" REQLOG="$REQLOG" python3 -c '
import http.server, os, sys, json

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        with open(os.environ["REQLOG"], "a") as lg:
            lg.write(self.path + "\t" + json.dumps(dict(self.headers)) + "\n")
        path = self.path.split("?", 1)[0]
        key = path.lstrip("/").replace("/", "_")
        f = os.path.join(os.environ["STUB_DIR"], key + ".json")
        status_f = os.path.join(os.environ["STUB_DIR"], key + ".status")
        status = 200
        if os.path.exists(status_f):
            status = int(open(status_f).read().strip())
        if os.path.exists(f):
            data = open(f, "rb").read()
        elif status == 204:
            data = b""
        else:
            status, data = 404, b"{\"message\":\"Not Found\"}"
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        if data:
            self.wfile.write(data)

port = int(sys.argv[1])
http.server.HTTPServer(("127.0.0.1", port), H).serve_forever()
' "$1" &
    STUB_PID=$!
    # POLL FOR READY, NOT A FIXED SLEEP. A fixed 0.3s was enough on a bare-metal sandbox
    # but not always under testenv's container, where process/port startup is slower
    # under load — the exact "budget/deadline is infrastructure" class of flake this
    # suite must not paper over with a longer fixed sleep (law-absence-needs-a-positive-
    # control: waiting on the real signal, not a guess at how long it takes).
    local _tries=0
    while ! python3 -c "
import socket, sys
s = socket.socket()
s.settimeout(0.2)
try:
    s.connect(('127.0.0.1', $1))
except OSError:
    sys.exit(1)
s.close()
" 2>/dev/null; do
        _tries=$((_tries + 1))
        [ "$_tries" -lt 50 ] || { echo "test-gh-intake: stub HTTP server never accepted a connection" >&2; return 1; }
        sleep 0.1
    done
}
STUB_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')"
start_stub "$STUB_PORT" || bail "stub HTTP server failed to start"
API="http://127.0.0.1:$STUB_PORT"

put_fixture() { printf '%s' "$2" > "$STUB_DIR/$1.json"; }
put_status() { printf '%s' "$2" > "$STUB_DIR/$1.status"; }

put_fixture "repos_acme_widgets_issues" \
    '[{"number":1,"title":"crash on save","body":"steps to repro","user":{"login":"alice"},"labels":[],"author_association":"OWNER"},
      {"number":2,"title":"feature idea","body":"line one\nline two","user":{"login":"stranger"},"labels":[],"author_association":"NONE"}]'

# ---- stub bd: models the store minimally, records every call -----------------------------
BDLOG="$T/bd.log"; : > "$BDLOG"
CREATED="$T/created"; : > "$CREATED"
cat > "$T/bin_bd" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BDLOG"
case " $* " in
    *" list "*)
        printf '[{"id":"sp-seed","external_ref":"seed","labels":["spira"]}'
        while read -r line; do printf ',%s' "$line"; done < "$CREATED"
        printf ']\n'
        exit 0 ;;
    *" create "*)
        ref=""; labels=""
        while [ $# -gt 0 ]; do
            case "$1" in --external-ref) ref="$2"; shift 2 ;; --labels) labels="$2"; shift 2 ;; *) shift ;; esac
        done
        cat >/dev/null # consume the body
        lbl_json="$(printf '%s' "$labels" | tr ',' '\n' | sed 's/.*/"&"/' | paste -sd, -)"
        printf '{"id":"sp-new%s","external_ref":"%s","labels":[%s]}\n' "$RANDOM" "$ref" "$lbl_json" >> "$CREATED"
        exit 0 ;;
    *" note "*|*" close "*) exit 0 ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$T/bin_bd"
export BDLOG CREATED

# ---- stub mail.sh: records subject + stdin body -------------------------------------------
MAILLOG="$T/mail.log"; : > "$MAILLOG"
cat > "$T/bin_mail.sh" <<'STUB'
#!/usr/bin/env bash
printf 'argv: %s\n' "$*" >> "$MAILLOG"
cat >> "$MAILLOG"
exit 0
STUB
chmod +x "$T/bin_mail.sh"
export MAILLOG

# ---- a real repo-map: RealRepo's root_with_git reads SPIRA_REPO_MAP directly now
# (sp-k6lku, "wave 4.13") through spira_config::repos, not a bash repo_root() seam, so the
# fixture is the map file itself, not a faked lib.sh function.
mkdir -p "$T/spira-home"
REPOCHECKOUT="$T/repo-checkout"; mkdir -p "$REPOCHECKOUT/.git"
REPOMAP="$T/repo-map"
printf 'widgets | %s\n' "$REPOCHECKOUT" > "$REPOMAP"

run_intake() {
    env -i PATH="$T:/usr/bin:/bin" HOME="$HOME" \
        BDLOG="$BDLOG" CREATED="$CREATED" MAILLOG="$MAILLOG" \
        SPIRA_DB=fixture SPIRA_BD="$T/bin_bd" SPIRA_MAIL_BIN="$T/bin_mail.sh" \
        SPIRA_HOME="$T/spira-home" SPIRA_REPO_MAP="$REPOMAP" \
        SPIRA_GH_INTAKE_REPO="acme/widgets" SPIRA_GH_INTAKE_BEAD_REPO="widgets" \
        SPIRA_GH_INTAKE_API="$API" SPIRA_GH_INTAKE_PRIORITY="2" \
        SPIRA_SCOPE_LABEL="spira" SPIRA_PLAN_LABEL="plan" \
        GITHUB_TOKEN="canary-token-must-never-be-sent" \
        "$BIN" "$@" 2>&1
}

echo
echo "1. wiring: a real run talks to the stub server, execs bd and mail.sh, creates real beads"
out="$(run_intake)"; rc=$?
is "exits 0" "0" "$rc"
want "logs the resolved repo checkout" "$REPOCHECKOUT" "$out"
if grep -q 'list --all' "$BDLOG"; then ok "bd list --all was invoked"; else bad "bd list --all was invoked" "not in $BDLOG"; fi
if grep -q 'github:acme/widgets#1' "$CREATED"; then ok "trusted issue #1 became a work bead"; else bad "trusted issue #1 became a work bead" "not in $CREATED"; fi
if grep -q 'github-untrusted:acme/widgets#2' "$CREATED"; then ok "untrusted issue #2 became an untrusted record"; else bad "untrusted issue #2 became an untrusted record" "not in $CREATED"; fi
if grep -q 'new untrusted' "$MAILLOG"; then ok "the digest was mailed via mail.sh"; else bad "the digest was mailed via mail.sh" "not in $MAILLOG"; fi

echo
echo "2. --dry-run makes no bd-create or mail call"
: > "$BDLOG"; : > "$CREATED"; : > "$MAILLOG"
out="$(run_intake --dry-run)"; rc=$?
is "exits 0" "0" "$rc"
want "reports what it would create" "would create work bead" "$out"
if [ -s "$CREATED" ]; then bad "no bead was actually created" "$(cat "$CREATED")"; else ok "no bead was actually created"; fi
if [ -s "$MAILLOG" ]; then bad "no mail was actually sent" "$(cat "$MAILLOG")"; else ok "no mail was actually sent"; fi

echo
echo "3. no credential ever reaches the wire"
if grep -qi 'authorization' "$REQLOG"; then
    bad "no request carries an Authorization header" "$(grep -i authorization "$REQLOG")"
else
    ok "no request carries an Authorization header"
fi
if grep -q 'canary-token-must-never-be-sent' "$REQLOG"; then
    bad "the GITHUB_TOKEN canary never appears on the wire" "found in $REQLOG"
else
    ok "the GITHUB_TOKEN canary never appears on the wire"
fi
[ -s "$REQLOG" ] && ok "positive control: the stub server actually received requests" \
    || bad "positive control: the stub server actually received requests" "empty $REQLOG"

echo
echo "4. an unresolvable bead repo is refused, not silently skipped"
out="$(env -i PATH="$T:/usr/bin:/bin" HOME="$HOME" \
    SPIRA_DB=fixture SPIRA_BD="$T/bin_bd" SPIRA_MAIL_BIN="$T/bin_mail.sh" \
    SPIRA_HOME="$T/spira-home" SPIRA_REPO_MAP="$REPOMAP" \
    SPIRA_GH_INTAKE_REPO="acme/widgets" SPIRA_GH_INTAKE_BEAD_REPO="no-such-repo" \
    SPIRA_GH_INTAKE_API="$API" "$BIN" 2>&1)"; rc=$?
is "exits 1" "1" "$rc"
want "names the unresolved repo" "no-such-repo" "$out"

echo
echo "5. systemd unit points at the built binary, not a deleted script"
SERVICE="$HERE/../systemd/spira-gh-intake.service"
if [ -f "$SERVICE" ] && grep -q '@SPIRA_PROD_ROOT@/bin/gh-intake' "$SERVICE"; then
    ok "spira-gh-intake.service ExecStarts @SPIRA_PROD_ROOT@/bin/gh-intake"
else
    bad "spira-gh-intake.service ExecStarts @SPIRA_PROD_ROOT@/bin/gh-intake" "not found in $SERVICE"
fi
if [ -e "$HERE/gh-intake.sh" ]; then
    bad "gh-intake.sh is deleted" "still present at $HERE/gh-intake.sh"
else
    ok "gh-intake.sh is deleted"
fi

if grep -q '^Environment=SPIRA_DB=@SPIRA_DB@$' "$SERVICE"; then
    ok "spira-gh-intake.service sets SPIRA_DB (binary exits without it)"
else
    bad "spira-gh-intake.service sets SPIRA_DB (binary exits without it)" "Environment=SPIRA_DB=@SPIRA_DB@ missing from $SERVICE"
fi

tl_summary
