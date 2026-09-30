//! conf — read the harness's one configuration surface (`spira/conf.sh`) the same way
//! `layout.sh` and `rebuild.sh` did: by sourcing it, not by re-deriving what it computes.
//!
//! `conf.sh` is ~2.7k lines (split-checkout detection, XDG config discovery, its own
//! typed-config resolution, dozens of defaulted keys) and its own rewrite is scheduled
//! **last** in the bash inventory, after everything that merely calls it
//! (`wiki/projects/spira/remaining-bash-inventory.md`, group 4). Re-deriving even the handful
//! of values `layout`/`rebuild` need — `SPIRA_COCKPIT`, `COCKPIT_RIGHT_PCT`, split-checkout
//! mode, and so on — would be porting `conf.sh` a few keys at a time from inside an unrelated
//! bead, exactly the kind of scope creep the wave brief's "retire rather than port" rule
//! exists to catch. So this module shells out to it once per invocation (the same cost the
//! bash paid every time it ran), parses the environment it leaves behind, and nothing else
//! here knows how the harness's own typed config file or an XDG directory is found
//! (that is spira-config's job — see `config-fence` in spira-lint's own DESIGN.md).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub struct Conf {
    vars: HashMap<String, String>,
}

impl Conf {
    /// Source `conf_sh` in a subshell (inheriting this process's own environment — so a
    /// caller-set override, e.g. `TMUX_BIN` or `COCKPIT_MAIL` in a test, reaches it exactly
    /// as it would a bash caller) and capture every variable it leaves set, NUL-delimited so
    /// a value containing a newline is not truncated.
    pub fn load(conf_sh: &Path) -> Result<Conf, String> {
        let out = Command::new("bash")
            .arg("-c")
            .arg(r#"set +u; . "$1" >/dev/null 2>&1; set -a; env -0"#)
            .arg("--")
            .arg(conf_sh)
            .output()
            .map_err(|e| format!("conf: failed to run bash to source {}: {e}", conf_sh.display()))?;
        if !out.status.success() {
            return Err(format!(
                "conf: sourcing {} exited {}",
                conf_sh.display(),
                out.status
            ));
        }
        Ok(Conf {
            vars: parse_env_nul(&out.stdout),
        })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    pub fn get_owned(&self, key: &str) -> Option<String> {
        self.get(key).map(|s| s.to_string())
    }

    /// Apply every variable `conf.sh` left set onto THIS process's own environment, so every
    /// later `std::env::var` call anywhere in the binary — including a different module's,
    /// e.g. `layout::Conf::from_env`'s bare env reads — sees exactly what a bash caller would
    /// have seen after `. conf.sh`. Mirrors the original scripts, which all self-sourced
    /// `conf.sh` as their very first action regardless of what launched them.
    pub fn apply_to_process_env(&self) {
        for (k, v) in &self.vars {
            std::env::set_var(k, v);
        }
    }
}

/// Source `$SPIRA_RELEASE/spira/conf.sh` and apply its result to this process's own
/// environment — the one self-sourcing step every one of `health`/`layout`/`rebuild` needs
/// at startup, since each one is invoked directly (by a tmux pane command or by systemd) with
/// only `SPIRA_RELEASE`+`PATH` guaranteed set, exactly as their bash originals were.
///
/// Silent on any failure (missing `SPIRA_RELEASE`, missing `conf.sh`, a `conf.sh` that itself
/// errors): a caller that already has its config in the environment — a developer running a
/// binary by hand from an already-`. conf.sh`'d shell — must not be blocked by this, matching
/// `layout.sh`'s own `{ set +u; . conf.sh 2>/dev/null; set -u; } || true`.
pub fn self_source() {
    let Some(release) = std::env::var("SPIRA_RELEASE").ok().filter(|s| !s.is_empty()) else { return };
    let conf_sh = Path::new(&release).join("spira").join("conf.sh");
    if let Ok(conf) = Conf::load(&conf_sh) {
        conf.apply_to_process_env();
    }
}

fn parse_env_nul(bytes: &[u8]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for entry in bytes.split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(entry);
        if let Some((k, v)) = s.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_nul_splits_on_first_equals_only() {
        let m = parse_env_nul(b"A=1\0B=x=y\0\0");
        assert_eq!(m.get("A").unwrap(), "1");
        assert_eq!(m.get("B").unwrap(), "x=y");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn parse_env_nul_handles_empty_input() {
        assert!(parse_env_nul(b"").is_empty());
    }

    #[test]
    fn load_reads_back_a_real_sourced_var() {
        let dir = testkit::TempDir::new("cockpit-ops-conf");
        let conf_sh = dir.join("conf.sh");
        std::fs::write(&conf_sh, "export SPIRA_COCKPIT=/fake/cockpit\nexport COCKPIT_RIGHT_PCT=33\n").unwrap();
        let conf = Conf::load(&conf_sh).unwrap();
        assert_eq!(conf.get("SPIRA_COCKPIT"), Some("/fake/cockpit"));
        assert_eq!(conf.get("COCKPIT_RIGHT_PCT"), Some("33"));
    }

    #[test]
    fn load_preserves_a_preexisting_env_override() {
        // conf.sh's own convention is ${VAR:-default}; a caller override must survive
        // through to the sourced result, the same as it would for a bash caller.
        let dir = testkit::TempDir::new("cockpit-ops-conf");
        let conf_sh = dir.join("conf.sh");
        std::fs::write(&conf_sh, ": \"${COCKPIT_RIGHT_PCT:=33}\"\nexport COCKPIT_RIGHT_PCT\n").unwrap();
        std::env::set_var("COCKPIT_RIGHT_PCT", "50");
        let conf = Conf::load(&conf_sh).unwrap();
        std::env::remove_var("COCKPIT_RIGHT_PCT");
        assert_eq!(conf.get("COCKPIT_RIGHT_PCT"), Some("50"));
    }

    #[test]
    fn load_errors_when_file_does_not_exist_is_not_fatal_to_parsing() {
        // `. "$1"` on a missing file fails inside the subshell, but the wrapper still runs
        // (matching layout.sh's own `{ set +u; . conf.sh 2>/dev/null; set -u; } || true`,
        // which never treats a conf.sh failure as fatal to the caller).
        let conf = Conf::load(Path::new("/does/not/exist/conf.sh")).unwrap();
        assert_eq!(conf.get("SPIRA_COCKPIT"), None);
    }
}
