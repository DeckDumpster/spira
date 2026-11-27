//! The ports against the host: content and the lifecycle machine over `LcClient`, mail
//! in-process, and the wall clock.

use crate::decide::{status_of_lc, BeadRow, BeadStatus, Scope};
use crate::ports::{Bd, Clock, Mailer, Rows};
use spira_store::incident::{Payload, Recorded, Row};

pub struct RealBd {
    /// The store client; `None` builds the configured one at each call.
    pub lc: Option<spira_store::lc_call::LcClient>,
}

impl RealBd {
    pub fn from_env() -> Result<RealBd, String> {
        Ok(RealBd {
            lc: None,
        })
    }

    /// A content write through the door; the database is the service's own, so `_db` is not passed on.
    pub(crate) fn run(&self, _db: &str, args: &[&str]) -> Result<(i32, String, String), String> {
        let argv: Vec<&str> = std::iter::once("content").chain(args.iter().copied()).collect();
        self.run_lc(&argv)
    }

    fn with_client<T>(&self, f: impl FnOnce(&spira_store::lc_call::LcClient) -> Result<T, String>) -> Result<T, String> {
        match &self.lc {
            Some(c) => f(c),
            None => f(&spira_store::lc_call::LcClient::configured()?),
        }
    }

    fn run_lc(&self, args: &[&str]) -> Result<(i32, String, String), String> {
        self.with_client(|c| {
            let r = c.call(args).map_err(String::from)?;
            Ok((r.code, r.out, r.err))
        })
    }

    /// `content show <id> --json`: the bead as the door gives it, or `None` when it cannot.
    fn show(&self, id: &str) -> Option<serde_json::Value> {
        let (rc, out, _) = self.run_lc(&["content", "show", id, "--json"]).ok()?;
        if rc != 0 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(json_only(&out)).ok()?;
        if v.is_array() {
            v.get(0).cloned()
        } else {
            Some(v)
        }
    }

    /// One `content list --all --limit 0 --json --brief` request, its rows as JSON.
    pub fn list_json(&self, extra: &[&str]) -> Result<Vec<serde_json::Value>, String> {
        let mut args = vec!["content", "list", "--all", "--limit", "0", "--json", "--brief"];
        args.extend_from_slice(extra);
        let (rc, out, err) = self.run_lc(&args)?;
        if rc != 0 {
            return Err(format!("content list exited {rc}: {}", err.trim()));
        }
        match serde_json::from_str::<serde_json::Value>(json_only(&out)) {
            Ok(serde_json::Value::Array(a)) => Ok(a),
            Ok(other) => Ok(vec![other]),
            Err(e) => Err(format!("content list answered something that is not JSON: {e}")),
        }
    }

    /// The rows of `ids`, a request per 100.
    pub fn list_ids_json(&self, ids: &[String], label: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
        let mut rows = Vec::new();
        for chunk in ids.chunks(100) {
            let joined = chunk.join(",");
            let mut extra = vec!["--id", joined.as_str()];
            if let Some(l) = label {
                extra.extend(["--label", l]);
            }
            rows.extend(self.list_json(&extra)?);
        }
        Ok(rows)
    }

    fn run_stdin(&self, args: &[&str], stdin_body: &str) -> Result<(i32, String, String), String> {
        let argv: Vec<&str> = std::iter::once("content").chain(args.iter().copied()).collect();
        self.with_client(|c| {
            let r = c.call_stdin(&argv, stdin_body).map_err(String::from)?;
            Ok((r.code, r.out, r.err))
        })
    }
}

/// The `incidents` verb over [`RealBd`]'s store client.
pub struct RealRows<'a>(pub &'a RealBd);

impl RealRows<'_> {
    fn answer(&self, args: &[&str]) -> Result<String, String> {
        let argv: Vec<&str> = std::iter::once("incidents").chain(args.iter().copied()).collect();
        let (code, out, err) = self.0.run_lc(&argv)?;
        if code == 0 {
            return Ok(out);
        }
        let said = [out.trim(), err.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        Err(format!("incidents {} exited {code}: {said}", args.first().copied().unwrap_or_default()))
    }

    fn json(&self, args: &[&str]) -> Result<serde_json::Value, String> {
        serde_json::from_str(json_only(&self.answer(args)?)).map_err(|e| format!("incidents answered something that is not JSON: {e}"))
    }
}

