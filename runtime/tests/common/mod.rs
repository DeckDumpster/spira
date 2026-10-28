//! `dolt sql` as the `Sql` port, for the container tier only: production reaches the store
//! through spira-lc's socket.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use runtime::store::{Row, Sql};
use runtime::{Result, RuntimeError};

/// `dolt sql` in a database directory, the SQL on stdin.
pub struct DoltCli {
    pub dir: PathBuf,
}

impl DoltCli {
    fn run(&self, sql: &str, json: bool) -> Result<Vec<u8>> {
        // batch-job: dolt sql in the container tier; bounded by the executor's effect deadline
        let mut cmd = Command::new("dolt");
        cmd.current_dir(&self.dir).arg("sql");
        if json {
            cmd.args(["-r", "json"]);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| RuntimeError::new(format!("dolt: {e}")))?;
        child.stdin.take().ok_or_else(|| RuntimeError::new("dolt: no stdin"))?.write_all(sql.as_bytes()).map_err(|e| RuntimeError::new(e.to_string()))?;
        let out = child.wait_with_output().map_err(|e| RuntimeError::new(e.to_string()))?;
        if !out.status.success() {
            return Err(RuntimeError::new(format!("dolt sql failed: {}", String::from_utf8_lossy(&out.stderr))));
        }
        Ok(out.stdout)
    }
}

impl Sql for DoltCli {
    fn execute(&self, sql: &str) -> Result<()> {
        self.run(sql, false).map(|_| ())
    }

    fn query(&self, sql: &str) -> Result<Vec<Row>> {
        let out = self.run(sql, true)?;
        if out.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(vec![]);
        }
        let v: serde_json::Value = serde_json::from_slice(&out).map_err(|e| RuntimeError::new(format!("dolt json: {e}")))?;
        let rows = v.get("rows").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        Ok(rows
            .iter()
            .filter_map(|r| r.as_object())
            .map(|o| {
                o.iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_string)))
                    .collect()
            })
            .collect())
    }
}
