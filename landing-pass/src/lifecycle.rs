//! The lifecycle switch (DESIGN.md §9). `lifecycle_enforce` is THE switch for everything
//! that touches the lifecycle machine, resolved as the aeon, queue and spira-claim crates
//! resolve it: the process environment's `SPIRA_LIFECYCLE_ENFORCE` wins (`1`/`true` = on,
//! anything else = off); else the typed `spira.lifecycle_enforce` in the document conf.sh
//! resolved, read through the spira-config library; else OFF. Binary presence is never
//! consulted — a spira-lc on disk does not turn anything on.
//!
//! OFF (production today): this binary never invokes spira-lc, not even to probe it.
//! ON: spira-lc is authoritative; one that cannot be reached is a loud refusal of the step
//! that needed it.

use std::path::Path;

pub fn lifecycle_on(env_value: Option<&str>, doc: Option<&Path>) -> bool {
    if let Some(v) = env_value {
        return v == "1" || v == "true";
    }
    let Some(p) = doc.filter(|p| p.is_file()) else { return false };
    match spira_config::load(p) {
        Ok(d) => d.spira.and_then(|s| s.lifecycle_enforce).unwrap_or(false),
        Err(_) => false,
    }
}

/// The lifecycle machine, as far as this pass needs it: is it there?
pub trait Lc {
    /// `spira-lc list --state IN_DELIVERY`, parsed. Only ever called with the switch ON.
    fn probe(&self) -> Result<(), String>;
}

pub struct RealLc {
    pub bin: Option<std::path::PathBuf>,
}

impl Lc for RealLc {
    fn probe(&self) -> Result<(), String> {
        let Some(bin) = self.bin.as_ref().filter(|b| crate::real::executable(b)) else {
            return Err("SPIRA_LC_BIN is not an executable".into());
        };
        let mut c = crate::util::command(bin);
        c.args(["list", "--state", "IN_DELIVERY"]).stdin(std::process::Stdio::null());
        let (rc, so, se) = crate::util::run_capture(c);
        if rc != 0 {
            let e = String::from_utf8_lossy(&se);
            return Err(format!("spira-lc list exited {rc}: {}", e.lines().next().unwrap_or("")));
        }
        serde_json::from_slice::<serde_json::Value>(&so).map(|_| ()).map_err(|e| format!("spira-lc list: {e}"))
    }
}

pub fn unreachable_line(why: &str, what: &str) -> String {
    format!("landing: lifecycle_enforce is on and spira-lc is unreachable ({why}) — {what}; fix the lifecycle machine or turn lifecycle_enforce off")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_wins_then_the_document_then_off() {
        let dir = crate::testutil::tmpdir("lc");
        let doc = dir.join("doc");
        std::fs::write(&doc, "[spira]\nlifecycle_enforce = true\n").unwrap();
        assert!(lifecycle_on(None, Some(&doc)));
        assert!(!lifecycle_on(Some("0"), Some(&doc)));
        assert!(lifecycle_on(Some("true"), None));
        assert!(!lifecycle_on(Some("yes"), None));
        assert!(!lifecycle_on(None, None));
        assert!(!lifecycle_on(None, Some(&dir.join("missing"))));
        std::fs::write(&doc, "[spira]\nlifecycle_enforce = false\n").unwrap();
        assert!(!lifecycle_on(None, Some(&doc)));
    }
}
