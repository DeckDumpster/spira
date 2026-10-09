#!/usr/bin/env bash
# test-suite-select-cli.sh — `suite-select` header parsing and changed-file selection, driven
# through its command line over a throwaway git repository.
# tier: T1
# covers: suite-select/* UC-test-infrastructure-01 UC-test-infrastructure-03 UC-test-infrastructure-04 UC-test-infrastructure-05 UC-test-infrastructure-06
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

command -v suite-select >/dev/null 2>&1 || bail "suite-select is not on PATH"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
R="$TMP/repo"
mkdir -p "$R/spira"
git init -q -b main "$R"
GIT=(git -C "$R" -c user.email=t@t -c user.name=t)

ss() { env -i PATH="$PATH" HOME="$TMP" suite-select "$@"; }
sel() { ss select --repo "$R" --suite-dir "$R/spira" "$@" 2>/dev/null; }
suite() {   # suite <name> <header lines...>
    local n="$1"; shift
    { printf '#!/usr/bin/env bash\n'; printf '%s\n' "$@"; printf 'set -uo pipefail\n'; } > "$R/spira/$n"
}
commit() { "${GIT[@]}" add -A && "${GIT[@]}" commit -q -m "$1"; }
flat() { tr '\n' ' ' | sed 's/ $//'; }

# ---- UC-01: header parser -------------------------------------------------
suite test-h.sh '# tier: T2' '# covers: spira/one.sh spira/two.sh' '# covers: spira/ignored.sh' \
    '# requires: testenv, podman' '# exclusive: yes' '# timeout: 90' '# priority: 3' '# selects-on: added,mode'
suite test-bare.sh
H="$R/spira/test-h.sh"
is "covers: the first line only"        "spira/one.sh spira/two.sh" "$(ss header covers "$H")"
is "tier"                               "T2"                        "$(ss header tier "$H")"
is "requires: comma separated"          "testenv podman"            "$(ss header requires "$H")"
is "exclusive"                          "yes"                       "$(ss header exclusive "$H")"
is "selects-on: comma separated"        "added mode"                "$(ss header selects-on "$H")"
is "no declaration: covers is empty"    ""                          "$(ss header covers "$R/spira/test-bare.sh")"
is "no declaration: tier is empty"      ""                          "$(ss header tier "$R/spira/test-bare.sh")"
is "missing file: covers is empty"      ""                          "$(ss header covers "$R/spira/nope.sh")"
suite test-sp.sh '# tier: T1' '# requires: testenv podman'
is "requires: space separated"          "testenv podman"            "$(ss header requires "$R/spira/test-sp.sh")"

# ---- selection fixtures ---------------------------------------------------
rm -f "$R/spira/test-h.sh" "$R/spira/test-sp.sh" "$R/spira/test-bare.sh"
suite test-a.sh '# tier: T1' '# covers: spira/alpha.sh'
suite test-s.sh '# tier: T1' '# covers: spira/alpha.sh' '# selects-on: added,mode'
suite test-fa.sh '# tier: T1' '# covers: spira/funcs.sh#fa'
suite test-fb.sh '# tier: T1' '# covers: spira/funcs.sh#fb'
suite test-funcs.sh '# tier: T1' '# covers: spira/funcs.sh'
suite test-n.sh '# tier: T1'
suite test-b.sh '# tier: T1' '# covers: spira/beta.sh'
printf 'echo a\n' > "$R/spira/alpha.sh"
printf 'echo b\n' > "$R/spira/beta.sh"
printf 'echo g\n' > "$R/spira/gamma.sh"
printf 'notes\n' > "$R/notes.md"
printf 'fa() {\n    echo a\n}\nfb() {\n    echo b\n}\ntop=1\n' > "$R/spira/funcs.sh"
commit base
files() { printf '%b' "$1" > "$TMP/files"; sel --files "$TMP/files" "${@:2}"; }

