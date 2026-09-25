#!/usr/bin/env bash
#
# exclude.sh — keep the beads database and its exports out of the harness repository.
#
#   exclude.sh check   [root]   audit a checkout; exit 1 if it carries beads data
#   exclude.sh staged  [root]   the pre-commit entry point — audits what is staged
#   exclude.sh filter           stdin: repo-relative paths -> stdout: the forbidden ones
#   exclude.sh scope   [root]   print the harness scope, one prefix per line
#   exclude.sh harness [root]   print the harness DIRECTORY itself, one per line
#   exclude.sh harness-in       stdin: repo-relative paths -> the harness directories in them
#   exclude.sh install [root]   write the .gitignore stanza and point core.hooksPath here
#
# WHY THIS EXISTS AS A PROGRAM AND NOT A PARAGRAPH.
# -------------------------------------------------
# A beads database is never public (law-beads-is-never-public). It accumulates internal
# working notes, agent memories and the overseer's own judgement, none of which are code,
# and the harness repository is the one thing here that is meant to be shared. "Remember
# not to commit the database" is a resolution; a program that exits non-zero is a
# mechanism, and this is the one place where the cost of forgetting is a public database —
# one that cannot be un-published by deleting the commit.
#
# TWO ASSERTIONS, AND THEY ARE NOT THE SAME ONE TWICE.
#
#   A. SEPARATION — no `.beads/` or `.dolt/` exists on disk inside the harness tree.
#      This deliberately IGNORES .gitignore, because it is a claim about what the checkout
#      IS, not about what git would carry. The harness checkout must not also be the beads
#      project directory, and on at least one installation here it is — whose tracked
#      files are precisely what must not ship: .beads/config.yaml, metadata.json, the
#      Dolt hooks.
#      Ignoring `.beads/` would silence assertion B while leaving that identity intact,
#      which is how a fence reports all-clear on the state it was built to refuse.
#
#   B. EXCLUSION — no path git would carry matches a beads-data pattern. Measured with
#      `git ls-files --cached --others --exclude-standard`, so the question asked is
#      exactly "could this be committed?" — gitignored runtime junk is not an offence, and
#      a staged file is one whether or not it has landed yet.
#
# THE SCOPE IS DERIVED, NEVER LISTED. The harness tree is found by its own signature — a
# directory holding `boundary`, `gate.sh` and `lib.sh` together. When that directory is the
# repository root the scope is the whole repository, which is the state this bead is
# building towards; while the harness still sits inside the wiki at .claude/spira/ the
# scope is that directory alone. So the same check is correct on both sides of the move
# with no step to remember at the moment of moving — and it stays correct for brain, which
# tracks raw/spira-beads/spira.jsonl and eleven other .jsonl corpora ON PURPOSE, into the
# only repository here that leaves the building and is private.
#
# A REPOSITORY WITH NO HARNESS IN IT IS SAID SO OUT LOUD, exit 3, never a silent pass: an
# empty scope and a clean scope are indistinguishable from the outside, and the wrong one
# reads as all-clear (law-absence-needs-a-positive-control).
#
# THIS IS FENCE TWO OF THREE, and none of the three is a wall.
#   0. .gitignore     — a stray database cannot be staged by accident
#   1. this, as a pre-commit hook — refuses the commit, and names its override
#   2. this, from gate.sh Layer 1 — refuses the LANDING, which is the fence that survives
#      `git commit --no-verify` and the sentinel's own merge
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"

