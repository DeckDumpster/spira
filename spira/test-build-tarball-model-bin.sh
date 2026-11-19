#!/usr/bin/env bash
#
# test-build-tarball-model-bin.sh — the tarball's model-bin/ holds exactly
# spira_config::release_env::MODEL_BINS (built from the real spira-config, no second list).
#
# CASES
#   1. Every MODEL_BINS name shipped in bin/ is linked in the tarball's model-bin/, and
#      model-bin/ holds nothing else.
#   2. POSITIVE CONTROL: a planted extra name in model-bin/ is reported by the comparison.
#
# tier: T2
# covers: spira/build-tarball.sh spira-config/src/** spira/test-tarball-bins.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
REPO_ROOT="$(cd "$HERE/.." && pwd -P)"

echo "test-build-tarball-model-bin.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

SC="$(cd "$REPO_ROOT" && cargo build -q -p spira-config --bin spira-config --message-format=json 2>"$TMP/cargo.err" \
    | python3 -I -c 'import sys,json
for l in sys.stdin:
    try: m=json.loads(l)
    except ValueError: continue
    if m.get("reason")=="compiler-artifact" and m.get("executable") and m["target"]["name"]=="spira-config": print(m["executable"])' | tail -1)"
[ -x "$SC" ] || { bad "built spira-config" "no executable: $(tail -3 "$TMP/cargo.err")"; tl_summary; exit 1; }

want="$("$SC" model-bins | LC_ALL=C sort)"
[ -n "$want" ] || { bad "MODEL_BINS read from source" "empty"; tl_summary; exit 1; }

REPO="$TMP/repo"
git init -q -b main "$REPO"
mkdir -p "$REPO/spira"; printf 'k=v\n' > "$REPO/spira/conf.sh"
git -C "$REPO" add . && git -C "$REPO" commit -qm fixture

mkdir -p "$TMP/bins"
for b in $want spira-supervise bd; do printf '#!/usr/bin/env bash\necho %s\n' "$b" > "$TMP/bins/$b"; chmod +x "$TMP/bins/$b"; done
cp "$SC" "$TMP/bins/spira-config"

env -i SPIRA_RELEASE="$(tl_release)" PATH="$PATH" HOME="$TMP/home" GIT_CONFIG_GLOBAL=/dev/null \
    GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
    bash "$HERE/build-tarball.sh" build --output "$TMP/out" --bin-dir "$TMP/bins" HEAD "$REPO" >"$TMP/build.log" 2>&1
is "build exits 0" "0" "$?"

rm -f "$TMP/bins/spira-config"
if env -i PATH="$PATH" HOME="$TMP/home" SPIRA_RELEASE="$(tl_release)" GIT_CONFIG_GLOBAL=/dev/null \
    bash "$HERE/build-tarball.sh" build --output "$TMP/out2" --bin-dir "$TMP/bins" HEAD "$REPO" >/dev/null 2>&1; then
    bad "a build with no staged spira-config fails" "exit 0"
else
    ok "a build with no staged spira-config fails"
fi

tarball="$(find "$TMP/out" -name 'spira-*.tar.gz' | head -1)"
mkdir -p "$TMP/unpack"; tar -xzf "$tarball" -C "$TMP/unpack"
TREE="$TMP/unpack/$(basename "${tarball%.tar.gz}")"

extra_names() { LC_ALL=C comm -3 <(printf '%s\n' "$want") <(ls "$1/model-bin" | LC_ALL=C sort); }

is "model-bin/ equals MODEL_BINS" "" "$(extra_names "$TREE")"
ln -s ../bin/bd "$TREE/model-bin/bd"
case "$(extra_names "$TREE")" in *bd*) ok "a planted extra model-bin name is reported" ;; *) bad "a planted extra model-bin name is reported" "silent" ;; esac

tl_summary
