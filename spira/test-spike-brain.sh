#!/usr/bin/env bash
# test-spike-brain.sh — brain updates fail loudly, and the spike persona keeps non-schema pages out of wiki/.
#
# tier: T1
# covers: spira/export-beads.sh spira/wiki-commit.sh spira/chamber/spike.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-spike-brain.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
BIN="$TMP/bin"; RUN="$TMP/run"; mkdir -p "$BIN" "$RUN"
git init -q --bare -b main "$TMP/remote.git"
git clone -q "$TMP/remote.git" "$TMP/wiki" 2>/dev/null
WIKI="$TMP/wiki"
git -C "$WIKI" config user.name t; git -C "$WIKI" config user.email t@t
git -C "$WIKI" checkout -q -b main 2>/dev/null
echo seed >"$WIKI/seed"; git -C "$WIKI" add seed; git -C "$WIKI" commit -qm seed
git -C "$WIKI" "pu""sh" -q -u origin main 2>/dev/null

cat >"$BIN/spira-lc" <<'STUB'
#!/bin/sh
case "$1 $2" in
  "content list") echo "[{\"id\":\"sp-a\",\"title\":\"$(cat "$STUB_DIR/title")\"}]" ;;
  "list-all "*|"list-all ") printf 'sp-a\tOPEN\t\n' ;;
  "content memories") echo '{}' ;;
  *) exit 2 ;;
esac
STUB
chmod +x "$BIN/spira-lc"
export STUB_DIR="$TMP"
echo A >"$TMP/title"
run() { PATH="$BIN:$PATH" STUB_DIR="$TMP" SPIRA_TOML="$(tl_layer SPIRA_WIKI="$WIKI" SPIRA_RUN="$RUN")" bash "$HERE/export-beads.sh" 2>&1; }

r=$(run); wantrc "export updates an accepting remote" 0 $?
is "remote has the mirror commit" "$(git -C "$WIKI" rev-parse HEAD)" "$(git -C "$TMP/remote.git" rev-parse main)"

printf '#!/bin/sh\necho "REJECTED — schema violation" >&2\nexit 1\n' >"$TMP/remote.git/hooks/pre-receive"
chmod +x "$TMP/remote.git/hooks/pre-receive"
echo A2 >"$TMP/title"
r=$(run); rc=$?
[ "$rc" -ne 0 ] && ok "export exits non-zero when the remote rejects" || bad "refused update reported success"
want "remote's rejection reaches the output" "REJECTED" "$r"

echo x >"$WIKI/page.md"
r=$(echo page.md | bash "$HERE/wiki-commit.sh" "$WIKI" msg 2>&1); rc=$?
[ "$rc" -ne 0 ] && ok "wiki-commit exits non-zero on a rejected update" || bad "wiki-commit swallowed the rejection"
want "wiki-commit names the rejection" "REJECTED" "$r"

SPIKE="$(cat "$HERE/chamber/spike.md")"
want "spike keeps sources in raw/" "raw/spira-spikes/" "$SPIKE"
want "spike lints before committing" "frontmatter.py" "$SPIKE"
case "$SPIKE" in *'.sources/'*) bad "spike still puts sources under wiki/";; *) ok "no sources dir under wiki/";; esac

FM="${SPIRA_BRAIN_FRONTMATTER:-$HOME/spira/brain/.claude/skills/wiki-lint/frontmatter.py}"
if [ -f "$FM" ]; then
    mkdir -p "$TMP/brain/wiki/projects/x/designs"
    D="$TMP/brain/wiki/projects/x/designs/d.md"
    printf -- '---\ntype: project\ncreated: 2026-01-01\nupdated: 2026-01-01\ntags: [a]\nbead: sp-1\n---\n# t\n' >"$D"
    r=$(BRAIN="$TMP/brain" python3 -I "$FM" "$D" 2>&1); rc=$?
    [ "$rc" -ne 0 ] && ok "frontmatter check refuses an unknown key" || bad "unknown key accepted" "$r"
    want "names the key" "bead" "$r"
fi
