#!/usr/bin/env bash
# testlib.sh — the one assertion library every spira/test-*.sh suite sources.
#
# Sourced, never executed:
#   HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   . "$HERE/testlib.sh"
#
# WHY THIS EXISTS. 459 of 466 suites define their own ok()/want()/nowant(), so a red
# prints one of three-plus wordings, 25 suites print no summary at all, and some skips
# exit 0 and are counted as green passes. Nothing downstream — gate-diag.sh, suites.sh,
# forge.sh's attribution — can read a suite's result without re-parsing prose it was never
# given a contract for. This is that contract: fixed function names, TAP 14 on stdout,
# and one JSONL row per case when a batch runner asks for it.
#
# FUNCTIONS, NAMED EXACTLY AS THE DE FACTO HELPERS THEY REPLACE (mechanical migration is
# the point — sp-qvjzb renames call sites, not call shapes):
#   ok    <name>                     record a pass
#   bad   <name> [detail]            record a failure
#   is      <name> <expected> <actual>   pass iff the two strings are equal
#   want    <name> <needle> <haystack>   pass iff haystack contains needle
#   nowant  <name> <needle> <haystack>   pass iff haystack does not contain needle
#   wantrc  <name> <expected-rc> <actual-rc>  pass iff the two codes are equal
#   plan  <n>                        declare the case count up front (optional — a suite
#                                     that never calls it gets a trailing plan instead)
#   report_cargo <out-file> <rc>     call ok/bad once per Rust test named in a `cargo
#                                     test` run's captured output — the mechanism every
#                                     Rust-crate suite in this tree calls instead of each
#                                     keeping its own copy (sp-crrwo)
#   skip  <reason>                   the WHOLE SUITE cannot run here: TAP skip-all,
#                                     exit 77 (the automake-skip code testenv-batch.sh and
#                                     suites.sh already special-case). A skip is never a
#                                     pass: it is its own status in TAP, in the JSONL, and
#                                     in every counter this library keeps.
#   bail  <reason>                   something is broken, not merely inapplicable: TAP
#                                     "Bail out!", exit 2, immediately. Never retried as
#                                     though it were a normal red.
#   tl_summary                       print the totals and end the suite; its exit status
#                                     is the suite's exit status, so it MUST be the last
#                                     statement in the file (not called from a subshell,
#                                     not followed by anything that resets $?).
#   copy_conf_registry <dest-dir>    copy spira/conf.d/ and conf-gen.sh beside a fixture's
#                                     own copy of conf.sh (sp-g3uwp) — every fixture that
#                                     `cp`'s $HERE/conf.sh somewhere needs this too, or
#                                     conf.sh's self-heal finds no registry to regenerate
#                                     from and refuses to source at all.
#   tl_subshell_safe                 call once, before the first assertion, when the
#                                     suite's ok/bad calls run inside `( )` subshells —
#                                     otherwise their writes to the counters never reach
#                                     the parent shell (sp-yt3re).
#
# CASE IDS. The <name> argument IS the case id — the same string appears as the TAP
# description and as the JSONL "case" field. Suites already give each assertion a
# stable, unique, human-readable name; a second identifier alongside it would be a second
# thing to keep in sync with nothing enforcing that they agree.
#
# HEADER CONVENTIONS, read by the runner (suite-select, via `suite-select header ...`),
# not by this file at suite run time:
#   # tier: T0..T4          how expensive a suite is, declared by its author
#   # covers: <space-separated tokens>   a path glob (existing convention) or a UC id
#                           of the form UC-<area>-NN (the catalogue sp-qu948 defines);
#                           the two kinds of token share one line and are told apart by
#                           the "UC-" prefix, so a suite that covers both code and a use
#                           case declares them together rather than in two directives that
#                           could drift apart.
#
# JSONL. Silent unless SPIRA_TESTLIB_JSONL names a writable file — a suite run by hand
# has no batch collecting rows and should not block on a path nobody gave it. Set by the
# batch runner, one shared file per batch: every case from every suite in that batch
# appends here, so the file is opened for append and never truncated by this library.
#
# NO PIPE INTO A MATCHER THAT EXITS EARLY IN want/nowant. Bash's [[ ]] is used directly
# rather than grep, so law-no-grep-q-under-pipefail does not apply here, but the same
# instinct holds: a helper that can fail for a reason OTHER than the assertion itself
# (a missing binary, a broken pipe) must not be indistinguishable from the assertion
# failing. [[ ]] has no such failure mode.
# Every suite's binaries find lib.sh through SPIRA_HOME first. Without it, slay/sending/
# spira-world walk up from their own resolved exe — which the gate builds outside the tree
# (a tmpfs target), so the walk finds nothing and every destroy/slay answers 2. A suite that
# sets its own SPIRA_HOME keeps it; production units always set it (sp-wiqcq is the binaries'
# own fix).
[ -n "${SPIRA_HOME:-}" ] || export SPIRA_HOME="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

set -u

# A suite never inherits a git location: an inherited GIT_DIR makes `git init --bare` or
# `git config` in a fixture write into the caller's repository instead.
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_PREFIX

_TL_NUM=0
_TL_PASS=0
_TL_FAIL=0
_TL_SKIP=0
_TL_PLANNED=""
_TL_INITED=0
_TL_SUBSHELL_SAFE=0
_TL_COUNTS=""
_TL_LAST_S=$SECONDS
_TL_LAST_US="${EPOCHREALTIME/./}"
_TL_SUITE="$(basename "${BASH_SOURCE[1]:-${0:-suite}}")"
_TL_JSONL="${SPIRA_TESTLIB_JSONL:-}"