fn row_of(v: &serde_json::Value) -> Result<Row, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("incidents answered an unreadable row: {e}"))
}

impl Rows for RealRows<'_> {
    fn raise(&self, payload: &Payload) -> Result<String, String> {
        let body = serde_json::to_string(&payload.clone().bounded()).map_err(|e| e.to_string())?;
        let v = self.json(&["raise", &body])?;
        v["key"].as_str().map(str::to_string).ok_or_else(|| format!("incidents raise answered no key: {v}"))
    }

    fn claim(&self, key: &str, by: &str, for_secs: i64) -> Result<Row, String> {
        row_of(&self.json(&["claim", key, "--by", by, "--for", &for_secs.to_string()])?)
    }

    fn record(&self, key: &str, outcome: &Recorded) -> Result<(), String> {
        let at;
        let mut args = vec!["record", key];
        match outcome {
            Recorded::Filed { bead } => {
                args.push("filed");
                args.extend(bead.as_deref());
            }
            Recorded::Refused { reason, lanes } => {
                args.extend(["refused", "--reason", reason]);
                if let Some(l) = lanes {
                    args.extend(["--lanes", l]);
                }
            }
            Recorded::Unread { reason, retry_at } => {
                at = retry_at.to_string();
                args.extend(["unread", "--reason", reason, "--retry-at", &at]);
            }
        }
        self.answer(&args).map(drop)
    }

    fn retry(&self, key: Option<&str>) -> Result<usize, String> {
        let mut args = vec!["retry"];
        args.extend(key);
        let v = self.json(&args)?;
        v["retried"].as_u64().map(|n| n as usize).ok_or_else(|| format!("incidents retry answered no count: {v}"))
    }

    fn list(&self) -> Result<Vec<(String, Row)>, String> {
        rows_listed(&self.answer(&["list"])?)
    }
}

