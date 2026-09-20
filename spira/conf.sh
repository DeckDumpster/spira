# conf.sh — the ONE configuration surface. Sourced, never executed.
#
# WHAT THIS FILE IS FOR. Every path this harness touches used to be written into the script
# that touched it: the wiki checkout, the beads database, the directory two binaries happen
# to live in, the seven repositories one operator has. A colleague cloning it has none of
# those, and the failure is not a message — it is `bd: command not found` from a systemd
# timer, into a log nobody reads. So the paths are collected here, in one place, and every
# script asks this file instead of knowing the answer.
#
# THREE SOURCES, IN THIS ORDER, AND THE FIRST ONE THAT SPEAKS WINS:
#
#   1. the ENVIRONMENT — because that is the seam every test suite already drives a fixture
#      through, and a config file that could override a test's own SPIRA_DB would point the
#      suite at the operator's real database. Environment first is not a convenience here;
#      it is what keeps the suites isolated.
#   2. the CONFIG FILE — `spira.conf`, the operator's box.
#   3. a DERIVED DEFAULT — computed from where this file sits, so a clean clone with no
#      config at all still resolves to something coherent rather than to someone else's box.
#
# WHERE THE CONFIG FILE IS LOOKED FOR, first hit wins:
#
#   $SPIRA_CONF                                  an explicit path; set it to a nonexistent
#                                                one to read no file at all
#   $SPIRA_REPO/spira.conf                       beside the checkout the harness runs from
#   ${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf
#   /etc/spira/spira.conf
#
# IT IS NOT SOURCED. A config file that is shell can set PATH, run a command, or shadow a
# function this library defines, and it is read by a process that summons agents. It is
# parsed as `KEY=value` lines, `#` comments, keys restricted to the ones named below, and
# nothing else is honoured — an unknown key is reported, not obeyed, because a typo that is
# silently ignored is a setting the operator believes is in force.

[ -n "${SPIRA_CONF_LOADED:-}" ] && return 0
SPIRA_CONF_LOADED=1

# --------------------------------------------------------------------------------------
# Every key this harness honours, with the default the CODE carries. A key absent from this
# list is refused when it appears in a config file.
#
# This list is also the allowlist: `spira_conf_read` refuses anything not named here, so a
# misspelled key is reported rather than silently ignored. The defaults themselves live in
# `spira_conf_defaults`, which runs AFTER the file is read, so a default may refer to a key
# the operator set.
# --------------------------------------------------------------------------------------
# SPIRA_HOME AND SPIRA_REPO ARE ABSENT FROM THIS LIST DELIBERATELY, and that is a fence.
# Where the harness IS is not a configuration question — it is a fact about where this file
# sits — and letting a config file answer it breaks the landing gate, which extracts a branch
# to a scratch tree and runs that tree's own suites. The config would point them back at the
# installed copy, so the gate would test the code already in force instead of the code being
# judged, and pass. The environment may still override both: that is the seam every suite
# drives a fixture through, and it is explicit rather than ambient.
SPIRA_CONF_KEYS="
SPIRA_HOME_REPO SPIRA_DB SPIRA_RUN SPIRA_GOAL
SPIRA_PATH SPIRA_WORKSPACES SPIRA_REPO_MAP SPIRA_PREFIX_MAP SPIRA_CHAMBER SPIRA_WATCHERS
SPIRA_ACTIONABLE SPIRA_ID_PREFIX SPIRA_HEALTH_TIMEOUT SPIRA_ANSWER_STATE SPIRA_ANSWER_MARK SPIRA_ANSWER_COMMENT_MARK SPIRA_SELF_CLOSED SPIRA_NOTIFY_AGE SPIRA_WAKE SPIRA_WAKE_WATCHERS
SPIRA_CLIENT_SETTINGS SPIRA_HOOK_LINES SPIRA_CTRL
SPIRA_MAIL SPIRA_MAIL_KINDS SPIRA_MAIL_READERS SPIRA_MAIL_UNREAD_AGE SPIRA_MAIL_SETTLE SPIRA_MAIL_SESSION_MAILBOX SPIRA_MAIL_REPEAT_WINDOW SPIRA_MAIL_TIDY_FRESH
SPIRA_COCKPIT SPIRA_COCKPIT_TRACE_LINES SPIRA_SNAP_STALE_S SPIRA_NOTIFY SPIRA_PANEL SPIRA_OPERATOR SPIRA_OPERATOR_ACTOR SPIRA_TZ SPIRA_ASK_LABEL SPIRA_RECLAIM_SKIP_LABEL SPIRA_OPERATED
SPIRA_CI_LABEL SPIRA_CI_PARK_MAX SPIRA_WORLD_STOP_LABEL
SPIRA_LAND_MAXSEC SPIRA_LAND_GATE_RESERVE SPIRA_VERDICT_TTL SPIRA_REBASE_ESCALATE_AT SPIRA_VERDICT_WINDOW SPIRA_REMEDY_WINDOW SPIRA_PR_STALL_MINS
SPIRA_LOOM_ADDR SPIRA_LOOM_BUDGET_MS SPIRA_LOOM_CACHE_S SPIRA_LOOM_BIN
SPIRA_BROKER_BIN SPIRA_CZAR_PASS_BIN
SPIRA_SPIKE_LABEL SPIRA_SPIKE_DIR SPIRA_SPIKE_PATHS
SPIRA_GROOMER_LABEL SPIRA_GROOM_ASK_LABEL SPIRA_SCOPE_LABEL SPIRA_PLAN_LABEL SPIRA_INCIDENT_LABEL SPIRA_CZAR_LABEL SPIRA_NO_LOOP_LABEL
SPIRA_CZAR_STAGE_DEADLOCK SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED SPIRA_CZAR_STAGE_SORT_FAILED SPIRA_CZAR_STAGE_LOOP_STALLED SPIRA_CZAR_STAGE_CI_STALLED SPIRA_CZAR_STAGE_STARVED SPIRA_CZAR_STAGE_CI_RED
SPIRA_MAECHEN_LABEL SPIRA_MAECHEN_LANDING_INTERVAL SPIRA_MAECHEN_MAX_GAP_SECONDS SPIRA_MAECHEN_MAX_BEADS SPIRA_MAECHEN_REMEDY_LABEL
COCKPIT_DB COCKPIT_BOTTOM_PCT COCKPIT_MAIL COCKPIT_RIGHT_PCT COCKPIT_MOUSE COCKPIT_CWD COCKPIT_SESSIONS COCKPIT_HOST
SPIRA_TOWN SPIRA_MIRROR SPIRA_EXPORTER SPIRA_DESIGN SPIRA_WIKI SPIRA_WIKI_HOOK SPIRA_DOLT_DATA
SPIRA_VIEW SPIRA_VIEW_SESSION
SPIRA_ALERT_GLOB
SPIRA_BD SPIRA_BD_PIN SPIRA_BD_TAG
SPIRA_FAYTHS SPIRA_MAX_AEONS SPIRA_MAX_LIVE_AEONS SPIRA_LANES SPIRA_LANES_MAX_LIVE SPIRA_QA_DEPTH SPIRA_THRASH_MINUTES
SPIRA_TOKEN_WINDOW_H SPIRA_TOKEN_PROJECTS SPIRA_CTX_WARN SPIRA_CTX_HIGH SPIRA_CTX_LIMIT
SPIRA_ARCHIVE
SPIRA_ARCHIVIST_EVERY SPIRA_ARCHIVIST_IDLE SPIRA_ARCHIVIST_MODEL SPIRA_ARCHIVIST_TIMEOUT
SPIRA_ARCHIVIST_PER_PASS
SPIRA_TESTDB_LIB SPIRA_TESTDB_BD SPIRA_TESTDB_DATA SPIRA_TESTDB_PORT
SPIRA_TESTENV_REGISTRY SPIRA_GH_INTAKE_REPO SPIRA_GH_INTAKE_PRIORITY SPIRA_GH_INTAKE_BEAD_REPO SPIRA_FLAKY_GH_REPO
SPIRA_GATE_TIMEOUT SPIRA_GATE_BUDGET SPIRA_GATE_SELECT_CAP
SPIRA_GATE_SUITES SPIRA_SUITE_STATE_FILE SPIRA_SUITES_STATE SPIRA_SUITES_BUDGET SPIRA_SUITE_TIMEOUT SPIRA_BATCH_MAXPAR
SPIRA_BATCH_ARTIFACT_DAYS SPIRA_BATCH_TAIL_LINES
SPIRA_SUITES_PRIORITY SPIRA_SUITES_STALE SPIRA_SELF_TEST SPIRA_INCIDENT_PRIORITY SPIRA_WATCHER_INTERVAL_S
SPIRA_FLAKE_QUARANTINE_AT SPIRA_FLAKE_WINDOW SPIRA_QUARANTINE_CLEAN_RUNS SPIRA_QUARANTINE_MAX_AGE
SPIRA_QUEUE_BATCH_MAX SPIRA_QUEUE_BATCH_WAIT SPIRA_QUEUE_CI_MAXSEC SPIRA_QUEUE_CI_IDLE_SEC SPIRA_CI_QUEUED_MAX_SECS SPIRA_STARVED_MAX_MINS SPIRA_LOOP_STALL_SECS SPIRA_CI_RED_MAX_SECS SPIRA_QUEUE_LOCAL_GATE SPIRA_QUEUE_INFRA_RETRIES SPIRA_QUEUE_STUCK_AGE SPIRA_QUEUE_DIR SPIRA_FORGE SPIRA_QUEUE_WAIT_LABEL SPIRA_QUEUE_ACTIONS_APP_ID
SPIRA_QUEUE_THROTTLE_DEPTH_AT SPIRA_QUEUE_THROTTLE_RELEASE_AT SPIRA_QUEUE_THROTTLE_STALL_MINS SPIRA_QUEUE_THROTTLE_OVERRIDE
SPIRA_AURON_RESTARTS SPIRA_AURON_RESTART_WINDOW
SPIRA_PROD SPIRA_INSTANCE
SPIRA_RELEASES SPIRA_RELEASES_KEEP
SPIRA_REVIEWER_MODEL SPIRA_REVIEWER_VERDICTS SPIRA_REVIEWER_TIMEOUT SPIRA_REVIEWER_DIFF_LIMIT
SPIRA_REVIEW_LABEL
SPIRA_CAPACITY_PROBE_MODEL SPIRA_CAPACITY_PROBE_INTERVAL SPIRA_CAPACITY_PROBE_WINDOW SPIRA_CAPACITY_PROBE_TIMEOUT
SPIRA_SELF_WINDOW
SPIRA_AGENT
SPIRA_STATUTE_CORE
SPIRA_GIT_NAME SPIRA_GIT_EMAIL
"

# --------------------------------------------------------------------------------------
# Where we are. SPIRA_HOME is the directory holding this file; everything else can be
# derived from it, which is what makes a clean clone runnable.
#
# BASH_SOURCE, not $0: this file is sourced, so $0 is whatever script sourced it, and
# addressing SPIRA_HOME as the caller's directory broke the moment a cockpit tool two
# directories away sourced lib.sh.
# --------------------------------------------------------------------------------------
# WHICH KEYS THE ENVIRONMENT ALREADY OWNS is recorded BEFORE anything is derived. Deriving
# first and testing "is it set?" afterwards makes every derived value indistinguishable from
# one the caller passed in, and the config file — which only fills what is unset — would then
# be silently overridden by defaults this file had just computed.
_spira_conf_env=""
for _k in $SPIRA_CONF_KEYS; do
    [ -n "${!_k+set}" ] && _spira_conf_env="$_spira_conf_env $_k"
done
unset _k

_spira_conf_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# Whether SPIRA_HOME was ANSWERED BY THE CALLER or derived is remembered, because it changes
# which repo-map wins below. An explicit SPIRA_HOME is a caller saying "this tree is the
# harness in force" — a fixture, or the gate's scratch checkout — and that tree's map is the
# one it means.
_spira_conf_home_env=""; [ -n "${SPIRA_HOME:-}" ] && _spira_conf_home_env=1
SPIRA_HOME="${SPIRA_HOME:-$_spira_conf_here}"

# The checkout the harness is installed in. `git -C ... --show-toplevel` rather than
# "two directories up", because the harness may be installed anywhere, and because a
# worktree must resolve to ITSELF — an aeon running from a worktree whose helpers resolve
# to the installed copy on main is a version skew that shows up as nothing at all.
SPIRA_REPO_DERIVED="$(git -C "$SPIRA_HOME" rev-parse --show-toplevel 2>/dev/null)" || SPIRA_REPO_DERIVED=""
# THE FALLBACK IS THE PARENT, not a fixed number of levels up. Outside a git checkout there
# is nothing to ask, and "two directories up" was an assumption about one layout that became
# silently wrong the moment the harness moved from `<repo>/.claude/spira` to `<repo>/spira` —
# resolving SPIRA_REPO to the parent of the repository, where nothing it looks for exists.
[ -n "$SPIRA_REPO_DERIVED" ] || SPIRA_REPO_DERIVED="$(cd "$SPIRA_HOME/.." 2>/dev/null && pwd -P)"
SPIRA_REPO="${SPIRA_REPO:-$SPIRA_REPO_DERIVED}"

# THE DERIVED VALUE IS KEPT, because "SPIRA_REPO was set deliberately" and "SPIRA_REPO fell
# out of where this file sits" mean different things to repo_root: the first is an override
# of the repository map, the second is not. Before it was kept, deriving a value made every
# home-repository lookup bypass the map — invisible here, where the two agree, and wrong
# everywhere else. NOT exported, for the same reason SPIRA_HOME is not: it is a fact about
# this copy of the harness, and whatever sources its own conf.sh resolves its own.


# One line, single-spaced, padded at both ends — because the membership test below is a
# `case` on " $key ", and a key that happened to sit at the end of a line in the list above
# was followed by a newline rather than a space and was refused as unknown. Six of the
# twenty-two keys, silently ignored, which is precisely the failure the allowlist exists to
# prevent happening to a typo.
SPIRA_CONF_KEYS=" $(echo $SPIRA_CONF_KEYS) "

