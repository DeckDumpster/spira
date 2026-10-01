//! The lib.sh seam (DESIGN.md §5): `bash -c <FIXED>` with every datum on stdin.
//!
//! FIXED is a compile-time constant. It reads NUL-framed records from stdin — the lib.sh
//! path, the fayth file, the fayth name, the function, then its arguments — sources lib.sh
//! (and so conf.sh) and the fayth exactly as aeon.sh did at its top, refuses any function
//! not on its allowlist, and calls it. Inside bash a function's arguments are not a
//! process argv, so no payload size can hit E2BIG (law-payloads-go-on-stdin).
//!
//! `_aeon_base`/`_aeon_repo_info`, `qualify_base_ref`/`spira_landrefs`, and the bare
//! `spira_landref` sp-27d3d added for the heartbeat's fuse (concurrently with this bead)
//! are all dropped from the allowlist (sp-o88bx, "wave 4.12"): family W
//! (`spira_landref`/`ref_remote`/`ref_branch`/`qualify_base_ref`/`spira_landrefs`) now
//! resolves in-process through `spira_config::repos`, so `run.rs`/`verdict.rs`/`claim.rs`
//! no longer shell into this seam for it at all — not even through a lib.sh shim.
//!
//! `_aeon_capacity_paused`, `capacity_reset_at` and `capacity_pause_set` are dropped the
//! same way (wave 4.26, family K → `capacity.rs`): `run.rs`/`sweep.rs`/`escape.rs`/
//! `teardown.rs` call that module in-process now, and `lib.sh`'s own `capacity_*`
//! functions are one-line shims onto `aeon capacity <verb>` for the one bash caller left
//! (`capacity.sh`) — not reached through this seam at all.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::ports::{Env, Seam};
use crate::util::{self, Out, Spec};

pub const FIXED: &str = r#"set -uo pipefail
IFS= read -r -d '' __aeon_lib || exit 96
IFS= read -r -d '' __aeon_fayth_file || exit 96
IFS= read -r -d '' __aeon_fayth_name || exit 96
IFS= read -r -d '' __aeon_fn || exit 96
__aeon_args=()
while IFS= read -r -d '' __aeon_a; do __aeon_args+=("$__aeon_a"); done
case "$__aeon_fn" in
    _aeon_snapshot|_aeon_rebase|\
    _aeon_thrash_meta|_aeon_world_gate|_aeon_fayth_ready|_aeon_summon_argv|\
    aeon_name_take|aeon_count|fayth_free|spira_event|release_own_claim|lc_claim_bead|\
    lc_bead_verified|park_unmapped|\
    spira_prune_worktrees|bead_reopen|bump_requeue|\
    bump_lapsed|write_lapse_record|thrash_streak_bump|requeues_of|\
    land_state|\
    land_mark|bead_is_work_type|bead_cited_commit_on_base|\
    other_beads_on_conflicts|spira_destroy_branch) ;;
    *) printf 'aeon seam: %s is not on the allowlist\n' "$__aeon_fn" >&2; exit 97 ;;
esac
. "$__aeon_lib" || exit 98
if [ -n "$__aeon_fayth_file" ]; then
    # shellcheck disable=SC1090
    . "$__aeon_fayth_file" || exit 98
fi
FAYTH="$__aeon_fayth_name"
[ -n "${SPIRA_REQUIRE_LABEL:-}" ] && FAYTH_LABELS="${FAYTH_LABELS:+$FAYTH_LABELS,}$SPIRA_REQUIRE_LABEL"

_aeon_snapshot() {
    env -0
    printf '__AEON_VARS__\0'
    local __v
    for __v in "$@"; do
        [ -n "${!__v+set}" ] && printf '%s\0%s\0' "$__v" "${!__v}"
    done
    printf '__AEON_READY__\0'
    local __r
    for __r in "${READY_ARGS[@]}"; do printf '%s\0' "$__r"; done
    printf '__AEON_EXCLUDE__\0'
    printf '%s\0' "$(fayth_exclude "$FAYTH" "${FAYTH_EXCLUDE_LABELS:-}")"
}
_aeon_world_gate() {
    world_gate "$FAYTH" "$1" >&2; local __rc=$?
    return $__rc
}
_aeon_fayth_ready() {
    fayth_ready "$FAYTH"
}
_aeon_summon_argv() {
    summon_argv "$FAYTH"
}
_aeon_rebase() {
    rebase_branch "$@" >&2; local __rc=$?
    printf '%s' "${REBASE_CONFLICTS:-}"
    return $__rc
}
_aeon_thrash_meta() {
    printf '%s\n' "$(bead_metadata "$1" thrash_streak)"
    printf '%s\n' "$(bead_metadata "$1" thrash_tip)"
    printf '%s\n' "$(bead_metadata "$1" thrash_last)"
}
"$__aeon_fn" "${__aeon_args[@]}"
"#;