# NO SUMMON JITTER IN SUITES (sp-1cdgq). aeon sleeps a random 0..SPIRA_SUMMON_JITTER seconds
# (default 20) before its session starts, so a batch summoned together does not start in
# lockstep. A suite driving the real binary paid that per launch, and one whose deadline was
# shorter than the jitter flipped about half the time (test-thrash-teardown). A suite that tests
# the jitter itself sets its own value after sourcing this.
export SPIRA_SUMMON_JITTER="${SPIRA_SUMMON_JITTER:-0}"
# THE SUITE'S PATH IS THE LAUNCHER'S (sp-isom7). testenv stages the tree under test as a
# release and sets every suite's PATH outright from it ($SPIRA_RELEASE/bin, then
# $SPIRA_RELEASE/spira), so a suite resolves tools by bare name exactly as production does.
# This library never touches PATH — which is also why `suite-select` below (wave 4.36,
# sp-bobsp: suite-covers.sh retired onto `suite-select header ...`) resolves by bare name
# rather than a path built from this file's own location.
# THE STAGED RELEASE CARRIES model-bin/ (sp-jq4wq). An enforced aeon refuses to start the
# model in a release with no executable model-bin/work (sp-zf4q3). testenv's stage script is
# compiled into the INSTALLED testenv, and one older than sp-zf4q3 stages no model-bin/, so
# every aeon session in a suite was refused. The tree's own spira-config makes it with the
# release builder's helper, idempotent and safe for a batch's suites to race; only a testenv
# staged release (/tmp/spira-release-*) is ever touched, and only when model-bin/ is absent.
case "${SPIRA_RELEASE:-}" in
    /tmp/spira-release-*)
        [ -e "$SPIRA_RELEASE/model-bin" ] || [ ! -x "$SPIRA_RELEASE/bin/spira-config" ] \
            || "$SPIRA_RELEASE/bin/spira-config" link-model-bin "$SPIRA_RELEASE" >/dev/null || true
        ;;
esac
_TL_TIER="$(suite-select header tier "${BASH_SOURCE[1]:-$0}")"
_TL_UC="$(suite-select header uc "${BASH_SOURCE[1]:-$0}")"

# THE ONE SOURCE OF CONFIG (per Ryan 2026-10-05): every process reads the spira.toml SPIRA_TOML
# names and nothing else — no environment override, no default. Each suite gets its own copy
# of the complete fixture (every key declared); a suite sets what it is about with tl_config,
# never `export SPIRA_X=`, which no process reads any more.
# Layers: the checked-in complete fixture (every key declared) as the base, then this suite's
# own override file, which holds ONLY what the suite changes.
_TL_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# Every repo a suite creates starts on `main`, whatever the host's own init.defaultBranch says:
# a declared base (origin/main, local/main) is never derived any more, so a fixture repo must
# actually HAVE the branch its repo-map row names. Git's own env config, so it reaches every
# git a suite runs (an `env -i` drops it — such a call names its branch itself).
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=init.defaultBranch GIT_CONFIG_VALUE_0=main
_TL_CONF_BASE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)/spira-config/tests/fixtures/complete.toml"
[ -f "$_TL_CONF_BASE" ] || { echo "testlib: no complete fixture at $_TL_CONF_BASE" >&2; exit 1; }
_TL_CONF_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tl-conf.XXXXXX")"
_TL_CONF_OVERRIDE="$_TL_CONF_DIR/$(basename "${BASH_SOURCE[1]:-$0}" .sh).override.toml"
printf '[spira]\n' > "$_TL_CONF_OVERRIDE"
export SPIRA_TOML="$_TL_CONF_BASE:$_TL_CONF_OVERRIDE"

# tl_config KEY=value ... — declare config in this suite's override file (SPIRA_FOO -> spira.foo,
# COCKPIT_FOO -> spira.cockpit_foo). Refuses on a key the schema does not know.
# A value is given in its shell form and written in the key's registered TYPE: a list as
# space- or comma-separated words, a bool as 1/0/true/false/yes/no/on/off.
tl_config() { _tl_declare "$_TL_CONF_OVERRIDE" "$@"; }

# tl_layer KEY=value ... — config for ONE call: prints a SPIRA_TOML value (this suite's layers
# plus a fresh layer holding just these keys), so nothing persists into the rest of the suite:
#   SPIRA_TOML="$(tl_layer SPIRA_DB=/nonexistent)" some-binary ...
tl_layer() {
    local f
    f="$(mktemp "$_TL_CONF_DIR/layer.XXXXXX")" || return 1
    printf '[spira]\n' > "$f"
    _tl_declare "$f" "$@" || return 1
    printf '%s' "$SPIRA_TOML:$f"
}

_tl_declare() {
    local file="$1" kv k v d t w out; shift
    for kv in "$@"; do
        k="${kv%%=*}"; v="${kv#*=}"
        d="spira.$(printf '%s' "${k#SPIRA_}" | tr '[:upper:]' '[:lower:]')"
        t="$(sed -n 's/^TYPE=//p' "$_TL_HERE/conf.d/$k" 2>/dev/null | head -1)"
        case "$t" in
            list)
                out=""
                for w in ${v//,/ }; do out="${out:+$out,}\"$w\""; done
                v="[$out]" ;;
            bool)
                case "$v" in
                    1|true|yes|on) v=true ;;
                    0|false|no|off|"") v=false ;;
                    *) echo "tl_config: $k is a bool, not '$v'" >&2; return 1 ;;
                esac ;;
        esac
        spira-config set "$d" "$v" "$file" >/dev/null \
            || { echo "tl_config: cannot declare $k" >&2; return 1; }
    done
}

_tl_init() {
    [ "$_TL_INITED" = 1 ] && return 0
    _TL_INITED=1
    printf 'TAP version 14\n'
}

# tl_subshell_safe — call once, before the first assertion, when a suite runs ok/bad
# (directly or via is/want/nowant/wantrc) inside `( )` subshells. A subshell's writes to
# _TL_NUM/_TL_PASS/_TL_FAIL/_TL_SKIP never reach the parent, so without this the parent's
# next assertion silently reuses stale counts and TAP case numbers. Backed by a plain file
# rather than a lock: every suite that needs this runs its subshells one after another,
# never concurrently, so a read-increment-write is never racing another writer.
tl_subshell_safe() {
    _TL_SUBSHELL_SAFE=1
    _TL_COUNTS="$(mktemp)"
    _tl_counts_flush
}

