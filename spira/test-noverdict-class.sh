#!/usr/bin/env bash
#
# test-noverdict-class.sh — spira_land_noverdict counts and escalates a harness-fault
#   NO_VERDICT by CLASS (repo+reason) across every branch it touches, while every other
#   NO_VERDICT reason keeps the old per-branch counter (sp-1pe0w).
#
# THE CASE THIS EXISTS FOR. Eight branches behind one dead container each accumulated their
# own three-strikes count and each filed its own ask — one fault read as eight. A harness-
# fault reason must instead share ONE counter across branches, so the class as a whole
# escalates once, naming every branch it touched — while an ordinary per-branch fault (a lock
# wait, a missing base ref) must NOT bleed across branches: three different branches each
# hitting a lock-timeout once must not escalate, only the same branch hitting it three times
# should.
#
# NO BD. ask_already_open's `bdjson list` is answered from a static empty
# SPIRA_BDJSON_FIXTURE array (lib.sh:bdq) — this suite is about the counting and the
# addressee, not about bd's own filtering, which test-cockpit-bd-contract.sh already covers
# against real bd (law-prefer-the-real-dependency).
#
# sp-31hjr: spira_land_noverdict is native in landing-pass now (was lib.sh) — driven
# through `landing-pass noverdict <id> <branch> <repo> <reason> <outcome>` (gate output on
# stdin), the same real-sender contract the bash function had, against the real compiled
# binary rather than a sourced function.
#
# tier: T1
# covers: landing-pass/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v landing-pass >/dev/null 2>&1 || bail "landing-pass is not on PATH"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
SH="$TMP/spira"
mkdir -p "$SH" "$TMP/run"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
: > "$SH/repo-map"

MAIL_LOG="$TMP/mail-log"; : > "$MAIL_LOG"
cat > "$SH/mail" <<MAILEOF
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
shift
subj=""
while [ "\$#" -gt 0 ]; do
    case "\$1" in --subject) subj="\$2"; shift 2 ;; *) shift ;; esac
done
printf '%s\n' "\$subj" >> "$MAIL_LOG"
MAILEOF
chmod +x "$SH/mail"

BD_FIXTURE="$TMP/bd.json"
printf '[]\n' > "$BD_FIXTURE"

# The fixture home first on PATH: the native port calls `mail` by name too (sp-gypjk).
export PATH="$SH:$PATH"
export SPIRA_HOME="$SH" SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/db.json" SPIRA_REPO_MAP="$SH/repo-map" \
       SPIRA_BDJSON_FIXTURE="$BD_FIXTURE" BEADS_NO_AUTO_IMPORT=1

noverdict() {   # noverdict <id> <branch> <repo> <reason> <outcome> <gate-output>
    printf '%s' "$6" | landing-pass noverdict "$1" "$2" "$3" "$4" "$5"
}
sent_count() { wc -l < "$MAIL_LOG" | tr -d ' '; }
last_subject() { tail -1 "$MAIL_LOG" 2>/dev/null; }

echo "test-noverdict-class.sh — a harness fault escalates by class, not by branch"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. Below the threshold, nothing fires — proves the mail path is reachable
# at all rather than the absence below being a broken harness (law-absence-needs-a-positive-
# control).
# --------------------------------------------------------------------------------------
noverdict sp-b1 spira/b1 fixture-repo harness-fault NO_VERDICT "batch: harness fault — container died mid-batch (ExitCode=137 OOMKilled=false)"
noverdict sp-b2 spira/b2 fixture-repo harness-fault NO_VERDICT "batch: harness fault — container died mid-batch (ExitCode=137 OOMKilled=false)"
is "two harness-fault occurrences on two branches do not yet escalate" 0 "$(sent_count)"