# spira_conf_file -> the path of the config file in force, or empty
spira_conf_file() {
    local c
    if [ -n "${SPIRA_CONF+set}" ]; then
        [ -f "$SPIRA_CONF" ] && printf '%s' "$SPIRA_CONF"
        return 0
    fi
    for c in "$SPIRA_REPO/spira.conf" \
             "${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf" \
             /etc/spira/spira.conf; do
        [ -f "$c" ] && { printf '%s' "$c"; return 0; }
    done
    return 0
}

# spira_conf_read <file> — apply the file's settings to any key NOT already set in the
# environment. Prints a warning for a key it does not know and for a malformed line; it
# never fails the caller, because a harness that refuses to start over one bad line in a
# config is a harness that cannot be repaired from the box it is broken on.
spira_conf_read() {
    local file="$1" line key val n=0
    [ -f "$file" ] || return 0
    while IFS= read -r line || [ -n "$line" ]; do
        n=$((n+1))
        line="${line#"${line%%[![:space:]]*}"}"          # ltrim
        case "$line" in ''|'#'*) continue ;; esac
        case "$line" in *=*) ;; *)
            printf 'spira.conf:%s: not KEY=value, ignored: %s\n' "$n" "$line" >&2
            continue ;;
        esac
        key="${line%%=*}"; val="${line#*=}"
        key="${key%"${key##*[![:space:]]}"}"             # rtrim key
        val="${val#"${val%%[![:space:]]*}"}"             # ltrim value
        val="${val%"${val##*[![:space:]]}"}"             # rtrim value
        # Quotes are stripped so a value with a trailing space can be written; they are not
        # required, because the common case is a path.
        case "$val" in
            \"*\") val="${val#\"}"; val="${val%\"}" ;;
            \'*\') val="${val#\'}"; val="${val%\'}" ;;
        esac
        case "$SPIRA_CONF_KEYS" in
            *" $key "*) ;;
            *) printf 'spira.conf:%s: unknown key %s, ignored\n' "$n" "$key" >&2
               continue ;;
        esac
        # `~` and $HOME expand, and nothing else does. A value is not shell: `$(date)` in a
        # config file read by the process that summons agents is a command this harness
        # would run as the operator.
        case "$val" in
            '~'|'~/'*) val="$HOME${val#\~}" ;;
        esac
        val="${val//\$HOME/$HOME}"
        val="${val//\$\{HOME\}/$HOME}"
        # THE ENVIRONMENT WINS. `${!key+set}` is the only test that distinguishes "unset"
        # from "set to empty" — and set-to-empty is meaningful here, since an empty
        # SPIRA_TOWN is how an operator says they have no Gas Town.
        case " $_spira_conf_env " in *" $key "*) continue ;; esac
        printf -v "$key" '%s' "$val"
    done < "$file"
}

# _spira_join <base> <rel> — join a base path and a relative segment without doubling
# slashes. Strips any trailing slash from base before appending "/rel", so a base of "/"
# (dirname of a repo at the filesystem root) produces "/rel" rather than "//rel".
_spira_join() { local _b="${1%/}"; printf '%s/%s' "$_b" "$2"; }