# The three functions below are called unconditionally from ok/bad/skip/bail/tl_summary
# and no-op (returning 0) when tl_subshell_safe was never called — the guard lives HERE,
# not at each call site, because a guard on the call site that lands as a function's last
# statement makes the whole function return false whenever the guard doesn't fire, which
# corrupts every is/want/nowant caller's `cond && ok || bad` (both branches then run).

_tl_counts_flush() {   # write _TL_NUM/_TL_PASS/_TL_FAIL/_TL_SKIP to the backing file
    [ "$_TL_SUBSHELL_SAFE" = 1 ] || return 0
    printf '%s %s %s %s\n' "$_TL_NUM" "$_TL_PASS" "$_TL_FAIL" "$_TL_SKIP" > "$_TL_COUNTS"
}

_tl_counts_load() {    # read them back — the first thing any counting function does
    [ "$_TL_SUBSHELL_SAFE" = 1 ] || return 0
    read -r _TL_NUM _TL_PASS _TL_FAIL _TL_SKIP < "$_TL_COUNTS"
}

_tl_counts_cleanup() {
    [ "$_TL_SUBSHELL_SAFE" = 1 ] || return 0
    rm -f "$_TL_COUNTS"
}

# _tl_json_escape <string> -> the string with \, " and control chars made JSON-safe.
# No external process: this runs once per assertion and a suite can have hundreds.
_tl_json_escape() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\t'/\\t}"
    printf '%s' "$s"
}

# _tl_case_ms -> milliseconds since the previous case (or source time), printed as the
# `#ms=<n>` line testenv reads into the case-timing family.
_tl_case_ms() {
    local now="${EPOCHREALTIME/./}"
    printf '#ms=%s\n' "$(( (now - _TL_LAST_US) / 1000 ))"
    _TL_LAST_US=$now
}

# _tl_jsonl <case> <status> [detail] -> append one row, or do nothing when no sink is set.
_tl_jsonl() {
    [ -n "$_TL_JSONL" ] || return 0
    local case_id="$1" status="$2" detail="${3:-}" secs=$(( SECONDS - _TL_LAST_S ))
    _TL_LAST_S=$SECONDS
    local uc_json="[]"
    if [ -n "$_TL_UC" ]; then
        uc_json="$(printf '%s' "$_TL_UC" | awk '{
            printf "["
            for (i=1;i<=NF;i++) { printf "%s\"%s\"", (i>1?",":""), $i }
            printf "]"
        }')"
    fi
    printf '{"suite":"%s","tier":"%s","case":"%s","status":"%s","seconds":%s,"uc":%s,"detail":"%s"}\n' \
        "$(_tl_json_escape "$_TL_SUITE")" "$(_tl_json_escape "$_TL_TIER")" \
        "$(_tl_json_escape "$case_id")" "$status" "$secs" "$uc_json" \
        "$(_tl_json_escape "$detail")" >> "$_TL_JSONL"
}

ok() {
    _tl_init
    _tl_counts_load
    _TL_NUM=$((_TL_NUM + 1)); _TL_PASS=$((_TL_PASS + 1))
    printf 'ok %s - %s\n' "$_TL_NUM" "$1"
    _tl_case_ms
    _tl_jsonl "$1" pass
    _tl_counts_flush
}

bad() {
    _tl_init
    _tl_counts_load
    _TL_NUM=$((_TL_NUM + 1)); _TL_FAIL=$((_TL_FAIL + 1))
    printf 'not ok %s - %s\n' "$_TL_NUM" "$1"
    [ -n "${2:-}" ] && printf '# %s\n' "$2"
    _tl_case_ms
    _tl_jsonl "$1" fail "${2:-}"
    _tl_counts_flush
}