# ---------------------------------------------------------------------------------------
# The forbidden shapes. Every one of them is beads data or the store beneath it.
#
# `*.jsonl` is here because `bd export` writes one, and an export of the issues is the
# database in the form that needs no tooling at all to read. That is exactly why brain
# keeps one — and exactly why the harness may not.
# ---------------------------------------------------------------------------------------
forbidden() {                       # forbidden <repo-relative-path> -> 0 if it may not ship
    local p="$1" base="${1##*/}"
    case "/$p/" in
        */.beads/*|*/.dolt/*|*/dolt/*|*/embeddeddolt/*|*/proxieddb/*) return 0 ;;
    esac
    case "$base" in
        *.jsonl|*.db|*.db-*|*.db?*|*.sqlite|*.sqlite3|*.sqlite3-*) return 0 ;;
        .beads-credential-key) return 0 ;;
    esac
    return 1
}

# ---------------------------------------------------------------------------------------
# scope — where the harness lives inside a list of repo-relative paths.
#
# Three files together, not one. `boundary` alone appears in prose, `gate.sh` alone could
# be any repository's, and `lib.sh` is the commonest filename in this tree; a directory
# holding all three is the harness and nothing else plausibly is.
#
# Prints "." when the harness IS the root — the post-move state, where everything counts.
# ---------------------------------------------------------------------------------------
# `all` prints every directory carrying the signature rather than stopping at the first.
# A repository is SUPPOSED to hold at most one harness, and the callers that ask about their
# own tree want a single answer; the caller asking whether some OTHER repository has quietly
# grown a copy wants all of them, because the second one is the finding.
scope_from_paths() {                # [all] — stdin: paths -> stdout: harness directories
    awk -v all="${1:-}" '
        { n = split($0, c, "/"); d = (n == 1 ? "." : substr($0, 1, length($0) - length(c[n]) - 1))
          seen[d "/" c[n]] = 1; dirs[d] = 1 }
        END { for (d in dirs)
                  if ((d "/boundary") in seen && (d "/gate.sh") in seen && (d "/lib.sh") in seen)
                      { print d; if (all == "") exit } }
    '
}

# widen — a harness ONE LEVEL UNDER THE ROOT means the whole repository is the harness.
# Shared by scope_of_root and filter so the two cannot drift into judging different scopes
# for the identical tree (sp-aoads).
widen() {                           # widen <dir> -> "." if one level under root, else <dir>
    case "$1" in */*|.) printf '%s\n' "$1" ;; *) printf '%s\n' "." ;; esac
}

harness_of_root() {                 # harness_of_root <root> -> the harness DIRECTORY, or exit 3
    local root="$1" d c
    d="$(git -C "$root" ls-files 2>/dev/null | scope_from_paths)"
    # A freshly `git init`-ed clone has nothing tracked yet and `ls-files` is empty, which
    # would read as "no harness here" and leave `install` refusing to arm the fence on the
    # very repository it was pointed at. So fall back to the filesystem, and look for the
    # whole signature rather than one member of it.
    if [ -z "$d" ]; then
        while IFS= read -r c; do
            c="${c#./}"; [ -n "$c" ] || c="."
            [ -f "$root/$c/boundary" ] && [ -f "$root/$c/gate.sh" ] && { d="$c"; break; }
        done < <(cd "$root" && find . -maxdepth 4 -name .git -prune -o -name lib.sh -printf '%h\n' 2>/dev/null)
    fi
    if [ -z "$d" ]; then
        echo "exclude: no harness tree in $root — nothing to guard here" >&2
        return 3
    fi
    printf '%s\n' "$d"
}

# scope — the part of the checkout these fences judge. Usually the harness directory, but a
# HARNESS ONE LEVEL UNDER THE ROOT MEANS THE WHOLE REPOSITORY IS THE HARNESS. Scoped to the
# directory in that case, a `.beads/` created at the root by running `bd` there would be
# neither ignored nor reported — and a checkout that IS a beads project directory is the
# exact state these fences exist to refuse. Buried deeper, the harness is a guest in someone
# else's tree and only its own subtree is its business.
scope_of_root() {                   # scope_of_root <root> -> the prefix, or exit 3
    local d
    d="$(harness_of_root "$1")" || return 3
    widen "$d"
}

