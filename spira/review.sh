#!/usr/bin/env bash
#
# review.sh — adversarial code review of a release unit's aggregate diff.
#
#   review.sh <tag>          run the adversarial review (default command)
#   review.sh run <tag>      same, explicit
#   review.sh verdict <tag>  print the stored verdict: ship, block, or none
#
# A release unit is an annotated tag produced by release.sh cut. The reviewer:
#   1. computes the aggregate diff from the previous unit to this one
#   2. calls SPIRA_REVIEWER_MODEL with an adversarial brief
#   3. parses the verdict (SHIP or BLOCK) and each finding
#   4. files each BLOCK-level finding as a bead naming the unit's tag
#   5. writes the verdict and cost to SPIRA_REVIEWER_VERDICTS/<tag>.verdict
#   6. exits 0 (ship) or 1 (block)
#
# IT IS ADVERSARIAL BY BRIEF, not by tone. Its job is to find what the gate
# cannot: intent violations, cross-commit interactions, and irreversible changes
# disguised as routine ones.
#
# VERDICT STORAGE. After a run the verdict is written to a file in
# SPIRA_REVIEWER_VERDICTS (default: $SPIRA_RUN/review-verdicts). promote.sh
# reads this to refuse a blocked unit. The file carries the verdict, cost,
# token counts, finding count and timestamp, so the reviewer's cost per unit
# is readable without re-parsing the trace.
#
# MODEL. SPIRA_REVIEWER_MODEL defaults to claude-fable-5-1. An id the CLI
# rejects does not fail loudly — the process exits non-zero, review.sh
# exits 2, and promote.sh refuses to promote the unreviewed unit. Verify the
# id on this box before deploying (the bead description explains why).
#
# COST. After each run, cost is written to SPIRA_REVIEWER_VERDICTS/<tag>.verdict
# as cost_usd, in_tok, and out_tok so the reviewer's per-unit cost is a
# queryable number, not a text field to parse.
#
# EXIT   0  ship — unit is clean, no blocking findings
#        1  block — blocking findings filed as beads; do not promote this unit
#        2  usage or fatal error (unit NOT reviewed; treat as unreviewed)
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

# ---------------------------------------------------------------------------
# CONFIGURATION — every value comes from conf.sh, with a local fallback so
# a colleague who has not set the key yet still gets sensible behaviour.
# ---------------------------------------------------------------------------
VERDICTS="${SPIRA_REVIEWER_VERDICTS:-$SPIRA_RUN/review-verdicts}"
MODEL="${SPIRA_REVIEWER_MODEL:-claude-fable-5-1}"
REVIEW_LABEL="$SPIRA_REVIEW_LABEL"
CLAUDE="${SPIRA_AGENT:-claude}"
# How many bytes of diff the reviewer reads. A diff larger than this is
# truncated with a note. 80 000 bytes fits the real diffs this harness
# produces and sits comfortably inside a context window.
DIFF_LIMIT="${SPIRA_REVIEWER_DIFF_LIMIT:-80000}"

# ---------------------------------------------------------------------------

_verdict_file() { printf '%s/%s.verdict' "$VERDICTS" "$1"; }

# ---- verdict <tag> — print the stored verdict (ship|block|none) ------------
do_verdict() {
    local tag="${1:-}"
    [ -n "$tag" ] || { printf 'usage: review.sh verdict <tag>\n' >&2; exit 2; }
    local vf; vf="$(_verdict_file "$tag")"
    if [ ! -f "$vf" ]; then
        printf 'none\n'
        return 0
    fi
    grep '^verdict: ' "$vf" | sed 's/^verdict: //'
}