is() {      # is <name> <expected> <actual>
    [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"
}

want() {    # want <name> <needle> <haystack>
    [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"
}

nowant() {  # nowant <name> <needle> <haystack>
    [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"
}

wantrc() {  # wantrc <name> <expected-rc> <actual-rc>
    [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted rc=$2 got rc=$3"
}

# report_cargo <out-file> <rc> — one ok/bad per `test <path> ... ok|FAILED` line a `cargo
# test` run wrote to <out-file>, instead of the whole run collapsing to a single line. Cargo
# interleaves output from parallel test threads, but each test's own result line is whole
# and unique, so line-based parsing needs no --test-threads=1.
#
# <rc> is cargo's own exit code, and decides the one case a per-test line can't: a run that
# produced no "test ... ok|FAILED" line at all is either a compile error (rc != 0 — the
# crate's one thing to report as failed) or a filter that matched zero tests (rc = 0 —
# nothing to report a case for, same as cargo itself saying nothing failed).
# copy_conf_registry <dest-dir> — copy $HERE/conf.d/ and $HERE/conf-gen.sh into
# <dest-dir> (created if needed). Every suite that `cp`'s $HERE/conf.sh into a fixture
# needs this too (sp-g3uwp): conf.sh's own self-heal (_spira_conf_gen_ensure) resolves
# the REAL file behind a *symlinked* conf.sh (readlink -f), so a fixture built with
# `ln -sf` (install_fixture_build and friends in lib-test-install.sh) needs nothing
# extra — but one that `cp`'s conf.sh loses that real path, and without its own
# conf.d/ beside the copy, conf-gen.sh has nothing to read and conf.sh refuses to
# source at all: "conf-gen.sh failed to regenerate ... refusing to run with a stale
# or missing generated file."
copy_conf_registry() {
    local dest="${1:?copy_conf_registry needs a destination directory}"
    mkdir -p "$dest"
    cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$dest/"
    # Generate now, so the fixture's first `source conf.sh` has nothing to regenerate and
    # prints nothing into a suite capturing a command's output with 2>&1 (sp-gt0ta).
    bash "$dest/conf-gen.sh" >/dev/null 2>&1 \
        || { echo "copy_conf_registry: conf-gen.sh failed in $dest" >&2; return 1; }
}

report_cargo() {
    local out="$1" rc="$2" line name detail seen=0
    while IFS= read -r line; do
        case "$line" in
            "test "*" ... ok")
                name="${line#test }"; name="${name% ... ok}"
                ok "$name"; seen=1
                ;;
            "test "*" ... FAILED")
                name="${line#test }"; name="${name% ... FAILED}"
                detail="$(awk -v t="---- $name stdout ----" '
                    $0 == t { grab=1; next }
                    grab && /^----/ { exit }
                    grab && /^failures:/ { exit }
                    grab { print }
                ' "$out" | head -5 | tr '\n' ' ')"
                bad "$name" "${detail:-see cargo output above}"
                seen=1
                ;;
        esac
    done < "$out"
    if [ "$seen" = 0 ] && [ "$rc" != 0 ]; then
        bad "cargo test" "$(tail -40 "$out" | tr '\n' ' ')"
    fi
}

# A stand-in spira-lc answering from files under $LC_FIX, for suites that drive a binary
# reading lifecycle rows. lc_fix_init <dir> builds it and sets LC_FIX and SPIRA_LC_BIN; run
# the binary under test with both passed through. Rows: lc_bead <STATE> <id> <tip> <since>,
# lc_delivery <STATE> <id> <mode> <entered_at> <version>. Calls to `event` are logged to
# $LC_FIX/events.log; `touch $LC_FIX/refuse` makes them exit 3.
lc_fix_init() {
    LC_FIX="${1:?lc_fix_init needs a directory}"
    rm -rf "$LC_FIX"; mkdir -p "$LC_FIX/bead" "$LC_FIX/delivery" "$LC_FIX/show"
    : > "$LC_FIX/events.log"
    cat > "$LC_FIX/spira-lc" <<'STUB'
#!/usr/bin/env bash
join() { local first=1 f; printf '['; for f in "$@"; do [ -f "$f" ] || continue; [ $first = 1 ] || printf ','; first=0; cat "$f"; done; printf ']\n'; }
case "$1" in
    list)
        if [ "$2" = "--delivery" ]; then join "$LC_FIX/delivery/${4:-}"/*; elif [ -n "${3:-}" ]; then join "$LC_FIX/bead/$3"/*; else join "$LC_FIX/bead"/*/*; fi ;;
    show) [ -f "$LC_FIX/show/$2" ] && cat "$LC_FIX/show/$2" || exit 1 ;;
    event) printf '%s\n' "$*" >> "$LC_FIX/events.log"; [ -f "$LC_FIX/refuse" ] && exit 3; exit 0 ;;
    *) exit 7 ;;
esac
STUB
    chmod +x "$LC_FIX/spira-lc"
    SPIRA_LC_BIN="$LC_FIX/spira-lc"
}
# A spira-lc whose `close` is the real verb's path for a bead with NO lifecycle row: the store
# alone is closed (sp-3fue0j — every Rust closer goes through `spira-lc close`, never bd). For
# suites that stub bd and have no lifecycle store. lc_close_stub <dir> [bd] [db] writes
# <dir>/spira-lc, which runs `<bd> [-C <db>] close <id> --force --reason <text>` (the reason from
# --reason or --reason-file), and exports SPIRA_LC_BIN at it. Without <bd>/<db> they are read at
# each call as the real verb reads them — spira.bd / spira.db from $SPIRA_TOML — so a suite
# that re-declares SPIRA_BD between runs closes through the bd it declared last. Every other
# verb exits 7, so a suite that needs rows uses lc_fix_init instead.
lc_close_stub() {
    local dir="${1:?lc_close_stub needs a directory}" bd="${2:-}" db="${3:-}"
    mkdir -p "$dir"
    cat > "$dir/spira-lc" <<STUB
#!/usr/bin/env bash
[ "\$1" = close ] || exit 7
id="\$2"; shift 2; reason=""
while [ \$# -gt 0 ]; do
    case "\$1" in
        --reason) reason="\$2"; shift 2 ;;
        --reason-file) if [ "\$2" = - ]; then reason="\$(cat)"; else reason="\$(cat "\$2")"; fi; shift 2 ;;
        --actor|--superseded-by) shift 2 ;;
        *) shift ;;
    esac
done
bd="$bd"; db="$db"
[ -n "\$bd" ] || bd="\$(spira-config get spira.bd 2>/dev/null)"; [ -n "\$bd" ] || bd=bd
[ -n "\$db" ] || db="\$(spira-config get spira.db 2>/dev/null)"
exec "\$bd" \${db:+-C "\$db"} close "\$id" --force --reason "\$reason"
STUB
    chmod +x "$dir/spira-lc"
    SPIRA_LC_BIN="$dir/spira-lc"; export SPIRA_LC_BIN
}
# A spira-lc for landing-pass suites, installed by name into <bindir> (already first on the
# suite's PATH) so the pass's lifecycle probe answers. `list` reads rows like lc_fix_init's;
# every verb is logged to <fixdir>/calls.log, and `touch <fixdir>/refuse` makes writes exit 3.
lc_path_stub() {   # lc_path_stub <bindir> <fixdir>
    local bindir="${1:?lc_path_stub needs a bin dir}" fix="${2:?lc_path_stub needs a fixture dir}"
    LC_FIX="$fix"; mkdir -p "$bindir" "$fix/bead" "$fix/delivery" "$fix/show"; : > "$fix/calls.log"
    # The two verbs that never touch the lifecycle machine's rows through this stub's
    # fixture — close-on-land (bd close + reap, sp-2c1n0) and content-landed (git only) —
    # go to the real spira-lc, the first one on PATH that is not this stub.
    local real="" c
    while IFS= read -r c; do [ "$c" -ef "$bindir/spira-lc" ] || { real="$c"; break; }; done < <(type -ap spira-lc 2>/dev/null)
    cat > "$bindir/spira-lc" <<STUB
#!/usr/bin/env bash
LC_FIX="$fix"
printf '%s\\n' "\$*" >> "\$LC_FIX/calls.log"
case "\$1" in close-on-land|content-landed) [ -n "$real" ] && exec "$real" "\$@" ;; esac
join() { local first=1 f; printf '['; for f in "\$@"; do [ -f "\$f" ] || continue; [ \$first = 1 ] || printf ','; first=0; cat "\$f"; done; printf ']\\n'; }
case "\$1" in
    list)
        if [ "\$2" = "--delivery" ]; then join "\$LC_FIX/delivery/\${4:-}"/*; elif [ -n "\${3:-}" ]; then join "\$LC_FIX/bead/\$3"/*; else join "\$LC_FIX/bead"/*/*; fi ;;
    show) [ -f "\$LC_FIX/show/\$2" ] && cat "\$LC_FIX/show/\$2" || exit 1 ;;
    *) [ -f "\$LC_FIX/refuse" ] && exit 3; exit 0 ;;
