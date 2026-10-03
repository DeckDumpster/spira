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

/// Pin the resolved switch into this process's environment before any child starts, so
/// every child resolves the same mode. OFF is SPIRA_LIFECYCLE_ENFORCE=0 — the one switch the
/// bash this binary still runs (pr-pass-branch.sh, the lib.sh seams) reads; spira-lc is
/// invoked by name, so there is no path to poison (sp-gypjk).
pub fn pin_for_children(on: bool) {
    std::env::set_var("SPIRA_LIFECYCLE_ENFORCE", if on { "1" } else { "0" });
}

/// The lifecycle machine, as far as this pass needs it: is it there?
pub trait Lc {
    /// `spira-lc list --state IN_DELIVERY`, parsed. Only ever called with the switch ON.
    fn probe(&self) -> Result<(), String>;
    /// `spira-lc list --state SUBMITTED`: bead id → the tip the row was submitted at.
    fn submitted(&self) -> Result<std::collections::HashMap<String, String>, String>;
    /// `spira-lc certify <id> <tip> <pass|red|infra> <detail>`: the gate outcome as an event.
    fn certify(&self, id: &str, tip: &str, outcome: &str, detail: &str) -> Result<String, String>;
}

pub struct RealLc {
    pub bin: Option<std::path::PathBuf>,
}

impl Lc for RealLc {
    fn probe(&self) -> Result<(), String> {
        let Some(bin) = self.bin.as_ref() else {
            return Err("no spira-lc program".into());
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

    fn submitted(&self) -> Result<std::collections::HashMap<String, String>, String> {
        let mut c = crate::util::command(self.bin()?);
        c.args(["list", "--state", "SUBMITTED"]).stdin(std::process::Stdio::null());
        let (rc, so, se) = crate::util::run_capture(c);
        if rc != 0 {
            let e = String::from_utf8_lossy(&se);
            return Err(format!("spira-lc list exited {rc}: {}", e.lines().next().unwrap_or("")));
        }
        parse_submitted(&so)
    }

    fn certify(&self, id: &str, tip: &str, outcome: &str, detail: &str) -> Result<String, String> {
        let mut c = crate::util::command(self.bin()?);
        c.args(["certify", id, tip, outcome, detail, "landing-pass"]).stdin(std::process::Stdio::null());
        let (rc, so, se) = crate::util::run_capture(c);
        let out = String::from_utf8_lossy(&so).trim().to_string();
        if rc != 0 {
            let e = String::from_utf8_lossy(&se);
            return Err(format!("spira-lc certify exited {rc}: {out} {}", e.lines().next().unwrap_or("")));
        }
        Ok(out)
    }
}

impl RealLc {
    fn bin(&self) -> Result<&std::path::PathBuf, String> {
        self.bin.as_ref().ok_or_else(|| "no spira-lc program".to_string())
    }
}

pub fn parse_submitted(json: &[u8]) -> Result<std::collections::HashMap<String, String>, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("spira-lc list: {e}"))?;
    let rows = v.as_array().ok_or("spira-lc list: not an array")?;
    let field = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(rows
        .iter()
        .filter(|r| field(r, "state") == "SUBMITTED")
        .map(|r| (field(r, "bead_id"), field(r, "tip")))
        .filter(|(id, tip)| !id.is_empty() && !tip.is_empty())
        .collect())
}

pub fn unreachable_line(why: &str, what: &str) -> String {
    format!("landing: lifecycle_enforce is on and spira-lc is unreachable ({why}) — {what}; fix the lifecycle machine or turn lifecycle_enforce off")
}
