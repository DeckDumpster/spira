//! The lib.sh seams (DESIGN.md §6). Each is a FIXED script run as `bash -c '<script>'
//! sentinel-<name>`: never assembled from data; data arrives on stdin; the environment
//! carries only configuration and file paths (law-payloads-go-on-stdin).
//!
//! The prelude sources lib.sh and lc.sh the way sentinel.sh did, and redefines `act` and
//! `progress` — which lib.sh functions call, and which sentinel.sh used to define — to
//! append to $SENTINEL_TALLY, so the counters keep their in-process semantics. Loading
//! lib.sh is silent here: the probe (S0) already surfaced whatever conf.sh had to say once
//! this pass, and a seam that cannot load it exits 97, which the caller names.

pub const PRELUDE: &str = r#"set -uo pipefail
. "$SENTINEL_LIB" >/dev/null 2>&1 || exit 97
. "${SENTINEL_LIB%/*}/lc.sh" >/dev/null 2>&1 || exit 97
act()      { printf 'act\t%s\n' "$*" >> "$SENTINEL_TALLY"; log "ACT $*"; }
progress() { printf 'progress\t%s\n' "$*" >> "$SENTINEL_TALLY"; log "ACT $*"; }
"#;

pub fn script(body: &str) -> String {
    format!("{PRELUDE}{body}\n")
}

/// S0 — the context probe. Read-only: the environment conf.sh resolved, the fixed list of
/// lib.sh variables the binary needs, the roster, and (audit) the repositories. lib.sh's
/// own chatter goes to stderr so stdout stays the NUL-separated record stream.
pub const PROBE: &str = r#"set -uo pipefail
. "$SENTINEL_LIB" >&2 || exit 97
. "${SENTINEL_LIB%/*}/lc.sh" >&2 || exit 97
env -0
printf '@vars\0'
for _v in SPIRA_POISON_ASKED SPIRA_REQUEUE_ASKED SPIRA_RECLAIM_ASKED SPIRA_POISON_LIFTED \
          SPIRA_ROSTER_WARN_STAMP SPIRA_CAPACITY_PAUSE; do
    printf '%s=%s\0' "$_v" "${!_v:-}"
done
printf 'SPIRA_HOME_REPO_RESOLVED=%s\0' "$(spira_home_repo)"
printf 'SPIRA_TOML_FILE=%s\0' "${SPIRA_TOML_FILE:-}"
printf '@fayths\0'
for _f in $(spira_fayths); do
    printf '%s\t%s\t%s\0' "$_f" "$(fayth_get "$_f" FAYTH_LABELS)" "$(fayth_get "$_f" FAYTH_EXCLUDE_LABELS)"
done
printf '@partitions\0'
while IFS= read -r _l; do [ -n "$_l" ] && printf '%s\0' "$_l"; done < <(fayth_partitions)
printf '@chamber\0'
while IFS= read -r _l; do [ -n "$_l" ] && printf '%s\0' "$_l"; done < <(fayth_names)
if [ "${SENTINEL_PROBE_REPOS:-0}" = 1 ]; then
    printf '@repos\0'
    while IFS= read -r _r; do
        [ -n "$_r" ] || continue
        _root="$(repo_root "$_r" 2>/dev/null)" || _root=""
        _refs=""
        [ -n "$_root" ] && { _refs="$(spira_landrefs "$_root" 2>/dev/null)" || _refs=""; }
        _q=0; repo_land_queued "$_r" && _q=1
        printf '%s\t%s\t%s\t%s\0' "$_r" "$_root" "$_refs" "$_q"
    done < <(spira_repos 2>/dev/null)
fi
printf '@end\0'
"#;

/// S1 — summon-only's gate: the world (halt/drain) and the account's capacity window.
/// Exit 0 = go on; the seam logs its own reason for stopping.
pub const SUMMON_GATE: &str = r#"world_gate fleet summon-only || exit 1
if capacity_paused; then
    log "summon-only: account out of capacity for another ${SPIRA_CAPACITY_LEFT}s — not summoning"
    exit 1
fi
exit 0"#;