esac
STUB
    chmod +x "$bindir/spira-lc"
}
# lc_called <fixdir> <verb> <bead> — did the pass send <verb> for <bead>?
lc_called() { grep -q "^$2 $3\b" "$1/calls.log" 2>/dev/null; }
lc_bead() {      # lc_bead <STATE> <id> <tip> <since>  (one row per id: a new STATE replaces the old)
    rm -f "$LC_FIX"/bead/*/"$2"
    mkdir -p "$LC_FIX/bead/$1" "$LC_FIX/show"
    printf '{"bead_id":"%s","state":"%s","tip":"%s","since":%s}' "$2" "$1" "$3" "$4" > "$LC_FIX/bead/$1/$2"
    printf '{"bead":{"bead_id":"%s","state":"%s","tip":"%s"}}' "$2" "$1" "$3" > "$LC_FIX/show/$2"
}
# lc_state_of <id> — the STATE an lc_bead row was last given ("" for none): the fixture's
# record of the state, for asserting what a suite seeded (sp-mve9i: state lives in spira-lc).
lc_state_of() { local f; for f in "$LC_FIX"/bead/*/"$1"; do [ -f "$f" ] && basename "$(dirname "$f")"; done | tail -1; }
lc_delivery() {  # lc_delivery <STATE> <id> <mode> <entered_at> <version>
    mkdir -p "$LC_FIX/delivery/$1"
    printf '{"bead_id":"%s","mode":"%s","state":"%s","version":%s,"entered_at":%s}' "$2" "$3" "$1" "$5" "$4" > "$LC_FIX/delivery/$1/$2"
    [ -f "$LC_FIX/show/$2" ] || printf '{"bead":{"tip":"deadbeef"}}' > "$LC_FIX/show/$2"
}

# lc_mirror_bd <dir> — a spira-lc whose `list` and `show <id>` answer from the bead store the
# suite already stubs, translated to lifecycle terms (sp-mve9i: decisions read the lifecycle
# row, never bd status, so a fixture written in bd words needs its rows in the machine too).
# The store is $SPIRA_BDJSON_FIXTURE when set, else `$SPIRA_BD -C $SPIRA_DB list --all`.
# open/blocked/deferred → READY, in_progress → WORKING (holder = assignee), closed →
# $LC_MIRROR_CLOSED (default LANDED); a `spira-poison` label → a poison hold, the ask label
# ($SPIRA_ASK_LABEL) → an ask hold; epics and events have no row. A fixture row may say
# `"_lc_state"` / `"_lc_holds"` to set its row outright, or `"_lc_rowless": true` for none;
# for a real store (which drops unknown fields), `<dir>/states` lines `<id> <STATE>` do it.
# Sets SPIRA_LC_BIN; pass it (and LC_MIRROR_CLOSED, if set) through any `env -i`.
lc_mirror_bd() {
    local dir="${1:?lc_mirror_bd needs a directory}"
    mkdir -p "$dir"
    cat > "$dir/spira-lc" <<'STUB'
#!/usr/bin/env bash
# `close` (sp-3fue0j): this mirror's rows ARE the store's, so the close is the store's.
if [ "$1" = close ]; then
    id="$2"; shift 2; reason=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --reason) reason="$2"; shift 2 ;;
            --reason-file) if [ "$2" = - ]; then reason="$(cat)"; else reason="$(cat "$2")"; fi; shift 2 ;;
            --actor|--superseded-by) shift 2 ;;
            *) shift ;;
        esac
    done
    exec "${SPIRA_BD:-bd}" -C "${SPIRA_DB:-.}" close "$id" --force --reason "$reason"
fi
if [ -n "${SPIRA_BDJSON_FIXTURE:-}" ]; then src="$(cat "$SPIRA_BDJSON_FIXTURE")"
else src="$("${SPIRA_BD:-bd}" -C "${SPIRA_DB:-.}" list --all --limit 0 --json 2>/dev/null)" || exit 2; fi
printf '%s' "$src" | LC_MIRROR_DIR="$(dirname "$0")" python3 -c '
import json, os, sys
verb = sys.argv[1] if len(sys.argv) > 1 else ""
try:
    beads = json.loads(sys.stdin.read() or "[]")
except ValueError:
    sys.exit(2)
if isinstance(beads, dict):
    beads = [beads]
words = {"open": "READY", "blocked": "READY", "deferred": "READY", "in_progress": "WORKING",
         "closed": os.environ.get("LC_MIRROR_CLOSED") or "LANDED"}
ask = os.environ.get("SPIRA_ASK_LABEL", "")
pinned = {}
try:
    for line in open(os.path.join(os.environ["LC_MIRROR_DIR"], "states")):
        f = line.split()
        if len(f) == 2:
            pinned[f[0]] = f[1]
