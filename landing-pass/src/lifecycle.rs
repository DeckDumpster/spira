//! The lifecycle machine is the only record: spira-lc is authoritative, and one that cannot
//! be reached is a loud refusal of the step that needed it. Every call is in-process over
//! [`LcClient`] (the lifecycle socket), never a `spira-lc` child.

use spira_store::lc_call::LcClient;

/// The lifecycle machine, as far as this pass needs it: is it there?
pub trait Lc {
    /// `spira-lc list --state IN_DELIVERY`, parsed.
    fn probe(&self) -> Result<(), String>;
    /// `spira-lc certify <id> <tip> <pass|red|infra> <detail>`: the gate outcome as an event.
    fn certify(&self, id: &str, tip: &str, outcome: &str, detail: &str) -> Result<String, String>;
    /// `spira-lc resubmit-submitted <id> <tip>`: make a SUBMITTED row hold the branch tip the gate judged.
    fn resubmit_submitted(&self, id: &str, tip: &str) -> Result<String, String>;
    /// `spira-lc list --state SUBMITTED,CERTIFIED --repo <name>`: each waiting row's `(id, tip)`; a row with no tip is omitted.
    fn waiting_tips(&self, repo: &str) -> Result<Vec<(String, String)>, String>;
}

/// The store's client: `pinned` (a test's scripted one) when given, else the route the
/// configuration declares for `bin` — the socket, in-process, in production.
pub fn client(pinned: Option<&LcClient>, bin: Option<&std::path::Path>) -> Result<LcClient, String> {
    if let Some(c) = pinned {
        return Ok(c.clone());
    }
    let bin = bin.ok_or_else(|| "no spira-lc program".to_string())?;
    LcClient::for_bin(&bin.to_string_lossy())
}

/// One store call as `(exit, stdout, stderr)`; a call with no answer is exit 2 ("cannot
/// tell"), its reason on stderr. The socket has one stream, so a refusal's reason (its
/// stdout there) stands in for the stderr a CLI child would have printed.
pub fn run(c: &LcClient, argv: &[&str]) -> (i32, String, String) {
    match c.call(argv) {
        Ok(r) if r.code != 0 && r.err.trim().is_empty() => (r.code, r.out.clone(), r.out),
        Ok(r) => (r.code, r.out, r.err),
        Err(e) => (spira_store::lc_call::CANNOT_TELL, String::new(), e.to_string()),
    }
}

pub struct RealLc {
    pub bin: Option<std::path::PathBuf>,
    /// A pinned client (tests); `None` resolves one from `bin`.
    pub client: Option<LcClient>,
}

impl Lc for RealLc {
    fn probe(&self) -> Result<(), String> {
        let c = client(self.client.as_ref(), self.bin.as_deref())?;
        let (rc, so, se) = run(&c, &["list", "--state", "IN_DELIVERY"]);
        if rc != 0 {
            return Err(format!("spira-lc list exited {rc}: {}", se.lines().next().unwrap_or("")));
        }
        serde_json::from_str::<serde_json::Value>(&so).map(|_| ()).map_err(|e| format!("spira-lc list: {e}"))
    }

    fn certify(&self, id: &str, tip: &str, outcome: &str, detail: &str) -> Result<String, String> {
        let c = client(self.client.as_ref(), self.bin.as_deref())?;
        let (rc, so, se) = run(&c, &["certify", id, tip, outcome, detail, "landing-pass"]);
        let out = so.trim().to_string();
        if rc != 0 {
            return Err(format!("spira-lc certify exited {rc}: {out} {}", se.lines().next().unwrap_or("")));
        }
        Ok(out)
    }

    fn waiting_tips(&self, repo: &str) -> Result<Vec<(String, String)>, String> {
        let c = client(self.client.as_ref(), self.bin.as_deref())?;
        let (rc, so, se) = run(&c, &["list", "--state", "SUBMITTED,CERTIFIED", "--repo", repo]);
        if rc != 0 {
            return Err(format!("spira-lc list exited {rc}: {}", se.lines().next().unwrap_or("")));
        }
        let rows: Vec<serde_json::Value> = serde_json::from_str(&so).map_err(|e| format!("spira-lc list: {e}"))?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let tip = r["tip"].as_str().filter(|t| !t.is_empty())?;
                Some((r["bead_id"].as_str()?.to_string(), tip.to_string()))
            })
            .collect())
    }

    fn resubmit_submitted(&self, id: &str, tip: &str) -> Result<String, String> {
        let c = client(self.client.as_ref(), self.bin.as_deref())?;
        let (rc, so, se) = run(&c, &["resubmit-submitted", id, tip, "landing-pass"]);
        let out = so.trim().to_string();
        if rc != 0 {
            return Err(format!("spira-lc resubmit-submitted exited {rc}: {out} {}", se.lines().next().unwrap_or("")));
        }
        Ok(out)
    }
}

pub fn unreachable_line(why: &str, what: &str) -> String {
    format!("landing: spira-lc is unreachable ({why}) — {what}; fix the lifecycle machine (it is the only record of a delivery)")
}
