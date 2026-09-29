#!/usr/bin/env bash
#
# config-fence.sh — nothing outside spira-config/ names spira.toml or repo-map, parses TOML
# config, or writes the config path.
#
#   config-fence.sh                 scan the tree; exit 1 naming each offender
#   config-fence.sh --scan FILE     scan one file; print the violation kind(s), one per line
#   config-fence.sh --allow         print the grandfather list and exit
#
# THE PROPERTY. spira.toml is a multi-tenant config store — global keys, per-repo gate
# commands, persona models — and repo-map duplicates part of it by hand. spira-config (the
# crate and its CLI) is the only thing that may find, parse or write either; everything else
# earns its way past this fence by calling spira-config, not by opening the file itself.
#
# THREE WAYS TO VIOLATE, checked independently per file:
#   name    the literal string "spira.toml" or "repo-map" appears anywhere in it, comments
#           included — that is where every occurrence this fence exists for was hiding
#   parse   a raw TOML/legacy parser runs on it: Rust toml::from_str / toml_edit::, or Python
#           tomllib — counted only in a file that also NAMEs the config, since either library
#           used on an unrelated document is not this defect
#   write   a shell redirection or sed -i targeting SPIRA_TOML/SPIRA_REPO_MAP, or a Rust
#           fs::write/File::create whose target expression contains "toml"
#
# THE ALLOWLIST (config-fence-allow) IS SHRINK-ONLY. spira.toml and repo-map are read
# directly by conf.sh and named throughout the harness scripts, tests and crates this bead's
# own description exists to fix (sp-a8gna, epic sp-zs04v); the cutover that deletes each
# reference is separate, not-yet-landed work. A path leaves this list when the bead that
# migrates it lands; nothing is added for a newly written file, which is exactly the growth
# this fence exists to refuse.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"
ALLOW="${SPIRA_CONFIG_FENCE_ALLOW:-$HERE/config-fence-allow}"

NAME_PAT='spira\.toml|repo-map'
PARSE_PAT='toml::from_str|toml_edit::|(^|[^A-Za-z0-9_.])tomllib([^A-Za-z0-9_]|$)'
WRITE_PAT='(>{1,2}[[:space:]]*"?\$\{?SPIRA_(TOML|REPO_MAP)|sed[[:space:]]+-i[^|]*SPIRA_(TOML|REPO_MAP)|fs::write\([^)]*[Tt][Oo][Mm][Ll]|File::create\([^)]*[Tt][Oo][Mm][Ll])'

# scan <file> -> the violation kind(s) it carries, one per line ("name"/"parse"/"write");
# exit 0 either way — the caller decides what a hit means.
scan() {
    local f="$1" named=0
    [ -f "$f" ] || return 0
    if grep -qE "$NAME_PAT" "$f" 2>/dev/null; then
        named=1
        printf 'name\n'
    fi
    [ "$named" = 1 ] && grep -qE "$PARSE_PAT" "$f" 2>/dev/null && printf 'parse\n'
    grep -qE "$WRITE_PAT" "$f" 2>/dev/null && printf 'write\n'
}

case "${1:-}" in
--scan)  scan "${2:?--scan needs a file}"; exit 0 ;;
--allow) grep -vE '^[[:space:]]*(#|$)' "$ALLOW" 2>/dev/null; exit 0 ;;
esac

git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || {
    printf 'config-fence: %s is not a git repository — nothing to scan\n' "$ROOT" >&2; exit 3; }

# THE SCOPE: harness scripts, the gate-suites list, chamber personas (executable prose, per
# CLAUDE.md) and every crate's Rust sources — everything plausibly able to touch config,
# outside spira-config/ itself. Tracked plus untracked-but-not-ignored, same as
# inventory.sh, so a violation in a file not yet committed is still a violation.
mapfile -t files < <(
    { git -C "$ROOT" ls-files -- 'spira/*.sh' 'spira/gate-suites' 'spira/chamber/*.fayth' 'spira/chamber/*.md' '*.rs'
      git -C "$ROOT" ls-files --others --exclude-standard -- 'spira/*.sh' 'spira/gate-suites' 'spira/chamber/*.fayth' 'spira/chamber/*.md' '*.rs'
    } | grep -v '^spira-config/' | sort -u
)
[ "${#files[@]}" -gt 0 ] || {
    printf 'config-fence: no files in scope — refusing to report clean\n' >&2; exit 3; }

mapfile -t allow < <(grep -vE '^[[:space:]]*(#|$)' "$ALLOW" 2>/dev/null)
declare -A allowed
for a in "${allow[@]:-}"; do [ -n "$a" ] && allowed["$a"]=1; done

bad=0
_offenders=()
for f in "${files[@]}"; do
    case "$f" in
        # THIS FENCE'S OWN SOURCE AND DATA — the docstring above and the pattern constants
        # name the very strings the fence hunts for.
        */config-fence.sh|config-fence.sh|*/config-fence-allow|config-fence-allow) continue ;;
        # THIS FENCE'S OWN TEST — the planted offenders are content under test.
        */test-config-fence.sh|test-config-fence.sh) continue ;;
    esac
    [ -n "${allowed[$f]:-}" ] && continue
    hits="$(scan "$ROOT/$f")"
    [ -n "$hits" ] || continue
    bad=1
    kinds="$(printf '%s' "$hits" | paste -sd, -)"
    _offenders+=("$f: $kinds")
done

if [ "$bad" = 0 ]; then
    printf 'config-fence: clean — %d file(s) checked, %d grandfathered\n' "${#files[@]}" "${#allow[@]}"
    exit 0
fi
cat >&2 <<'WHY'

REFUSED by config-fence.sh — the files below name spira.toml or repo-map, parse TOML config
directly, or write the config path. Only spira-config (the crate and its CLI) may do any of
these; call it instead of opening the file yourself.

A file caught mid-cutover to spira-config belongs in spira/config-fence-allow, not fixed by
rewriting the reference to dodge this scan — that list is shrink-only, so adding to it for a
newly written file defeats the reason it exists.
WHY
printf '%s\n' "${_offenders[@]}" >&2
exit 1
