//! The ports against the host: content through `bdq`, the lifecycle machine over `LcClient`,
//! mail in-process, and the wall clock. Each content call is `timeout $BD_TIMEOUT bdq "$@"`,
//! retried on a dead pooled connection ("invalid connection").

use std::io::Write;
use std::process::{Command, Stdio};

use crate::decide::{status_of_lc, BeadRow, BeadStatus, Scope};
use crate::ports::{Bd, Clock, Mailer};

pub struct RealBd {
    pub bdq: String,
    pub timeout_secs: u64,
    pub retries: u32,
    /// The store client; `None` builds the configured one at each call.
    pub lc: Option<spira_config::lc_call::LcClient>,
}

impl RealBd {
    /// `BD_TIMEOUT` carries no `spira/conf.d/<KEY>` entry, so it stays an ad hoc environment
    /// read; `SPIRA_BDQ_CONN_RETRIES` is registered config.
    pub fn from_env() -> Result<RealBd, String> {
        Ok(RealBd {
            bdq: spira_config::release_env::tool("bdq")?,
            timeout_secs: std::env::var("BD_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(180),
            retries: spira_config::process::cfg("SPIRA_BDQ_CONN_RETRIES").ok().and_then(|v| v.parse().ok()).unwrap_or(2),
            lc: None,
        })
    }

    /// `bdq` addresses the configured database itself, so `_db` is not passed on.
    pub(crate) fn run(&self, _db: &str, args: &[&str]) -> Result<(i32, String, String), String> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            // FORK-EDGE: unserved verb content (bd reads) and content store write (label/set-state/dep/duplicate)
            let mut cmd = Command::new("timeout");
            cmd.arg(self.timeout_secs.to_string()).arg(&self.bdq).args(args);
            let out = cmd.output().map_err(|e| format!("{}: {e}", self.bdq))?;
            let rc = out.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            if rc == 0 || attempt >= self.retries.max(1) || !stderr.contains("invalid connection") {
                return Ok((rc, stdout, stderr));
            }
        }
    }

    fn run_lc(&self, args: &[&str]) -> Result<(i32, String, String), String> {
        let configured;
        let client = match &self.lc {
            Some(c) => c,
            None => {
                configured = spira_config::lc_call::LcClient::configured()?;
                &configured
            }
        };
        let r = client.call(args).map_err(String::from)?;
        Ok((r.code, r.out, r.err))
    }

    fn run_stdin(&self, args: &[&str], stdin_body: &str) -> Result<(i32, String, String), String> {
        // FORK-EDGE: content store write (bd note)
        let mut cmd = Command::new("timeout");
        cmd.arg(self.timeout_secs.to_string())
            .arg(&self.bdq)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", self.bdq))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(stdin_body.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| format!("{}: {e}", self.bdq))?;
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

/// [`json_only`] for `main.rs`'s operator `list`.
pub fn json_only_pub(s: &str) -> &str {
    json_only(s)
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
            let external_ref = b.get("external_ref").and_then(|v| v.as_str()).map(str::to_string);
            let labels = b
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let closed_at = b.get("closed_at").and_then(|v| v.as_str()).map(str::to_string);
            // The state is the lifecycle row's, filled in by `join_lc` (sp-jgjvh).
            Some(BeadRow { id, status: BeadStatus::Other, external_ref, labels, closed_at })
        })
        .collect()
}

/// bd rows with each bead's state from its lifecycle row, kept when it is in `scope`.
pub fn join_lc(rows: Vec<BeadRow>, lc: &std::collections::HashMap<String, spira_config::lc_state::Row>, scope: Scope) -> Vec<BeadRow> {
    rows.into_iter()
        .map(|mut r| {
            r.status = status_of_lc(lc.get(&r.id));
            r
        })
        .filter(|r| scope.holds(r.status))
        .collect()
}

