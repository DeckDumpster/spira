//! The ports against the host: `bd` on PATH (or `$SPIRA_BD`), `mail` on PATH, and the
//! wall clock. Mirrors lib.sh's `bdq`: `timeout $BD_TIMEOUT bd -C "$db" "$@"`, retried once
//! on a dead pooled connection ("invalid connection").

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::decide::{BeadRow, BeadStatus};
use crate::ports::{Bd, Clock, Mailer};

pub struct RealBd {
    pub bd_bin: String,
    pub timeout_secs: u64,
    pub retries: u32,
}

impl RealBd {
    pub fn from_env() -> RealBd {
        RealBd {
            bd_bin: std::env::var("SPIRA_BD").unwrap_or_else(|_| "bd".into()),
            timeout_secs: std::env::var("BD_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(180),
            retries: std::env::var("SPIRA_BDQ_CONN_RETRIES").ok().and_then(|v| v.parse().ok()).unwrap_or(2),
        }
    }

    fn run(&self, db: &str, args: &[&str]) -> Result<(i32, String, String), String> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut cmd = Command::new("timeout");
            cmd.arg(self.timeout_secs.to_string()).arg(&self.bd_bin).arg("-C").arg(db).args(args);
            let out = cmd.output().map_err(|e| format!("{}: {e}", self.bd_bin))?;
            let rc = out.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            if rc == 0 || attempt >= self.retries.max(1) || !stderr.contains("invalid connection") {
                return Ok((rc, stdout, stderr));
            }
        }
    }

    fn run_stdin(&self, db: &str, args: &[&str], stdin_body: &str) -> Result<(i32, String, String), String> {
        let mut cmd = Command::new("timeout");
        cmd.arg(self.timeout_secs.to_string())
            .arg(&self.bd_bin)
            .arg("-C")
            .arg(db)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", self.bd_bin))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(stdin_body.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| format!("{}: {e}", self.bd_bin))?;
        let rc = out.status.code().unwrap_or(-1);
        Ok((rc, String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()))
    }
}

fn json_only(s: &str) -> &str {
    // bd --json can print warnings on stdout before the payload (lib.sh's json_only):
    // skip to the first line that starts with '[' or '{'. Position 0 is checked FIRST
    // (the common, warning-free case), then each position right after a '\n' — the
    // reverse order silently matched a LATER line's '{' before ever looking at line 0's
    // '[', truncating the array's opening bracket and making every list() call parse as
    // zero rows (found via the dedup-recurrence parity check in this session, sp-0ekp7).
    let candidates = std::iter::once(0).chain(s.match_indices('\n').map(|(i, _)| i + 1));
    for i in candidates {
        let rest = &s[i..];
        let trimmed = rest.trim_start();
        if trimmed.starts_with('[') || trimmed.starts_with('{') {
            return &s[i..];
        }
    }
    &s[s.len()..]
}

fn parse_status(s: &str) -> BeadStatus {
    match s {
        "open" => BeadStatus::Open,
        "in_progress" => BeadStatus::InProgress,
        "closed" => BeadStatus::Closed,
        _ => BeadStatus::Other,
    }
}

