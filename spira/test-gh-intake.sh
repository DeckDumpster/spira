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
# covers: gh-intake/src/*.rs systemd/spira-gh-intake.service systemd/spira-gh-intake.timer UC-dispatch-06
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-gh-intake.sh"


T="$(mktemp -d)"; trap 'rm -rf "$T"; [ -n "${STUB_PID:-}" ] && kill "$STUB_PID" 2>/dev/null || true' EXIT INT TERM

BIN="$(command -v gh-intake 2>/dev/null || true)"
[ -n "$BIN" ] || bail "gh-intake is not on PATH (the tree's build provides it)"

# ---- stub HTTP server: serves canned JSON by exact path, logs every request ---------------
STUB_DIR="$T/stub"; mkdir -p "$STUB_DIR"
REQLOG="$T/requests.log"; : > "$REQLOG"
STUB_PID=""
PORTFILE="$T/stub.port"
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

srv = http.server.HTTPServer(("127.0.0.1", 0), H)
tmp = sys.argv[1] + ".tmp"
with open(tmp, "w") as pf:
    pf.write(str(srv.server_address[1]))
os.rename(tmp, sys.argv[1])
srv.serve_forever()
' "$PORTFILE" &
    STUB_PID=$!
    local _deadline=$((SECONDS + 60))
    until [ -s "$PORTFILE" ]; do
        kill -0 "$STUB_PID" 2>/dev/null || { echo "test-gh-intake: stub HTTP server exited before binding" >&2; return 1; }
        [ "$SECONDS" -lt "$_deadline" ] || { echo "test-gh-intake: stub HTTP server not ready within 60s" >&2; return 1; }
        sleep 0.1
    done
    STUB_PORT="$(cat "$PORTFILE")"
}
start_stub || bail "stub HTTP server failed to start"
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
# SPIRA_HOME IS THE HOME now (locate_home no longer searches): every binary reads
# <home>/conf.d, so this stub home needs the registry (sfail round 2, pattern 1).
ln -s "$HERE/conf.d" "$T/spira-home/conf.d"
REPOCHECKOUT="$T/repo-checkout"; mkdir -p "$REPOCHECKOUT/.git"
REPOMAP="$T/repo-map"
printf 'widgets | %s\n' "$REPOCHECKOUT" > "$REPOMAP"

run_intake() {
    tl_config SPIRA_DB=fixture SPIRA_BD="$T/bin_bd" SPIRA_REPO_MAP="$REPOMAP" \
        SPIRA_GH_INTAKE_REPO="acme/widgets" SPIRA_GH_INTAKE_BEAD_REPO="widgets" \
        SPIRA_GH_INTAKE_PRIORITY="2" SPIRA_SCOPE_LABEL="spira" SPIRA_PLAN_LABEL="plan"
    env -i PATH="$T:/usr/bin:/bin" HOME="$HOME" \
        SPIRA_TOML="$SPIRA_TOML" \
        BDLOG="$BDLOG" CREATED="$CREATED" MAILLOG="$MAILLOG" \
        SPIRA_MAIL_BIN="$T/bin_mail.sh" \
        SPIRA_HOME="$T/spira-home" \
        SPIRA_GH_INTAKE_API="$API" \
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
tl_config SPIRA_DB=fixture SPIRA_BD="$T/bin_bd" SPIRA_REPO_MAP="$REPOMAP" \
    SPIRA_GH_INTAKE_REPO="acme/widgets" SPIRA_GH_INTAKE_BEAD_REPO="no-such-repo"
out="$(env -i PATH="$T:/usr/bin:/bin" HOME="$HOME" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_MAIL_BIN="$T/bin_mail.sh" \
    SPIRA_HOME="$T/spira-home" \
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