except (OSError, KeyError):
    pass
rows = []
for b in beads:
    if not isinstance(b, dict) or not b.get("id") or b.get("_lc_rowless"):
        continue
    if b.get("issue_type") in ("epic", "event") and "_lc_state" not in b:
        continue
    labels = b.get("labels") or []
    state = pinned.get(b["id"]) or b.get("_lc_state") or words.get(b.get("status") or "open", "READY")
    holds = b.get("_lc_holds")
    if holds is None:
        holds = (["poison"] if "spira-poison" in labels else []) + (["ask"] if ask and ask in labels else [])
    rows.append({"bead_id": b["id"], "state": state, "holds": holds,
                 "holder": (b.get("assignee") or None) if state == "WORKING" else None})
if verb == "list":
    want = sys.argv[3] if len(sys.argv) > 3 and sys.argv[2] == "--state" else None
    print(json.dumps([r for r in rows if want is None or r["state"] == want]))
elif verb == "show":
    hit = [r for r in rows if r["bead_id"] == (sys.argv[2] if len(sys.argv) > 2 else "")]
    if not hit:
        sys.exit(1)
    print(json.dumps({"bead": hit[0], "delivery": None}))
else:
    sys.exit(7)
' "$@"
STUB
    chmod +x "$dir/spira-lc"
    SPIRA_LC_BIN="$dir/spira-lc"
}

plan() {    # plan <n> — must be called before the first ok/bad/want/nowant/wantrc
    _tl_init
    _TL_PLANNED="$1"
    printf '1..%s\n' "$_TL_PLANNED"
}

# skip <reason> — the whole suite cannot run here. TAP's skip-all form, and exit 77:
# the automake-skip code the batch runner and the timed runner both already treat as
# distinct from red. Never call this after any case has already run — a partial suite
# that then claims "skipped" would hide the cases that did run.
skip() {
    _tl_init
    _tl_counts_load
    if [ "$_TL_NUM" -gt 0 ]; then
        # A partial suite claiming "skipped" would hide the cases that already ran —
        # that is a suite-authoring bug, not a real skip, so it fails loudly rather
        # than being folded into either outcome.
        bail "skip called after $_TL_NUM case(s) already ran: $1"
    fi
    _TL_SKIP=$((_TL_SKIP + 1))
    printf '1..0 # SKIP %s\n' "$1"
    _tl_jsonl "(suite)" skip "$1"
    _tl_counts_cleanup
    exit 77
}

# bail <reason> — something is broken, not merely inapplicable. Stops the run immediately;
# a bailed suite must never be retried as though its failure were a normal assertion red.
bail() {
    _tl_init
    printf 'Bail out! %s\n' "$1"
    _tl_jsonl "(suite)" bail "$1"
    _tl_counts_cleanup
    exit 2
}

# tl_summary — must be the last statement in the suite; its exit status becomes the
# suite's exit status. Prints the trailing plan when `plan` was never called (TAP permits
# the plan at either end), then the human-readable totals every reader of this tree
# already expects, then returns pass/fail as $fail -eq 0 always has.
tl_summary() {
    _tl_init
    _tl_counts_load
    [ -n "$_TL_PLANNED" ] || printf '1..%s\n' "$_TL_NUM"
    printf '\n%d passed, %d failed, %d skipped\n' "$_TL_PASS" "$_TL_FAIL" "$_TL_SKIP"
    # suites.sh's setup-fault detector greps this exact line (`ASSERTIONS 0`) to tell a
    # suite that failed before its first case from one whose cases actually ran and lost.
    printf 'ASSERTIONS %d\n' "$((_TL_PASS + _TL_FAIL))"
    _tl_counts_cleanup
    [ "$_TL_FAIL" -eq 0 ]
}

# aeon_fixture_agent <shim> — point SPIRA_AGENT at <shim> through a wrapper that gives the
# fixture back the environment it is written against. Since sp-v62vn every model session
# runs restricted (aeon/src/restrict.rs: HOME, SPIRA_RUN and a PATH of /usr/bin:/bin plus the
# release's model-bin — no TMP, no SPIRA_DB, no bd), so a legacy shim that records its
# prompt under $TMP and tells its story with `bd close` silently did nothing. The shim is a
# FIXTURE standing in for the model, not the model: it may reach the suite's bd store,
# because the lifecycle stand-in (lc_aeon_mirror) reads that close as the session's
# `work submit`. TMP, SPIRA_DB, SPIRA_BD and PATH are captured when this is called — call it
# once the suite's PATH and database are set. The restricted environment itself is
# asserted where it is the subject (test-aeon-lifecycle-cutover.sh, aeon's restrict tests).
# Any further variable names after the shim are captured the same way, for a shim that
# reads more of the suite's world (a wiki path, a groom log).
aeon_fixture_agent() {
    local shim="${1:?aeon_fixture_agent needs the shim path}" outer="${1}.fixture-env" k
    shift
    {
        printf '#!/usr/bin/env bash\n'
        for k in TMP SPIRA_DB SPIRA_BD PATH "$@"; do
            [ -n "${!k:-}" ] && printf 'export %s=%q\n' "$k" "${!k}"
        done
        printf 'exec %q "$@"\n' "$shim"
    } > "$outer"
    chmod +x "$outer"
    export SPIRA_AGENT="$outer"   # bash-level readers in the suite
    tl_config SPIRA_AGENT="$outer"   # every binary: the one source
}

# lc_row_state <id> — the STATE the lifecycle machine on PATH (lc_aeon_mirror, for the aeon
# suites) answers for <id>, "" when it has no row. Since sp-v62vn every model session is
# restricted, so the teardown's old conversion of a builder's bd close into open +
# spira-submitted (sp-qsona) never runs: "the session finished its bead" is this row reading
# SUBMITTED, never a bd label.
lc_row_state() {
    spira-lc show "$1" 2>/dev/null | python3 -c '
import sys, json
try: print(json.load(sys.stdin)["bead"]["state"])
except Exception: print("")' 2>/dev/null
}

