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
/// lib.sh variables the binary needs, and the roster. lib.sh's own chatter goes to stderr
/// so stdout stays the NUL-separated record stream. The repositories (audit's own need) are
/// no longer this script's job (sp-k6lku, "wave 4.13"): `@vars` already carries every
/// `SPIRA_*` key — `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED`
/// included — so `main::probe` builds a `spira_config::repos::Registry` from that same
/// snapshot in-process instead of this script shelling into `repo_root`/`spira_landrefs`/
/// `repo_land_queued`/`spira_repos` (themselves, since sp-37rmg/sp-o88bx, lib.sh shims that
/// only re-shelled into the `spira-config` binary) once per mapped repository.
pub const PROBE: &str = r#"set -uo pipefail
. "$SENTINEL_LIB" >&2 || exit 97
env -0
printf '@vars\0'
# EVERY SPIRA_* SHELL VARIABLE, EXPORTED OR NOT. conf.sh and lib.sh set many keys without
# exporting them (SPIRA_REPO, SPIRA_HOME, SPIRA_GH, the CPU quota…),
# and `env -0` above sees only exported ones — so a fixed list here silently dropped them and
# the binary fell back to defaults.
for _v in $(compgen -v SPIRA_); do
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

    /// The probe reports SPIRA_* variables lib.sh SETS BUT DOES NOT EXPORT: `env -0` alone
    /// dropped SPIRA_REPO and SPIRA_GH, and the binary fell back to defaults.
    #[test]
    fn probe_reports_unexported_spira_variables() {
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
        assert!(vars.contains("SPIRA_REPO=/the/repo\0"), "{out:?}");
        assert!(vars.contains("SPIRA_GH=/the/gh-app.sh\0"), "{out:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