/// S2 — CHECK 7: the lane-then-pool summon loop under summon.lock.
pub const CK7: &str = "ck7_summon_pass";

/// S3 — land_escalate: line 1 of stdin is the subject tail, the rest the evidence.
pub const LAND_ESCALATE: &str = r#"IFS= read -r _why
_ev="$(cat)"
land_escalate "$_why" "$_ev""#;

/// S4 — CHECK 3b/3c: queue-mode dependents and coordination beads with open children.
pub const CHECK3B: &str = r#"mark_queue_waiters 2>/dev/null || true
close_landed_queue_waiters 2>/dev/null || true
mark_open_children 2>/dev/null || true"#;

/// S5 — spira_event: four NUL-terminated fields on stdin (kind, target, title, detail).
pub const EVENT: &str = r#"IFS= read -r -d '' _k; IFS= read -r -d '' _t; IFS= read -r -d '' _ti; IFS= read -r -d '' _de
spira_event "$_k" "$_t" "$_ti" "$_de" || true"#;

/// S6 — trace_tail: line 1 the session log path, line 2 the line count.
pub const TRACE_TAIL: &str = r#"IFS= read -r _f; IFS= read -r _n
trace_tail "$_f" "$_n""#;

/// S7 — CHECK 7c's detector.
pub const DETECT_UNCLAIMABLE: &str = "detect_unclaimable_ready 2>/dev/null";
/// S8 — one incident per unclaimable bead; S7's output on stdin.
pub const FILE_UNCLAIMABLE: &str = r#"file_unclaimable_incidents "$(cat)""#;
/// S9 — CHECK 7d's detector.
pub const DETECT_COLLISIONS: &str = "detect_branch_collisions 2>/dev/null";
/// S10 — free, un-label or park each collision; S9's output on stdin.
pub const PARK_COLLISIONS: &str = r#"park_branch_collisions "$(cat)""#;

#[cfg(test)]
mod tests {
    use super::*;

    /// G9: no seam interpolates a Rust value; every one is a constant with the prelude.
    #[test]
    fn seams_are_constants_with_the_prelude() {
        for body in [
            SUMMON_GATE,
            CK7,
            LAND_ESCALATE,
            CHECK3B,
            EVENT,
            TRACE_TAIL,
            DETECT_UNCLAIMABLE,
            FILE_UNCLAIMABLE,
            DETECT_COLLISIONS,
            PARK_COLLISIONS,
        ] {
            let s = script(body);
            assert!(
                s.starts_with("set -uo pipefail\n. \"$SENTINEL_LIB\" >/dev/null 2>&1 || exit 97")
            );
            assert!(s.contains("act()      { printf 'act\\t%s\\n' \"$*\" >> \"$SENTINEL_TALLY\""));
        }
        assert!(PROBE.contains("printf '@end\\0'"));
    }

    /// The seams run for real against a stub lib.sh: stdin reaches the function, the tally
    /// records act/progress, and nothing travels in argv.
    #[test]
    fn seam_protocol_against_a_stub_lib() {
        let d = std::env::temp_dir().join(format!("sentinel-seam-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("lib.sh"),
            "log() { printf 'LOG %s\\n' \"$*\"; }\nland_escalate() { printf 'why=%s ev=%s\\n' \"$1\" \"$2\"; act escalated; progress moved; }\n",
        )
        .unwrap();
        std::fs::write(d.join("lc.sh"), "").unwrap();
        let tally = d.join("tally");
        let o = std::process::Command::new("bash")
            .args(["-c", &script(LAND_ESCALATE), "sentinel-land-escalate"])
            .env("SENTINEL_LIB", d.join("lib.sh"))
            .env("SENTINEL_TALLY", &tally)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin
                    .take()
                    .unwrap()
                    .write_all(b"the leg is down\nline one\nline two\n")?;
                c.wait_with_output()
            })
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(
            out.contains("why=the leg is down ev=line one\nline two"),
            "{out}"
        );
        assert!(out.contains("LOG ACT escalated"));
        assert_eq!(
            std::fs::read_to_string(&tally).unwrap(),
            "act\tescalated\nprogress\tmoved\n"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