# spira_conf_defaults — fill in whatever is still unset. Ordered: later defaults refer to
# earlier ones.
spira_conf_defaults() {
    # In a worktree, rev-parse --git-common-dir returns an absolute path to the
    # shared .git; dirname of that is the main repo, so basename is its identity.
    # In a plain checkout it returns a relative path — fall back to SPIRA_REPO.
    local _spira_gcd
    _spira_gcd="$(git -C "$SPIRA_REPO" rev-parse --git-common-dir 2>/dev/null)" || _spira_gcd=""
    case "$_spira_gcd" in
        /*)  : "${SPIRA_HOME_REPO:=$(basename "$(dirname "$_spira_gcd")")}" ;;
        *)   : "${SPIRA_HOME_REPO:=$(basename "$SPIRA_REPO")}" ;;
    esac
    # THE INSTANCE NAME. Two instances (e.g. 'prod' and 'test') may run side by side on
    # one machine; each reads its own database and writes its own runtime tree. 'prod' is
    # the default so every existing installation is unaffected by this key's existence.
    # An unset SPIRA_INSTANCE is identical to SPIRA_INSTANCE=prod — the suffix below
    # collapses to empty and every derived path resolves to the path it always had.
    : "${SPIRA_INSTANCE:=prod}"
    # The suffix appended to instance-specific path segments. Empty for 'prod'; the prod
    # case must produce no suffix so a clean clone with no config sees exactly the paths
    # every existing box already has — not a new layout that would break on upgrade.
    local _spira_inst_sfx=""
    [ "${SPIRA_INSTANCE}" = "prod" ] || _spira_inst_sfx="-${SPIRA_INSTANCE}"

    # The database is NOT under the checkout by default. A beads database accumulates
    # internal working notes and agent memories, so a default that puts it inside a git
    # repository is one `git add -A` away from publishing them (law-beads-is-never-public).
    # INSTANCE-QUALIFIED: each instance gets its own sidecar database so stopping or wiping
    # 'test' leaves the 'prod' store untouched. For prod the suffix is empty; the path is
    # identical to what every existing box has.
    : "${SPIRA_DB:=${XDG_DATA_HOME:-$HOME/.local/share}/spira${_spira_inst_sfx}/db}"
    # INSTANCE-QUALIFIED: each instance writes its own runtime tree — pid files, the aeon
    # ledger, the cockpit state — so a test instance cannot overwrite prod's working state.
    # For prod the suffix is empty; the path is unchanged.
    # A read-only SPIRA_REPO (release tarball) cannot grow .runtime; fall back to
    # XDG_DATA_HOME so mkdir on first use succeeds.
    if [ -z "${SPIRA_RUN:-}" ]; then
        if [ -w "$SPIRA_REPO" ]; then
            SPIRA_RUN="$SPIRA_REPO/.runtime/spira${_spira_inst_sfx}"
        else
            SPIRA_RUN="${XDG_DATA_HOME:-$HOME/.local/share}/spira${_spira_inst_sfx}/run"
        fi
    fi
    # THE OPERATIONAL CONTROL PLANE FILE. Durable state — suspensions, pauses, drains —
    # that lives outside source control and survives install.sh, pull, and reset.
    # Defaults to the gitignored runtime directory so no git operation ever touches it.
    # An operator who wants it elsewhere (e.g. truly outside the repository directory)
    # sets this key.
    : "${SPIRA_CTRL:=$SPIRA_RUN/control}"
    # MEMORIES CACHE. render_memories writes the memories JSON here on a live read so
    # subsequent calls (concierge launch, aeon summon) hit local disk. rule.sh enact/retire
    # delete it so the next render sees fresh data. Empty (not unset) disables caching,
    # which is why this key uses `=` not `:=` — an explicitly empty value is an answer.
    : "${SPIRA_MEMORIES_CACHE=$SPIRA_RUN/memories-cache.json}"
    : "${SPIRA_MEMORIES_CACHE_AGE:=300}"
    : "${SPIRA_MAIL:=$SPIRA_RUN/mail}"
    : "${SPIRA_MAIL_KINDS:=$SPIRA_HOME/mail/kinds}"
    : "${SPIRA_MAIL_READERS:=}"
    : "${SPIRA_MAIL_UNREAD_AGE:=1800}"
    : "${SPIRA_MAIL_SETTLE:=2}"
    : "${SPIRA_MAIL_SESSION_MAILBOX:=concierge}"
    : "${SPIRA_MAIL_REPEAT_WINDOW:=14400}"
    : "${SPIRA_MAIL_TIDY_FRESH:=86400}"
    : "${SPIRA_GOAL:=sp-spira}"
    : "${SPIRA_PATH:=}"
    # Git checkout: workspaces is the parent of SPIRA_REPO. Artifact deployment: SPIRA_REPO
    # is a release dir inside the releases directory, so workspaces is two levels up — one
    # level would land inside the releases dir and double SPIRA_RELEASES. dirname returns "/"
    # for a root-mounted repo; _spira_join prevents double slashes at derivation sites.
    if [ -d "$SPIRA_REPO/.git" ] || [ -f "$SPIRA_REPO/.git" ]; then
        : "${SPIRA_WORKSPACES:=$(dirname "$SPIRA_REPO")}"
    else
        : "${SPIRA_WORKSPACES:=$(dirname "$(dirname "$SPIRA_REPO")")}"
    fi
    # OPERATOR-OWNED CONFIGURATION THAT BELONGS BESIDE THE REPO-MAP, not inside the harness
    # tree. An operator whose harness lives in a repository they did not write would otherwise
    # have to keep this file in the checkout — where a push might share it — or remember to
    # set SPIRA_PREFIX_MAP on every box. ~/.config/spira is where repo-map already lives.
    : "${SPIRA_PREFIX_MAP:=${XDG_CONFIG_HOME:-$HOME/.config}/spira/prefix-map}"
    : "${SPIRA_CHAMBER:=$SPIRA_HOME/chamber}"
    # THE ONE LIST OF WHAT SHOULD BE WATCHING. One `daemon` row is one systemd unit, so this
    # file decides what `install.sh` enables; pointing the key elsewhere is how an operator
    # keeps their own rows out of a checkout they may push.
    : "${SPIRA_WATCHERS:=$SPIRA_HOME/watchers}"
    # WHICH OF A WATCHER'S LINES A READER IS SHOWN BY DEFAULT — an extended regular expression
    # matched against the whole line by `watchd.sh drain` and `watchd.sh tail`, which share it
    # so that the command a session hook advertises and the command a session latches with
    # cannot disagree about what matters. Everything else stays in the log, where `--all` and
    # the file itself still reach it.
    #
    # ACTIONABLE means: something is stuck, something broke, something needs a human choice, or
    # a thing being waited on finished. The default names the vocabulary this harness's own
    # watchers emit plus the words any watcher reaches for; an operator whose watchers speak
    # differently sets their own, which is the whole reason this is a key and not a literal —
    # a literal in two commands is how those two commands come to disagree.
    #
    # `=` AND NOT `:=`, WHICH IS THE ONE PLACE IN THIS FILE THAT DIFFERS. Every other default
    # fills an unset OR empty key, because empty means "not answered". Here empty is an answer:
    # an operator who blanks this key has edited it on purpose, and quietly substituting the
    # default would ignore the edit entirely. So the empty value survives to `watchd.sh`, which
    # refuses it and names `--all` — because as a regular expression an empty pattern matches
    # every line, and turning the filter off is a thing to ask for rather than to fall into.
    : "${SPIRA_ACTIONABLE=ANSWERED|COMMENTED|ESCALAT|STRANDED|POISON|DEGRADED|BLOCKED|UNREACHABLE|FAIL|ERROR|LANDED|⚠}"
    # THE ID PREFIX OF THIS INSTALLATION'S OWN BEADS, without the hyphen. It is what a health
    # assertion looks for to prove a watcher is reading THIS database and not one that was
    # retired underneath it.
    #
    # WHY ABSENCE AND NOT PRESENCE IS THE TEST, which is the whole reason this key exists.
    # A database here legitimately holds beads imported under other prefixes, so an assertion
    # keyed on "names a foreign prefix" passes on a watcher that is entirely blind — measured
    # once at 145 of 1825 rows carrying the local prefix. What proves the wrong database is
    # that NOT ONE local id appears. This generalises the guard the mirror exporter carries.
    #
    # Derived from the goal epic rather than written in, because the goal is a bead in this
    # database and therefore already answers the question.
    : "${SPIRA_ID_PREFIX:=${SPIRA_GOAL%%-*}}"
    # HOW LONG A HEALTH COMMAND MAY RUN, in seconds. A probe is an operator-supplied command
    # run by `watchd.sh status`, and `status` is what a session hook runs at every start — so
    # an unbounded one hangs the opening of a context window rather than merely being slow.
    # A probe that runs out of time is DEGRADED, which is the honest reading: it did not
    # prove the watcher is seeing anything.
    : "${SPIRA_HEALTH_TIMEOUT:=10}"
    # THE CODING AGENT'S OWN SETTINGS FILE — the one the client reads, not one of ours. The
    # session hook is registered in it, and it is the only file in this harness that belongs
    # to a program the harness does not ship. It is a key rather than a literal for one
    # reason above all: a suite that asserted against the real path would edit the operator's
    # live client configuration on every run.
    : "${SPIRA_CLIENT_SETTINGS:=$HOME/.claude/settings.json}"
    # HOW MANY LINES THE SESSION HOOK MAY SPEND, as a ceiling. Its output is prepended to a
    # context window that has just opened, which is the most expensive place any text in this
    # harness can go, so the budget is enforced by MEASUREMENT rather than hoped for: the
    # status table and the latch commands are printed whole, and the preview of unread events
    # is given exactly what is left over.
    #
    # The default leaves roughly half the output to the preview for a handful of watchers,
    # which is a screenful and no more — the point of a catch-up is that it costs a glance.
    # Zero means no budget at all, which is a thing to ask for rather than to fall into.
    : "${SPIRA_HOOK_LINES:=32}"
    : "${SPIRA_COCKPIT:=$(dirname "$SPIRA_HOME")/cockpit}"
    # HOW MANY TRAILING MOMENTS THE COLLECTOR RECORDS PER AEON. Each one becomes a key
    # in the snapshot and a row on the pane. 0 restores the single-line behaviour.
    #
    # TWO, NOT THREE. Every live aeon spends this many rows, so it is the figure that decides
    # how much of the column is left for everything else when three aeons are awake — and the
    # third moment was the one paying least: "what led here" is answered by the moment before
    # the current one, and the one before that is history the trace file already holds. The
    # row it gave back is what INFLOW is built on.
    : "${SPIRA_COCKPIT_TRACE_LINES:=2}"
    # Snapshot age measures merge cadence, not probe duration. The supervisor loop calls
    # _merge_fragments on every tick (~5s), so a healthy box always has a fresh snapshot.
    # 60s = 12 ticks: enough slack for a slow merge under load, short enough that a dead
    # supervisor loop is visible within a minute.
    : "${SPIRA_SNAP_STALE_S:=60}"
    # HOW LONG AN ACTIONABLE EVENT MAY WAIT WITH NO READER before it is escalated through a
    # channel that needs no session, in seconds. `watchd.sh notify` is what enforces it.
    #
    # WHY THERE IS A POLL HERE AT ALL. A session hook fires at a SESSION BOUNDARY, which is a
    # property of one client: when an answer is given while nothing is running, nothing fires
    # until the next session opens, and for a headless agent that is never. The escalation is
    # the only path that does not require a reader to exist.
    #
    # WHY IT IS THIS LONG. The window has to be comfortably longer than an ordinary gap
    # between a watcher emitting and somebody latching, because every escalation inside that
    # gap is a false alarm — and a false alarm is a real cost, not a harmless one
    # (law-alerts-must-be-actionable). It has to be far shorter than the hours a nobody-is-
    # running interval actually lasts. Half an hour is between those, and it is a key rather
    # than a literal because which one an operator's watchers deserve is theirs to say.
    : "${SPIRA_NOTIFY_AGE:=1800}"
    # The command a watcher runs to prompt the reading session; empty disables waking.
    [ -x "$SPIRA_REPO/concierge.sh" ] && : "${SPIRA_WAKE=$SPIRA_REPO/concierge.sh wake}"
    : "${SPIRA_WAKE:=}"
    # Watchers that deliver by waking the reader, so no session is told to hold a Monitor on them.
    : "${SPIRA_WAKE_WATCHERS:=answers}"
    # WHERE THE ANSWER WATCHER KEEPS WHAT IT HAS ALREADY SEEN. One key rather than two
    # literals: the watcher writes this file and its health assertion reads it, and those two
    # disagreeing is a permanent DEGRADED against a watcher that is working perfectly — a
    # false alarm, which is the expensive kind (law-alerts-must-be-actionable). Under SPIRA_RUN
    # so the harness tree stays read-only; install.sh migrates any existing file on upgrade.
    : "${SPIRA_ANSWER_STATE:=$SPIRA_RUN/answered-seen.json}"
    # THE MARKS answered-since.sh WRITES — one per answer leg, kept separate because a comment
    # does not bump the bead's updated_at and a single cursor over closes cannot track how far
    # the comment leg has read. Both live under SPIRA_RUN for the same reason as SPIRA_ANSWER_STATE.
    : "${SPIRA_ANSWER_MARK:=$SPIRA_RUN/answered-mark}"
    : "${SPIRA_ANSWER_COMMENT_MARK:=$SPIRA_RUN/answered-comment-mark}"
    # WHERE THE HARNESS RECORDS BEADS IT CLOSED ITSELF. watch-answers.sh and answered-since.sh
    # filter these ids so a harness-initiated close is not announced as an operator verdict.
    # Under SPIRA_RUN so the harness tree stays read-only; install.sh migrates any existing file.
    : "${SPIRA_SELF_CLOSED:=$SPIRA_RUN/self-closed}"
    # THE LABEL THAT MEANS "WAITING ON THE OPERATOR". It is the one the escalation gate defers
    # on, the one every persona's predicate excludes, and the one the attention panel reads,
    # so all of those must agree on it — which is why it is one key and not five literals.
    # Changing it on a live installation orphans every bead already carrying the old value.
    : "${SPIRA_ASK_LABEL:=needs-operator}"
    # THE LABEL THAT PROTECTS AN IN_PROGRESS BEAD FROM TIME-BASED RECLAIM. When an aeon
    # exits because its bead's only open dep carries the ask label, the lease goes stale
    # but the bead is not a dead worker — the aeon followed its contract. CHECK 2 applies
    # this label so the reaper skips the bead; CHECK 2 also removes it when the dep closes,
    # after which the reaper reclaims the stale lease as it normally would. The default
    # derives from the ask label so the two stay paired on a generic installation.
    : "${SPIRA_RECLAIM_SKIP_LABEL:=spira-waiting-operator}"
    # THE LABEL THAT MEANS "PARKED ON A CI RUN". An aeon puts it on a bead whose pull request
    # is open so nothing pays for a session to sit and watch a test suite; the CI sweep takes
    # it off again when the run resolves. Every predicate that decides what an aeon may claim
    # excludes it, and so does the stalled-work report — which is what stops parked work
    # looking abandoned, and is also why a park applied where no run exists is permanent AND
    # invisible. One key rather than five literals, for the same reason as the ask label: the
    # sweep, the personas, the report and the panel must agree on it or the panel is the half
    # nobody notices is wrong, because it simply shows fewer.
    : "${SPIRA_CI_LABEL:=awaiting-ci}"
    # HOW LONG A PARK MAY LAST BEFORE IT IS TREATED AS LOST, in seconds. A park is a promise
    # that something else is watching; past the longest plausible run that promise is false,
    # and the bead should be back in the report that would have found it rather than excluded
    # from it. Ninety minutes is longer than any run this was written against — raise it if
    # your CI is slower, and set it to 0 to disable the deadline, which reinstates the
    # permanent invisible park and should be a deliberate choice.
    : "${SPIRA_CI_PARK_MAX:=5400}"
    # THE LABEL THAT MEANS "THIS BEAD NEEDS THE WORLD HALTED WHILE IT RUNS". An aeon that
    # claims such a bead must drain the live pool and call world.sh stop before starting its
    # session, then world.sh start after — whether or not the session succeeds. The fence in
    # aeon.sh refuses the claim when live aeons are present and SPIRA_WORLD_STOP_SKIP is unset.
    : "${SPIRA_WORLD_STOP_LABEL:=world-stop}"
    # THE TEST FIXTURE SERVER, which is deliberately NOT the one holding real data. Fixtures
    # on the production server leaked into it, slowed it as they piled up, and made their own
    # cleanup a storm; a fixture build measured 6s against an empty server and 84s against
    # production. This store is disposable — wiping it costs nothing but the next build.
    # DERIVED, never a literal. A hardcoded operator path here is refused by
    # inventory.sh, which gate-spira.sh runs FIRST and independently of the suites — so
    # one literal default made origin/main refuse every branch, including the branches
    # that would have removed it (sp-2p7o, landed 14:15, blocked everything until 15:0x).
    : "${SPIRA_TESTDB_DATA:=$(_spira_join "$SPIRA_WORKSPACES" beads-test)}"
    : "${SPIRA_TESTDB_PORT:=3308}"
    # WHERE THE TEST IMAGE IS PUBLISHED, if anywhere. Empty means build it locally and
    # never reach the network, which is the right default: the registry is somebody's
    # account, and a harness that reached for one by default would fail on every machine
    # whose operator has not got that account.
    #
    # WHY THIS IS WORTH CONFIGURING. The image is ~1.8 GB and its build downloads a Go
    # toolchain, compiles bd from source and installs a Rust toolchain. A machine that
    # keeps the image between runs pays that once. A machine created for a single CI run
    # and destroyed afterwards pays it every run, which is most of the wall clock.
    #
    # PULLING IS AS SAFE AS BUILDING, and for the same reason: the tag is the hash of the
    # build closure, so an image built from different inputs has a different name and a
    # stale one is unreachable rather than merely unlikely. Set this to the repository
    # prefix only -- the tag comes from the closure and is never written by hand.
    : "${SPIRA_TESTENV_REGISTRY:=}"
    # WHICH TRACKER gh-intake.sh ingests from, as "owner/repo". Empty means there is
    # no inbox and intake refuses to guess: a default pointing at somebody else's
    # repository would quietly file their reports into this operator's graph.
    #
    # THE BRIDGE IS ONE WAY. Nothing here ever writes to that tracker, and the token
    # it uses should be scoped so that it could not — the store holds internal notes
    # and judgement that must not reach a public issue list (law-beads-is-never-public).
    : "${SPIRA_GH_INTAKE_REPO:=}"
    # WHICH GITHUB ORG/REPO the gate watcher scans for completed runs carrying "flaky suite"
    # annotations. Empty means no scan. Format: "owner/repo".
    : "${SPIRA_FLAKY_GH_REPO:=}"
    # WHAT AN INGESTED ISSUE IS WORTH. A report from outside names something broken for
    # somebody who is not this machine, so it enters ABOVE the band the harness files its own
    # findings in. Left at the tracker default it entered below them, and the loop — which
    # takes work in priority order — reached every self-observed defect first, indefinitely.
    : "${SPIRA_GH_INTAKE_PRIORITY:=1}"
    # WHICH REPOSITORY LABEL to put on ingested beads. Default is the tracker name.
    : "${SPIRA_GH_INTAKE_BEAD_REPO:=}"
    # HOW LONG A REPOSITORY'S OWN GATE COMMAND MAY RUN, in seconds. gate.sh wraps the command
    # under `timeout` at this budget. A gate killed at the deadline exits 124 and is reported
    # as a timeout (NO_VERDICT), not a branch fault — but the bead note is empty and the next
    # aeon hunts a test failure that never happened. The default was 900 until the spira gate
    # suite sweep measured ~1860s on a cold worktree with a shared Dolt server (sp-p4rl); 2700
    # covers that with margin and is the value observed to pass unchanged branches that 900
    # killed mid-sweep (sp-gys, sp-snyj).
    : "${SPIRA_GATE_TIMEOUT:=2700}"
    # THE GATE'S TIME BUDGET, in seconds. gate-spira.sh times itself per suite and in total;
    # when the total exceeds this value the gate files a bead against the harness — it does
    # NOT fail the branch, because the branch did not cause the overrun. The mechanism exists
    # because the previous 43-suite gate was not built in a day: each suite was individually
    # justified while the total grew to 17 minutes unchecked. A budget is the only thing that
    # makes that argument explicit — adding a check that would push the gate over budget is
    # caught on the timed run and on the gate's own self-check, not on the first innocent branch
    # that trips it. Set against a measurement: gate-spira.sh measured ~210s when this key was
    # added (2026-09-08); 300 gives headroom while still catching the next suite added without argument.
    : "${SPIRA_GATE_BUDGET:=300}"
    # HOW MANY SUITES THE LANDING GATE MAY SELECT. 0 = no cap. When the
    # coverage-based selection exceeds this, ejected suites are kept and
    # covered suites fill the remaining slots; excluded suites are logged.
    : "${SPIRA_GATE_SELECT_CAP:=0}"
    # HOW LONG A LANDING PASS MAY RUN, and how much of that it keeps in reserve so it never
    # begins a gate it cannot finish. Settable because the right number is a fact about this
    # host's gate: 3600 was correct until the gate budget was raised to 2700 to cover the
    # suite sweep, after which a 1200s reserve would let the pass start a gate it could not
    # finish — the same shape as the four consecutive kills at 1800.
    : "${SPIRA_LAND_MAXSEC:=5400}"
    # THE RESERVE MATCHES THE GATE BUDGET. A pass that starts a gate with fewer seconds left
    # than the gate is allowed to run will be killed mid-gate by RuntimeMaxSec, which is the
    # "four consecutive passes killed mid-gate" scar. The reserve is at least SPIRA_GATE_TIMEOUT
    # so a gate that is started can finish.
    : "${SPIRA_LAND_GATE_RESERVE:=2700}"
    # HOW LONG A GATE VERDICT MAY BE REUSED, in seconds. The gate computes each verdict once
    # and keys it by everything the verdict depends on that it can name — the tree, the base,
    # the changed file list, the repository's gate command and this harness — so a reused
    # verdict is never about a different question. What the key CANNOT name is the box: the
    # toolchain the command ran under, what was installed beside it, what the network
    # answered. Those drift while the key stands still, which makes a verdict a claim about a
    # moment as well as about a tree. A day bounds that drift and still spans the whole of a
    # branch's life from an aeon's own gate to its landing, which is the reuse worth having.
    #
    # 0 disables reuse entirely: every entry reads as expired and every gate runs its suites.
    : "${SPIRA_VERDICT_TTL:=86400}"
    # HOW MANY REBASE-CONFLICT REOPENS BEFORE THE BEAD IS ESCALATED INSTEAD. A bead reopened
    # this many times for a rebase conflict is not learning from the reopen, and repeating it
    # cycles the machinery while an aeon session is spent on every turn. At this threshold the
    # landing pass labels the bead needs-operator and asks rather than reopening again.
    : "${SPIRA_REBASE_ESCALATE_AT:=3}"
    # .invalid: not a real domain. @spira.local trips is_aeon_email in branch-guard.sh.
    : "${SPIRA_GIT_NAME:=spira}"
    : "${SPIRA_GIT_EMAIL:=spira@spira.invalid}"
    # HOW MANY COMMITS BACK aeon.sh AND sentinel CHECK5 WALK when asking "is there a commit
    # that names this bead?" The bound must be the same in both places: aeon.sh walks the
    # branch (and the landing refs when the branch walk finds nothing); the sentinel walks
    # the landing refs directly. If the two use different values they give different answers
    # about a bead whose commit landed many sessions ago. 400 is large enough to span an
    # active repository's daily output many times over, cheap enough to run on every bead.
    : "${SPIRA_VERDICT_WINDOW:=400}"
    : "${SPIRA_REMEDY_WINDOW:=30}"
    # MINUTES A PR-MODE BRANCH MAY SIT IN REBASED/pr-open BEFORE THE STALL DETECTOR FIRES.
    # Default 60 — long enough to let CI complete without false positives, short enough to catch
    # a repo with allow_auto_merge=false before the operator notices the queue is not moving.
    : "${SPIRA_PR_STALL_MINS:=60}"
    # HOW MANY AEONS MAY RUN AT ONCE, ACROSS EVERY PERSONA. Until 2026-09-07 this key was
    # validated, documented and read by nothing: each persona had a private cap and no pool
    # coordinated them, so the box's real ceiling was whatever the caps happened to sum to.
    # Four is the sum of what the two shipped personas declared, so this default changes
    # nothing on a host that was already running them and starts enforcing an order.
    : "${SPIRA_MAX_AEONS:=4}"
    # THE DECLARED LANES — named scheduling partitions whose capacity does not compete with
    # SPIRA_MAX_AEONS. A lane fayth draws from its own FAYTH_MAX_CONCURRENT rather than from
    # the pool, so the pool can be fully occupied by builders while the lane fayth still has
    # room. This is what "ops cannot be starved by builders" has always meant; SPIRA_LANES
    # makes it declared configuration rather than a property implied by FAYTH_ROLE=party.
    # A new lane is added here and given a name; fayths join it with FAYTH_LANE=<name>.
    # The ops and groomer lanes are declared by default because ops.fayth and groomer.fayth
    # ship using them. The qa lane is declared alongside them because qa.fayth ships using it.
    # The maechen lane is declared because maechen.fayth ships using it.
    # The czar lane is declared because czar.fayth ships using it.
    # A lane fayth still functions if its lane name is absent from this list (the mechanism is
    # FAYTH_LANE set, not membership here), but the declaration makes it visible to operators
    # reading SPIRA_LANES for the list of scheduled partitions.
    : "${SPIRA_LANES:=ops groomer qa maechen czar}"
    # THE WHOLE-FLEET CEILING — how many aeons may exist at once, counting lane fayths.
    # SPIRA_MAX_AEONS is the task pool and a lane draws outside it, so the two of them
    # together are the box's real ceiling (pool + one per lane) and neither one alone is the
    # answer to "how many aeons run at once". That is correct while the constraint is cores.
    # It is wrong while the constraint is a single shared account, which every aeon draws on
    # and which the operator's own sessions draw on too.
    #
    # EMPTY BY DEFAULT, meaning no ceiling and exactly the behaviour that shipped: a host
    # constrained by cores rather than by an account must not acquire this by upgrading.
    : "${SPIRA_MAX_LIVE_AEONS:=}"
    # THE COLLECTIVE LANE CAP — how many fleet slots lanes may hold at once.
    # When set with SPIRA_MAX_LIVE_AEONS, the last fleet slot prefers a lane: a task fayth
    # is held back when any lane has ready work and lanes are below this cap. Empty by
    # default: no collective cap and no preference rule.
    : "${SPIRA_LANES_MAX_LIVE:=}"
    # HOW LONG THE DELIVERABLE-PROGRESS WALL GIVES AN AEON BEFORE REQUEUEING IT. The wall
    # trips when the aeon's deliverable (commits ahead of the base ref + file writes in its
    # worktree) has not moved for this many minutes while turns still advance. 20 minutes is
    # the default; the case that motivated this would have been freed at ~20 rather than 76.
    # A gate suppresses the fuse, so a correct mid-review aeon is never tripped.
    : "${SPIRA_THRASH_MINUTES:=20}"
    # DEPTH OF THE QA SWEEP — controls how wide the periodic QA pass looks.
    # Three settings, each a strict superset of the one before it:
    #
    #   scars    released defects and incidents only — the default. One pass over a small,
    #            authoritative set: every defect that got through is a candidate assertion.
    #
    #   modules  the above, plus modules ranked by how often they appear in a reopen or
    #            an incident. Bounded by the ranking rather than exhaustive.
    #
    #   wide     the above, plus changed code that no assertion touches, and unasserted
    #            end-to-end properties. The expensive setting; expect noise, which is the
    #            point — discrimination is Ryan's to adjust by moving this slider.
    #
    # This is a configured operator choice, never a judgement in a brief.
    : "${SPIRA_QA_DEPTH:=scars}"
    # ---- THE READ SURFACE OVER THE LIVE GRAPH ------------------------------------------
    # Where Loom listens. Localhost is the default because a bead carries internal working
    # notes and the operator's own judgement, so the address it is reachable at is a
    # deliberate choice rather than something a default should make on anyone's behalf.
    : "${SPIRA_LOOM_ADDR:=127.0.0.1:8788}"
    # The deadline on ONE `bd` query behind that endpoint, in milliseconds. A query per
    # request is the simple choice and it is the right one only while it stays cheap; this is
    # the number that says when it has stopped being. Over it the request is refused rather
    # than served late, because a refusal that quietly degrades to stale data is a signal
    # nobody ever sees. The shipped value is a measured p95 INSIDE the CPUQuota=20% fence
    # the unit sets — an unfenced calibration is not a calibration. If you change CPUQuota,
    # re-measure and update this number to match; 500ms was the original unfenced p95 and it
    # broke every request once the fence landed.
    : "${SPIRA_LOOM_BUDGET_MS:=1500}"
    # How long a parsed snapshot is held, in seconds. This bounds the cost by TIME rather
    # than by viewer, so ten open tabs cost one query instead of ten. It is a cache and not a
    # background job: nothing runs when nobody is looking, and 0 disables it.
    : "${SPIRA_LOOM_CACHE_S:=15}"
    # The compiled Loom binary. A release tarball places it at bin/loom inside the release
    # directory; a source checkout places it at loom/target/release/loom after a cargo build.
    # Check the tarball layout first so an operator who activates a tarball does not need to
    # set this key explicitly and does not need cargo on PATH.
    if [ -z "${SPIRA_LOOM_BIN:-}" ]; then
        if [ -f "$SPIRA_REPO/bin/loom" ]; then
            SPIRA_LOOM_BIN="$SPIRA_REPO/bin/loom"
        else
            SPIRA_LOOM_BIN="$SPIRA_REPO/loom/target/release/loom"
        fi
    fi
    # THE SPIKE PARTITION, in one place because it is read in four: the spike fayth's
    # predicate, the brief handed to a spike aeon, the confinement check the landing worker
    # runs, and whatever files the bead. A literal in four files is how four programs come to
    # disagree, and the half nobody notices is wrong is the one that simply matches less.
    : "${SPIRA_SPIKE_LABEL:=spike}"
    # Where a spike writes its document, relative to the root of whatever repository its bead
    # names. A spike's deliverable is a document, so it needs somewhere to put one that is
    # true of a repository this harness has never seen; a colleague who keeps notes elsewhere
    # moves it here rather than in a prompt.
    : "${SPIRA_SPIKE_DIR:=docs/spikes}"
    # The path prefixes a spike branch is allowed to LAND, space-separated. Everything a
    # spike learns belongs in its document and beside it; a proof of concept is evidence FOR
    # that document rather than a change to the repository, so it lives on a branch of its own
    # and is named in the document. Two prefixes rather than one because the sources a spike
    # preserved need not sit under the document — set it to the trees your own notes use.
    : "${SPIRA_SPIKE_PATHS:=$SPIRA_SPIKE_DIR}"
    # THE GROOMER PARTITION, mirroring the spike partition: read by groomer.fayth's predicate
    # and by any scanner that queries for groom trigger beads. One definition keeps the label
    # name consistent across fayth, scanner and anything else that files trigger beads.
    : "${SPIRA_GROOMER_LABEL:=groom}"
    # THE LABEL APPLIED TO A BEAD WHEN THE GROOMER HAS ESCALATED IT. A bead carrying this
    # label is skipped on subsequent passes — an operator answer is pending. Without the
    # label, every pass after the first escalation files a second note rather than waiting.
    : "${SPIRA_GROOM_ASK_LABEL:=groom-asked}"
    # THE MAECHEN PARTITION AND TUNING KNOBS. Maechen is the retrospective persona: it reads
    # the failure distribution, names recurring classes, and cuts remedy beads.
    #
    # SPIRA_MAECHEN_LABEL — the sweep label that wakes Maechen. Read by maechen.fayth's
    # predicate and by whatever trigger files sweep beads (sp-emzov). One definition keeps
    # them consistent.
    : "${SPIRA_MAECHEN_LABEL:=maechen-sweep}"
    #
    # SPIRA_MAECHEN_REMEDY_LABEL — the label applied to every bead Maechen cuts. The
    # admissibility check (sp-ymwz5) and the flatline measurement query by this label.
    : "${SPIRA_MAECHEN_REMEDY_LABEL:=maechen-remedy}"
    #
    # SPIRA_MAECHEN_LANDING_INTERVAL — how many landings trigger a pass (counted from the
    # commit graph, not from bead status). At the 2026-09-11 rate of ~6/hour, 25 landings
    # takes ~4h, so the 3h gap ceiling binds and this is the volume floor.
    : "${SPIRA_MAECHEN_LANDING_INTERVAL:=25}"
    #
    # SPIRA_MAECHEN_MAX_GAP_SECONDS — maximum gap between passes. Even if the landing volume
    # threshold has not been reached, a pass fires after this many seconds. 3h = 10800.
    : "${SPIRA_MAECHEN_MAX_GAP_SECONDS:=10800}"
    #
    # SPIRA_MAECHEN_MAX_BEADS — output bound per pass. A retrospective that files twelve
    # findings has not prioritised; it has flooded. Three is the default: enough to address
    # the top class with its test and its guard, not enough to flood the board.
    : "${SPIRA_MAECHEN_MAX_BEADS:=3}"
    # THE SCOPE LABEL prepended to every persona's partition. Every fayth predicate reads
    # this key rather than the literal "spira", so the fleet's work scope is a runtime choice.
    # Two values matter: a non-empty string (scope restriction; only beads carrying that label
    # are claimed) and "" (no scope restriction; the partition is the persona label alone, e.g.
    # "plan" for builder). An empty value must never produce a leading comma in a fayth's
    # AND-labels, which would match nothing and look exactly like "no work ready".
    #
    # DEFAULT IS THE HOME REPO NAME, not a literal. A literal "spira" aimed every install at a
    # repository it may not own. Deriving from SPIRA_HOME_REPO gives each install its own
    # scope automatically; an install where that is "spira" is unchanged; an install with no
    # resolvable home repo gets an empty scope (no restriction) rather than a wrong literal.
    #
    # NO COLON in the = form: ${var=default} assigns only when the variable is UNSET, not
    # when it is empty. Empty is a valid and meaningful value here (no scope restriction), and
    # the colon form would silently promote it back, defeating the feature.
    : "${SPIRA_SCOPE_LABEL=$SPIRA_HOME_REPO}"
    # THE PLAN PARTITION LABEL — the label that marks a bead as ready plan work for a builder.
    # Declared here so the fayth predicate, the sentinel, and any other reader that needs to
    # say "plan bead" all read the same value. A literal in multiple files is how those
    # multiple programs come to disagree (law-schema-over-code). The default is "plan" — the
    # value the store has always used — so upgrading a clean install changes nothing.
    : "${SPIRA_PLAN_LABEL:=plan}"
    : "${SPIRA_INCIDENT_LABEL:=incident}"
    # NO-LOOP LABEL — marks a bead as intentionally unclaimable. READY_ARGS excludes it, so
    # fayth_ready and detect_unclaimable_ready never see it. Without this label, a bead that
    # must not be worked can only be expressed by accident; the unclaimable detector then files
    # a remedy to make it claimable. Empty string disables the feature entirely.
    : "${SPIRA_NO_LOOP_LABEL:=no-loop}"
    # THE CZAR PARTITION LABEL — the label on queue-state escalation beads. Watchtower
    # attaches this label instead of SPIRA_INCIDENT_LABEL to the queue-check incidents it
    # files, routing them to the czar rather than to ops. One definition keeps czar.fayth,
    # watchtower, and any other reader in agreement on which label means "queue event for the
    # czar" (law-schema-over-code). The labels are mutually exclusive by design: a bead that
    # carries both would be claimable by both ops and czar, which is a race condition.
    : "${SPIRA_CZAR_LABEL:=czar-trigger}"
    # OUTCOME WINDOW: minutes after a czar-trigger bead closes before checking if the
    # condition that fired it has cleared. A new bead for the same class within this
    # window means the czar's action did not hold (law-measure-the-outcome).
    : "${SPIRA_CZAR_OUTCOME_MINS:=30}"
    # UNCLAIMED THRESHOLD: minutes a czar-trigger bead may stay open before watchtower
    # escalates it as unclaimed. The czar has a 5-minute summoning budget; this window
    # is wider to allow for sentinel cadence and rate-limit pauses.
    : "${SPIRA_CZAR_UNCLAIMED_MINS:=10}"
    # The name the operator's OWN comments are recorded under, so the attention panel can tell
    # a reply of theirs from a reply of the agent's. Both write into the same thread, and a
    # panel that cannot separate them announces the agent's own comment back to it as an answer.
    : "${SPIRA_OPERATOR_ACTOR:=operator}"
    if [ -z "${SPIRA_PANEL:-}" ]; then
        if [ -f "$SPIRA_REPO/bin/panel" ]; then
            SPIRA_PANEL="$SPIRA_REPO/bin/panel"
        else
            SPIRA_PANEL="$SPIRA_COCKPIT/panel/target/release/panel"
        fi
    fi
    if [ -z "${SPIRA_BROKER_BIN:-}" ]; then
        if [ -f "$SPIRA_REPO/bin/broker" ]; then
            SPIRA_BROKER_BIN="$SPIRA_REPO/bin/broker"
        else
            SPIRA_BROKER_BIN="$SPIRA_REPO/broker/target/release/broker"
        fi
    fi
    if [ -z "${SPIRA_CZAR_PASS_BIN:-}" ]; then
        if [ -f "$SPIRA_REPO/bin/czar-pass" ]; then
            SPIRA_CZAR_PASS_BIN="$SPIRA_REPO/bin/czar-pass"
        else
            SPIRA_CZAR_PASS_BIN="$SPIRA_REPO/czar-pass/target/release/czar-pass"
        fi
    fi
    : "${SPIRA_OPERATOR:=the operator}"
    # The timezone dates are written in. Empty means the host's own, which is right until
    # the host is a server in one zone and the operator reads its output in another — the
    # case where an evening's work lands under tomorrow's date and a chronological log
    # quietly stops being chronological.
    : "${SPIRA_TZ:=}"
    : "${SPIRA_WIKI:=}"
    : "${COCKPIT_DB:=$SPIRA_DB}"
    : "${COCKPIT_BOTTOM_PCT:=28}"
    # Whether an operator is present. 1 (default) means the cockpit is staffed; doctor.sh
    # treats missing operator tools as FAIL. Set to 0 in spira.conf for a headless fixture
    # or a CI box where no operator is reading escalations; doctor.sh downgrades to WARN.
    : "${SPIRA_OPERATED:=1}"
    # The mail client in the cockpit's bottom-left pane; empty, or not on PATH, means no pane.
    : "${COCKPIT_MAIL=aerc}"
    # How wide the ops column is, as a percentage of the window. The dashboard is a
    # FULL-HEIGHT right column, so this is the only dimension it has; COCKPIT_BOTTOM_PCT
    # divides the left column between the session and the mail pane and no longer touches it.
    : "${COCKPIT_RIGHT_PCT:=33}"

    # MOUSE MODE, ON BY DEFAULT. The cockpit is panes the operator clicks into --
    # the attention panel especially -- and with mouse off a click does nothing at
    # all: no error, no focus change, so it reads as a dead panel rather than as a
    # setting. tmux defaults it off and there need not be a ~/.tmux.conf on the box,
    # so the cockpit sets it itself rather than depending on one.
    #
    # THE TRADE IT MAKES. With mouse on, dragging selects into tmux's copy-mode
    # instead of the terminal's own selection, so a terminal-native copy needs Shift
    # held down. That is the whole cost, it is per-operator, and it is why this is a
    # key and not a constant: set COCKPIT_MOUSE = off to keep native selection.
    : "${COCKPIT_MOUSE:=on}"
    # WHERE THE COCKPIT'S PANES OPEN. The top pane holds the operator's own session, so its
    # working directory decides which project's instructions that session loads — not a
    # cosmetic choice. It defaults to the wiki when one is configured, because an operator
    # who keeps notes works there rather than in the harness they are merely running.
    : "${COCKPIT_CWD:=${SPIRA_WIKI:-$SPIRA_REPO}}"
    # HOW LONG A CLIENT MAY BE IDLE BEFORE ensure DETACHES IT. Ghost clients — terminals
    # whose PTY is no longer actively used but remain attached to the tmux server — drive the
    # window-size flap: with window-size latest, a ghost becomes "latest" whenever its session
    # is touched, shrinking the cockpit window to the ghost's small terminal height until the
    # operator's client regains "latest". Detaching them eliminates the root cause.
    # Default is 6 hours (21600 s); set to 0 to disable detachment.
    : "${COCKPIT_CLIENT_IDLE_SECS:=21600}"
    # Optional, and EMPTY IS THE DEFAULT for every one of them. Each names something a
    # colleague does not have — a Gas Town, a wiki, a design document — and every caller
    # must treat empty as "skip this", never as "guess". That is rule 2 of the boundary:
    # an optional call is not a dependency, a hard path is.
    : "${SPIRA_TOWN:=}"
    : "${SPIRA_MIRROR:=}"
    : "${SPIRA_EXPORTER:=}"
    : "${SPIRA_DESIGN:=}"
    : "${SPIRA_WIKI_HOOK:=}"
    # THE VIEW FOLLOWER: the program that keeps the attention surface pointed at whatever the
    # operator should be looking at right now. Empty means this installation has none, and the
    # optional manifest row that would watch it is dropped rather than run against a path that
    # is not there. It is a key rather than a shipped script because the surface it steers is
    # the operator's — a terminal multiplexer here, something else elsewhere.
    #
    # THE CONTRACT IT MUST MEET, so the manifest can both run it and judge it:
    #   <prog> watch    a loop that enacts the state; this is what the unit starts
    #   <prog> status   prints `want: <session>` — the multiplexer session that SHOULD be
    #                   visible. That one line is what makes blindness measurable, because
    #                   the view it is steering can be read independently.
    : "${SPIRA_VIEW:=}"
    # The multiplexer session whose visible window the follower steers. Only ever consulted
    # when SPIRA_VIEW names something, so it costs an installation without one nothing.
    : "${SPIRA_VIEW_SESSION:=cockpit}"
    # THE SESSIONS rebuild.sh CREATES WHEN BUILDING A COCKPIT FROM NOTHING. brain and hunk
    # are structural — the cockpit LINKS their windows. Any additional names are convenience
    # sessions recreated alongside them. Override in spira.conf to match your own workflow.
    : "${COCKPIT_SESSIONS:=brain hunk chat}"
    # THE HOST THE LAPTOP DIALER CONNECTS TO. No default: a wrong default silently dials
    # somebody else's box. Set in spira.conf on the laptop, or export it in the environment.
    : "${COCKPIT_HOST:=}"
    # The Dolt server's own data directory, which is NOT the beads project directory: `bd -C`
    # is pointed at the latter, and the former is where the server keeps every database it
    # serves. Empty means this installation does not manage the server (dolt is run another
    # way). The default is a derived path so a fresh install gets server mode by default;
    # set explicitly to empty only if you run dolt yourself.
    # NO-COLON FORM: an explicit empty value from a config file or env is preserved as-is —
    # the colon form would replace it with the derived default, defeating the opt-out.
    : "${SPIRA_DOLT_DATA=${XDG_DATA_HOME:-$HOME/.local/share}/spira/dolt}"
    # The alert units whose failure should be filed as an incident bead, as a find(1) name
    # pattern. Empty means none: these are the operator's own unit names and nothing here can
    # guess them, so install-intake.sh says so rather than wiring whatever matches.
    : "${SPIRA_ALERT_GLOB:=}"

    # ---- WHAT THE ACCOUNT SPENDS -------------------------------------------------------
    # The rate limit is charged against a rolling window, and every figure the token meter
    # reports is "inside the window" — so this number decides what the dashboard means. It is
    # a fact about the operator's PLAN, not about this box, which is why it is a key: a
    # colleague on a different plan reads a window of a different length.
    : "${SPIRA_TOKEN_WINDOW_H:=5}"
    # Where the interactive sessions write their transcripts. The aeons' own traces are found
    # under SPIRA_RUN and need no key, because the harness put them there; this directory
    # belongs to the client, and a client that moves it would otherwise make the session half
    # of the split silently read zero — which is the reading that looks like good news.
    : "${SPIRA_TOKEN_PROJECTS:=$HOME/.claude/projects}"
    # THE THRESHOLDS A LIVE SESSION IS MEASURED AGAINST, shared by the status line and the
    # dashboard so that the two cannot disagree about how close to the edge a session is. The
    # defaults are what this context window actually costs: a session opens near 50,000, and
    # every long one ends up pinned near the ceiling, re-reading all of it on every turn.
    : "${SPIRA_CTX_WARN:=200000}"
    : "${SPIRA_CTX_HIGH:=400000}"
    : "${SPIRA_CTX_LIMIT:=1000000}"

    # WHERE THE TRANSCRIPTS ARE KEPT. The client's own directory is unversioned, on whatever
    # volume the home directory sits on, and promises nothing about retention — so this is a
    # copy of it that outlives both. It defaults under the runtime directory because that is
    # gitignored: the bodies carry paths, credentials read aloud and everything anyone ever
    # said, and a default inside a shared checkout is one `git add -A` away from publishing
    # all of it. Point it at whichever volume has the room; nothing here ever deletes.
    : "${SPIRA_ARCHIVE:=$SPIRA_RUN/archive}"
    # WHERE THE BD BINARY PIN LIVES. A pin file records which bd is installed —
    # its migration count, version string, sha256, and build flags — so doctor.sh
    # can detect an unannounced rebuild before the loop tries to run. Default is
    # machine-local (under SPIRA_RUN, which is gitignored), not in the harness tree.
    # Populate it after every bd install with: spira/bd-pin.sh write
    : "${SPIRA_BD_PIN:=$SPIRA_RUN/bd-pin}"
    # THE RELEASE TAG THIS HARNESS EXPECTS TO BE RUNNING. Both build-bd.sh (which builds
    # or downloads the binary) and doctor.sh (which checks the running binary) read this
    # value, so the two cannot disagree about which version is correct. Changing it here
    # changes what doctor.sh refuses and what build-bd.sh targets.
    : "${SPIRA_BD_TAG:=v1.2.1}"

    # ---- THE ARCHIVIST: WHEN A FULL SESSION GETS ITS UNFINISHED BUSINESS RESCUED ---------
    # HOW MANY TURNS BETWEEN SWEEPS. The timer fires every five minutes; on each pass, a
    # session whose turn count has advanced by at least this delta since the last successful
    # archive is swept. A session nobody has typed in costs a stat and a measurement; one that
    # has moved 40 turns gets an archivist. Context depth is not in the trigger at all — a
    # 74k session that has moved 40 turns is swept, a 900k session that has moved none is not
    # — because what the archivist covers is TURNS, and its cost is proportional to them.
    : "${SPIRA_ARCHIVIST_EVERY:=40}"
    # HOW RECENTLY A TRANSCRIPT MUST HAVE BEEN WRITTEN TO COUNT AS LIVE. Everything the
    # archivist rescues is rescued so that the session can be cleared, which only matters
    # while somebody is still sitting in it. Far too short and a session that pauses to read
    # is declared over; far too long and every transcript on the disk is swept on every pass,
    # which is the unbounded fan-out this box already has a scar from.
    : "${SPIRA_ARCHIVIST_IDLE:=1800}"
    # A KEY BECAUSE THE JUDGEMENT IS THE PRODUCT. What is being asked for is which of a
    # thousand turns was a question nobody answered and which verdict generalises into law —
    # not a summary. That is worth the strong model here, and a colleague running mostly
    # routine sessions may reasonably disagree, which is what makes it configuration.
    : "${SPIRA_ARCHIVIST_MODEL:=claude-opus-5}"
    # A HARD CEILING, unlike an aeon's. An aeon has none because a clock cannot tell slow from
    # stuck and its heartbeat can; this has no heartbeat and no lease, and it holds the sweep
    # while it runs, so an archivist wedged on a huge transcript would stop every other session
    # from ever being looked at.
    : "${SPIRA_ARCHIVIST_TIMEOUT:=900}"
    # HOW MANY SESSIONS ONE SWEEP MAY ARCHIVE. Serial-and-unbounded is what turns a quiet
    # morning into a 20-minute pass when several sessions drift past the threshold together;
    # with a budget the work still drains — drift does not disappear — but at a rate the
    # account window can absorb, and the timer is the throttle rather than the session count.
    : "${SPIRA_ARCHIVIST_PER_PASS:=1}"
    # WHERE A REPOSITORY'S TEST-FIXTURE LIBRARY SITS, relative to that repository's ROOT.
    # An aeon builds one fixture at summon for a repository that has one and exports it, so
    # every suite the session runs resets that fixture instead of building its own — measured
    # here at 0.2s against 55s, against ~15 single-suite runs in a session.
    #
    # RELATIVE, because it is resolved inside the aeon's WORKTREE: the tree whose suites will
    # consume the fixture is the tree that should build it, which is the same reason the
    # landing gate builds from the branch's copy and not the installed one. A repository that
    # has no such file simply gets no fixture, and that is how "which repositories does this
    # apply to" is answered without a list of repository names.
    #
    # The default is this harness's own directory name, so a clone that keeps the layout needs
    # no configuration at all.
    local _tdb; _tdb="$(basename "$SPIRA_HOME")"
    : "${SPIRA_TESTDB_LIB:=$_tdb/testdb.sh}"
    # THE EMBEDDED bd BINARY used by test fixtures. Fixtures need a CGO-enabled build for
    # embedded Dolt; the production binary (the one aeons and the sentinel use) may be the
    # CGO_ENABLED=0 build, which is incompatible. A separate binary avoids a schema-mismatch
    # forced on the production database when the two share different migration counts.
    # The default name `bd-embedded` is a sibling of `bd` on PATH; an operator whose main
    # binary already has CGO support can set this to `bd` to consolidate.
    : "${SPIRA_TESTDB_BD:=bd-embedded}"

    # WHICH SUITES THE LANDING GATE RUNS, as a file of repository-relative paths, one per
    # line. suites.sh reads it to run everything the `spira/test-*.sh` glob finds that this
    # file does NOT name — so the gated set and the timed set are complements by construction
    # and no suite can fall between them. That is the whole defect the file exists to close:
    # the list used to live inside gate-spira.sh where nothing else could read it, and five of
    # nine suites were running nowhere at all before anybody compared the two by hand.
    #
    # THE GATE ITSELF IGNORES THIS KEY and reads the copy beside it in the tree under trial,
    # which is not a disagreement but the same rule as SPIRA_HOME: the gate extracts a branch
    # to a scratch tree and must judge THAT tree, so a configured path would point it back at
    # the installed copy and it would test the code already in force. In every real
    # installation the two resolve to the same file. What the key buys is a fixture that can
    # pin it somewhere disposable, which is what stops a suite asserting against the shipped
    # list and passing just as well with the list written back into the code.
    : "${SPIRA_GATE_SUITES:=$SPIRA_HOME/gate-suites}"
    # WHERE THE TIMED RUN RECORDS WHAT IT FOUND — one file per suite, holding a status, the
    # epoch it was written and how long the suite took. A green cycle has to leave a POSITIVE
    # record: without one, "no bead was filed" reads identically whether every suite passed or
    # the runner has not run since the box came up, and the reassuring reading is the one an
    # empty directory gives (law-absence-needs-a-positive-control).
    : "${SPIRA_SUITES_STATE:=$SPIRA_RUN/suites}"
    # HOW LONG ONE TIMED PASS MAY TAKE, in seconds. The pass runs INSIDE an Ops session, whose
    # own wall is FAYTH_TIMEOUT_SECONDS — 480 — enforced by systemd rather than requested. So
    # this is under it with room for the session to read the sweep, run the scan and write up
    # what it found. A pass that runs out of budget stops cleanly and leaves a cursor, so the
    # suites it did not reach lead the next pass rather than being the ones that are never run.
    : "${SPIRA_SUITES_BUDGET:=420}"
    # HOW LONG ANY ONE SUITE MAY RUN, in seconds, in the gate and in the timed pass alike. One
    # key for both, because a suite that is affordable in one and not the other is a suite
    # whose cost nobody has decided.
    : "${SPIRA_SUITE_TIMEOUT:=600}"
    # THE PRIORITY A RED FROM THE TIMED PASS IS FILED AT. Routine by default: the timed pass
    # blocks nothing and reopens nothing, and by law-reversibility-outranks-coverage a failure
    # caught twenty minutes after landing is fine when a revert undoes it. A suite that covers
    # something where that is not true says so ITSELF, with a `# priority: N` line beside its
    # `# covers:` line — the priority of what a suite covers is a claim only the suite's author
    # can make, and a central table of it would be a second list to keep in step with the glob.
    # ROUTINE IS P3, NOT P2. A red suite is the harness reporting on itself, and self-reported
    # defects filed above the band that outside bug reports arrive in is a priority inversion:
    # with one aeon, 123 P2 suite reds in three days kept 22 reported bugs from ever being
    # reached. A suite whose subject genuinely outranks a user's bug says so itself.
    : "${SPIRA_SUITES_PRIORITY:=3}"
    # WHAT A MACHINE-OBSERVED INCIDENT IS WORTH BY DEFAULT. This was a bare 1 inside
    # incident.sh: every condition the harness noticed about itself opened at the most urgent
    # band, whether or not anything was broken for anyone. The callers that mean P1 — a dead
    # canary, a wedged landing, the watchtower's blockage checks — say so on the call, and are
    # unaffected. What moves is everything that never chose, which is what filled the band.
    : "${SPIRA_INCIDENT_PRIORITY:=3}"
    # HOW OFTEN A WATCHER FIRES, in seconds. Used by incident.sh to distinguish
    # a bead closed while its condition was still live (closed within one interval
    # before the next filing) from a genuine recurrence after a real fix.
    # Default matches the observed 30-minute watchtower cadence.
    : "${SPIRA_WATCHER_INTERVAL_S:=1800}"
    # AURON RESTART-LOOP DETECTION thresholds. A Restart=always unit never reaches
    # 'failed', so incident intake misses it; Auron detects it by watching NRestarts.
    # RESTARTS: how many restarts within the window trigger an alert.
    # RESTART_WINDOW: the measurement window in seconds; the baseline resets at expiry
    # so a loop that stopped more than this long ago does not continue to fire.
    : "${SPIRA_AURON_RESTARTS:=5}"
    : "${SPIRA_AURON_RESTART_WINDOW:=3600}"
    # HOW OLD A SUITE'S RESULT MAY BE BEFORE IT IS NO LONGER EVIDENCE, in seconds. Past this
    # the watchtower reports the suite as unrun rather than as green, because a stale pass and
    # a runner that has stopped are the same silence from outside. Longer than the interval at
    # which the sweep names the scan, so an ordinary quiet hour does not read as a fault.
    : "${SPIRA_SUITES_STALE:=21600}"
    # THE SUITE LIFECYCLE STATE FILE — path relative to the repository root.
    # Read from the TREE UNDER TEST so the gate, CI and the hourly run judge the
    # tree they carry rather than the installed copy. Transitions are written by
    # suites.sh quarantine|disable|activate, which commit the file on a branch.
    : "${SPIRA_SUITE_STATE_FILE:=spira/suite-state}"
    # AUTOMATIC-QUARANTINE THRESHOLDS. The queue quarantines a suite once it has
    # SPIRA_FLAKE_QUARANTINE_AT flake observations within SPIRA_FLAKE_WINDOW seconds.
    : "${SPIRA_FLAKE_QUARANTINE_AT:=2}"
    : "${SPIRA_FLAKE_WINDOW:=604800}"
    # HOW MANY CONSECUTIVE CLEAN HOURLY RUNS lift an automatic quarantine, and how
    # long a quarantine may stand before the operator is mailed.
    : "${SPIRA_QUARANTINE_CLEAN_RUNS:=10}"
    : "${SPIRA_QUARANTINE_MAX_AGE:=604800}"
    # ---- MERGE QUEUE (queue land mode) -------------------------------------------------
    # HOW MANY CERTIFIED BRANCHES FIT IN ONE BATCH. The batch builder (sp-h3g55) collects
    # certified branches up to this limit or until SPIRA_QUEUE_BATCH_WAIT seconds have
    # passed since the oldest was certified.
    : "${SPIRA_QUEUE_BATCH_MAX:=8}"
    # HOW LONG THE BATCH BUILDER WAITS FOR MORE BRANCHES before closing a batch with
    # fewer than SPIRA_QUEUE_BATCH_MAX. In seconds.
    : "${SPIRA_QUEUE_BATCH_WAIT:=1800}"
    # HOW LONG A CI RUN MAY RUN before it is a candidate for the stuck check.
    # Per-repository override: SPIRA_QUEUE_CI_MAXSEC_<NAME> where <NAME> is the
    # repo-map name uppercased with hyphens replaced by underscores.
    : "${SPIRA_QUEUE_CI_MAXSEC:=3600}"
    # HOW LONG WITH NO RUN ACTIVITY before a run is treated as hung and re-queued.
    # Per-repository override: SPIRA_QUEUE_CI_IDLE_SEC_<NAME>.
    : "${SPIRA_QUEUE_CI_IDLE_SEC:=600}"
    # HOW LONG A CI JOB MAY BE IN QUEUED STATUS (no runner assigned) before czar.sh --pass
    # fires ci-stalled. A queued job with a torn-down VM label will never start; the czar
    # cancels the stuck run and re-dispatches the whole workflow. In seconds.
    : "${SPIRA_CI_QUEUED_MAX_SECS:=600}"
    # HOW LONG A PARTITION MAY REMAIN STARVED (ready work, no serving aeons) before czar.sh
    # --pass fires the starved detector. In minutes.
    : "${SPIRA_STARVED_MAX_MINS:=20}"
    # HOW LONG WITHOUT A LANDING PASS COMPLETING before czar.sh --pass fires loop-stalled.
    # Above the 2700s local-gate timeout so an ordinary batch gate does not trigger it.
    : "${SPIRA_LOOP_STALL_SECS:=3000}"
    # HOW LONG A BATCH OPEN FILE MAY SIT AFTER ITS CI RUN COMPLETES RED before czar.sh
    # --pass fires ci-red (verdict not acting on a red result). In seconds.
    : "${SPIRA_CI_RED_MAX_SECS:=600}"
    # WHETHER A BATCH RUNS THE LOCAL GATE before its PR opens. 0 skips it; CI still runs.
    : "${SPIRA_QUEUE_LOCAL_GATE:=0}"
    # HOW MANY TIMES THE BATCH BUILDER RE-RUNS A WORKFLOW before mailing the operator.
    : "${SPIRA_QUEUE_INFRA_RETRIES:=2}"
    # HOW OLD THE OLDEST CERTIFIED BRANCH MAY BE before the batch builder is considered
    # stuck and an alert is filed.
    : "${SPIRA_QUEUE_STUCK_AGE:=7200}"
    # WHERE OPEN-BATCH RECORDS ARE KEPT — one file per open batch.
    : "${SPIRA_QUEUE_DIR:=$SPIRA_RUN/queue}"
    # THE FORGE SEAM — the executable batch.sh calls to open pull requests. Empty means use
    # $SPIRA_HOME/forge.sh. A fixture sets this to a local script so the suite never reaches
    # the real forge.
    : "${SPIRA_FORGE:=$SPIRA_HOME/forge.sh}"
    # LABEL APPLIED TO A BEAD THAT IS READY BUT HAS A CLOSED BLOCKER IN A QUEUE-MODE
    # REPOSITORY WHOSE LANDSTATE HAS NOT YET REACHED LANDED. Excludes the bead from
    # fayth_ready so it is not summoned until the blocker's change is pushed to base.
    : "${SPIRA_QUEUE_WAIT_LABEL:=spira-queue-waiting}"
    # THE GITHUB ACTIONS APP ID used in the required-status-checks rule set by queue.sh
    # protect. Omitting it (or setting it to -1) lets the source be inferred from history,
    # which admits a hand-posted commit status that bypasses gate enforcement. 15368 is the
    # GitHub Actions app. Must be a non-negative integer.
    : "${SPIRA_QUEUE_ACTIONS_APP_ID:=15368}"
    # THE ADMISSION THROTTLE (sp-h7zzx) — Little's Law with a rework feedback term.
    # Two conditions, two outcomes:
    #   depth >= DEPTH_AT AND drain active → throttle new builders; queue is over capacity
    #   depth >= DEPTH_AT AND drain zero  → escalate, do not throttle; queue is stalled
    # Throttling a stalled queue delays repairs rather than reducing load.
    #
    # SPIRA_QUEUE_THROTTLE_DEPTH_AT: engage when CERTIFIED depth reaches this many branches.
    # Default is two batches; tune up if the queue recovers faster than builders refill it.
    : "${SPIRA_QUEUE_THROTTLE_DEPTH_AT:=$(( ${SPIRA_QUEUE_BATCH_MAX:-8} * 2 ))}"
    # SPIRA_QUEUE_THROTTLE_RELEASE_AT: lift when depth drops to this. Hysteresis gap prevents
    # oscillation; set below DEPTH_AT so lifting does not immediately re-engage.
    : "${SPIRA_QUEUE_THROTTLE_RELEASE_AT:=${SPIRA_QUEUE_BATCH_MAX:-8}}"
    # SPIRA_QUEUE_THROTTLE_STALL_MINS: if nothing has landed in this many minutes, the queue
    # is stalled rather than busy. Stall → escalate, not throttle.
    # Matches the loop-stall threshold by default (SPIRA_LOOP_STALL_SECS / 60 = 50m).
    : "${SPIRA_QUEUE_THROTTLE_STALL_MINS:=50}"
    # SPIRA_QUEUE_THROTTLE_OVERRIDE: set to 'off' to pin the automated throttle disabled.
    # The stamp file is never written while this is 'off'; pool follows SPIRA_MAX_AEONS alone.
    : "${SPIRA_QUEUE_THROTTLE_OVERRIDE:=}"
    # WHETHER THIS INSTALLATION RUNS ITS OWN TEST SUITES on a timer. On by default when
    # SPIRA_REPO is a git checkout (development mode — the operator can land changes); off
    # when it is not (a consumer installation from a release tarball, where SPIRA_REPO has
    # no .git directory). A consumer has no reason to self-test: the suites assert against
    # the harness source, and a consumer installation carries a read-only release snapshot
    # that will never change between installs. The beads those suites file are noise that
    # competes with the operator's own work queue and cannot be worked (the bead carries a
    # repo: label that resolves to nothing in the consumer's repo-map).
    #
    # Set to 0 to disable; set to 1 to enable even on a non-development installation.
    # Empty or absent means "derive from the checkout": detect .git in SPIRA_REPO_DERIVED.
    if [ -z "${SPIRA_SELF_TEST:-}" ]; then
        if [ -d "${SPIRA_REPO_DERIVED:-}/.git" ] || [ -f "${SPIRA_REPO_DERIVED:-}/.git" ]; then
            SPIRA_SELF_TEST=1
        else
            SPIRA_SELF_TEST=0
        fi
    fi

    # ---- RELEASE ACTIVATION (activate.sh) -----------------------------------------------
    # WHERE RELEASE TARBALLS ARE UNPACKED. Each activation unpacks a tarball into a
    # timestamped subdirectory here and swaps the 'current' symlink atomically. systemd units
    # render ExecStart= paths through 'current', so a swap is a deployment. The disk holding
    # SPIRA_WORKSPACES is the right place: it is the large, nearly-empty volume that exists
    # specifically to avoid competing with / for rollback depth.
    # DERIVED FROM SPIRA_WORKSPACES; _spira_join prevents double slashes when SPIRA_WORKSPACES
    # is "/" (a container root).
    : "${SPIRA_RELEASES:=$(_spira_join "$SPIRA_WORKSPACES" spira-releases)}"
    # HOW MANY RELEASES TO KEEP. Old releases beyond this count are pruned after each
    # activation (best-effort; a prune failure never fails the activation). Each release
    # directory is the unpacked contents of one tarball — about 1.4 MB — so 100 releases
    # total roughly 140 MB. Per Ryan: "They're tiny. make it 100."
    : "${SPIRA_RELEASES_KEEP:=100}"

    # THE ACTIVATED RELEASE — the ONLY directory systemd executes. activate.sh swaps
    # the 'current' symlink here atomically on each deployment; ExecStart= paths resolve
    # through it so a swap is a deploy. A missing symlink means no release has been
    # activated yet; install.sh refuses until activate.sh runs at least once.
    # The harness lives under spira/ inside the release directory, so the executable path
    # is current/spira/sentinel.sh, not current/sentinel.sh.
    # NO-COLON FORM preserves SPIRA_PROD= for single-checkout mode.
    : "${SPIRA_PROD=$(_spira_join "$SPIRA_RELEASES" current/spira)}"

    # ---- THE REVIEWER: ADVERSARIAL REVIEW AT THE RELEASE-UNIT BOUNDARY -------------------
    # THE MODEL IS STRONG BY DESIGN. The reviewer looks for intent violations, cross-commit
    # interactions, and irreversible changes — the class of defect per-change review is worst
    # at. A cheaper model here misses the findings the gate cannot catch.
    # Verify the model id answers on this box before deploying: an id the CLI rejects does
    # not fail loudly — the process exits non-zero, review.sh exits 2, and promote.sh refuses
    # to promote the unreviewed unit (which is the safe failure mode).
    : "${SPIRA_REVIEWER_MODEL:=claude-fable-5-1}"
    # WHERE VERDICTS ARE WRITTEN. One file per release tag, named <tag>.verdict. The file
    # carries verdict, cost, token counts and finding count so the reviewer's per-unit cost
    # is readable without re-parsing the trace.
    : "${SPIRA_REVIEWER_VERDICTS:=$SPIRA_RUN/review-verdicts}"
    # HOW LONG ONE REVIEW MAY RUN, in seconds. A review that exceeds this exits 2 (error);
    # promote.sh refuses to promote an unreviewed unit, so the unit waits for a successful run.
    : "${SPIRA_REVIEWER_TIMEOUT:=300}"
    # HOW MUCH OF A DIFF THE REVIEWER READS. Diffs larger than this are truncated with a
    # note in the prompt; the reviewer still runs and may find what it can within the window.
    : "${SPIRA_REVIEWER_DIFF_LIMIT:=80000}"
    # THE LABEL APPLIED TO FINDING BEADS. The deployment controller (sp-gsmx.5) and the
    # groomer query on this label to find open findings for a release unit.
    : "${SPIRA_REVIEW_LABEL:=review-finding}"

    # CAPACITY PROBE — while a pause is in force and its horizon is far out, the harness
    # probes the account to detect early recovery. These keys gate that probe.
    #
    # PROBE_MODEL matches the builder persona's model: a probe that the builder's model
    # cannot answer is evidence the account is genuinely out for builders. An operator
    # whose pool uses a different model sets this key. Probe with the cheapest capable
    # model — a refused probe costs nothing; a served one costs one minimal request.
    : "${SPIRA_CAPACITY_PROBE_MODEL:=claude-sonnet-4-6}"
    # PROBE_INTERVAL: minimum seconds between probes. One per hour is enough — the reset
    # time from a refusal is typically several hours, so a probe that keeps the pause for
    # an hour costs nothing and one that lifts it early unblocks the whole queue.
    : "${SPIRA_CAPACITY_PROBE_INTERVAL:=3600}"
    # PROBE_WINDOW: horizon threshold beyond which probing makes sense, in seconds.
    # A pause with less than this remaining is likely about to expire on its own; probing
    # it costs a request and saves at most a few minutes. Default is one five-hour window.
    : "${SPIRA_CAPACITY_PROBE_WINDOW:=18000}"
    # PROBE_TIMEOUT: seconds allowed for one probe request. A probe that times out is
    # treated as refused — conservative, because a non-answering API is not evidence the
    # account is open.
    : "${SPIRA_CAPACITY_PROBE_TIMEOUT:=30}"

    # SELF-MONITORING WINDOW: how many minutes back cockpit-metrics.py looks when deciding
    # what is "repeating now" and whether a stillborn or stall alert is in force. Narrow
    # enough to suppress a burst that ended hours ago; wide enough to cover the ~2-minute
    # sentinel cadence across a meaningful run of passes.
    : "${SPIRA_SELF_WINDOW:=60}"

    # THE AGENT CLI BINARY. Named once so every tool that invokes it reads the same setting.
    # The invocation shape (-p --output-format stream-json --verbose --model ...) is NOT
    # changed by this key — pointing it at another vendor's CLI will not work; only the
    # binary name is configurable here.
    # SPIRA_CLAUDE is the deprecated name for this key. If the old name is set and the new
    # one is not, honour it and warn once so an existing installation is not broken by the
    # rename.
    if [ -n "${SPIRA_CLAUDE:-}" ] && [ -z "${SPIRA_AGENT:-}" ]; then
        printf 'spira: SPIRA_CLAUDE is deprecated; rename it to SPIRA_AGENT in spira.conf\n' >&2
        SPIRA_AGENT="${SPIRA_CLAUDE}"
    fi
    : "${SPIRA_AGENT:=claude}"

    # WHICH STATUTES GET FULL TEXT AT SUMMON. render_memories renders these in complete
    # paragraph form; everything else is rendered as a slug-only index line. Seeded from
    # citation frequency in commits and bead text (measured 2026-09-11): those cited most
    # often are the ones a violation costs most to miss. Being wrongly in core costs ~88
    # words; being wrongly out costs an untraced violation. The default is generous.
    # CSV of slug names, without the leading path — e.g. "law-foo,law-bar".
    : "${SPIRA_STATUTE_CORE:=law-absence-needs-a-positive-control,law-fence-loops-on-shared-hardware,law-alerts-must-be-actionable,law-prefer-the-real-dependency,law-gates-run-in-a-clean-environment,law-closed-is-not-landed,law-a-regression-test-must-be-seen-to-fail,law-guard-binds-the-caller,law-verify-nothing-was-dropped,law-gate-earns-its-place,law-arm-before-you-retire,law-hand-land-the-unblocker,law-no-close-reason-admits-unfinished}"

    # THE MAP FALLS BACK TO THE EXAMPLE, and that is what makes a clean clone runnable at
    # all. The real map is one operator's inventory of checkouts and does not ship; the
    # example does. Resolution runs beside the config file first, because that is where an
    # operator whose harness lives in a repository they did not write can keep theirs.
    if [ -z "${SPIRA_REPO_MAP:-}" ]; then
        local cf d c
        cf="$(spira_conf_file)"
        [ -n "$cf" ] && d="$(dirname "$cf")" || d=""
        # AN EXPLICIT SPIRA_HOME PUTS ITS OWN MAP FIRST. Otherwise the config-dir map leads,
        # for an operator whose harness lives in a repository they did not write.
        #
        # The conditional is not a nicety. Every fixture in these suites plants a map at
        # $SPIRA_HOME/repo-map and sets SPIRA_HOME to reach it; with the config-dir map
        # unconditionally ahead, all of them silently read the operator's REAL seven
        # repositories instead — four suites at once, reporting "0 movements" and "not an
        # ancestor" as though landing were broken, with nothing naming the map they read
        # (law-gates-run-in-a-clean-environment).
        if [ -n "$_spira_conf_home_env" ]; then
            set -- "$SPIRA_HOME/repo-map" ${d:+"$d/repo-map"} "$SPIRA_HOME/repo-map.example"
        else
            set -- ${d:+"$d/repo-map"} "$SPIRA_HOME/repo-map" "$SPIRA_HOME/repo-map.example"
        fi
        for c in "$@"; do
            [ -f "$c" ] && { SPIRA_REPO_MAP="$c"; break; }
        done
        : "${SPIRA_REPO_MAP:=$SPIRA_HOME/repo-map}"
    fi
}

# --------------------------------------------------------------------------------------
# CHECKOUT MODE. SPIRA_PROD is the tree systemd executes; SPIRA_REPO is the tree being
# developed. Their relation decides what several programs may assume, and they must all
# decide it the same way — doctor and the unit manifest disagreeing about this is how a
# box ends up reporting a fatal for a unit that is correctly absent.
#
#   split-checkout   SPIRA_PROD outside SPIRA_REPO. promote.sh carries commits from one
#                    to the other, so a dirty or mid-landing dev tree never executes.
#   single-checkout  SPIRA_PROD inside SPIRA_REPO. One tree, developed and executed.
#                    Supported, and the only sane shape on a box whose production lives
#                    on another machine and is reached through a tagged release. It costs
#                    what it obviously costs: an edit is live the moment it is saved.
#
# Returns 0 for single-checkout, 1 for split-checkout AND for an empty SPIRA_PROD — an
# unset production directory is its own error, reported where it is diagnosed rather than
# folded into this answer.
spira_single_checkout() {
    [ -n "${SPIRA_PROD:-}" ] || return 1
    local _p _r
    _p="$(cd "$SPIRA_PROD" 2>/dev/null && pwd -P)" || _p=""
    [ -n "$_p" ] || _p="$SPIRA_PROD"
    _r="$(cd "${SPIRA_REPO:-}" 2>/dev/null && pwd -P)" || _r=""
    [ -n "$_r" ] || _r="${SPIRA_REPO:-}"
    [ -n "$_r" ] || return 1
    # Strip the trailing slash before interpolating: when _r is "/" the unstripped form
    # produces "//*", which a case statement never matches.
    case "$_p/" in "${_r%/}/"*) return 0 ;; *) return 1 ;; esac
}

SPIRA_CONF_FILE="$(spira_conf_file)"
[ -n "$SPIRA_CONF_FILE" ] && spira_conf_read "$SPIRA_CONF_FILE"
spira_conf_defaults
unset _spira_conf_here _spira_conf_env _spira_conf_home_env

# --------------------------------------------------------------------------------------
# PATH. `bd`, `git`, `gh` and the configured agent CLI live wherever the operator put them,
# and everything here is invoked from systemd, where a login shell's PATH does not exist.
# Bootstrapping in one place is the difference between working and failing silently.
#
# SPIRA_PATH is prepended and is the config's business; the tail is the box's own and is
# not, so it is not written into the config.
# --------------------------------------------------------------------------------------
export PATH="${SPIRA_PATH:+$SPIRA_PATH:}$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"

# SPIRA_BD — resolved once, deterministically, after SPIRA_PATH is applied. When the
# environment or a config file already set it (both captured before this point), the value
# survives unchanged. When neither did, it resolves to the first bd on the PATH this
# harness just assembled — rather than whatever PATH the calling context carries. PATH
# order differs between a login shell, a systemd unit and an aeon's confined environment,
# so a caller that fell through to ${SPIRA_BD:-bd} in lib.sh could silently pick a
# different binary in each context (sp-s2zvn, scar from 2026-09-08).
if [ -z "${SPIRA_BD:-}" ]; then
    SPIRA_BD="$(command -v bd 2>/dev/null || echo bd)"
fi
export SPIRA_BD

# BD SCHEMA REFUSAL. When the resolved bd's migration count disagrees with the database's,
# bd exits 0 with the complaint on stdout — callers that check exit status read success and
# parse the error as data. Catching it here, once, stops the mismatch from propagating to
# every bdq call. Only runs when the database is present; on a fresh install or in a test
# fixture that has not yet called testdb_up, $SPIRA_DB/.beads does not exist and the check
# is skipped entirely. bd's version string does not order against release tags — a dev build
# knows MORE migrations than a tagged release — so pin by migration count, not version string.
if [ -d "${SPIRA_DB:-}/.beads" ]; then
    # SCHEMA CHECK CACHE. Running `bd migrate schema` on every conf.sh source opens the
    # store once before any actual work — doubling the load. The check is cached by the
    # bd binary's mtime and size: if neither has changed since the last successful pass,
    # the cursor cannot have moved (migrations are applied by bd, not by the database alone).
    # Both fields are required: mtime alone has second-level resolution, so two distinct
    # binaries built in the same second but with different content share a mtime but differ
    # in size. The stamp file lives in SPIRA_RUN so it is instance-qualified.
    _spira_schema_stamp="${SPIRA_RUN}/bd-schema-stamp"
    _spira_bd_mtime="$(stat -c '%Y %s' "$SPIRA_BD" 2>/dev/null || true)"
    _spira_schema_cached=0
    if [ -n "$_spira_bd_mtime" ] && [ -f "$_spira_schema_stamp" ]; then
        _spira_stamp_val="$(cat "$_spira_schema_stamp" 2>/dev/null || true)"
        [ "$_spira_stamp_val" = "$_spira_bd_mtime" ] && _spira_schema_cached=1
    fi
    if [ "$_spira_schema_cached" = 0 ]; then
        _spira_bd_out="$(timeout 30 "$SPIRA_BD" -C "$SPIRA_DB" migrate schema 2>&1)"
        _spira_bd_rc=$?
        _spira_schema_ok=0
        if [ "$_spira_bd_rc" -eq 0 ]; then
            _spira_schema_ok=1
        elif grep -q 'dolt_server_port.*deprecated' <<< "$_spira_bd_out"; then
            # A deprecated field in metadata.json; schema is unaffected (sp-lh8r).
            _spira_schema_ok=1
        elif grep -q 'locked by another dolt process' <<< "$_spira_bd_out"; then
            # Lock contention means the store is in embedded mode — one exclusive lock,
            # many waiters. Embedded mode is refused by doctor.sh; run it to diagnose.
            printf 'spira: bd locked — store is in embedded mode (dolt_mode); run doctor.sh\n' >&2
            [ -z "${SPIRA_DOCTOR:-}" ] && { unset _spira_bd_out _spira_bd_rc _spira_schema_ok; exit 1; }
        else
            _spira_bd_db="$(printf '%s\n' "$_spira_bd_out" | grep -oE 'database is at v[0-9]+' | grep -oE '[0-9]+')"
            _spira_bd_bin="$(printf '%s\n' "$_spira_bd_out" | grep -oE 'binary knows up to v[0-9]+' | grep -oE '[0-9]+')"
            # Report both versions, rendering ? when one cannot be read. (sp-1khst)
            if [ -n "${_spira_bd_db:-}" ] || [ -n "${_spira_bd_bin:-}" ]; then
                printf 'spira: bd schema mismatch — database is at v%s, %s knows up to v%s\n' \
                    "${_spira_bd_db:-?}" "$SPIRA_BD" "${_spira_bd_bin:-?}" >&2
                printf 'spira: rebuild bd at v%s or set SPIRA_BD in %s\n' \
                    "${_spira_bd_db:-?}" "${SPIRA_CONF_FILE:-spira.conf}" >&2
            else
                printf 'spira: bd migrate schema failed — %s\n' \
                    "$(printf '%s\n' "$_spira_bd_out" | head -1)" >&2
                printf 'spira: bd is %s\n' "$SPIRA_BD" >&2
            fi
            unset _spira_bd_db _spira_bd_bin
            # SPIRA_DOCTOR=1 means doctor.sh is the caller; it runs its own schema check.
            [ -z "${SPIRA_DOCTOR:-}" ] && { unset _spira_bd_out _spira_bd_rc _spira_schema_ok; exit 1; }
        fi
        unset _spira_bd_out _spira_bd_rc
        # Write the stamp only after a successful check (best-effort; failure is silent).
        if [ "${_spira_schema_ok:-0}" = 1 ] && [ -n "$_spira_bd_mtime" ] && mkdir -p "$SPIRA_RUN" 2>/dev/null; then
            printf '%s\n' "$_spira_bd_mtime" > "$_spira_schema_stamp" 2>/dev/null || true
        fi
        unset _spira_schema_ok
    fi
    unset _spira_schema_stamp _spira_bd_mtime _spira_schema_cached _spira_stamp_val
fi

# BD_IGNORE_SCHEMA_SKEW WAS EXPORTED HERE AND IS GONE, because the recovery it was waiting on
# has run. The database was at schema v61, migrated by the accidental v1.2.0/v1.2.1 release,
# and every bd newer than v1.1.0 refused it outright — a refusal that EXITS 0 with the
# complaint on stdout, so a caller checking status read success and parsed an error as data.
# On 2026-09-08 a CGO rebuild of bd landed in ~/.local/bin, fayth_ready returned nothing, the
# sentinel logged "no fayth in the chamber" for every persona, and Spira summoned nothing for
# six minutes while the panes showed 0 ready rather than a fault.
#
# The rollback (sp-6ylz, 2026-09-08 18:14) ran and was reverted three minutes later at 18:17:
# at v53 every write to the production database failed. The cursor has been at v61 since 18:17.
# Reads and writes work without BD_IGNORE_SCHEMA_SKEW because this bd is a CGO_ENABLED=0
# (server-mode) dev build from main that knows all 61 migrations — NOT because the cursor
# moved. The rule the scar teaches: bd's version string does not order against release tags.
# A dev build from main knows MORE migrations than a tagged release (v1.2.2 knows only 53),
# so pin by migration count, never by version string. See spira/bd-pin.sh and SPIRA_BD_PIN.
# The revert is in the dolt log of database spira: commit piud5v03ri22ktnshufhl5dk09m24f5b,
# 'revert: restore schema cursor to v61 — rollback broke writes (bd 1.1.0 knows 61, not 53)'.

# --------------------------------------------------------------------------------------
# WHAT IS EXPORTED, AND WHAT MUST NEVER BE.
#
# EXPORTED, because the readers are not all shell: a python one-liner, an aeon's session and
# the Rust attention panel are all children of a process that sourced this file, and the
# alternative — threading each value through argv at every call site — is where one gets
# dropped and the tool silently reads a different database.
#
# NOT EXPORTED: EVERYTHING DERIVED FROM WHERE THIS FILE SITS. SPIRA_HOME, SPIRA_REPO, the
# maps, the chamber, the cockpit, the runtime directory. Those differ per COPY of the
# harness, and anything that sources its own conf.sh must resolve them from its own location.
# Exporting them cost eleven tests in one go: a fixture library, sourced by a suite for its
# database helpers, published the live installation's SPIRA_HOME into the environment of the
# sentinel that suite then ran under a temporary one — so the sentinel read the operator's
# REAL repo-map, found none of the fixture's repositories, and landed nothing, while every
# log line it printed looked ordinary (law-gates-run-in-a-clean-environment).
#
# It is safe to export the rest precisely because the landing gate runs its trial under
# `env -i`: configuration reaches the harness's own children and stops at the boundary of
# anything being judged.
#
# SPIRA_FAYTHS and SPIRA_MAX_AEONS are also not exported. They are policy for THIS host, read
# in-process, and exporting SPIRA_FAYTHS is the scar that statute is named for — it reached a
# test suite through systemd and made the suite assert the host's roster instead of the
# defaults it was written to check.

# --------------------------------------------------------------------------------------
# THE EXIT STATUS THAT MEANS "NO VERDICT", as opposed to "failed". The landing gate withholds
# a verdict when it cannot obtain the tree it judges in, and a caller that reads that as a red
# gate charges a queue to the branch — reopening a finished bead as having "failed the landing
# gate", three of which poison it and escalate to the operator over a lock it never contended
# for. It is a shared constant rather than a literal in the gate and again in its caller,
# because two programs that disagree about this number fail in exactly that direction.
#
# DELIBERATELY NOT SETTABLE, and so not in SPIRA_CONF_KEYS: it is the protocol between the
# gate and whoever runs it, not a fact about a host. A configurable one could be set to 0,
# which would turn every withheld verdict into a pass. 75 is EX_TEMPFAIL — "try again" —
# and is outside the range a gate command of its own would return.
SPIRA_GATE_NOVERDICT=75

# FOUR OUTCOMES, AND WHOSE FAULT EACH ONE IS. Every judgement in this harness returns exactly
# one of these, and the caller's whole decision follows from which. Before this there were
# two — 0 and "non-zero" — so a gate that timed out, a gate that could not find its own
# worktree, and a repository whose suites fail on its own base were all delivered to the
# landing pass as "this branch is broken". Each was then fixed by adding one more special
# case at the caller, and the shape recurred five times in two days (sp-p4rl, sp-d21,
# sp-io5j, sp-snyj, sp-1aex).
#
#   PASS       0   judged, and good                     -> land it
#   FAIL       1   judged, and bad                      -> the BRANCH is at fault
#   BASE_FAIL 76   the same suites fail on the base     -> the BASE is at fault
#   NO_VERDICT 75  not judged at all                    -> the MACHINERY is at fault
#
# THE RULE THAT MATTERS: BASE_FAIL and NO_VERDICT may never reopen a bead and never charge an
# attempt. Three charged attempts poison a bead and escalate to the operator, so a lock, a
# timeout or somebody else's red main could — and repeatedly did — walk finished work to
# poison and then report it as the branch's failure.
#
# A JUDGEMENT WITH NO EVIDENCE IS NO_VERDICT BY CONSTRUCTION. A gate killed at its deadline
# printed an empty `tail -20`, which arrived as a bare "failed" with nothing in it to act on
# (sp-p4rl); the one refusal path that printed nothing at all did the same (sp-io5j). FAIL
# has to be able to show its work or it is not a FAIL.
#
# 76 is EX_UNAVAILABLE — "the service is not available" — which is precisely the claim: the
# base this branch must merge into is not in a fit state to judge against.
SPIRA_GATE_BASEFAIL=76

# The names, for logs and for the bead notes a human reads. Keyed by status so that a caller
# that has a number can always render the word, and one place decides the wording.
spira_gate_outcome() {   # spira_gate_outcome <status> -> PASS|FAIL|BASE_FAIL|NO_VERDICT
    case "${1:-}" in
        0)  echo PASS ;;
        75) echo NO_VERDICT ;;
        76) echo BASE_FAIL ;;
        *)  echo FAIL ;;
    esac
}

# Does this outcome say the BRANCH is at fault? The one question every caller actually asks,
# in one place, so that "may I reopen the bead and charge an attempt?" cannot drift between
# the landing pass, the sentinel and any future caller.
spira_gate_blames_branch() {   # spira_gate_blames_branch <status> -> 0 if the branch is at fault
    case "${1:-}" in
        0|75|76) return 1 ;;
        *)       return 0 ;;
    esac
}

# --------------------------------------------------------------------------------------
export SPIRA_INSTANCE \
       SPIRA_DB COCKPIT_DB COCKPIT_BOTTOM_PCT COCKPIT_MAIL COCKPIT_RIGHT_PCT COCKPIT_MOUSE COCKPIT_CWD COCKPIT_CLIENT_IDLE_SECS SPIRA_PATH SPIRA_GOAL \
       SPIRA_WORKSPACES SPIRA_OPERATOR SPIRA_OPERATOR_ACTOR SPIRA_TZ SPIRA_ASK_LABEL SPIRA_RECLAIM_SKIP_LABEL \
       SPIRA_CI_LABEL SPIRA_CI_PARK_MAX \
       SPIRA_TESTDB_BD SPIRA_TESTDB_DATA SPIRA_TESTDB_PORT SPIRA_INCIDENT_PRIORITY SPIRA_TESTENV_REGISTRY SPIRA_GH_INTAKE_REPO SPIRA_GH_INTAKE_PRIORITY SPIRA_GH_INTAKE_BEAD_REPO SPIRA_FLAKY_GH_REPO \
       SPIRA_LAND_MAXSEC SPIRA_LAND_GATE_RESERVE \
       SPIRA_LOOM_ADDR SPIRA_LOOM_BUDGET_MS SPIRA_LOOM_CACHE_S SPIRA_LOOM_BIN SPIRA_BROKER_BIN SPIRA_RUN SPIRA_SYSTEMCTL \
       SPIRA_SPIKE_LABEL SPIRA_SPIKE_DIR SPIRA_SPIKE_PATHS SPIRA_SCOPE_LABEL \
       SPIRA_PLAN_LABEL SPIRA_INCIDENT_LABEL SPIRA_NO_LOOP_LABEL \
       SPIRA_TOWN SPIRA_MIRROR SPIRA_EXPORTER SPIRA_DESIGN SPIRA_WIKI SPIRA_WIKI_HOOK SPIRA_DOLT_DATA \
       SPIRA_ALERT_GLOB \
       SPIRA_GATE_NOVERDICT SPIRA_GATE_BASEFAIL \
       SPIRA_BD SPIRA_CONF_FILE SPIRA_PROD SPIRA_CTRL \
       SPIRA_RELEASES \
       SPIRA_REVIEWER_VERDICTS SPIRA_REVIEWER_MODEL SPIRA_REVIEW_LABEL

# --------------------------------------------------------------------------------------
# NAME WHAT IS MISSING. A harness that dies with `bd: command not found` from a timer has
# told the operator nothing: not which program, not what it is for, not where to get it.
#
# `spira_require` is cheap enough to call at the top of anything — it is a `command -v` —
# and it is the only reason a fresh box gets a sentence instead of a shell error.
# --------------------------------------------------------------------------------------
spira_require() {        # spira_require <bin> [<bin>...] -> 0, or 1 having named each one
    local b missing=""
    for b in "$@"; do command -v "$b" >/dev/null 2>&1 || missing="$missing $b"; done
    [ -z "$missing" ] && return 0
    for b in $missing; do
        printf 'spira: required program not found on PATH: %s — %s\n' \
            "$b" "$(spira_bin_purpose "$b")" >&2
    done
    printf 'spira: PATH is %s\n' "$PATH" >&2
    printf 'spira: if it is installed elsewhere, set SPIRA_PATH in %s\n' \
        "${SPIRA_CONF_FILE:-spira.conf}" >&2
    return 1
}

# --------------------------------------------------------------------------------------
# THE DEPENDENCY MANIFEST — three functions over one list of programs.
#
#   spira_bin_purpose <bin>   what it is for
#   spira_bin_tier    <bin>   runtime | optional | dev
#   spira_bin_absent  <bin>   what actually happens on a box without it
#
# WHY A TIER. "Dependency" conflated two different things: what is needed to RUN the loop
# and what is needed to DEVELOP it. Nothing recorded the difference, so the development set
# was never checked anywhere — and a program that only the test path uses could go missing
# on a box that looked, by every check that existed, completely healthy.
#
# WHY A THIRD FIELD FOR ABSENCE. `tier` says who needs it; it does not say what its absence
# DOES, and those come apart in the case that matters. A missing program usually fails
# loudly or turns one named feature off. But `bd-embedded` missing does neither: the fixture
# builder silently falls back to a shared Dolt server, which is not "tests off" — it is
# tests running on the architecture that was deleted for failing 5 of 6 concurrent builds.
# A downgrade nobody announces is indistinguishable from health, and stayed that way here
# for a week while it produced a hundred beads that read as ordinary test failures.
#
# So: anything whose absence CHANGES BEHAVIOUR rather than stopping it must say so here,
# and doctor.sh prints that sentence rather than a generic "not found".
# --------------------------------------------------------------------------------------

# Every program the harness or its tests invoke, in one list. doctor.sh iterates this rather
# than carrying its own copy — two lists is how the development set came to be checked by
# nothing at all.
SPIRA_BINS="${SPIRA_BINS:-bd git python3 flock dolt gh tmux node cargo jq zstd bd-embedded podman go inotifywait aerc hunk}"

spira_bin_tier() {
    case "$1" in
        # runtime: the loop cannot run at all.
        bd|git|python3|flock)      echo runtime ;;
        # optional: the loop runs; one named feature is off.
        dolt|gh|tmux|node|cargo|jq|zstd) echo optional ;;
        # operator: needed on an operated instance (SPIRA_OPERATED=1). doctor.sh FAILs
        # when these are missing; SPIRA_OPERATED=0 downgrades to WARN for headless boxes.
        inotifywait|aerc|hunk|go)  echo operator ;;
        # dev: needed to DEVELOP or TEST Spira, never to run it.
        bd-embedded|podman)        echo dev ;;
        *)                         echo optional ;;
    esac
}

# What a box without this program actually does. Empty means the ordinary case — it fails
# loudly, or the one feature named in spira_bin_purpose is simply off.
spira_bin_absent() {
    case "$1" in
        bd-embedded)
            echo "test fixtures fall back to a shared Dolt server instead of a private embedded store per fixture. Not a feature off: concurrent fixture builds contend on one schema lock, measured at 610s with 5 of 6 failing, against 25s with 0 failing on embedded. Suites fail in ways that read as defects in the code under test. Restore with build-bd.sh --install (needs a Go toolchain)." ;;
        podman)
            echo "every suite that builds a container fixture cannot run; testenv.sh and the suites that use it fail rather than skip." ;;
        inotifywait)
            echo "no mail is delivered to registered readers mid-session — escalations and verdict replies wait for the next session start." ;;
        *) echo "" ;;
    esac
}

spira_bin_purpose() {
    case "$1" in
        bd)      echo "the beads issue tracker — the substrate; nothing runs without it" ;;
        bd-embedded) echo "the CGO build of bd that opens an embedded Dolt store — the test fixture engine" ;;
        podman)  echo "container fixtures for the suites that need a whole machine" ;;
        go)      echo "building the pinned bd from source when no prebuilt exists for this platform; without it a bd mismatch cannot be recovered locally" ;;
        dolt)    echo "the SQL server beads stores its database in" ;;
        git)     echo "every repository operation" ;;
        gh)      echo "opening and landing pull requests (repos whose land mode is 'pr')" ;;
        claude)  echo "the agent an aeon is a session of" ;;
        tmux)    echo "the cockpit panes" ;;
        python3) echo "every JSON payload this harness parses" ;;
        cargo)   echo "building the decisions panel; not needed to run the loop" ;;
        node)    echo "gating the browser page's view model; the loop itself never needs it" ;;
        jq)      echo "optional JSON convenience" ;;
        flock)   echo "serialising writers that share one path — the transcript archive, and the landing gate's per-repository tree" ;;
        zstd)    echo "compressing archived transcripts; gzip is used when it is absent" ;;
        inotifywait) echo "delivering mail the moment it arrives (inotify-tools); without it mail waits for the next session start" ;;
        hunk)    echo "the review pane: designs and diffs are read and commented on in hunk (npm hunkdiff, needs node)" ;;
        aerc)    echo "the operator's mail client for reading and answering; any Maildir client works" ;;
        *)       echo "required by the harness" ;;
    esac
}

# watch_unit_name <watcher-name> -> the installed systemd unit name for a daemon watcher.
#
# MIRRORS inst_watch_name IN systemd/units.sh — one formula, two callers. A watcher named
# "answers" installs as spira-watch-answers-prod.service (not spira-watch@answers.service,
# which is the template form and is never instantiated). Querying the template form always
# returns 'inactive', so a stopped watcher and a running watcher are indistinguishable.
# Every caller that queries or restarts a watcher unit goes through this function so the two
# cannot drift.
watch_unit_name() { printf 'spira-watch-%s-%s.service' "$1" "${SPIRA_INSTANCE:-prod}"; }

# spira_unit <base> [service|timer] -> the unit name for this installation.
# Tries the instance-qualified form first (spira-<base>-<instance>.<type>); if that
# unit is not loaded (neither enabled nor active), falls back to the plain form.
# Returns '?' if neither form is known to systemd — a unit that cannot be found must
# not be queried for health, which would report 'inactive' about an unrelated subject
# (law-absence-needs-a-positive-control). Callers must treat '?' as unknown state.
#
# This is the same resolution the TIMER_BASES loop in world.sh uses, extracted so that
# every caller agrees on which name to address rather than each hard-coding one form.
spira_unit() {
    local base="$1" t="${2:-service}"
    local SC="${SPIRA_SYSTEMCTL:-systemctl}"
    local inst="spira-${base}${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.${t}"
    local plain="spira-${base}.${t}"
    if "$SC" --user is-enabled "$inst" >/dev/null 2>&1 ||
       "$SC" --user is-active  "$inst" >/dev/null 2>&1; then
        printf '%s' "$inst"
    elif "$SC" --user is-enabled "$plain" >/dev/null 2>&1 ||
         "$SC" --user is-active  "$plain" >/dev/null 2>&1; then
        printf '%s' "$plain"
    else
        printf '?'
    fi
}