fn rows_from_json(text: &str) -> Vec<BeadRow> {
    let parsed: serde_json::Value = match serde_json::from_str(json_only(text)) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr: Vec<serde_json::Value> = match parsed {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    arr.into_iter()
        .filter_map(|b| {
            let id = b.get("id")?.as_str()?.to_string();
            let status = parse_status(b.get("status").and_then(|v| v.as_str()).unwrap_or(""));
            let external_ref = b.get("external_ref").and_then(|v| v.as_str()).map(str::to_string);
            let labels = b
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let closed_at = b.get("closed_at").and_then(|v| v.as_str()).map(str::to_string);
            Some(BeadRow { id, status, external_ref, labels, closed_at })
        })
        .collect()
}

impl Bd for RealBd {
    fn list(&self, db: &str, statuses: &[&str], label: Option<&str>, closed_after: Option<&str>) -> Result<Vec<BeadRow>, String> {
        let mut args: Vec<String> = vec!["list".into(), "--status".into(), statuses.join(","), "--limit".into(), "0".into(), "--json".into()];
        if let Some(l) = label {
            args.push("--label".into());
            args.push(l.into());
        }
        if let Some(c) = closed_after {
            args.push("--closed-after".into());
            args.push(c.into());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (rc, out, _err) = self.run(db, &arg_refs)?;
        if rc != 0 {
            return Err(format!("bd list exited {rc}"));
        }
        Ok(rows_from_json(&out))
    }

    fn create(
        &self,
        db: &str,
        title: &str,
        kind: &str,
        priority: &str,
        labels: &str,
        external_ref: &str,
        body: &str,
        actor: Option<&str>,
    ) -> Result<String, String> {
        let args = [
            "create", title, "--type", kind, "--priority", priority, "--labels", labels, "--external-ref", external_ref, "--body-file", "-",
            "--silent",
        ];
        let mut cmd = Command::new("timeout");
        cmd.arg(self.timeout_secs.to_string())
            .arg(&self.bd_bin)
            .arg("-C")
            .arg(db)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(a) = actor {
            cmd.env("BEADS_ACTOR", a);
        }
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", self.bd_bin))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(body.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| format!("{}: {e}", self.bd_bin))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if id.is_empty() {
            return Err("bd create printed no id".into());
        }
        Ok(id)
    }

    fn label_add(&self, db: &str, id: &str, label: &str) -> bool {
        self.run(db, &["label", "add", id, label]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn label_remove(&self, db: &str, id: &str, label: &str) -> bool {
        self.run(db, &["label", "remove", id, label]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn label_list(&self, db: &str, id: &str) -> Vec<String> {
        // `--json`, not the default human rendering: without it this returned a "🏷 Labels
        // for <id>:" header line plus each label prefixed "  - ", so the payload-hash
        // lookup below (an exact `starts_with("payload-hash:")` match) never matched and
        // every recurrence re-wrote the note body as if the payload had changed — found
        // via test-incident.sh's "an unchanged payload is recorded exactly once" case.
        let Ok((rc, out, _)) = self.run(db, &["label", "list", id, "--json"]) else {
            return Vec::new();
        };
        if rc != 0 {
            return Vec::new();
        }
        serde_json::from_str::<Vec<String>>(json_only(&out)).unwrap_or_default()
    }
    fn note(&self, db: &str, id: &str, text: &str) -> bool {
        self.run_stdin(db, &["note", id, "--stdin"], text).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn set_state(&self, db: &str, id: &str, kv: &str) -> bool {
        self.run(db, &["set-state", id, kv]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn reopen(&self, db: &str, id: &str) -> bool {
        self.run(db, &["reopen", id]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn show_closed_at(&self, db: &str, id: &str) -> Option<String> {
        let (rc, out, _) = self.run(db, &["show", id, "--json"]).ok()?;
        if rc != 0 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(json_only(&out)).ok()?;
        let obj = if v.is_array() { v.get(0)?.clone() } else { v };
        obj.get("closed_at").and_then(|c| c.as_str()).map(str::to_string)
    }
    fn show_created_at(&self, db: &str, id: &str) -> Option<String> {
        let (rc, out, _) = self.run(db, &["show", id, "--json"]).ok()?;
        if rc != 0 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(json_only(&out)).ok()?;
        let obj = if v.is_array() { v.get(0)?.clone() } else { v };
        // Real bd's field is `created_at`; the bash's grep keyed on a literal `"created"`
        // key that never existed in bd's schema, so its age-in-hours clause never fired —
        // an accreted accident, dropped here (DESIGN.md Decisions).
        obj.get("created_at").or_else(|| obj.get("created")).and_then(|c| c.as_str()).map(str::to_string)
    }
    fn reachable(&self, db: &str) -> bool {
        self.run(db, &["list", "--limit", "1"]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn sql(&self, db: &str, query: &str) -> Result<String, String> {
        let (rc, out, err) = self.run(db, &["sql", query])?;
        if rc != 0 {
            return Err(err);
        }
        Ok(out)
    }
}

pub struct RealMailer;

impl Mailer for RealMailer {
    fn send_operator_question(&self, subject: &str, default: &str, body: &str) -> bool {
        let full_body = format!("## Question\n{subject}\n\n## Default\n{default}\n\n{body}\n");
        let mut cmd = Command::new("mail");
        cmd.args(["send", "operator", "--from", "Incident <incident@spira>", "--subject", subject, "--kind", "question", "--default", default])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let Ok(mut child) = cmd.spawn() else { return false };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(full_body.as_bytes());
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    }
}

pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_only_strips_a_leading_warning_line() {
        let input = "\u{1f4a1} some warning\n[{\"id\":\"sp-a\"}]\n";
        assert_eq!(json_only(input), "[{\"id\":\"sp-a\"}]\n");
    }

    #[test]
    fn json_only_leaves_clean_output_untouched() {
        // Regression: a naive "check positions right after '\n' before position 0" scan
        // matched the SECOND line's leading '{' before ever considering line 0's '[',
        // silently truncating the array's opening bracket — every bd list() call parsed
        // as zero rows no matter how many beads existed (found via the dedup/recurrence
        // parity check in this session, sp-0ekp7). Multi-element, pretty-printed output
        // (bd's real shape) is exactly the case that broke.
        let input = "[\n  {\n    \"id\": \"sp-a\"\n  },\n  {\n    \"id\": \"sp-b\"\n  }\n]\n";
        assert_eq!(json_only(input), input);
    }

    #[test]
    fn json_only_empty_array_round_trips() {
        assert_eq!(json_only("[]\n"), "[]\n");
    }

    #[test]
    fn rows_from_json_parses_a_real_multi_element_bd_shape() {
        let input = "[\n  {\n    \"id\": \"sp-a\",\n    \"status\": \"open\",\n    \"external_ref\": \"incident:x\",\n    \"labels\": [\"ref:aa\", \"spira\"]\n  },\n  {\n    \"id\": \"sp-b\",\n    \"status\": \"closed\",\n    \"closed_at\": \"2026-09-01T00:00:00Z\"\n  }\n]\n";
        let rows = rows_from_json(input);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "sp-a");
        assert_eq!(rows[0].external_ref.as_deref(), Some("incident:x"));
        assert_eq!(rows[0].labels, vec!["ref:aa".to_string(), "spira".to_string()]);
        assert_eq!(rows[1].id, "sp-b");
        assert_eq!(rows[1].closed_at.as_deref(), Some("2026-09-01T00:00:00Z"));
    }

    #[test]
    fn rows_from_json_empty_array_is_empty_vec() {
        assert_eq!(rows_from_json("[]\n").len(), 0);
    }
}