# in_scope "." <path> is always true; otherwise the path must sit under the prefix.
in_scope() { [ "$1" = "." ] || case "$2/" in "$1"/*) return 0 ;; *) return 1 ;; esac; }

refusal() {                         # refusal <what> <override-hint> < offending paths
    {
        echo
        echo "REFUSED by exclude.sh — $1 carries beads data."
        echo
        sed 's/^/    /'
        echo
        echo "A beads database is never public. It holds internal working notes, agent"
        echo "memories and the overseer's own judgement, and the harness repository is the"
        echo "one thing here meant to be shared. A commit that publishes it cannot be"
        echo "undone by deleting the commit."
        echo
        echo "The database belongs to whoever RUNS the harness and lives in no shared"
        echo "repository. Keep the two checkouts separate; if you need the issues as text,"
        echo "export them into a private repository, never this one."
        echo
        echo "$2"
    } >&2
}

cmd="${1:-check}"; shift || true
ROOT="${1:-$(git rev-parse --show-toplevel 2>/dev/null || echo .)}"

case "$cmd" in

# ---------------------------------------------------------------------------------------
# filter — pure. Reads candidate paths, prints the forbidden ones. Scope is taken from the
# same list when it carries the harness signature, so the gate can hand it a branch's whole
# tree and get an answer with no filesystem access and no assumption about where the
# harness sits in THAT ref. With no signature in the list it filters nothing and says so,
# rather than quietly passing everything. Widened with the same rule as scope_of_root
# (sp-aoads), or a harness one level under the root would scan only its own directory here
# while check/staged/install all scan the whole tree for the identical layout.
# ---------------------------------------------------------------------------------------
filter)
    input="$(cat)"
    d="$(scope_from_paths <<< "$input")"
    if [ -z "$d" ]; then
        echo "exclude: no harness tree in the path list — nothing to guard" >&2
        exit 3
    fi
    d="$(widen "$d")"
    rc=1
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        in_scope "$d" "$p" || continue
        forbidden "$p" || continue
        printf '%s\n' "$p"; rc=0
    done <<< "$input"
    exit "$rc"
    ;;

scope)
    scope_of_root "$ROOT"
    ;;

# ---------------------------------------------------------------------------------------
# harness / harness-in — WHERE the harness is, as opposed to what these fences judge.
#
# They differ, and only in the case that matters: a harness ONE LEVEL under the root means
# the repository exists to hold it, so `scope` widens to "." while the code still lives in
# its own directory. A caller asking "does this tree contain a copy of the harness" must
# have the directory, not the widened scope, or every repository whose root it is answers
# the same as one that carries no harness at all.
#
# EXPOSED RATHER THAN RESTATED. The signature — `boundary`, `gate.sh` and `lib.sh` in one
# directory — is the only thing here that knows what a harness looks like, and a second
# program carrying its own copy of that definition is how two programs come to disagree
# about which trees are the harness. `harness-in` is the pure form: it reads a path list and
# touches no filesystem, so it can answer about a ref rather than about a checkout.
# ---------------------------------------------------------------------------------------
harness)
    harness_of_root "$ROOT"
    ;;

harness-in)
    d="$(scope_from_paths all)"
    if [ -z "$d" ]; then
        echo "exclude: no harness tree in the path list" >&2
        exit 3
    fi
    printf '%s\n' "$d"
    ;;

# ---------------------------------------------------------------------------------------
# check — the standing audit of a checkout. Both assertions.
# ---------------------------------------------------------------------------------------
check)
    d="$(scope_of_root "$ROOT")" || exit 3
    bad=0

    # A — separation. On disk, gitignore disregarded. `-name` on the directory itself, so a
    # database with a thousand files reports as the one directory it is.
    #
    # Shallow, and pruned, on purpose. The claim is about this checkout's IDENTITY — `bd`
    # puts its store at the root of the tree it is run in — and a store buried five levels
    # down is a vendored copy that assertion B already catches unless it is ignored, in
    # which case it is genuinely not shipping. An unbounded find would instead walk
    # .runtime/, whose worktrees are other repositories' and none of this check's business,
    # and a fence that false-fires is a fence that gets overridden by reflex.
    search="$ROOT"; [ "$d" = "." ] || search="$ROOT/$d"
    stores="$(find "$search" -maxdepth 3 \
        \( -name .git -o -name .runtime -o -name target -o -name node_modules \) -prune -o \
        \( -name .beads -o -name .dolt \) -print 2>/dev/null)"
    if [ -n "$stores" ]; then
        printf '%s\n' "$stores" | refusal "the harness tree at ${search}" \
            "This checkout IS a beads project directory. That is the thing to fix — the
harness checkout and the beads project directory must be separate trees. Point \`bd\` at
the database's own directory with \`bd -C <db>\`, and remove the store from here."
        bad=1
    fi

    # B — exclusion. What git would carry: tracked, plus untracked that .gitignore permits.
    offenders="$(git -C "$ROOT" ls-files --cached --others --exclude-standard 2>/dev/null \
        | while IFS= read -r p; do in_scope "$d" "$p" && forbidden "$p" && printf '%s\n' "$p"; done)"
    if [ -n "$offenders" ]; then
        printf '%s\n' "$offenders" | refusal "$ROOT" \
            "Remove them, and add the ignore stanza so it cannot happen again:
    $HERE/exclude.sh install $ROOT"
        bad=1
    fi

    [ "$bad" = 0 ] || exit 1
    echo "exclude: clean — harness scope '$d' in $ROOT carries no beads data"
    ;;

# ---------------------------------------------------------------------------------------
# staged — the pre-commit entry point.
#
# It refuses the commit and names its override, because a fence is a polite refusal and not
# a wall. The override is `git commit --no-verify`, which git already provides and which
# there is no point pretending to take away — the fence that survives it is the landing
# gate, and that one is not the committer's to bypass.
# ---------------------------------------------------------------------------------------
staged)
    d="$(scope_of_root "$ROOT")" || exit 0    # not a harness checkout; not this hook's business
    offenders="$(git -C "$ROOT" diff --cached --name-only --diff-filter=ACMR 2>/dev/null \
        | while IFS= read -r p; do in_scope "$d" "$p" && forbidden "$p" && printf '%s\n' "$p"; done)"
    [ -n "$offenders" ] || exit 0
    printf '%s\n' "$offenders" | refusal "this commit" \
        "Unstage them:  git restore --staged <path>
The landing gate runs this same check on the branch's whole tree, so \`--no-verify\`
postpones the refusal rather than clearing it."
    exit 1
    ;;

# ---------------------------------------------------------------------------------------
# install — fences 0 and 1, together, idempotently.
#
# core.hooksPath rather than a copy into .git/hooks, because .git/hooks is not cloned and a
# colleague would get the harness with its fence missing and no sign of it. A tracked hooks
# directory travels; one `git config` line arms it, and install.sh is where that line goes.
# ---------------------------------------------------------------------------------------
install)
    git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || {
        echo "exclude: $ROOT is not a git repository" >&2; exit 1; }
    d="$(scope_of_root "$ROOT")" || exit 3

    # The stanza is ANCHORED TO THE SCOPE, for the same reason the check is. A bare
    # `*.jsonl` in the wiki repository would silently ignore the next year-chunk added to
    # raw/ — the corpora there are deliberate, and a fence that quietly swallows correct
    # work is worse than none. Anchored, `install` is correct in the wiki today and in the
    # harness repository after the move, with nothing to rewrite at the moment of moving.
    pfx=""; [ "$d" = "." ] || pfx="$d/**/"
    gi="$ROOT/.gitignore"
    if ! grep -q '^# --- beads data: never in a shared repository' "$gi" 2>/dev/null; then
        { echo
          echo "# --- beads data: never in a shared repository (exclude.sh) ------------------------------"
          echo "# A beads database holds internal working notes, agent memories and the overseer's own"
          echo "# judgement. It belongs to whoever runs the harness, is never public, and is in no shared"
          echo "# repository. \`*.jsonl\` is here because \`bd export\` writes one, and an export of the"
          echo "# issues is the database in the form that needs no tooling at all to read."
          [ -n "$pfx" ] && echo "# Anchored to the harness tree, which is all this repository ships."
          for pat in .beads/ .dolt/ .beads-credential-key '*.jsonl' '*.db' '*.db-*' \
                     '*.sqlite' '*.sqlite3' '*.sqlite3-*'; do
              echo "${pfx}${pat}"
          done
        } >> "$gi"
        echo "exclude: appended the ignore stanza to $gi (scope '$d')"
    else
        echo "exclude: ignore stanza already present in $gi"
    fi

    # THE HOOK PATH FOLLOWS THE HARNESS DIRECTORY, NOT THE SCOPE. The two differ whenever the
    # repository exists to hold the harness: the scope widens to the root while the hook
    # itself still lives beside the code that ships it.
    h="$(harness_of_root "$ROOT")" || exit 3
    hooks="$h/hooks"; [ "$h" = "." ] && hooks="hooks"
    if [ -x "$ROOT/$hooks/pre-commit" ]; then
        git -C "$ROOT" config core.hooksPath "$hooks"
        echo "exclude: core.hooksPath -> $hooks"
    else
        echo "exclude: no executable hook at $ROOT/$hooks/pre-commit — hook NOT armed" >&2
        exit 1
    fi
    ;;

*)
    sed -n '3,10p' "$0" >&2
    exit 2
    ;;
esac