impl Bd for RealBd {
    fn list(&self, db: &str, scope: Scope, label: Option<&str>, closed_after: Option<&str>) -> Result<Vec<BeadRow>, String> {
        // The state first: a machine that cannot answer is Err, never "no incident".
        let lc = spira_config::lc_state::list().map(spira_config::lc_state::index)?;
        let mut base: Vec<String> = vec!["list".into(), "--all".into(), "--limit".into(), "0".into(), "--json".into()];
        if let Some(c) = closed_after {
            base.push("--closed-after".into());
            base.push(c.into());
        }
        let mut queries: Vec<Vec<String>> = Vec::new();
        match label {
            Some(l) => {
                let mut q = base.clone();
                q.push("--label".into());
                q.push(l.into());
                queries.push(q);
            }
            // The unlabelled fallback over every unfinished bead: those ids from the machine,
            // their content from bd by id, chunked (never the whole store).
            None if scope == Scope::Unfinished => {
                let mut ids: Vec<&String> = lc.values().filter(|r| !r.past_builder()).map(|r| &r.bead_id).collect();
                ids.sort();
                for chunk in ids.chunks(100) {
                    let mut q = base.clone();
                    q.push("--id".into());
                    q.push(chunk.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(","));
                    queries.push(q);
                }
            }
            None => queries.push(base),
        }
        let mut rows = Vec::new();
        for q in queries {
            let arg_refs: Vec<&str> = q.iter().map(String::as_str).collect();
            let (rc, out, _err) = self.run(db, &arg_refs)?;
            if rc != 0 {
                return Err(format!("bd list exited {rc}"));
            }
            rows.extend(rows_from_json(&out));
        }
        Ok(join_lc(rows, &lc, scope))
    }