/// The shell variables captured once at startup (exported or not), still read through the
/// bash seam. FAYTH_* come from the sourced fayth file; LANDSTATE/LAND_EVICTION_REASONS are
/// lib.sh's own derived values (family S, a later wave4 bead); SPIRA_HOME/SPIRA_REPO/
/// SPIRA_REPO_DERIVED are per-copy facts `spira_config::resolve()` never produces (see that
/// module's `ResolveInput` doc); the rest here (SPIRA_WORLD_STOP_SKIP, SPIRA_TOML_FILE,
/// SPIRA_MEMORIES_CMD, SPIRA_ALLOW_PROD_DIRTY, SPIRA_CLOSE_REASON_OVERRIDE,
/// SPIRA_WORKFLOW_RUN_CONSIDERED, BD_TIMEOUT, SPIRA_BDQ_CONN_RETRIES, SPIRA_BDJSON_FIXTURE,
/// SPIRA_TRACE_MARK, SPIRA_SUMMON, PATH, HOME, DB) have no `spira/conf.d/<KEY>` entry at
/// all — ad hoc overrides or lib.sh literals, never conf.sh's.
///
/// UNTIL WAVE 4.8 this list also carried every name conf.sh's own registry resolves
/// (SPIRA_RUN, SPIRA_DB, SPIRA_BD, SPIRA_WIKI, SPIRA_ASK_LABEL, SPIRA_MAX_AEONS, ...): `_aeon_snapshot`
/// read each one back out of the bash process that had just sourced conf.sh, a second,
/// bash-shaped derivation of values `main.rs`'s `merge_resolved_config` now computes
/// in-process and merges into `Snapshot.vars` after this seam call returns — see that
/// function's own doc for the list and for why SPIRA_HOME/SPIRA_REPO/SPIRA_REPO_DERIVED are
/// still inserted explicitly.
pub const SNAPSHOT_VARS: &[&str] = &[
    "SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_DERIVED", "LANDSTATE",
    "SPIRA_WORLD_STOP_SKIP",
    "SPIRA_MEMORIES_CMD",
    "SPIRA_TOML_FILE",
    "SPIRA_ALLOW_PROD_DIRTY", "SPIRA_CLOSE_REASON_OVERRIDE", "SPIRA_WORKFLOW_RUN_CONSIDERED",
    "BD_TIMEOUT", "SPIRA_BDQ_CONN_RETRIES", "SPIRA_BDJSON_FIXTURE",
    "SPIRA_TRACE_MARK", "LAND_EVICTION_REASONS", "SPIRA_SUMMON", "PATH", "HOME", "DB",
    "FAYTH_NAME", "FAYTH_LABELS", "FAYTH_EXCLUDE_LABELS", "FAYTH_MAX_CONCURRENT",
    "FAYTH_ELASTIC", "FAYTH_LEASE_MINUTES", "FAYTH_HEARTBEAT_SECONDS", "FAYTH_TIMEOUT_SECONDS",
    "FAYTH_MEMORY_PREFIXES", "FAYTH_STATUTE_CORE", "FAYTH_TOOLS", "FAYTH_PROJECT_INSTRUCTIONS",
    "FAYTH_SYSTEM_PROMPT", "FAYTH_SOP_REQUIRED", "FAYTH_GROOM_ESCALATION_CHECK",
    "FAYTH_GRAPH_ONLY",
];

