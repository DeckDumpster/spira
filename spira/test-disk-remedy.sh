#!/usr/bin/env bash
#
# test-disk-remedy.sh — disk-remedy.sh reaps a worktree on the bead's LIFECYCLE state, never on
# bd's status: a terminal row is reaped, a live row is kept, a bead with no row is kept.
#
# tier: T1
# covers: spira/disk-remedy.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
BIN="$T/bin"; mkdir -p "$BIN" "$T/run/worktree"/{sp-done,sp-live,sp-norow}
DESTROYED="$T/destroyed"; : > "$DESTROYED"

cat > "$BIN/spira-lc" <<'S'
#!/usr/bin/env bash
[ "$1" = show ] || exit 2
case "$2" in
    sp-done) echo '{"bead":{"bead_id":"sp-done","state":"LANDED","version":3}}' ;;
    sp-live) echo '{"bead":{"bead_id":"sp-live","state":"WORKING","version":3}}' ;;
    *) exit 1 ;;
esac
S
cat > "$BIN/sending" <<S
#!/usr/bin/env bash
[ "\$1" = destroy-worktree ] && echo "\$*" >> "$DESTROYED"
exit 0
S
REAL_CONFIG="$(command -v spira-config)"
cat > "$BIN/spira-config" <<S
#!/usr/bin/env bash
case "\$*" in "repo root"*) echo /nonexistent/repo ;; *) exec "$REAL_CONFIG" "\$@" ;; esac
S
printf '#!/usr/bin/env bash\necho closed\n' > "$BIN/bdq"
cp "$BIN/bdq" "$BIN/bd"
chmod +x "$BIN"/*

out="$(env -i PATH="$BIN:$PATH" HOME="$T" SPIRA_TOML="$(tl_layer SPIRA_RUN="$T/run")" SPIRA_HOME="$HERE/.." \
    bash "$HERE/disk-remedy.sh" 2>&1)"
got="$(cat "$DESTROYED")"; [ -n "$got" ] || echo "# remedy output: $out" | head -c 600 >&2

want "a landed bead's worktree is destroyed (bd says closed for every bead, so only the row can have decided)" "sp-done" "$got"
nowant "a working bead's worktree is kept though bd says closed" "sp-live" "$got"
nowant "a bead with no lifecycle row is kept" "sp-norow" "$got"
tl_summary
