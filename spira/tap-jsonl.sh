# tap-jsonl.sh — turn one suite's captured output into results.jsonl rows. Sourced,
# never executed.
#
# ONE PARSER, ONE PLACE (same reasoning as suite-covers.sh, which this sources): gate-
# diag.sh and suites.sh both need this and neither should carry its own copy.
#
# A suite that has not migrated to testlib.sh (migration is sp-qvjzb) emits no TAP.
# tap_jsonl_rows falls back to one row for the whole suite, built from the caller's own
# classification of the suite's exit — so every suite has a results.jsonl row on day
# one, and migrating a suite to testlib.sh only adds per-case rows where there was one.
#
# covers: spira/tap-jsonl.sh spira/gate-diag.sh spira/suites.sh
_tj_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$_tj_dir/suite-covers.sh"
unset _tj_dir

_tap_json_escape() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\t'/\\t}"
    printf '%s' "$s"
}

_tap_uc_json() {  # _tap_uc_json <space-separated UC ids> -> a JSON array
    local uc="$1"
    [ -n "$uc" ] || { printf '[]'; return 0; }
    printf '%s' "$uc" | awk '{
        printf "["
        for (i=1;i<=NF;i++) printf "%s\"%s\"", (i>1?",":""), $i
        printf "]"
    }'
}

# tap_jsonl_rows <suite-basename> <suite-source-file> <out-file> <fallback-status> <seconds>
# Prints one JSONL row per line of stdout: one per TAP case when <out-file>'s first line
# is "TAP version 14", else a single suite-level row (case: null) built from
# <fallback-status>, which the caller derives from its own .result vocabulary — this
# function does not interpret status codes it was not given directly.
tap_jsonl_rows() {
    local suite="$1" src="$2" out="$3" fallback="$4" secs="$5"
    local tier uc uc_json
    tier="$(suite_tier_of "$src")"
    uc="$(suite_uc_of "$src")"
    uc_json="$(_tap_uc_json "$uc")"

    if [ -r "$out" ] && head -1 "$out" 2>/dev/null | grep -qxF 'TAP version 14'; then
        awk -v suite="$(_tap_json_escape "$suite")" -v tier="$(_tap_json_escape "$tier")" \
            -v uc="$uc_json" -v secs="$secs" '
        function esc(s) { gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); return s }
        function emit(case_id, status, detail) {
            printf "{\"suite\":\"%s\",\"tier\":\"%s\",\"case\":\"%s\",\"status\":\"%s\",\"seconds\":%s,\"uc\":%s,\"detail\":\"%s\"}\n", \
                suite, tier, esc(case_id), status, secs, uc, esc(detail)
        }
        /^ok [0-9]+ - / {
            if (pending) emit(pcase, pstat, pdet)
            pending = 1; pstat = "pass"; pdet = ""
            pcase = $0; sub(/^ok [0-9]+ - /, "", pcase)
            next
        }
        /^not ok [0-9]+ - / {
            if (pending) emit(pcase, pstat, pdet)
            pending = 1; pstat = "fail"; pdet = ""
            pcase = $0; sub(/^not ok [0-9]+ - /, "", pcase)
            next
        }
        /^# / {
            if (pending && pstat == "fail" && pdet == "") { pdet = $0; sub(/^# /, "", pdet) }
            next
        }
        /^1\.\.0 # SKIP / {
            if (pending) emit(pcase, pstat, pdet)
            pending = 0
            detail = $0; sub(/^1\.\.0 # SKIP /, "", detail)
            emit("(suite)", "skip", detail)
            next
        }
        /^Bail out! / {
            if (pending) emit(pcase, pstat, pdet)
            pending = 0
            detail = $0; sub(/^Bail out! /, "", detail)
            emit("(suite)", "bail", detail)
            next
        }
        {
            if (pending) { emit(pcase, pstat, pdet); pending = 0 }
        }
        END { if (pending) emit(pcase, pstat, pdet) }
        ' "$out"
        return 0
    fi

    local rstatus
    case "$fallback" in
        ok)                       rstatus="pass" ;;
        skip)                     rstatus="skip" ;;
        unreached)                rstatus="unreached" ;;
        timeout|red|quarantined-red) rstatus="fail" ;;
        *)                        rstatus="fail" ;;
    esac
    printf '{"suite":"%s","tier":"%s","case":"(suite)","status":"%s","seconds":%s,"uc":%s,"detail":"not migrated to testlib.sh"}\n' \
        "$(_tap_json_escape "$suite")" "$(_tap_json_escape "$tier")" "$rstatus" "$secs" "$uc_json"
}
