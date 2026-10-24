#!/usr/bin/env bash
#
# test-bead-pilgrimage.sh — bead.sh pilgrimage: a design's pilgrimage bead is filed for the
# pilgrim with the design's Intent and User path, blocked by every build bead; a design with
# no User path is refused before anything is filed.
#
# tier: T1
# covers: spira/bead.sh spira/chamber/pilgrim.fayth bead/**
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/chamber" "$T/run" "$T/testrepo"
ln -s "$HERE/conf.d" "$T/conf.d"
tl_config SPIRA_CHAMBER="$T/chamber"

cat > "$T/chamber/builder.fayth" <<'FAYTH'
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan}"
FAYTH_TOOLS="Bash,Edit,Write"
FAYTH_MODEL=claude-sonnet-5
FAYTH_MAX_CONCURRENT=4
FAYTH
cat > "$T/chamber/pilgrim.fayth" <<'FAYTH'
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PILGRIM_LABEL:-pilgrimage}"
FAYTH_LANE=pilgrimage
FAYTH_TOOLS="Bash,Read"
FAYTH_MODEL=claude-opus-5-5
FAYTH_MAX_CONCURRENT=1
FAYTH
cat > "$T/repos.map" <<MAP
testrepo | $T/testrepo | push | origin/main | | true | plan
MAP

BD_LOG="$T/bd.log"; SEQ="$T/seq"; echo 0 > "$SEQ"
cat > "$T/stub-bd" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    create)
        n=$(( $(cat "$SEQ_PATH") + 1 )); echo "$n" > "$SEQ_PATH"
        printf '%s\n' "$*" >> "$BD_LOG_PATH"; printf 'sp-n%s\n' "$n"; exit 0 ;;
    dep)  printf '%s\n' "$*" >> "$BD_LOG_PATH"; exit 0 ;;
    show) echo '[{"id":"'"${2:-}"'","status":"open","issue_type":"task","labels":["plan","repo:testrepo"]}]'; exit 0 ;;
    *)    exit 0 ;;
esac
STUB
chmod +x "$T/stub-bd"
printf '#!/usr/bin/env bash\nexit 0\n' > "$T/stub-lc"; chmod +x "$T/stub-lc"

TOOLS="$(command -v spira-config)" && TOOLS="$(dirname "$TOOLS")"
tl_config SPIRA_BD="$T/stub-bd" SPIRA_DB="$T/db" SPIRA_REPO_MAP="$T/repos.map" \
    SPIRA_PLAN_LABEL="plan" SPIRA_PILGRIM_LABEL="walkabout" SPIRA_SCOPE_LABEL="testscope" \
    SPIRA_INCIDENT_LABEL="incident" SPIRA_RUN="$T/run"

bead_sh() {
    env -i HOME="$T" PATH="$HERE:${TOOLS:+$TOOLS:}/usr/bin:/bin" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$T/none.conf" \
        BD_LOG_PATH="$BD_LOG" SEQ_PATH="$SEQ" SPIRA_LC_BIN="$T/stub-lc" \
        SPIRA_HOME="$T" SPIRA_REPO="$T" bead.sh "$@" 2>&1
}

cat > "$T/design.md" <<'DESIGN'
# Agent tools

## Intent

Let an agent save a card to a collection.

## User path

1. Sign in through the real sign-in.
2. Ask the agent to save a card.

## Test strategy

Not part of the pilgrimage.
DESIGN
sed 's/## User path/## Walkthrough/' "$T/design.md" > "$T/no-path.md"

echo "test-bead-pilgrimage.sh"
: > "$BD_LOG"
builds=()
for n in one two three; do
    id="$(bead_sh file "build $n" --for builder --repo testrepo --body-file /dev/null)"
    builds+=("$id")
done
is "N build beads filed" "3" "${#builds[@]}"

args=(); for b in "${builds[@]}"; do args+=(--after "$b"); done
pid="$(bead_sh pilgrimage "$T/design.md" --repo testrepo "${args[@]}")"; rc=$?
is   "pilgrimage exits 0" "0" "$rc"
is   "the pilgrimage is the N+1th bead" "sp-n4" "$pid"
creates="$(grep -c '^create' "$BD_LOG")"
is   "N+1 beads are created" "4" "$creates"
want "the pilgrimage is titled for the design" "Pilgrimage: Agent tools" "$(grep '^create' "$BD_LOG" | tail -1)"
want "the pilgrimage carries the pilgrim's partition label" "walkabout" "$(grep '^create' "$BD_LOG" | tail -1)"
for b in "${builds[@]}"; do
    want "the pilgrimage is blocked by $b" "dep add $pid $b" "$(cat "$BD_LOG")"
done
is   "no other edge is wired" "3" "$(grep -c '^dep add' "$BD_LOG")"

: > "$BD_LOG"
out="$(bead_sh pilgrimage "$T/no-path.md" --repo testrepo "${args[@]}")"; rc=$?
is   "a design with no User path is refused" "2" "$rc"
want "the refusal names the missing section" "User path" "$out"
is   "nothing is filed for it" "0" "$(grep -c . "$BD_LOG")"

out="$(bead_sh pilgrimage "$T/design.md" --repo testrepo)"; rc=$?
is   "a pilgrimage with no build bead is refused" "2" "$rc"
is   "nothing is filed without a build bead" "0" "$(grep -c . "$BD_LOG")"

tl_summary