# lc_aeon_mirror <dir> — a spira-lc, installed by name into <dir> (put <dir> first on the
# aeon's PATH), for the legacy aeon suites whose model shim closes its bead in bd. The aeon
# reads a bead's state from its lifecycle row, never bd status (sp-mve9i), and since sp-v62vn
# (no off mode) it also CLAIMS through the machine: its ready set is `spira-lc list` (through
# spira-claim fayth-ready --json) and its claim is a Claim event. So the fixture's bd story is
# told to it in lifecycle terms, a row per non-epic bd bead:
#   $SPIRA_RUN/lc-row/<id> ("<STATE> [reason] [tip]"), when the suite wrote one;
#   else bd closed (the builder's close is its submit) → SUBMITTED;
#   else a claim this stand-in applied ($SPIRA_RUN/lc-claim/<id>, the holder) → WORKING;
#   else bd in_progress → WORKING (holder = assignee); anything else → READY.
# A `spira-poison` label is a poison hold, the ask label ($SPIRA_ASK_LABEL) an ask hold.
# Verbs: `show <id>` / `state <id>` (no bd row → exit 1), `list [--state S]`, `create-bead` (0),
# `unclaim <id> <actor>` (spira-lc's own rule: a WORKING row is released only by its holder,
# else exit 1; any other state is already released, 0), and
# `event bead <id> ... --actor A --kind K`: Claim applies only to a READY/REWORK row (else
# exit 3, refused — a bead another aeon holds is never taken over) and records the holder
# (and appends "<id> <actor>" to $SPIRA_RUN/lc-claims.log, so a suite can ask who claimed);
# Release/HolderDead drop that claim; Submit writes lc-row SUBMITTED; any other kind (Renew,
# Hold, ...) applies without changing the row. Every other verb goes to the real spira-lc
# further down PATH, exactly as before the stub.
lc_aeon_mirror() {
    local dir="${1:?lc_aeon_mirror needs a directory}"
    mkdir -p "$dir"
    cat > "$dir/spira-lc" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
    show|state|list|event|create-bead|unclaim) ;;
    *) for c in $(type -ap spira-lc); do [ "$c" -ef "$0" ] || exec "$c" "$@"; done; exit 2 ;;
esac
[ "$1" = create-bead ] && exit 0
src="$(BD_IGNORE_SCHEMA_SKEW=1 "${SPIRA_BD:-bd}" -C "${SPIRA_DB:-.}" list --all --limit 0 --json 2>/dev/null | sed -n '/^[[{]/,$p')"
printf '%s' "$src" | python3 -c '
import json, os, sys
args = sys.argv[1:]
verb = args[0]
run = os.environ.get("SPIRA_RUN") or "/nonexistent"
try:
    beads = json.loads(sys.stdin.read() or "[]")
except ValueError:
    sys.exit(2)
if isinstance(beads, dict):
    beads = [beads]
ask = os.environ.get("SPIRA_ASK_LABEL", "")
def read(path):
    try:
        return open(path).read().strip()
    except OSError:
        return None
def row(b):
    i = b["id"]
    labels = b.get("labels") or []
    r = {"bead_id": i, "state": "READY", "reason": "", "tip": "", "holder": None, "lease_until": None,
         "version": 0, "holds": (["poison"] if "spira-poison" in labels else []) + (["ask"] if ask and ask in labels else [])}
    pinned, claim = read(os.path.join(run, "lc-row", i)), read(os.path.join(run, "lc-claim", i))
    if pinned:
        f = pinned.split()
        r["state"] = f[0]
        r["reason"] = f[1] if len(f) > 1 else ""
        r["tip"] = f[2] if len(f) > 2 else ""
        if f[0] == "WORKING":
            r["holder"] = claim
    elif b.get("status") == "closed":
        r["state"] = "SUBMITTED"
    elif claim:
        r["state"], r["holder"] = "WORKING", claim
    elif b.get("status") == "in_progress":
        r["state"], r["holder"] = "WORKING", (b.get("assignee") or None)
    return r
rows = {b["id"]: row(b) for b in beads
        if isinstance(b, dict) and b.get("id") and b.get("issue_type") not in ("epic", "event")}
if verb == "list":
    want = args[2] if len(args) > 2 and args[1] == "--state" else None
    print(json.dumps([r for r in rows.values() if want is None or r["state"] == want]))
elif verb == "show":
    r = rows.get(args[1] if len(args) > 1 else "")
    if r is None:
        sys.exit(1)
    print(json.dumps({"bead": r, "delivery": None}))
elif verb == "state":
    r = rows.get(args[1] if len(args) > 1 else "")
    if r is None:
        sys.exit(1)
    print(r["state"])
elif verb == "unclaim":
    i, actor = (args[1] if len(args) > 1 else ""), (args[2] if len(args) > 2 else "")
    r = rows.get(i)
    if r is None or not actor:
        sys.exit(1)
    if r["state"] != "WORKING":
        sys.exit(0)
    if r["holder"] != actor:
        sys.exit(1)
    claim = os.path.join(run, "lc-claim", i)
    if os.path.exists(claim):
        os.remove(claim)
elif verb == "event":
    i = args[2] if len(args) > 2 else ""
    opt = {args[k]: args[k + 1] for k in range(3, len(args) - 1) if args[k].startswith("--")}
    kind, actor = opt.get("--kind", ""), opt.get("--actor", "")
    r = rows.get(i)
    if r is None:
        sys.exit(1)
    claims = os.path.join(run, "lc-claim")
    if "Claim" in kind:
        if r["state"] not in ("READY", "REWORK"):
            sys.exit(3)
        os.makedirs(claims, exist_ok=True)
        open(os.path.join(claims, i), "w").write(actor)
        open(os.path.join(run, "lc-claims.log"), "a").write("%s %s\n" % (i, actor))
        pinned = os.path.join(run, "lc-row", i)
        if os.path.exists(pinned):
            os.remove(pinned)
    elif "Release" in kind or "HolderDead" in kind:
        if os.path.exists(os.path.join(claims, i)):
            os.remove(os.path.join(claims, i))
    elif "Submit" in kind:
        os.makedirs(os.path.join(run, "lc-row"), exist_ok=True)
        open(os.path.join(run, "lc-row", i), "w").write("SUBMITTED\n")