# ---- run <tag> — adversarial review ----------------------------------------
do_run() {
    local tag="${1:-}"
    [ -n "$tag" ] || { printf 'usage: review.sh run <tag>\n' >&2; exit 2; }

    # Find which managed repository holds this tag.
    local repo="" name="" n
    for n in $(spira_repos); do
        local r; r="$(repo_root "$n" 2>/dev/null)" || continue
        if git -C "$r" rev-parse --verify "refs/tags/$tag" >/dev/null 2>&1; then
            repo="$r"; name="$n"; break
        fi
    done
    [ -n "$repo" ] || {
        printf 'review: tag %s not found in any managed repository\n' "$tag" >&2
        exit 2
    }

    # Read the release tag message (contains bead: and prev: lines).
    local msg
    msg="$(git -C "$repo" tag -l --format='%(contents)' "$tag" 2>/dev/null)"
    [ -n "$msg" ] || {
        printf 'review: %s has no embedded bead list (is this a release tag?)\n' "$tag" >&2
        exit 2
    }

    local prev_tag bead_ids
    prev_tag="$(printf '%s\n' "$msg" | grep '^prev: ' | head -1 | sed 's/^prev: //')"
    bead_ids="$(printf '%s\n' "$msg" | grep '^bead: ' | sed 's/^bead: //')"

    # Resolve the commit this tag points to.
    local tag_sha
    tag_sha="$(git -C "$repo" rev-parse "${tag}^{commit}" 2>/dev/null)" || {
        printf 'review: cannot resolve commit for %s\n' "$tag" >&2; exit 2
    }

    # Compute the aggregate diff. The range is from the previous unit's commit
    # (exclusive) to this unit's commit (inclusive). With no previous tag the
    # diff is the full unit against the empty tree.
    local diff_text=""
    if [ -n "$prev_tag" ] && [ "$prev_tag" != "(none)" ] \
       && git -C "$repo" rev-parse --verify "refs/tags/$prev_tag" >/dev/null 2>&1; then
        local prev_sha
        prev_sha="$(git -C "$repo" rev-parse "${prev_tag}^{commit}" 2>/dev/null)"
        diff_text="$(git -C "$repo" diff "${prev_sha}..${tag_sha}" 2>/dev/null)" || diff_text=""
    else
        # No previous tag: diff against the empty tree — the well-known constant SHA.
        # Computing it avoids a dependency on the exact git version.
        local empty_tree
        empty_tree="$(git hash-object -t tree /dev/null 2>/dev/null \
            || printf '4b825dc642cb6eb9a060e54bf8d69288fbee4904')"
        diff_text="$(git -C "$repo" diff "${empty_tree}..${tag_sha}" 2>/dev/null)" || diff_text=""
    fi

    if [ -z "$diff_text" ]; then
        log "review: $tag has an empty diff — marking ship"
        _write_verdict "$tag" "ship" "?" "0" "0" "0" "empty diff"
        printf 'ship\n'
        exit 0
    fi

    # Build the review prompt. The diff is truncated at DIFF_LIMIT bytes to
    # avoid context overflow; the model is told when truncation occurred.
    local diff_head truncated_note=""
    diff_head="$(printf '%s' "$diff_text" | head -c "$DIFF_LIMIT")"
    [ "${#diff_text}" -gt "$DIFF_LIMIT" ] && truncated_note=" (truncated at ${DIFF_LIMIT} bytes)"

    # Write the prompt to a temp file so arbitrary diff content (%, \n, etc.)
    # passes through without shell interpretation.
    local prompt_file trace
    prompt_file="$(mktemp)"
    trace="$(mktemp)"
    trap 'rm -f "$prompt_file" "$trace"' EXIT INT TERM
    _write_prompt "$tag" "$name" "$bead_ids" "$diff_head" "$truncated_note" > "$prompt_file"

    log "review: adversarial review of $tag (model=$MODEL)"

    # INJECTABLE for the same reason as the archivist: conf.sh replaces PATH,
    # so a PATH shim cannot substitute. A test that puts a fake first on PATH
    # and does not set SPIRA_AGENT would run the real model at cost.
    timeout "${SPIRA_REVIEWER_TIMEOUT:-300}" \
        "${CLAUDE}" -p --output-format stream-json --verbose \
            --model "$MODEL" \
        < "$prompt_file" > "$trace" 2>&1
    local rc=$?

    rm -f "$prompt_file"

    if [ "$rc" -ne 0 ]; then
        rm -f "$trace"
        printf 'review: %s exited %d reviewing %s — unit NOT reviewed\n' "$CLAUDE" "$rc" "$tag" >&2
        exit 2
    fi

    # Extract the result text and cost fields from the stream-json trace.
    local result cost_usd in_tok out_tok
    result="$(_extract_result "$trace")"
    cost_usd="$(_extract_cost "$trace" cost_usd)"
    in_tok="$(_extract_cost "$trace" in_tok)"
    out_tok="$(_extract_cost "$trace" out_tok)"
    rm -f "$trace"

    # Parse the VERDICT line. An absent or unreadable VERDICT is treated as
    # BLOCK: an unreadable response is not evidence of safety.
    local verdict
    verdict="$(printf '%s\n' "$result" \
        | grep -oE '^VERDICT: (SHIP|BLOCK)' | head -1 \
        | sed 's/^VERDICT: //' | tr '[:upper:]' '[:lower:]')"
    [ -n "$verdict" ] || verdict="block"

    # File findings as beads (only for a BLOCK verdict).
    local findings_count=0
    if [ "$verdict" = "block" ]; then
        findings_count="$(_file_findings "$result" "$tag" "$name")"
    fi

    _write_verdict "$tag" "$verdict" "$cost_usd" "$in_tok" "$out_tok" "$findings_count" ""

    log "review: $tag verdict=$verdict findings=$findings_count cost_usd=$cost_usd in_tok=$in_tok out_tok=$out_tok"
    printf '%s\n' "$verdict"

    [ "$verdict" = "ship" ] && exit 0 || exit 1
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# _write_verdict <tag> <verdict> <cost_usd> <in_tok> <out_tok> <findings> [note]
# Writes atomically (write-beside, mv) so an interrupted write leaves no
# partial verdict file that would be misread as a completed review.
_write_verdict() {
    local tag="$1" verdict="$2" cost="$3" in="$4" out="$5" nf="$6" note="${7:-}"
    mkdir -p "$VERDICTS"
    local tmp; tmp="$(mktemp "$VERDICTS/.tmp.XXXXXX")"
    {
        printf 'tag: %s\n'       "$tag"
        printf 'verdict: %s\n'   "$verdict"
        printf 'model: %s\n'     "$MODEL"
        printf 'timestamp: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'cost_usd: %s\n'  "$cost"
        printf 'in_tok: %s\n'    "$in"
        printf 'out_tok: %s\n'   "$out"
        printf 'findings: %s\n'  "$nf"
        [ -n "$note" ] && printf 'note: %s\n' "$note"
    } > "$tmp"
    mv "$tmp" "$(_verdict_file "$tag")"
}

# _write_prompt <tag> <name> <bead_ids> <diff_head> <truncated_note>
# Writes the adversarial review prompt to stdout. printf '%s\n' is used
# throughout so diff content (%, backslash, etc.) passes through literally.
_write_prompt() {
    local tag="$1" name="$2" bead_ids="$3" diff_head="$4" truncated_note="$5"
    # THE PROMPT IS ADVERSARIAL BY BRIEF, not by tone. The discriminators are
    # the three things per-change review is worst at: intent, interaction, and
    # irreversibility. Style and coverage are explicitly excluded so the model
    # does not produce noise that trains the operator to ignore the output.
    printf '%s\n' \
"You are an adversarial code reviewer for a software release unit. Your job is
to find what the automated gate cannot: intent violations, cross-commit
interactions, and irreversible changes disguised as routine ones.

This is NOT a style review. Only report DEFECTS of these kinds:
  1. A change whose effect contradicts the stated intent of the beads it contains
     (a bead that says \"detect X\" but the code does Y)
  2. A cross-commit interaction: two changes in this unit that are each safe in
     isolation but together break an invariant
  3. An irreversible action with no rollback path (schema drops, data migrations
     that cannot be undone, safety mechanisms removed without replacement)"
    printf '\nRELEASE UNIT: %s\nREPOSITORY: %s\n' "$tag" "$name"
    printf 'BEADS IN THIS UNIT:\n%s\n' "$bead_ids"
    printf '\nAGGREGATE DIFF%s:\n```diff\n' "$truncated_note"
    printf '%s\n' "$diff_head"
    printf '%s\n' \
'```

Respond with EXACTLY one of these two formats:

If the unit is clean:
VERDICT: SHIP

If the unit has defects:
VERDICT: BLOCK
FINDING: <one-line title naming the file and the defect>
<specific description: which hunk, what the failure mode is, why a gate cannot catch it>
---
FINDING: <another title>
<description>
---

Rules:
- Only BLOCK if you found a genuine defect of one of the three kinds above.
- If unsure, SHIP — a false block stops a queue; a missed finding produces a
  bead and a fix, which is the intended remediation path.
- Every FINDING must name the specific hunk or file and state the failure mode.
- Do not report style, coverage gaps, or hypothetical issues.'
}

# _extract_result <trace-file> -> result text on stdout
# Finds the last stream-json event of type "result" and prints its result field.
_extract_result() {
    python3 - "$1" <<'PY'
import json, sys
result_text = ""
with open(sys.argv[1]) as fh:
    for line in fh:
        if '"type":"result"' not in line:
            continue
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
        except ValueError:
            continue
        if isinstance(d, dict) and d.get("type") == "result":
            result_text = d.get("result", "")
sys.stdout.write(result_text)
PY
}

# _extract_cost <trace-file> <field> -> value
# Extracts cost_usd, in_tok, or out_tok from the stream-json trace.
_extract_cost() {
    python3 - "$1" "$2" <<'PY'
import json, sys
field = sys.argv[2]
last = None
with open(sys.argv[1]) as fh:
    for line in fh:
        if '"type":"result"' not in line:
            continue
        try:
            d = json.loads(line.strip())
        except ValueError:
            continue
        if isinstance(d, dict) and d.get("type") == "result":
            last = d
d = last or {}
u = d.get("usage") or {}

def num(v):
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return "?"
    return str(int(round(v)))

def money(v):
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return "?"
    return "%.4f" % v

if field == "cost_usd":
    print(money(d.get("total_cost_usd")))
elif field == "in_tok":
    print(num(u.get("input_tokens")))
elif field == "out_tok":
    print(num(u.get("output_tokens")))
else:
    print("?")
PY
}

# _file_findings <result> <tag> <name> -> count on stdout
# Parses FINDING blocks from the reviewer's result text and files each as a bead.
_file_findings() {
    local result="$1" tag="$2" name="$3"
    local count=0 in_finding=0 title="" body=""
    # Each block: "FINDING: <title>\n<body lines>\n---\n"
    # Uses <<< so _file_one_finding runs in the current shell and can update $count.
    while IFS= read -r line; do
        case "$line" in
            FINDING:*)
                # Flush the previous finding before starting a new one.
                if [ -n "$title" ]; then
                    _file_one_finding "$tag" "$name" "$title" "$body" >/dev/null \
                        && count=$((count+1)) || true
                fi
                title="${line#FINDING: }"
                body=""; in_finding=1
                ;;
            ---)
                if [ "$in_finding" = 1 ] && [ -n "$title" ]; then
                    # Redirect stdout: the bead id returned by _file_one_finding
                    # is for internal dedup, not for the count. It must not flow
                    # into the $(...) capture that produces findings_count.
                    _file_one_finding "$tag" "$name" "$title" "$body" >/dev/null \
                        && count=$((count+1)) || true
                fi
                title=""; body=""; in_finding=0
                ;;
            *)
                [ "$in_finding" = 1 ] && body="${body}${line}
