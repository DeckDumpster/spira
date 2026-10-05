//! The lifecycle machine is the only record: spira-lc is authoritative, and one that cannot
//! be reached is a loud refusal of the step that needed it.

/// The lifecycle machine, as far as this pass needs it: is it there?
pub trait Lc {
    /// `spira-lc list --state IN_DELIVERY`, parsed.
    fn probe(&self) -> Result<(), String>;
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

pub fn unreachable_line(why: &str, what: &str) -> String {
    format!("landing: spira-lc is unreachable ({why}) — {what}; fix the lifecycle machine (it is the only record of a delivery)")
}
