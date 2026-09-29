//! The lifecycle switch (DESIGN.md §9). `lifecycle_enforce` is THE switch for everything
//! that touches the lifecycle machine, resolved by the shared spira-config rule: the process
//! environment's `SPIRA_LIFECYCLE_ENFORCE` wins (`1`/`true` = on, anything else = off); else
//! the typed `spira.lifecycle_enforce`; else OFF. Binary presence is never consulted — a
//! spira-lc on disk does not turn anything on.
//!
//! OFF (production today): this binary never invokes spira-lc, not even to probe it.
//! ON: spira-lc is authoritative; one that cannot be reached is a loud refusal of the step
//! that needed it.

/// This invocation's switch: `spira_config::lifecycle_enforce` — the one resolution rule the
/// harness's crates share — over the document conf.sh resolved (`SPIRA_TOML_FILE`).
pub fn lifecycle_on(doc: Option<&std::path::Path>) -> bool {
    spira_config::lifecycle_enforce(doc)
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
