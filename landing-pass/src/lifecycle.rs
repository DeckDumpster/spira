//! The lifecycle machine is the only record: spira-lc is authoritative, and one that cannot
//! be reached is a loud refusal of the step that needed it.

/// Pin the switch into this process's environment before any child starts, so the bash
/// this binary still runs (the lib.sh seams) resolves the same mode.
pub fn pin_for_children(on: bool) {
    std::env::set_var("SPIRA_LIFECYCLE_ENFORCE", if on { "1" } else { "0" });
}

/// The lifecycle machine, as far as this pass needs it: is it there?
pub trait Lc {
    /// `spira-lc list --state IN_DELIVERY`, parsed. Only ever called with the switch ON.
    fn probe(&self) -> Result<(), String>;
    /// `spira-lc list --state SUBMITTED`: bead id → the tip the row was submitted at.
    fn submitted(&self) -> Result<std::collections::HashMap<String, String>, String>;
    /// `spira-lc list --state CERTIFIED`: bead id → the tip the row was certified at. A push
    /// or hold repository lands its own CERTIFIED beads (the gate it runs records GatePass
    /// before the land), so for those the pass reads CERTIFIED as ready too.
    fn certified(&self) -> Result<std::collections::HashMap<String, String>, String>;
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
        self.list_state("SUBMITTED")
    }

    fn certified(&self) -> Result<std::collections::HashMap<String, String>, String> {
        self.list_state("CERTIFIED")
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

    fn list_state(&self, state: &str) -> Result<std::collections::HashMap<String, String>, String> {
        let mut c = crate::util::command(self.bin()?);
        c.args(["list", "--state", state]).stdin(std::process::Stdio::null());
        let (rc, so, se) = crate::util::run_capture(c);
        if rc != 0 {
            let e = String::from_utf8_lossy(&se);
            return Err(format!("spira-lc list exited {rc}: {}", e.lines().next().unwrap_or("")));
        }
        parse_in_state(&so, state)
    }
}

pub fn parse_submitted(json: &[u8]) -> Result<std::collections::HashMap<String, String>, String> {
    parse_in_state(json, "SUBMITTED")
}

/// `spira-lc list` rows in `state`: bead id → tip.
pub fn parse_in_state(json: &[u8], state: &str) -> Result<std::collections::HashMap<String, String>, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("spira-lc list: {e}"))?;
    let rows = v.as_array().ok_or("spira-lc list: not an array")?;
    let field = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(rows
        .iter()
        .filter(|r| field(r, "state") == state)
        .map(|r| (field(r, "bead_id"), field(r, "tip")))
        .filter(|(id, tip)| !id.is_empty() && !tip.is_empty())
        .collect())
}

pub fn unreachable_line(why: &str, what: &str) -> String {
    format!("landing: lifecycle_enforce is on and spira-lc is unreachable ({why}) — {what}; fix the lifecycle machine or turn lifecycle_enforce off")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_in_state_keeps_only_rows_in_that_state() {
        let j = br#"[{"bead_id":"sp-a","state":"CERTIFIED","tip":"aa"},{"bead_id":"sp-b","state":"SUBMITTED","tip":"bb"},{"bead_id":"sp-c","state":"CERTIFIED","tip":""}]"#;
        let c = parse_in_state(j, "CERTIFIED").unwrap();
        assert_eq!(c.len(), 1, "a row with no tip is not a candidate");
        assert_eq!(c.get("sp-a").map(String::as_str), Some("aa"));
        let s = parse_submitted(j).unwrap();
        assert_eq!(s.keys().collect::<Vec<_>>(), vec!["sp-b"]);
    }
}