"
                ;;
        esac
    done <<< "$result"
    # Flush a trailing finding (no closing ---).
    if [ -n "$title" ]; then
        _file_one_finding "$tag" "$name" "$title" "$body" >/dev/null \
            && count=$((count+1)) || true
    fi

    printf '%d' "$count"
}

# _file_one_finding <tag> <name> <title> <body> -> bead id on stdout, 0 on success
_file_one_finding() {
    local tag="$1" name="$2" title="$3" body="$4"
    # The external ref keys on tag + an 8-char hash of the title. Re-running a
    # review for the same unit does not double-file the same finding.
    local ref; ref="review:${tag}:$(printf '%s' "$title" | md5sum | cut -c1-8)"

    # Deduplicate against existing open/in-progress beads with this ref.
    # --external-ref is not a bd list filter; fetch by label and filter in Python.
    local existing
    existing="$(bdjson list --status open,in_progress \
        --label "$REVIEW_LABEL" --limit 0 2>/dev/null \
        | python3 -c '
import json, sys
target = sys.argv[1]
try:
    d = json.load(sys.stdin)
    items = d if isinstance(d, list) else [d]
    for item in items:
        if isinstance(item, dict) and item.get("external_ref") == target:
            print(item["id"])
            break
except Exception:
    pass
' "$ref" 2>/dev/null)"
    [ -n "${existing:-}" ] && { printf '%s\n' "$existing"; return 0; }

    # Write body to a temp file (not to a shell argument) so content with
    # shell metacharacters is not interpreted (law-commit-messages-via-stdin
    # applies to --body-file as well).
    local tmpbody; tmpbody="$(mktemp)"
    printf 'Review finding in release unit %s\n\n%s\n' "$tag" "$body" > "$tmpbody"

    local id
    id="$(bdq create \
        "review finding: ${title}" \
        --type bug \
        --priority 2 \
        --labels "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL},${REVIEW_LABEL}" \
        --external-ref "$ref" \
        --body-file "$tmpbody" \
        --silent 2>/dev/null | tr -d '[:space:]')"
    rm -f "$tmpbody"
    [ -n "${id:-}" ] || return 1
    # THE REPO DIMENSION IS WRITTEN WITH SET-STATE — not embedded in --labels at creation.
    # This enforces single-valuedness at the substrate level.
    bdq set-state "$id" "repo=${name}" >/dev/null 2>&1 || true
    log "review: filed finding $id — $title"
    bdq note "$id" "review-unit: $tag" >/dev/null 2>&1 || true
    printf '%s\n' "$id"
}

# ---------------------------------------------------------------------------
CMD="${1:-}"
case "$CMD" in
    run)     shift; do_run "$@" ;;
    verdict) shift; do_verdict "$@" ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    "")      printf 'usage: review.sh <tag>\n       review.sh verdict <tag>\n' >&2; exit 2 ;;
    *)       do_run "$CMD" ;;
esac