/// Every `SNAPSHOT_VARS` name wave 4.8 retired from the bash seam call, resolved
/// in-process instead (`main.rs`'s `merge_resolved_config`) — kept as its own list so a
/// parity check can diff this crate's old and new answers key by key.
pub const RETIRED_SNAPSHOT_VARS: &[&str] = &[
    "SPIRA_RUN", "SPIRA_DB", "SPIRA_BD", "SPIRA_MAIL", "SPIRA_WIKI",
    "SPIRA_CHAMBER_OVERLAY", "SPIRA_TESTDB_LIB", "SPIRA_TESTDB_PORT", "SPIRA_WORLD_STOP_LABEL",
    "SPIRA_ASK_LABEL", "SPIRA_SUBMITTED_LABEL", "SPIRA_SCOPE_LABEL",
    "SPIRA_REPO_MAP", "SPIRA_HOME_REPO", "SPIRA_THRASH_MINUTES",
    "SPIRA_THRASH_STREAK_CAP", "SPIRA_BRIEF_KEEP_RECURRENCES", "SPIRA_BRIEF_NOTES_MAX_CHARS",
    "SPIRA_SPIKE_DIR", "SPIRA_SPIKE_PATHS", "SPIRA_MAECHEN_MAX_BEADS",
    "SPIRA_MAECHEN_REMEDY_LABEL", "SPIRA_STATUTE_CORE", "SPIRA_MEMORIES_CACHE",
    "SPIRA_MEMORIES_CACHE_AGE", "SPIRA_AGENT",
    "SPIRA_LIFECYCLE_ENFORCE",
    "SPIRA_MAX_AEONS", "SPIRA_VERDICT_WINDOW", "SPIRA_EVICTION_ESCALATE_AT",
    "SPIRA_GH_API", "SPIRA_WORKFLOW_ONLY_PATHS", "SPIRA_CLAIM_RETRIES",
    "SPIRA_CLAIM_RETRY_DELAY_S",
    // sp-1cdgq (landed after this bead branched) added SPIRA_SUMMON_JITTER to the bash
    // SNAPSHOT_VARS list to fix a real bug: aeon_sh's own `conf.n(JITTER_ENV, ...)` fell
    // back to its 20s default forever because this registry key (spira/conf.d's own —
    // string-typed, no generated default, resolves empty unless set via the environment
    // or the resolved config document) was never on the allowlist `_aeon_snapshot` was
    // asked for. Superseded
    // here rather than ported: resolve() already reads it correctly from either source
    // (verified: env override and a toml `summon_jitter` field both reach it), so it
    // belongs with every other registry key resolved in-process, not back in the bash
    // seam's own arg list.
    "SPIRA_SUMMON_JITTER",
];

/// conf.sh's resolution, captured once (DESIGN.md §3).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub env: BTreeMap<String, String>,
    pub vars: BTreeMap<String, String>,
    pub ready_args: Vec<String>,
    pub claim_exclude: String,
}

pub fn parse_snapshot(raw: &str) -> Result<Snapshot, String> {
    let mut parts = raw.split('\0');
    let mut snap = Snapshot::default();
    let mut section = 0;
    let mut pending: Option<String> = None;
    for rec in parts.by_ref() {
        match rec {
            "__AEON_VARS__" => {
                section = 1;
                continue;
            }
            "__AEON_READY__" => {
                section = 2;
                continue;
            }
            "__AEON_EXCLUDE__" => {
                section = 3;
                continue;
            }
            _ => {}
        }
        match section {
            0 => {
                if let Some((k, v)) = rec.split_once('=') {
                    snap.env.insert(k.to_string(), v.to_string());
                }
            }
            1 => match pending.take() {
                None => pending = Some(rec.to_string()),
                Some(k) => {
                    snap.vars.insert(k, rec.to_string());
                }
            },
            2 => {
                if !rec.is_empty() {
                    snap.ready_args.push(rec.to_string());
                }
            }
            _ => {
                if !rec.is_empty() {
                    snap.claim_exclude = rec.to_string();
                }
            }
        }
    }
    if section < 3 {
        return Err("snapshot is incomplete (lib.sh did not source, or the seam died)".into());
    }
    Ok(snap)
}

/// The real seam.
pub struct BashSeam<'a> {
    pub lib: PathBuf,
    pub fayth_file: PathBuf,
    pub fayth: String,
    pub env: &'a Env,
}

pub fn frame(lib: &str, fayth_file: &str, fayth: &str, func: &str, args: &[String]) -> Vec<u8> {
    let mut b = Vec::new();
    for r in [lib, fayth_file, fayth, func].into_iter().chain(args.iter().map(|s| s.as_str())) {
        b.extend_from_slice(r.as_bytes());
        b.push(0);
    }
    b
}