/// `incidents list`'s answer as keyed rows.
pub fn rows_listed(text: &str) -> Result<Vec<(String, Row)>, String> {
    let v: serde_json::Value = serde_json::from_str(json_only(text)).map_err(|e| format!("incidents list: not JSON: {e}"))?;
    v.as_array()
        .ok_or_else(|| format!("incidents list: not an array: {v}"))?
        .iter()
        .map(|r| Ok((r["key"].as_str().unwrap_or_default().to_string(), row_of(r)?)))
        .collect()
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

#[cfg(test)]
fn rows_from_json(text: &str) -> Vec<BeadRow> {
    let parsed: serde_json::Value = match serde_json::from_str(json_only(text)) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr: Vec<serde_json::Value> = match parsed {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    bead_rows(arr)
}

fn bead_rows(arr: Vec<serde_json::Value>) -> Vec<BeadRow> {
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
pub fn join_lc(rows: Vec<BeadRow>, lc: &std::collections::HashMap<String, spira_store::lc_state::Row>, scope: Scope) -> Vec<BeadRow> {
    rows.into_iter()
        .map(|mut r| {
            r.status = status_of_lc(lc.get(&r.id));
            r
        })
        .filter(|r| scope.holds(r.status))
        .collect()
}

impl RealBd {
    /// [`Bd::list`] with each row's bead as the door gave it (its labels and edges).
    pub fn list_beads(
        &self,
        scope: Scope,
        label: Option<&str>,
        closed_after: Option<&str>,
        external_ref: Option<&str>,
    ) -> Result<Vec<(BeadRow, serde_json::Value)>, String> {
        // A machine that cannot answer is Err, never "no incident". The door refuses a list
        // of every bead, so the unfinished ones are named by id from the lifecycle.
        let (beads, lc) = if closed_after.is_some() || external_ref.is_some() {
            let mut extra = Vec::new();
            if let Some(x) = external_ref {
                extra.extend(["--external-ref", x]);
            }
            if let Some(c) = closed_after {
                extra.extend(["--closed-after", c]);
            }
            if let Some(l) = label {
                extra.extend(["--label", l]);
            }
            let beads = self.list_json(&extra)?;
            let ids: Vec<String> = beads.iter().filter_map(|b| b.get("id")?.as_str().map(str::to_string)).collect();
            let lc = self.with_client(|c| spira_store::lc_state::list_ids_on(c, &ids).map(spira_store::lc_state::index))?;
            (beads, lc)
        } else {
            let lc = self.with_client(|c| spira_store::lc_state::list_live_on(c).map(spira_store::lc_state::index))?;
            let mut ids: Vec<String> = lc.values().filter(|r| !r.past_builder()).map(|r| r.bead_id.clone()).collect();
            ids.sort();
            (self.list_ids_json(&ids, label)?, lc)
        };
        let rows = bead_rows(beads.clone());
        let by_id: std::collections::HashMap<&str, &serde_json::Value> = beads.iter().filter_map(|b| Some((b.get("id")?.as_str()?, b))).collect();
        let joined = join_lc(rows, &lc, scope);
        Ok(joined.into_iter().filter_map(|r| by_id.get(r.id.as_str()).map(|b| (*b).clone()).map(|b| (r, b))).collect())
    }
}

impl Bd for RealBd {
    fn incident_rows(&self) -> Box<dyn Rows + '_> {
        Box::new(RealRows(self))
    }

    fn list(&self, _db: &str, scope: Scope, label: Option<&str>, closed_after: Option<&str>, external_ref: Option<&str>) -> Result<Vec<BeadRow>, String> {
        Ok(self.list_beads(scope, label, closed_after, external_ref)?.into_iter().map(|(r, _)| r).collect())
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
        _actor: Option<&str>,
    ) -> Result<String, String> {
        let args = [
            "create", title, "--type", kind, "--priority", priority, "--labels", labels, "--external-ref", external_ref, "--body-file", "-",
            "--silent",
        ];
        let (rc, out, err) = self.run_stdin(&args, body)?;
        if rc != 0 {
            return Err(err.trim().to_string());
        }
        let id = out.trim().to_string();
        if id.is_empty() {
            return Err("content create printed no id".into());
        }
        Ok(id)
    }

    fn label_add(&self, db: &str, id: &str, label: &str) -> bool {
        self.run(db, &["label", "add", id, label]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn label_remove(&self, db: &str, id: &str, label: &str) -> bool {
        self.run(db, &["label", "remove", id, label]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn label_list(&self, _db: &str, id: &str) -> Vec<String> {
        self.show(id)
            .and_then(|b| b.get("labels").and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect()))
            .unwrap_or_default()
    }
    fn note(&self, _db: &str, id: &str, text: &str) -> bool {
        self.run_stdin(&["note", id, "--stdin"], text).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn reopen(&self, _db: &str, id: &str, cause: &str) -> bool {
        spira_store::lifecycle_row::reopen(id, cause, "incident").is_ok()
    }
    fn relate(&self, db: &str, a: &str, b: &str) -> bool {
        self.run(db, &["dep", "relate", a, b]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn duplicate(&self, db: &str, id: &str, survivor: &str) -> bool {
        self.run(db, &["duplicate", id, "--of", survivor]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn live_successor(&self, _db: &str, id: &str) -> Option<String> {
        let obj = self.show(id)?;
        let successor = obj
            .get("dependencies")?
            .as_array()?
            .iter()
            .find(|d| d.get("type").and_then(|t| t.as_str()) == Some("supersedes"))?
            .get("depends_on_id")?
            .as_str()?
            .to_string();
        let row = self.with_client(|c| spira_store::lc_state::row_on(c, &successor)).ok()??;
        (!row.terminal()).then_some(successor)
    }
    fn show_closed_at(&self, _db: &str, id: &str) -> Option<String> {
        self.show(id)?.get("closed_at").and_then(|c| c.as_str()).map(str::to_string)
    }
    fn show_created_at(&self, _db: &str, id: &str) -> Option<String> {
        let obj = self.show(id)?;
        obj.get("created_at").or_else(|| obj.get("created")).and_then(|c| c.as_str()).map(str::to_string)
    }
    fn reachable(&self, _db: &str) -> bool {
        self.run_lc(&["content", "list", "--limit", "1", "--json", "--brief"]).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn fact(&self, id: &str, kind: &str, cause: &str) -> bool {
        let args = ["fact", "add", id, "--kind", kind, "--actor", "incident", "--cause", cause];
        self.run_lc(&args).map(|(rc, ..)| rc == 0).unwrap_or(false)
    }
    fn fact_count(&self, id: &str, kind: &str) -> Option<usize> {
        let (rc, out, _) = self.run_lc(&["fact", "list", "--ids", id, "--kinds", kind]).ok()?;
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
        class: "policy",
        bead: "",
        urgent: false,
        digest: false,
        dry_run: false,
        requested_by: "",
    };
    let basis = "Whether a recurring alert keeps paging is a choice about which alarms exist, so it is policy.";
    (args, format!("## Question\n{subject}\n\n## Default\n{default}\n\n## Class basis\n{basis}\n\n{body}\n"))
}

impl Mailer for RealMailer {
    fn send_operator_question(&self, subject: &str, default: &str, body: &str) -> bool {
        let (args, full_body) = question(subject, default, body);
        // A refusal is said on stderr, as the `mail send` this replaces said it: an escalation
        // that silently did not happen reads as one that did.
        let sent = mail::env::Env::load().and_then(|env| {
            let bd = mail::bead::BdCli::configured(env.bd_conn_retries)?;
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
    #[test_id(ta28r18)]
    #[test]
    fn join_lc_reads_state_from_the_lifecycle_row_not_bd_status() {
        use spira_store::lc_state::Row;
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

    impl spira_store::lc_call::Transport for Scripted {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            Ok(spira_store::lc_call::Reply { code: 0, out: self.1.clone(), err: String::new(), read_ended: false })
        }
    }

    /// sp-ipwlic: facts are written and counted through the store client, never a program.
    #[test_id(tg8dfto)]
    #[test]
    fn facts_go_through_the_store_client() {
        let t = std::sync::Arc::new(Scripted(Default::default(), "[{\"kind\":\"k\"},{\"kind\":\"k\"}]".into()));
        let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(t.clone(), true)) };
        assert!(bd.fact("sp-1", "k", "why"));
        assert_eq!(bd.fact_count("sp-1", "k"), Some(2));
        let verbs: Vec<String> = t.0.lock().unwrap().iter().map(|a| a[..2].join(" ")).collect();
        assert_eq!(verbs, vec!["fact add", "fact list"]);
    }

    #[test_id(tiw6za9)]
    #[test]
    fn incident_writes_go_through_the_door() {
        let t = std::sync::Arc::new(Scripted(Default::default(), "sp-new\n".into()));
        let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(t.clone(), true)) };
        assert!(bd.duplicate("db", "sp-a", "sp-b"));
        assert!(bd.relate("db", "sp-a", "sp-b"));
        assert!(bd.label_add("db", "sp-a", "x"));
        assert!(bd.note("db", "sp-a", "text"));
        assert_eq!(bd.create("db", "T", "bug", "2", "l", "ref", "body", None).unwrap(), "sp-new");
        let seen = t.0.lock().unwrap();
        assert_eq!(seen.len(), 5);
        assert!(seen.iter().all(|a| a[0] == "content"), "{seen:?}");
        assert_eq!(seen[0][1..], ["duplicate", "sp-a", "--of", "sp-b"]);
        assert_eq!(seen[1][1..], ["dep", "relate", "sp-a", "sp-b"]);
        assert_eq!(seen[2][1..], ["label", "add", "sp-a", "x"]);
        assert_eq!(seen[3][1..], ["note", "sp-a", "--stdin"]);
        assert_eq!(seen[4][1], "create");
        assert!(seen[4].windows(2).any(|w| w == ["--body-file", "-"]), "{:?}", seen[4]);
    }

    /// The question carries what the `mail send` argv did: operator, the incident sender, a
    /// question with its default, the body framed under Question and Default.
    #[test_id(twlqx2q)]
    #[test]
    fn an_operator_question_keeps_the_cli_sends_fields() {
        let (a, body) = question("disk full", "prune", "detail");
        assert_eq!((a.mailbox, a.from, a.kind, a.default, a.subject), ("operator", Some("Incident <incident@spira>"), "question", "prune", "disk full"));
        assert_eq!(a.class, "policy");
        assert_eq!(mail::cmds::class_refusal(a.class, a.default, &body), None);
        assert!(body.starts_with("## Question\ndisk full\n\n## Default\nprune\n\n## Class basis\n"), "{body}");
        assert!(body.ends_with("detail\n"), "{body}");
    }

    /// sp-7crdc8: the payload-hash label survives the read through the door, and no program is spawned.
    #[test_id(tua31q0)]
    #[test]
    fn label_list_reads_payload_hash_labels_through_the_door() {
        let rows = vec![serde_json::json!({"id": "sp-1", "labels": ["payload-hash:abc", "incident"]})];
        let fake = std::sync::Arc::new(spira_store::lc_call::fake::Content::new(rows));
        let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(fake, true)) };
        assert_eq!(bd.label_list("", "sp-1"), vec!["payload-hash:abc", "incident"]);
        assert_eq!(bd.show_closed_at("", "sp-1"), None);
        assert!(bd.reachable(""));
    }

    /// sp-7crdc8: unfinished beads are read a request per 100 live ids, every one bounded by `--id`.
    #[test_id(ttp21wp)]
    #[test]
    fn a_list_of_many_live_beads_is_chunked_by_id() {
        let live: Vec<_> = (0..250).map(|i| serde_json::json!({"bead_id": format!("sp-{i:03}"), "state": "READY"})).collect();
        let rows: Vec<_> = (0..250).map(|i| serde_json::json!({"id": format!("sp-{i:03}"), "status": "open", "labels": ["incident"]})).collect();
        let fake = std::sync::Arc::new(spira_store::lc_call::fake::Content::new(rows).with_live(live));
        let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(fake.clone(), true)) };
        assert_eq!(bd.list("", Scope::Unfinished, Some("incident"), None, None).unwrap().len(), 250);
        let seen = fake.requests();
        assert_eq!(seen.len(), 1 + 3, "{seen:?}");
        assert!(seen[1..].iter().all(|r| r.iter().any(|a| a == "--id")), "{seen:?}");
    }

    /// sp-7crdc8: a closed-after list is one bounded request.
    #[test_id(tsgaylk)]
    #[test]
    fn a_closed_after_list_is_one_bounded_request() {
        let rows = vec![serde_json::json!({"id": "sp-1", "labels": ["incident"]})];
        let live = vec![serde_json::json!({"bead_id": "sp-1", "state": "LANDED"})];
        let fake = std::sync::Arc::new(spira_store::lc_call::fake::Content::new(rows).with_live(live));
        let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(fake.clone(), true)) };
        bd.list("", Scope::HandedOn, Some("incident"), Some("2026-01-01"), None).unwrap();
        let seen = fake.requests();
        let lists: Vec<_> = seen.iter().filter(|r| r.iter().any(|a| a == "--closed-after")).collect();
        assert_eq!(lists.len(), 1, "{seen:?}");
    }

    #[test_id(tihykg4)]
    #[test]
    fn a_list_by_reference_asks_the_door_for_that_reference_in_both_scopes() {
        for (scope, state, label, since) in [(Scope::Unfinished, "WORKING", None, None), (Scope::HandedOn, "LANDED", Some("incident"), Some("2026-01-01"))] {
            let rows = vec![
                serde_json::json!({"id": "sp-1", "external_ref": "incident:x", "labels": ["incident"], "closed_at": "2026-01-02"}),
                serde_json::json!({"id": "sp-2", "external_ref": "incident:y", "labels": ["incident"], "closed_at": "2026-01-02"}),
            ];
            let live = vec![serde_json::json!({"bead_id": "sp-1", "state": state})];
            let fake = std::sync::Arc::new(spira_store::lc_call::fake::Content::new(rows).with_live(live));
            let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(fake.clone(), true)) };
            let found = bd.list("", scope, label, since, Some("incident:x")).unwrap(); // BDQ_READ_CONSIDERED="a test of the store door against its fake, not a bd read"
            assert_eq!(found.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["sp-1"], "{scope:?}");
            let mut want = vec!["content", "list", "--all", "--limit", "0", "--json", "--brief", "--external-ref", "incident:x"]; // BDQ_READ_CONSIDERED="the argv a test expects, not a read"
            want.extend(since.iter().flat_map(|d| ["--closed-after", d]));
            want.extend(label.iter().flat_map(|l| ["--label", l]));
            assert_eq!(fake.requests(), [want, vec!["list", "--ids", "sp-1"]], "{scope:?}"); // BDQ_READ_CONSIDERED="the argv a test expects, not a read"
        }
    }

    #[test_id(tm1j95g)]
    #[test]
    fn every_content_list_a_filing_sends_asks_for_brief_rows() {
        use crate::fake::{cfg, FakeMailer, FixedClock};
        let reference = "incident:x";
        let ref_label = format!("ref:{}", crate::decide::ref_hash(reference));
        let live: Vec<_> = (0..250).map(|i| serde_json::json!({"bead_id": format!("sp-{i:03}"), "state": "READY"})).collect();
        let mut rows: Vec<_> = (0..250).map(|i| serde_json::json!({"id": format!("sp-{i:03}"), "status": "open", "labels": ["incident"]})).collect();
        rows[7] = serde_json::json!({"id": "sp-007", "status": "open", "external_ref": reference, "labels": ["incident", ref_label]});
        let fake = std::sync::Arc::new(spira_store::lc_call::fake::Content::new(rows).with_live(live));
        let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(fake.clone(), true)) };
        let known = vec!["spira".to_string()];
        let out = crate::run::file_one(&bd, &FakeMailer::default(), &FixedClock(1_000_000), &cfg(&known), reference, "x failed", b"payload", "spira,incident", &mut vec![]);
        assert!(matches!(&out, crate::run::FileOutcome::Filed(id) if id == "sp-007"), "{out:?}");
        assert!(bd.reachable(""));
        let seen = fake.requests();
        assert!(!seen.iter().any(|r| r.get(1).is_some_and(|v| v == "create")), "{seen:?}");
        let lists: Vec<_> = seen.iter().filter(|r| r.starts_with(&["content".to_string(), "list".to_string()])).collect();
        assert_eq!(lists.len(), 3 + 1, "{seen:?}");
        let full: Vec<_> = lists.iter().filter(|r| !r.iter().any(|a| a == "--brief")).collect();
        assert!(full.is_empty(), "{} of {} content lists lack --brief: {:?}", full.len(), lists.len(), full.first().map(|r| &r[..6]));
    }

    #[test_id(t6823r3)]
    #[test]
    fn a_reply_that_is_not_json_is_an_error_never_no_incident() {
        let t = std::sync::Arc::new(Scripted(Default::default(), "garbled".into()));
        let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(t, true)) };
        assert!(bd.list_json(&["--id", "sp-1"]).is_err());
    }

    #[test_id(twvkbrd)]
    #[test]
    fn live_successor_follows_a_supersedes_edge_to_a_live_bead() {
        let edge = |to: &str| serde_json::json!([{"type": "supersedes", "depends_on_id": to}]);
        let rows = vec![
            serde_json::json!({"id": "sp-a", "dependencies": edge("sp-live")}),
            serde_json::json!({"id": "sp-b", "dependencies": edge("sp-done")}),
        ];
        let live = vec![
            serde_json::json!({"bead_id": "sp-live", "state": "READY"}),
            serde_json::json!({"bead_id": "sp-done", "state": "LANDED"}),
        ];
        let fake = std::sync::Arc::new(spira_store::lc_call::fake::Content::new(rows).with_live(live));
        let bd = RealBd { lc: Some(spira_store::lc_call::LcClient::over(fake, true)) };
        assert_eq!(bd.live_successor("", "sp-a"), Some("sp-live".to_string()));
        assert_eq!(bd.live_successor("", "sp-b"), None);
    }

    #[test_id(te1i1ka)]
    #[test]
    fn json_only_strips_a_leading_warning_line() {
        let input = "\u{1f4a1} some warning\n[{\"id\":\"sp-a\"}]\n";
        assert_eq!(json_only(input), "[{\"id\":\"sp-a\"}]\n");
    }

    #[test_id(tqrr5do)]
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

    #[test_id(tfaz0gb)]
    #[test]
    fn json_only_empty_array_round_trips() {
        assert_eq!(json_only("[]\n"), "[]\n");
    }

    #[test_id(tjg80zc)]
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

    #[test_id(t2b4fsw)]
    #[test]
    fn rows_from_json_empty_array_is_empty_vec() {
        assert_eq!(rows_from_json("[]\n").len(), 0);
    }
}

#[cfg(test)]
mod vtime_tests {
    use super::*;
    use crate::ports::Clock;

    #[test_id(tk0lllf)]
    #[test]
    fn real_clock_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, || RealClock.now() as u64);
        assert_eq!(got, 1_900_000_000);
    }
}