# --------------------------------------------------------------------------------------
# THE CLASS ESCALATES ON THE THIRD OCCURRENCE, WHICHEVER BRANCH IT LANDS ON — a third,
# DIFFERENT branch tips it over, proving the counter is shared rather than reset per branch.
# --------------------------------------------------------------------------------------
noverdict sp-b3 spira/b3 fixture-repo harness-fault NO_VERDICT "batch: harness fault — container died mid-batch (ExitCode=137 OOMKilled=false)"
is "the third occurrence, on a third branch, escalates exactly once" 1 "$(sent_count)"
want "the ask names the repository and the reason" "fixture-repo cannot be judged" "$(last_subject)"
want "and every branch the fault touched"          "spira/b1"                     "$(last_subject)"
want "  including the second"                      "spira/b2"                     "$(last_subject)"
want "  including the third"                        "spira/b3"                    "$(last_subject)"

# --------------------------------------------------------------------------------------
# A FOURTH OCCURRENCE ON THE SAME DAY DOES NOT RE-ASK. The class is deduped exactly like a
# per-branch fault already was.
# --------------------------------------------------------------------------------------
noverdict sp-b4 spira/b4 fixture-repo harness-fault NO_VERDICT "batch: harness fault — container died mid-batch (ExitCode=137 OOMKilled=false)"
is "a fourth occurrence the same day does not re-ask" 1 "$(sent_count)"

# --------------------------------------------------------------------------------------
# EVERY OTHER NO_VERDICT REASON STAYS PER-BRANCH. Three DIFFERENT branches each hitting a
# lock-timeout once must not escalate — the negative control that proves class-keying is
# scoped to harness-fault and did not quietly swallow every reason.
# --------------------------------------------------------------------------------------
: > "$MAIL_LOG"
noverdict sp-c1 spira/c1 fixture-repo lock-timeout NO_VERDICT "gate: lock held"
noverdict sp-c2 spira/c2 fixture-repo lock-timeout NO_VERDICT "gate: lock held"
noverdict sp-c3 spira/c3 fixture-repo lock-timeout NO_VERDICT "gate: lock held"
is "three different branches each hitting lock-timeout once do not escalate" 0 "$(sent_count)"

# THE SAME BRANCH, HIT THREE TIMES, STILL ESCALATES — the per-branch path is intact, not
# merely disabled alongside the class path above.
noverdict sp-c1 spira/c1 fixture-repo lock-timeout NO_VERDICT "gate: lock held"
noverdict sp-c1 spira/c1 fixture-repo lock-timeout NO_VERDICT "gate: lock held"
is "one branch hitting lock-timeout three times still escalates" 1 "$(sent_count)"
want "and names only that branch" "spira/c1 cannot be judged" "$(last_subject)"

# --------------------------------------------------------------------------------------
# A STALE CLASS WINDOW ESCALATES AGAIN. An .asked marker older than
# SPIRA_NOVERDICT_CLASS_WINDOW is a fault that already got its ask and went away — a
# recurrence on a later day is new information, not the same incident.
# --------------------------------------------------------------------------------------
: > "$MAIL_LOG"
_asked="$SPIRA_RUN/noverdict/fixture-repo-harness-fault.asked"
[ -e "$_asked" ] || bad "positive control: the harness-fault .asked marker exists" "not found at $_asked"
touch -d '2 days ago' "$_asked"
noverdict sp-b5 spira/b5 fixture-repo harness-fault NO_VERDICT "batch: harness fault — container died mid-batch (ExitCode=1 OOMKilled=true)"
noverdict sp-b6 spira/b6 fixture-repo harness-fault NO_VERDICT "batch: harness fault — container died mid-batch (ExitCode=1 OOMKilled=true)"
noverdict sp-b7 spira/b7 fixture-repo harness-fault NO_VERDICT "batch: harness fault — container died mid-batch (ExitCode=1 OOMKilled=true)"
is "a fault recurring after the window resets escalates again" 1 "$(sent_count)"
nowant "and the new ask does not carry yesterday's branches" "spira/b1" "$(last_subject)"
want "and names the new branches instead" "spira/b5" "$(last_subject)"

tl_summary