impl Seam for BashSeam<'_> {
    fn call(&self, func: &str, args: &[String]) -> Out {
        let env = self.env.seam();
        let data = frame(&self.lib.display().to_string(), &self.fayth_file.display().to_string(), &self.fayth, func, args);
        util::run(Spec {
            prog: "bash",
            args: vec!["-c".into(), FIXED.into()],
            env: Some(&env),
            cwd: None,
            stdin: Some(data),
            timeout: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Env;
    use std::io::Write;

    #[test]
    fn frame_is_nul_separated_records() {
        let f = frame("/l", "/f", "builder", "fn", &["a b".into(), "".into()]);
        assert_eq!(f, b"/l\0/f\0builder\0fn\0a b\0\0");
    }

    #[test]
    fn snapshot_parses_all_sections() {
        let raw = "PATH=/bin\0HOME=/h\0__AEON_VARS__\0SPIRA_RUN\0/run\0FAYTH_LABELS\0spira,plan\0__AEON_READY__\0ready\0--limit\0\u{30}\0__AEON_EXCLUDE__\0spira-poison,fayth:ops\0";
        let s = parse_snapshot(raw).unwrap();
        assert_eq!(s.env.get("PATH").unwrap(), "/bin");
        assert_eq!(s.vars.get("SPIRA_RUN").unwrap(), "/run");
        assert_eq!(s.vars.get("FAYTH_LABELS").unwrap(), "spira,plan");
        assert_eq!(s.ready_args, vec!["ready", "--limit", "0"]);
        assert_eq!(s.claim_exclude, "spira-poison,fayth:ops");
    }

    #[test]
    fn snapshot_incomplete_is_an_error() {
        assert!(parse_snapshot("PATH=/bin\0").is_err());
    }

    /// The fixed script against a stand-in lib.sh: the allowlist refuses, the args arrive
    /// intact (spaces, newlines, empty), the fayth is sourced and SPIRA_REQUIRE_LABEL folded.
    #[test]
    fn fixed_script_sources_lib_and_fayth_and_passes_args_on_stdin() {
        let dir = testkit::TempDir::new("aeon-seam");
        let lib = dir.join("lib.sh");
        let mut f = std::fs::File::create(&lib).unwrap();
        writeln!(f, "READY_ARGS=(ready --limit 0)\nfayth_exclude() {{ printf 'x:%s:%s' \"$1\" \"$2\"; }}\naeon_count() {{ printf '%s|' \"$@\"; printf '%s' \"$FAYTH_LABELS\"; }}").unwrap();
        std::fs::write(dir.join("b.fayth"), "FAYTH_LABELS=spira,plan\nFAYTH_EXCLUDE_LABELS=p\n").unwrap();
        let mut orig = BTreeMap::new();
        orig.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
        orig.insert("SPIRA_REQUIRE_LABEL".to_string(), "express".to_string());
        let env = Env::new(orig, BTreeMap::new());
        let seam = BashSeam { lib: lib.clone(), fayth_file: dir.join("b.fayth"), fayth: "builder".into(), env: &env };
        let o = seam.call("aeon_count", &["a b\nc".into(), "".into()]);
        assert_eq!(o.stdout, "a b\nc||spira,plan,express");
        let o = seam.call("rm_rf_everything", &[]);
        assert_eq!(o.code, 97);
        let o = seam.call("_aeon_snapshot", &["FAYTH_LABELS".into(), "NOPE".into()]);
        let s = parse_snapshot(&o.stdout).unwrap();
        assert_eq!(s.vars.get("FAYTH_LABELS").unwrap(), "spira,plan,express");
        assert!(!s.vars.contains_key("NOPE"));
        assert_eq!(s.ready_args, vec!["ready", "--limit", "0"]);
        assert_eq!(s.claim_exclude, "x:builder:p");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// sp-1cdgq found SPIRA_SUMMON_JITTER unreachable because it was missing from
    /// SNAPSHOT_VARS (this seam's own bash allowlist). Wave 4.8 moves the fix: the name
    /// is deliberately NOT in SNAPSHOT_VARS any more (it is a `RETIRED_SNAPSHOT_VARS`
    /// registry key now, resolved in-process — see that list's own doc), so the seam
    /// call itself must no longer carry it; `conf::tests::merge_resolved_config_reaches_
    /// a_retired_registry_key_env_can_still_override` (conf.rs) is the test that now
    /// proves the end-to-end guarantee sp-1cdgq's own test proved for the bash path.
    #[test]
    fn summon_jitter_is_no_longer_asked_of_the_bash_seam() {
        assert!(
            !SNAPSHOT_VARS.contains(&"SPIRA_SUMMON_JITTER"),
            "SPIRA_SUMMON_JITTER is resolved in-process now (RETIRED_SNAPSHOT_VARS) — asking the bash seam for it too would just mean two sources of truth"
        );
        assert!(RETIRED_SNAPSHOT_VARS.contains(&"SPIRA_SUMMON_JITTER"));
    }
}
