#!/usr/bin/env bash
#
# test-testenv-curl-bounds.sh — every curl invocation in the test image build carries a
# bounded connect timeout, a bounded total timeout, and a retry (sp-lqhl6).
#
# curl -fsSL alone hangs for as long as a stalled connection holds the TCP socket open,
# which on the "Acquire the test image" gate step holds a runner for as long as GitHub's
# own default ceiling allows. Every RUN line in the Containerfile that calls curl must
# name --retry, --connect-timeout and --max-time; the check below plants the original
# unbounded shape as a positive control before trusting the shipped file's silence
# (law-absence-needs-a-positive-control), then proves the surviving flags actually bound
# a real connection attempt rather than merely being present as text.
#
# tier: T2
# covers: spira/testenv/Containerfile
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

CONTAINERFILE="$HERE/testenv/Containerfile"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# join_continuations <file> — Containerfile RUN lines wrap with a trailing backslash;
# join each such run into one logical line so a curl invocation split across lines is
# seen whole rather than truncated at the first line break.
join_continuations() {
    awk '
    {
        line = $0
        if (buf != "") { line = buf " " line }
        if (line ~ /\\$/) { sub(/\\$/, "", line); buf = line }
        else { print line; buf = "" }
    }
    END { if (buf != "") print buf }
    ' "$1"
}

# missing_bounds <file> — one line per curl invocation missing --retry, --connect-timeout
# or --max-time. Silent output means every invocation in <file> carries all three.
missing_bounds() {
    join_continuations "$1" | grep -oE 'curl +-[^|&]*' | while IFS= read -r inv; do
        ok=1
        for flag in --retry --connect-timeout --max-time; do
            case "$inv" in
                *"$flag"*) ;;
                *) ok=0 ;;
            esac
        done
        [ "$ok" = 1 ] || printf 'MISSING: %s\n' "$inv"
    done
}

echo "test-testenv-curl-bounds.sh"

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL — the original defect, planted (sp-sbc6o's unbounded duckdb fetch).
# ---------------------------------------------------------------------------------------
cat > "$TMP/bad.Containerfile" <<'EOF'
ARG DUCKDB_VERSION=1.5.5
RUN curl -fsSL -o /tmp/duckdb.gz \
    "https://github.com/duckdb/duckdb/releases/download/v${DUCKDB_VERSION}/duckdb_cli-linux-amd64.gz" && \
    gunzip -c /tmp/duckdb.gz > /usr/local/bin/duckdb
EOF
out="$(missing_bounds "$TMP/bad.Containerfile")"
want "SEEN RED: an unbounded curl -fsSL -o fetch is caught" "MISSING: curl -fsSL -o /tmp/duckdb.gz" "$out"

# ---------------------------------------------------------------------------------------
# THE CORRECT FORM IS SILENT.
# ---------------------------------------------------------------------------------------
cat > "$TMP/good.Containerfile" <<'EOF'
ARG DUCKDB_VERSION=1.5.5
RUN curl -fsSL --retry 3 --retry-all-errors --connect-timeout 10 --max-time 300 \
    -o /tmp/duckdb.gz \
    "https://github.com/duckdb/duckdb/releases/download/v${DUCKDB_VERSION}/duckdb_cli-linux-amd64.gz" && \
    gunzip -c /tmp/duckdb.gz > /usr/local/bin/duckdb
EOF
out="$(missing_bounds "$TMP/good.Containerfile")"
is "GREEN: a bounded curl -fsSL -o fetch is silent" "" "$out"

# ---------------------------------------------------------------------------------------
# A package-list mention of curl (apt-get install curl) is not an invocation.
# ---------------------------------------------------------------------------------------
cat > "$TMP/aptonly.Containerfile" <<'EOF'
RUN apt-get update -q && \
    apt-get install -yq --no-install-recommends \
        curl \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*
EOF
out="$(missing_bounds "$TMP/aptonly.Containerfile")"
is "GREEN: curl named only as a package is silent" "" "$out"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, this now means something.
# ---------------------------------------------------------------------------------------
out="$(missing_bounds "$CONTAINERFILE")"
is "the shipped Containerfile: every curl fetch is bounded" "" "$out"
[ -n "$out" ] && printf '%s\n' "$out"

# ---------------------------------------------------------------------------------------
# THE FLAGS ACTUALLY BOUND A REAL CONNECTION. 192.0.2.1 is RFC 5737 TEST-NET-1: reserved,
# never routed, guaranteed not to answer. Extract the exact flags the Containerfile ships
# for the duckdb fetch and fire them at that address — if the flags were typos or the
# wrong curl option names, this is where that would show up instead of at a hung gate.
#
# --retry-all-errors resends the connect attempt on a connect-timeout too, so the
# worst case is (retries + 1) * --connect-timeout plus curl's own backoff between
# tries, not --connect-timeout alone: with --retry 3 --connect-timeout 10 that is a
# deterministic ~47s, never observed here past 90s. Bounded and finite is the bar —
# the defect this suite guards against is unbounded, not merely slow.
# ---------------------------------------------------------------------------------------
flags="$(join_continuations "$CONTAINERFILE" | grep -oE 'curl +-fsSL[^|&]*duckdb\.gz' \
    | sed -E 's/-o [^ ]+//; s/"[^"]*"//; s/curl //')"
if [ -z "$flags" ]; then
    bad "duckdb curl flags extracted from the Containerfile for the live probe" \
        "no matching curl invocation found"
else
    start=$SECONDS
    timeout 120 curl $flags -o /dev/null "http://192.0.2.1/unreachable" >/dev/null 2>&1
    rc=$?
    elapsed=$((SECONDS - start))
    not_hung() { [ "$1" -lt 90 ] && ok "$2" || bad "$2" "took ${1}s"; }
    not_hung "$elapsed" "live probe: an unreachable host fails within the bound, not after it"
    [ "$rc" -ne 0 ] && ok "live probe: an unreachable host is a curl failure, not a silent hang" \
        || bad "live probe: an unreachable host is a curl failure, not a silent hang" "rc=0"
fi

tl_summary