# ---- UC-03: selection over a changed-file set -----------------------------
is "covered file: its suite plus the always-run suite" "test-a.sh test-n.sh" "$(files 'spira/alpha.sh\n' --report-file "$TMP/r" | flat)"
is "selects-on suite stays out of a plain edit"        0 "$(files 'spira/alpha.sh\n' | grep -c '^test-s.sh$')"
is "added file fires the selects-on suite"             1 "$(files 'A\tspira/alpha.sh\n' | grep -c '^test-s.sh$')"
is "empty diff: only the always-run suite"             "test-n.sh" "$(files '' | flat)"
is "inert file: only the always-run suite"             "test-n.sh" "$(files 'notes.md\n' | flat)"
files 'Makefile\n' --mode-file "$TMP/m" >/dev/null
is "unmapped file: mode is all"                        "all" "$(cat "$TMP/m")"
is "unmapped file: every suite selected"               "$(ls "$R/spira" | grep -c '^test-.*\.sh$')" "$(files 'Makefile\n' | grep -c '^test-')"
files 'Makefile\n' --no-all-fallback --mode-file "$TMP/m" >/dev/null
is "--no-all-fallback: mode stays diff"                "diff" "$(cat "$TMP/m")"
is "--no-all-fallback: only the always-run suite"      "test-n.sh" "$(files 'Makefile\n' --no-all-fallback | flat)"
is "--no-nocov drops the always-run suite"             "test-a.sh" "$(files 'spira/alpha.sh\n' --no-nocov | flat)"

printf 'echo A\n' > "$R/spira/alpha.sh"
commit edit-alpha
is "--base/--head matches --files on a plain edit"     "$(files 'spira/alpha.sh\n' | flat)" "$(sel --base HEAD~1 --head HEAD | flat)"
chmod +x "$R/spira/alpha.sh"
commit mode-alpha
want "a mode change fires the selects-on suite"        "test-s.sh" "$(sel --base HEAD~1 --head HEAD)"

# ---- UC-05: unclaimed source file -----------------------------------------
printf 'spira/gamma.sh\n' > "$TMP/files"
out="$(ss select --repo "$R" --suite-dir "$R/spira" --files "$TMP/files" --report-file "$TMP/r" 2>&1)"; rc=$?
wantrc "unclaimed source file: selection fails"        1 "$rc"
want   "it names the file"                             "spira/gamma.sh" "$out"
want   "report lists the unclaimed file"               "unclaimed:spira/gamma.sh" "$(cat "$TMP/r")"
printf 'Makefile\n' > "$TMP/files"
ss select --repo "$R" --suite-dir "$R/spira" --files "$TMP/files" --report-file "$TMP/r" >/dev/null 2>&1
want   "report lists an unplaced file"                 "unplaced:Makefile" "$(cat "$TMP/r")"
printf 'spira/beta.sh\n' > "$TMP/files"
ss select --repo "$R" --suite-dir "$R/spira" --files "$TMP/files" >/dev/null 2>&1; wantrc "a claimed source file selects cleanly" 0 "$?"

# ---- UC-04: file#func narrowing -------------------------------------------
sed -i 's/echo a/echo A/' "$R/spira/funcs.sh"
commit edit-fa
got="$(sel --base HEAD~1 --head HEAD --no-all-fallback | flat)"
is "hunk inside fa: fa and whole-file suites, not fb" "test-fa.sh test-funcs.sh test-n.sh" "$(printf '%s\n' $got | sort | flat)"
sed -i 's/top=1/top=2/' "$R/spira/funcs.sh"
commit edit-global
got="$(sel --base HEAD~1 --head HEAD --no-all-fallback | sort | flat)"
is "global hunk: every suite naming the file" "test-fa.sh test-fb.sh test-funcs.sh test-n.sh" "$got"
printf 'spira/funcs.sh\n' > "$TMP/files"
is "--files mode is conservative" "test-fa.sh test-fb.sh test-funcs.sh test-n.sh" "$(sel --files "$TMP/files" --no-all-fallback | sort | flat)"

# ---- UC-06: a multi-commit branch diff selects the union of its members ----
"${GIT[@]}" tag before-branch
printf 'echo B\n' > "$R/spira/beta.sh"; commit member-1
printf 'echo AA\n' > "$R/spira/alpha.sh"; commit member-2
got="$(sel --base before-branch --head HEAD --no-all-fallback | sort | flat)"
is "branch diff: union of both members' suites" "test-a.sh test-b.sh test-n.sh" "$got"
printf 'echo GG\n' > "$R/spira/gamma.sh"; commit member-3-unmapped
out="$(ss select --repo "$R" --suite-dir "$R/spira" --base before-branch --head HEAD 2>&1)"; rc=$?
wantrc "an unclaimed member refuses the branch diff" 1 "$rc"
want   "it names the unclaimed member" "spira/gamma.sh" "$out"
printf 'echo mk\n' > "$R/Makefile"; "${GIT[@]}" rm -q -f spira/gamma.sh; commit member-4-plumbing
sel --base before-branch --head HEAD --mode-file "$TMP/m" >/dev/null
is "an unmapped member falls back to all" "all" "$(cat "$TMP/m")"

tl_summary