' "$@"
STUB
    chmod +x "$dir/spira-lc"
}

# lc_socket_mirror <dir> — a stand-in lifecycle service on a Unix socket, for a legacy suite
# that drives a real `spira-lc` verb whose own state read (`show`/`list`, through
# $SPIRA_LC_SOCKET) must answer — close-on-land reads the bead's state there (sp-mve9i). It
# answers `show <id>` and `list` from the suite's bd store in lifecycle terms: closed (the
# builder's close is its submit) or open + $SPIRA_SUBMITTED_LABEL → SUBMITTED, in_progress →
# WORKING, anything else → READY; every other verb "cannot tell" (2). Exports
# SPIRA_LC_SOCKET; the server exits on its own when the suite's shell does.
lc_socket_mirror() {
    local dir="${1:?lc_socket_mirror needs a directory}"
    mkdir -p "$dir"
    export SPIRA_LC_SOCKET="$dir/lc.sock"
    # round 3 fix (pattern 3/7): SPIRA_LC_SOCKET is a registered key — the plain export
    # above is for this function's own shell use; a real binary (sending, queue, ...)
    # only finds this mock's socket through SPIRA_TOML now, or it falls to the complete
    # fixture's placeholder /run/user/.../spira-lc/sock and reports "did not answer".
    tl_config SPIRA_LC_SOCKET="$SPIRA_LC_SOCKET"
    rm -f "$SPIRA_LC_SOCKET"
    python3 - "$SPIRA_LC_SOCKET" "$$" <<'PY' >"$dir/server.log" 2>&1 &
import json, os, socket, subprocess, sys, threading, time
path, owner = sys.argv[1], int(sys.argv[2])
def watchdog():
    while True:
        try:
            os.kill(owner, 0)
        except OSError:
            os._exit(0)
        time.sleep(1)
threading.Thread(target=watchdog, daemon=True).start()
sub = os.environ.get("SPIRA_SUBMITTED_LABEL") or "spira-submitted"
# Holds are the row's (sp-psztcc): a hold/unhold event is recorded here and shown on the row.
holds = {}
def bd(args):
    env = dict(os.environ, BD_IGNORE_SCHEMA_SKEW="1")
    o = subprocess.run([os.environ.get("SPIRA_BD") or "bd", "-C", os.environ.get("SPIRA_DB") or ".", *args, "--json"],
                       capture_output=True, text=True, env=env, timeout=30)
    t = o.stdout[o.stdout.find("[") if "[" in o.stdout else 0:]
    try:
        v = json.loads(t or "[]")
    except ValueError:
        return []
    return v if isinstance(v, list) else [v]
def row(b):
    st = b.get("status") or "open"
    labels = b.get("labels") or []
    state = "SUBMITTED" if st == "closed" or sub in labels else {"in_progress": "WORKING"}.get(st, "READY")
    return {"bead_id": b["id"], "state": state, "holds": json.dumps(sorted(holds.get(b["id"], set()))), "version": "1",
            "holder": (b.get("assignee") or None) if state == "WORKING" else None}
def answer(args):
    if args[:1] == ["show"] and len(args) > 1:
        hit = [b for b in bd(["show", args[1]]) if isinstance(b, dict) and b.get("id") == args[1]]
        return (0, json.dumps({"bead": row(hit[0]), "delivery": None})) if hit else (1, "{}")
    if args[:1] == ["list"]:
        return 0, json.dumps([row(b) for b in bd(["list", "--all", "--limit", "0"]) if isinstance(b, dict) and b.get("id")])
    if args[:2] == ["event", "bead"] and len(args) > 2 and "--kind" in args:
        kind = json.loads(args[args.index("--kind") + 1])
        bid = args[2]
        if isinstance(kind, dict) and "Hold" in kind:
            holds.setdefault(bid, set()).add(kind["Hold"]["kind"].lower())
        elif isinstance(kind, dict) and "Unhold" in kind:
            holds.get(bid, set()).discard(kind["Unhold"]["kind"].lower())
        elif kind == "AskWithdrawn":
            holds.get(bid, set()).discard("ask")
        return 0, ""
    return 2, "cannot tell: stand-in lifecycle service"
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(path)
srv.listen(16)
while True:
    c, _ = srv.accept()
    try:
        f = c.makefile("rw")
        code, out = answer(json.loads(f.readline() or "[]"))
        f.write(json.dumps({"exit_code": code, "stdout": out}) + "\n")
        f.flush()
    except Exception as e:
        print(e, file=sys.stderr)
    finally:
        c.close()
PY
    local _i
    for _i in $(seq 1 50); do [ -S "$SPIRA_LC_SOCKET" ] && return 0; sleep 0.1; done
    bail "lc_socket_mirror: the stand-in lifecycle service never opened $SPIRA_LC_SOCKET: $(cat "$dir/server.log")"
}

# A suite declaring `# requires: testenv` (systemctl on a user manager, install/uninstall,
# production paths) refuses here, before any of its own code runs, when SPIRA_IN_TESTENV
# is not 1 — the one thing a statute could not stop (sp-nxvjm) a structural check can.
if suite-select header testenv-unmet "${BASH_SOURCE[1]:-$0}"; then
    bail "requires: testenv — run via testenv, not directly (SPIRA_IN_TESTENV != 1)"
fi

# TAP version 14 MUST be the first line of output, so it is printed at SOURCE time
# rather than lazily on first use: a suite that prints its own name (the common
# convention this library inherits) does so after `. testlib.sh`, never before.
_tl_init
