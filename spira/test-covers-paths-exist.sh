#!/usr/bin/env bash
# test-covers-paths-exist.sh — every path a shell script names on its `# covers:` line exists.
# tier: T1
# covers: spira/*.sh cockpit/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$HERE")"

tracked_files() {  # tracked_files <root> -> every file path under it, relative
    if git -C "$1" rev-parse --git-dir >/dev/null 2>&1; then git -C "$1" ls-files
    else (cd "$1" && find . -type f -not -path './.git/*' -not -path './target/*' | sed 's#^\./##'); fi
}

covers_of() {  # covers_of <suite> -> the globs on its covers: line, continuation lines folded
    awk '
        state == 0 { if (match($0, /^# *covers: */)) { out = substr($0, RLENGTH + 1); state = 1 } else if ($0 ~ /^set -/) exit; next }
        state == 1 {
            if ($0 ~ /^#[ \t]{2,}[^ \t#]/ && $0 !~ /^# [A-Za-z][A-Za-z_-]*:/) { line = $0; sub(/^#[ \t]+/, "", line); out = out " " line }
            else exit
        }
        END { print out }
    ' <(head -80 "$1")
}

dead_covers() {  # dead_covers <root> <files-list> -> "<script>: <glob>" per glob matching no file
    local f g p hit
    while IFS= read -r f; do
        case "$f" in *.sh) ;; *) continue ;; esac
        f="$1/$f"
        set -f
        for g in $(covers_of "$f"); do
            [[ "$g" =~ ^(UC|G)-[0-9A-Za-z-]+$ ]] && continue
            hit=0
            while IFS= read -r p; do
                if [[ "$p" == $g || "$p" == "${g%/}"/* ]]; then hit=1; break; fi
            done < "$2"
            [ "$hit" = 1 ] || echo "${f#"$1"/}: $g"
        done
        set +f
    done < "$2"
}

PLANT="$(mktemp -d)"; trap 'rm -rf "$PLANT"' EXIT INT TERM
printf '#!/usr/bin/env bash\n# covers: spira/lib.sh spira/no-such-file.sh\nset -u\n' > "$PLANT/test-planted.sh"
mkdir "$PLANT/spira"; : > "$PLANT/spira/lib.sh"
printf 'test-planted.sh\nspira/lib.sh\n' > "$PLANT/files"
[ "$(dead_covers "$PLANT" "$PLANT/files")" = "test-planted.sh: spira/no-such-file.sh" ] \
    && echo "  PASS  planted dead covers path is flagged" \
    || { echo "  FAIL  matcher missed the planted offender"; exit 1; }

tracked_files "$ROOT" > "$PLANT/tree"
dead="$(dead_covers "$ROOT" "$PLANT/tree")"
if [ -z "$dead" ]; then echo "  PASS  every covers: path exists"
else echo "  FAIL  covers: names a path that does not exist:"; echo "$dead" | sed 's/^/    /'; exit 1; fi
