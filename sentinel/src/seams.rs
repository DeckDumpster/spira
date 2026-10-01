//! The lib.sh seams (DESIGN.md §6). Each is a FIXED script run as `bash -c '<script>'
//! sentinel-<name>`: never assembled from data; data arrives on stdin; the environment
//! carries only configuration and file paths (law-payloads-go-on-stdin).
//!
//! The prelude sources lib.sh the way sentinel.sh did (lc.sh is gone — lib.sh's lifecycle
//! calls are `spira-lc` caller verbs now, sp-arpjt), and redefines `act` and
//! `progress` — which lib.sh functions call, and which sentinel.sh used to define — to
//! append to $SENTINEL_TALLY, so the counters keep their in-process semantics. Loading
//! lib.sh is silent here: the probe (S0) already surfaced whatever conf.sh had to say once
//! this pass, and a seam that cannot load it exits 97, which the caller names.

pub const PRELUDE: &str = r#"set -uo pipefail
. "$SENTINEL_LIB" >/dev/null 2>&1 || exit 97
act()      { printf 'act\t%s\n' "$*" >> "$SENTINEL_TALLY"; log "ACT $*"; }
progress() { printf 'progress\t%s\n' "$*" >> "$SENTINEL_TALLY"; log "ACT $*"; }
"#;

pub fn script(body: &str) -> String {
    format!("{PRELUDE}{body}\n")
}

/// S0 — the context probe. Read-only: the environment conf.sh resolved, the fixed list of
/// lib.sh variables the binary needs, the roster, and (audit) the repositories. lib.sh's
/// own chatter goes to stderr so stdout stays the NUL-separated record stream.
///
/// UNTIL WAVE 4.8 this `@vars` section was `for _v in $(compgen -v SPIRA_); do ...`: every
/// SPIRA_* shell variable, exported or not (conf.sh/lib.sh set many without exporting them
/// — SPIRA_REPO, SPIRA_HOME, SPIRA_GH, the CPU quota — and `env -0` above sees only
/// exported ones). That compgen dump is RETIRED (wave4-decomposition.md row (b),
/// "sentinel PROBE @vars"): `crate::probe` (main.rs) now merges `spira_config::resolve()`'s
/// own in-process answer into the parsed `Context.vars` after this script returns, instead
/// of re-deriving the same values by shelling out a second time. `@vars` stays a section
/// header (now carrying only `SPIRA_HOME_REPO_RESOLVED`/`SPIRA_TOML_FILE`, still lib.sh's
/// own, not conf.sh's) rather than disappearing outright, so `Context::parse` and every
/// existing fixture that names the section order keep working unchanged.
pub const PROBE: &str = r#"set -uo pipefail
. "$SENTINEL_LIB" >&2 || exit 97
env -0
printf '@vars\0'
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

/// S1 — summon-only's gate: the world (halt/drain). Exit 0 = go on; the seam logs its own
/// reason for stopping. The capacity window used to be checked here too
/// (`capacity_paused`), but that call also ran `capacity_probe_maybe` and could delete
/// the pause file — a second probe owner alongside aeon's own (wave4-decomposition.md
/// (c)3: "if both callers' ports each probe, the cost doubles"). Wave 4.26 moves the
/// capacity check to `summon.rs`'s own in-process read (`aeon::capacity::pause_state`,
/// never mutating, never probing) right after this seam call returns.
pub const SUMMON_GATE: &str = r#"world_gate fleet summon-only || exit 1
exit 0"#;

/// S2 — CHECK 7: the lane-then-pool summon loop under summon.lock.
pub const CK7: &str = "ck7_summon_pass";

/// S4 — CHECK 3b: queue-mode dependents. (CHECK 3c, open children, is Rust: open_children.rs.)
pub const CHECK3B: &str = r#"mark_queue_waiters 2>/dev/null || true
close_landed_queue_waiters 2>/dev/null || true"#;

/// S5 — spira_event: four NUL-terminated fields on stdin (kind, target, title, detail).
pub const EVENT: &str = r#"IFS= read -r -d '' _k; IFS= read -r -d '' _t; IFS= read -r -d '' _ti; IFS= read -r -d '' _de
spira_event "$_k" "$_t" "$_ti" "$_de" || true"#;

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
            CHECK3B,
            EVENT,
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

    /// Wave 4.8 retired the `compgen -v SPIRA_` dump: `@vars` now carries only
    /// `SPIRA_HOME_REPO_RESOLVED`/`SPIRA_TOML_FILE` from the script itself —
    /// `crate::probe` (main.rs) merges `spira_config::resolve()`'s own answer into
    /// `Context.vars` afterward, in-process, rather than this script re-deriving it a
    /// second time by shelling out. See `main.rs`'s own
    /// `probe_merges_resolved_config_into_vars_without_shelling_a_second_time` for that
    /// merge's own test.
    #[test]
    fn vars_section_no_longer_dumps_the_whole_shell() {
        let d = testkit::TempDir::new("sentinel-probe");
        std::fs::write(
            d.join("lib.sh"),
            "SPIRA_REPO=/the/repo\nSPIRA_GH=/the/gh-app.sh\nspira_home_repo() { echo spira; }\nspira_fayths() { :; }\nfayth_partitions() { :; }\nfayth_names() { :; }\n",
        )
        .unwrap();
        let o = std::process::Command::new("bash")
            .args(["-c", PROBE])
            .env("SENTINEL_LIB", d.join("lib.sh"))
            .env_remove("SPIRA_REPO")
            .env_remove("SPIRA_GH")
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        let vars = out.split("@vars\0").nth(1).unwrap_or("");
        assert!(vars.starts_with("SPIRA_HOME_REPO_RESOLVED=spira\0SPIRA_TOML_FILE="), "{out:?}");
        assert!(!vars.contains("SPIRA_REPO=/the/repo\0"), "the bash dump must no longer carry conf vars: {out:?}");
        assert!(!vars.contains("SPIRA_GH=/the/gh-app.sh\0"), "{out:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