    fn create(
        &self,
        _db: &str,
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
        // FORK-EDGE: content store write (bd create)
        let mut cmd = Command::new("timeout");
        cmd.arg(self.timeout_secs.to_string())
            .arg(&self.bdq)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(a) = actor {
            cmd.env("BEADS_ACTOR", a);
        }
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", self.bdq))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(body.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| format!("{}: {e}", self.bdq))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if id.is_empty() {
            return Err("bdq create printed no id".into());
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
    fn note(&self, _db: &str, id: &str, text: &str) -> bool {
        self.run_stdin(&["note", id, "--stdin"], text).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn set_state(&self, db: &str, id: &str, kv: &str) -> bool {
        self.run(db, &["set-state", id, kv]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn set_repo(&self, id: &str, repo: &str) -> bool {
        spira_config::lifecycle_row::set_repo(id, repo).is_ok()
    }
    fn reopen(&self, _db: &str, id: &str, cause: &str) -> bool {
        spira_config::lifecycle_row::reopen(id, cause, "incident").is_ok()
    }
    fn relate(&self, db: &str, a: &str, b: &str) -> bool {
        self.run(db, &["dep", "relate", a, b]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn duplicate(&self, db: &str, id: &str, survivor: &str) -> bool {
        self.run(db, &["duplicate", id, "--of", survivor]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn live_successor(&self, db: &str, id: &str) -> Option<String> {
        let (rc, out, _) = self.run(db, &["show", id, "--json"]).ok()?;
        if rc != 0 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(json_only(&out)).ok()?;
        let obj = if v.is_array() { v.get(0)?.clone() } else { v };
        let successor = obj
            .get("dependencies")?
            .as_array()?
            .iter()
            .find(|d| d.get("dependency_type").and_then(|t| t.as_str()) == Some("supersedes"))?
            .get("id")?
            .as_str()?
            .to_string();
        let row = spira_config::lc_state::row(&successor).ok()??;
        (!row.terminal()).then_some(successor)
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
    fn fact(&self, id: &str, kind: &str, cause: &str) -> bool {
        let args = ["fact", id, "--kind", kind, "--actor", "incident", "--cause", cause];
        self.run_lc(&args).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn fact_count(&self, id: &str, kind: &str) -> Option<usize> {
        let (rc, out, _) = self.run_lc(&["facts", "--ids", id, "--kinds", kind]).ok()?;
        if rc != 0 {
            return None;
        }
        serde_json::from_str::<Vec<serde_json::Value>>(json_only(&out)).ok().map(|v| v.len())
    }
}

pub struct RealMailer;

/// The operator question an incident raises, as the `mail send` it used to spawn would have
/// read it.
pub fn question<'a>(subject: &'a str, default: &'a str, body: &str) -> (mail::cmds::SendArgs<'a>, String) {
    let args = mail::cmds::SendArgs {
        mailbox: "operator",
        from: Some("Incident <incident@spira>"),
        subject,
        kind: "question",
        default,
        class: "",
        bead: "",
        urgent: false,
        digest: false,
        dry_run: false,
        requested_by: "",
    };
    (args, format!("## Question\n{subject}\n\n## Default\n{default}\n\n{body}\n"))
}

impl Mailer for RealMailer {
    fn send_operator_question(&self, subject: &str, default: &str, body: &str) -> bool {
        let (args, full_body) = question(subject, default, body);
        // A refusal is said on stderr, as the `mail send` this replaces said it: an escalation
        // that silently did not happen reads as one that did.
        let sent = mail::env::Env::load().and_then(|env| {
            let bd = mail::bead::BdCli { bdq: spira_config::release_env::tool("bdq")?, conn_retries: env.bd_conn_retries };
            mail::cmds::send_recorded(&bd, &env, &args, full_body).map(drop)
        });
        if let Err(e) = &sent {
            eprintln!("incident: the operator question was not sent: {e}");
        }
        sent.is_ok()
    }
}

pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> i64 {
        spira_config::vtime::now_epoch() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sp-jgjvh: the dedup's open/closed is the lifecycle row's, whatever bd's status says;
    /// a rowless bead is in neither pass.
    #[test]
    fn join_lc_reads_state_from_the_lifecycle_row_not_bd_status() {
        use spira_config::lc_state::Row;
        let rows = rows_from_json(
            r#"[{"id":"a","status":"closed","external_ref":"r"},{"id":"b","status":"open","external_ref":"r"},
                {"id":"c","status":"open","external_ref":"r"},{"id":"d","status":"open"}]"#,
        );
        let lc = [("a", "WORKING"), ("b", "SUBMITTED"), ("c", "REWORK")]
            .iter()
            .map(|(i, st)| (i.to_string(), Row { bead_id: i.to_string(), state: st.to_string(), ..Default::default() }))
            .collect();
        let ids = |v: Vec<BeadRow>| v.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(join_lc(rows.clone(), &lc, Scope::Unfinished)), vec!["a", "c"]);
        assert_eq!(ids(join_lc(rows, &lc, Scope::HandedOn)), vec!["b"]);
    }

    struct Scripted(std::sync::Mutex<Vec<Vec<String>>>, String);

    impl spira_config::lc_call::Transport for Scripted {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_config::lc_call::Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            Ok(spira_config::lc_call::Reply { code: 0, out: self.1.clone(), err: String::new() })
        }
    }

    /// sp-ipwlic: facts are written and counted through the store client, never a program.
    #[test]
    fn facts_go_through_the_store_client() {
        let t = std::sync::Arc::new(Scripted(Default::default(), "[{\"kind\":\"k\"},{\"kind\":\"k\"}]".into()));
        let bd = RealBd { bdq: "/nonexistent/bdq".into(), timeout_secs: 1, retries: 1, lc: Some(spira_config::lc_call::LcClient::over(t.clone(), true)) };
        assert!(bd.fact("sp-1", "k", "why"));
        assert_eq!(bd.fact_count("sp-1", "k"), Some(2));
        let verbs: Vec<String> = t.0.lock().unwrap().iter().map(|a| a[0].clone()).collect();
        assert_eq!(verbs, vec!["fact", "facts"]);
    }

    /// The question carries what the `mail send` argv did: operator, the incident sender, a
    /// question with its default, the body framed under Question and Default.
    #[test]
    fn an_operator_question_keeps_the_cli_sends_fields() {
        let (a, body) = question("disk full", "prune", "detail");
        assert_eq!((a.mailbox, a.from, a.kind, a.default, a.subject), ("operator", Some("Incident <incident@spira>"), "question", "prune", "disk full"));
        assert_eq!(body, "## Question\ndisk full\n\n## Default\nprune\n\ndetail\n");
    }

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

#[cfg(test)]
mod vtime_tests {
    use super::*;
    use crate::ports::Clock;

    #[test]
    fn real_clock_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, || RealClock.now() as u64);
        assert_eq!(got, 1_900_000_000);
    }
}
