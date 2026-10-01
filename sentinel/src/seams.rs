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
/// so stdout stays the NUL-separated record stream.
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
///
/// The repositories (audit's own need) are ALSO no longer this script's job (sp-k6lku,
/// "wave 4.13"): once `main::probe` has merged the resolved config above, `Context.vars`
/// carries `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED` correctly
/// resolved, and `main::resolve_repos` builds a `spira_config::repos::Registry` from that
/// same snapshot in-process instead of this script shelling into `repo_root`/
/// `spira_landrefs`/`repo_land_queued`/`spira_repos` (themselves, since sp-37rmg/sp-o88bx,
/// lib.sh shims that only re-shelled into the `spira-config` binary) once per mapped
/// repository.
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
printf '@end\0'
"#;

/// S1/S2 RETIRED (wave 4.27, family G, sp-gzmd2): the world gate and CHECK 7's whole
/// summon loop (`world_gate`, `ck7_summon_pass`, `summon_fayth`, `summon_argv`) now run
/// in-process — `pass::Sentinel::world_gate`/`ck7_summon_pass` in `summon.rs`, under a
/// real OS flock on `summon.lock` rather than a bash seam under one. lib.sh's own copies
/// are one-line shims onto `sentinel --world-gate`/`--summon`/`--summon-argv`/
/// `--named-unit-stop`, kept only for `aeon --escape`'s seam and `acceptance-local.sh`.

// S4 (CHECK 3b: mark_queue_waiters/close_landed_queue_waiters) and S7–S10 (CHECK 7c/7d's
// detectors) are retired (wave 4.28, sp-fbqsv): native now, in waiters.rs and detect.rs.

/// S5 — spira_event: four NUL-terminated fields on stdin (kind, target, title, detail).
pub const EVENT: &str = r#"IFS= read -r -d '' _k; IFS= read -r -d '' _t; IFS= read -r -d '' _ti; IFS= read -r -d '' _de
spira_event "$_k" "$_t" "$_ti" "$_de" || true"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// G9: no seam interpolates a Rust value; every one is a constant with the prelude.
    #[test]
    fn seams_are_constants_with_the_prelude() {
        for body in [EVENT] {
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
