use crate::{Filing, Host, World};
use spira_config::bounded::{bounded, bounded_for};
use std::path::PathBuf;
use std::process::Command;

const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];
const PERSONA: &str = "builder";

pub struct Real {
    pub ssh: String,
    pub bead: String,
    pub lc: String,
}

impl Default for Real {
    fn default() -> Real {
        Real { ssh: "ssh".into(), bead: "bead.sh".into(), lc: spira_config::lifecycle_row::lc_bin() }
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn run(mut c: Command) -> Result<String, String> {
    let out = c.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

impl Real {
    fn remote(&self, h: &Host, script: &str) -> Result<String, String> {
        let mut c = bounded_for(&self.ssh, "60");
        c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--", &h.ssh, script]);
        run(c)
    }

    fn bead(&self, args: &[&str]) -> Result<String, String> {
        // batch-job: bead.sh opens the store and runs as long as the filing takes
        let mut c = bounded_for(&self.bead, "120");
        c.args(args);
        run(c)
    }

    fn with_body<T>(&self, body: &str, f: impl FnOnce(&str) -> Result<T, String>) -> Result<T, String> {
        let p: PathBuf = std::env::temp_dir().join(format!("filings-collect-{}.md", std::process::id()));
        std::fs::write(&p, body).map_err(|e| e.to_string())?;
        let r = f(&p.to_string_lossy());
        let _ = std::fs::remove_file(&p);
        r
    }
}

impl World for Real {
    fn list(&self, h: &Host) -> Result<Vec<String>, String> {
        let home = sh_quote(&h.home);
        let script = format!("test -d {home} || {{ echo 'no such directory' >&2; exit 3; }}; cd {home}/filings/outbox 2>/dev/null || exit 0; ls -1");
        Ok(self.remote(h, &script)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
    }

    fn read(&self, h: &Host, name: &str) -> Result<String, String> {
        self.remote(h, &format!("cat -- {}", sh_quote(&format!("{}/filings/outbox/{name}", h.home))))
    }

    fn ack(&self, h: &Host, name: &str) -> Result<(), String> {
        let (acked, from) = (format!("{}/filings/acked", h.home), format!("{}/filings/outbox/{name}", h.home));
        self.remote(h, &format!("mkdir -p -- {0} && mv -- {1} {0}/", sh_quote(&acked), sh_quote(&from))).map(|_| ())
    }

    fn is_open(&self, id: &str) -> Result<bool, String> {
        let mut c = bounded(&self.lc);
        c.args(["show", id]);
        let out = c.output().map_err(|e| e.to_string())?;
        match out.status.code() {
            Some(1) => Ok(false),
            Some(0) => {
                let doc: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("{id}: not JSON: {e}"))?;
                let state = doc["bead"]["state"].as_str().ok_or_else(|| format!("{id}: no state"))?;
                Ok(!TERMINAL.contains(&state))
            }
            _ => Err(format!("cannot tell whether {id} is open: {}", String::from_utf8_lossy(&out.stdout).trim())),
        }
    }

    fn file(&self, f: &Filing, body: &str) -> Result<String, String> {
        let out = self.with_body(body, |p| self.bead(&["file", &f.title, "--for", PERSONA, "--repo", &f.repo, "--body-file", p]))?;
        out.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(String::from).ok_or_else(|| "bead.sh printed no id".into())
    }

    fn append(&self, id: &str, body: &str, note: &str) -> Result<(), String> {
        self.with_body(body, |p| self.bead(&["amend", id, "--append-body", p, "--note", note])).map(|_| ())
    }
}
