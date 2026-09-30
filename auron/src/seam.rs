//! The lib.sh seam, for exactly the two functions auron needs that still carry real
//! graph semantics: `bdq` (the guarded `bd` wrapper — repo-label and destructive-vocabulary
//! refusals, the connection retry, `SPIRA_BDJSON_FIXTURE`) and `bead_reopen` (the
//! landstate-withdrawal and submitted-label interaction a bare `bd reopen` does not have).
//! Reimplementing either in Rust would mean keeping a second copy of `bdq`'s guards and
//! `bead_reopen`'s landstate rules in step with lib.sh by hand; this is the same design
//! aeon's own seam uses for the same reason (aeon/src/seam.rs).
//!
//! Unlike aeon's seam, this one does not source a fayth file or set `FAYTH` — auron has
//! no fayth of its own. `bash -c <FIXED>` with every datum NUL-framed on stdin, same as
//! aeon's, for the same reason: a bead body can be arbitrarily long and an argv has a
//! kernel ceiling (law-payloads-go-on-stdin).

use std::path::PathBuf;

use crate::util::{self, Out, Spec};

pub const FIXED: &str = r#"set -uo pipefail
IFS= read -r -d '' __auron_lib || exit 96
IFS= read -r -d '' __auron_fn || exit 96
__auron_args=()
while IFS= read -r -d '' __auron_a; do __auron_args+=("$__auron_a"); done
case "$__auron_fn" in
    _auron_bdq|_auron_bead_reopen|_auron_snapshot) ;;
    *) printf 'auron seam: %s is not on the allowlist\n' "$__auron_fn" >&2; exit 97 ;;
esac
. "$__auron_lib" || exit 98
_auron_bdq() { bdq "$@"; }
_auron_bead_reopen() { bead_reopen "$@"; }
# conf.sh's resolution, dumped whole: auron reads SPIRA_RUN, SPIRA_DB, SPIRA_REPO,
# SPIRA_EXPORTER, SPIRA_SYSTEMCTL, SPIRA_INSTANCE, SPIRA_TZ, BD_TIMEOUT and PATH out of
# this, exactly as auron.sh had them after sourcing lib.sh (and so conf.sh) itself.
_auron_snapshot() { env -0; }
"$__auron_fn" "${__auron_args[@]}"
"#;

pub fn frame(lib: &str, func: &str, args: &[String]) -> Vec<u8> {
    let mut b = Vec::new();
    for r in [lib, func].into_iter().chain(args.iter().map(|s| s.as_str())) {
        b.extend_from_slice(r.as_bytes());
        b.push(0);
    }
    b
}

/// One lib.sh function through the fixed seam script.
pub trait Seam: Send + Sync {
    fn call(&self, func: &str, args: &[String]) -> Out;
}

pub struct BashSeam<'a> {
    pub lib: PathBuf,
    pub env: &'a std::collections::BTreeMap<String, String>,
}

impl Seam for BashSeam<'_> {
    fn call(&self, func: &str, args: &[String]) -> Out {
        let data = frame(&self.lib.display().to_string(), func, args);
        util::run(Spec { prog: "bash", args: vec!["-c".into(), FIXED.into()], env: Some(self.env), cwd: None, stdin: Some(data), timeout: None })
    }
}

/// `bdq <args...>` through the seam.
pub fn bdq(seam: &dyn Seam, args: &[&str]) -> Out {
    seam.call("_auron_bdq", &args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

/// `bdq <args...> --json`, stderr dropped, stdout passed through `json_only`.
pub fn bdjson(seam: &dyn Seam, args: &[&str]) -> String {
    let mut a: Vec<&str> = args.to_vec();
    a.push("--json");
    util::json_only(&bdq(seam, &a).stdout)
}

/// `bead_reopen <id> <cause> [note] [suites]` through the seam.
pub fn bead_reopen(seam: &dyn Seam, id: &str, cause: &str, note: &str) -> Out {
    seam.call("_auron_bead_reopen", &[id.to_string(), cause.to_string(), note.to_string()])
}

/// conf.sh's resolution (lib.sh + conf.sh sourced once, `env -0`), as a map.
pub fn snapshot(seam: &dyn Seam) -> Result<std::collections::BTreeMap<String, String>, Out> {
    let o = seam.call("_auron_snapshot", &[]);
    if !o.success() {
        return Err(o);
    }
    let mut env = std::collections::BTreeMap::new();
    for rec in o.stdout.split('\0') {
        if let Some((k, v)) = rec.split_once('=') {
            env.insert(k.to_string(), v.to_string());
        }
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn frame_is_nul_separated_records() {
        let f = frame("/l", "fn", &["a b".into(), "".into()]);
        assert_eq!(f, b"/l\0fn\0a b\0\0");
    }

    /// The fixed script against a stand-in lib.sh: the allowlist refuses an unlisted
    /// function, and a listed one's args arrive intact (spaces, empty).
    #[test]
    fn fixed_script_sources_lib_and_refuses_off_allowlist() {
        let dir = testkit::TempDir::new("auron-seam");
        let lib = dir.join("lib.sh");
        let mut f = std::fs::File::create(&lib).unwrap();
        writeln!(f, "bdq() {{ printf '%s|' \"$@\"; }}\nbead_reopen() {{ printf 'reopened:%s:%s' \"$1\" \"$2\"; }}").unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
        let seam = BashSeam { lib: lib.clone(), env: &env };
        let o = bdq(&seam, &["list", "a b", ""]);
        assert_eq!(o.stdout, "list|a b||");
        let o = seam.call("rm_rf_everything", &[]);
        assert_eq!(o.code, 97);
        let o = bead_reopen(&seam, "sp-x", "alert-recur", "the condition returned");
        assert_eq!(o.stdout, "reopened:sp-x:alert-recur");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_captures_libsh_resolved_vars() {
        let dir = testkit::TempDir::new("auron-seam-snap");
        let lib = dir.join("lib.sh");
        std::fs::write(&lib, "SPIRA_RUN=/resolved/run\nexport SPIRA_RUN\nSPIRA_DB=/resolved/db\nexport SPIRA_DB\n").unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
        let seam = BashSeam { lib, env: &env };
        let snap = snapshot(&seam).unwrap();
        assert_eq!(snap.get("SPIRA_RUN").map(|s| s.as_str()), Some("/resolved/run"));
        assert_eq!(snap.get("SPIRA_DB").map(|s| s.as_str()), Some("/resolved/db"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
