#!/bin/sh
# rc-stale-round.sh <repo> <member-a> <member-b> — one move of the stale-round-pointer world, run
# again and again: open round rc-a, open round rc-b beside it, certify rc-b, abandon rc-a's row
# alone with requeue-orphans (its record stays the primary `round`), then drive rc-b with
# round-recert until it lands. Exit 3 for a move that has to wait or try again, 9 for a wrong
# answer, 0 for a move made.
set -u
repo="$1" a="$2" b="$3"
state_of() {  # state_of <bead>
    env -u SPIRA_DB -u SPIRA_LC_SOCKET -u SPIRA_LC_PASSWORD_FILE sim probe "$SIM_WORLD" | tr -d ' ' | grep "\"bead\":\"$1\"" | grep -o '"lc_state":"[A-Z_]*"' | cut -d'"' -f4
}
batch_of() {  # batch_of <batch> — its lifecycle state, empty if it has no row
    spira-lc show-batch "$1" 2>/dev/null | tr -d ' ' | grep -o '"state":"[A-Z_]*"' | head -1 | cut -d'"' -f4
}
refused_on_a() {  # refused_on_a <output> — the refusal that names the abandoned round as the open one
    printf '%s\n' "$1" | grep -q "the open round for $repo is rc-a, not rc-b"
}
sa=$(batch_of rc-a) sb=$(batch_of rc-b)
if [ -z "$sa" ]; then
    [ "$(state_of "$a")" = CERTIFIED ] && [ "$(state_of "$b")" = CERTIFIED ] || exit 3
    queue round open "$repo" --members "$a" --name rc-a || exit 9
    exit 0
fi
if [ -z "$sb" ]; then
    queue round open "$repo" --members "$b" --name rc-b || exit 9
    exit 0
fi
case "$sb" in
OPEN)
    out=$(queue round certify rc-b "$repo" 2>&1); rc=$?
    printf '%s\n' "$out"
    refused_on_a "$out" && exit 9
    [ "$rc" = 0 ] || exit 9
    exit 0 ;;
esac
if [ "$sa" = OPEN ]; then
    spira-lc requeue-orphans --actor reconciler --apply --stale-secs 0 || exit 9
    [ "$(batch_of rc-a)" = ABANDONED ] || exit 9
    [ "$(batch_of rc-b)" = "$sb" ] || exit 9
    exit 0
fi
case "$sb" in
LANDED|SETTLED) exit 0 ;;
ABANDONED|DISCARDED) exit 9 ;;
esac
# round-recert finds its release from where its own executable really is, and the world's release
# links its executables out of the build tree; a release of links with a real round-recert in its
# bin/ is the layout an installed release has. Every other name links to what the world's PATH
# runs, so the world's own round-vm stands in for the VM.
rel="$SIM_WORLD/recert-release"
if [ ! -x "$rel/bin/round-recert" ]; then
    on_path=$(command -v round-recert) || exit 9
    src=$(dirname "$(dirname "$on_path")")
    [ -d "$src/spira" ] || exit 9
    mkdir -p "$rel/bin" || exit 9
    for f in "$src"/*; do [ "${f##*/}" = bin ] || ln -sfn "$(readlink -f "$f")" "$rel/${f##*/}" || exit 9; done
    for f in "$src"/bin/*; do
        n=${f##*/}
        ln -sfn "$(command -v "$n" || readlink -f "$f")" "$rel/bin/$n" || exit 9
    done
    # round-recert hands queue a cleared environment, and the world's round-vm answers only
    # when it is told which world it is in.
    vm=$(command -v round-vm) || exit 9
    rm -f "$rel/bin/round-vm" || exit 9
    printf '#!/bin/sh\nSIM_WORLD=%s exec %s "$@"\n' "'$SIM_WORLD'" "'$vm'" > "$rel/bin/round-vm" && chmod +x "$rel/bin/round-vm" || exit 9
    rm -f "$rel/bin/round-recert" && cp "$(readlink -f "$src/bin/round-recert")" "$rel/bin/round-recert" || exit 9
fi
out=$("$rel/bin/round-recert" --drive --batch rc-b --repo "$repo" 2>&1)
printf '%s\n' "$out"
refused_on_a "$out" && exit 9
printf '%s\n' "$out" | grep -q '^round-recert: rc-b landed at ' && exit 0
exit 3
