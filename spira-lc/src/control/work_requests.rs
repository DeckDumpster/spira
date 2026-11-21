//! Recorded `work` requests: a verb that runs a program is written here by lc-serve and run by
//! the scheduler, so no program starts inside the service. `work-requests/<id>` holds the
//! request; `work-request-index/<state>/<id>` lists those a runner still owes, so a poll reads
//! the open ones and never the finished.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use state_store::{Backend, Compare, Object, Request, Result, Selector, Txn};

use super::store::Store;
use crate::control::keys;
use crate::{flag, CANNOT_TELL, REFUSED};

pub const VERB: &str = "work-request";

/// The most output bytes a finished request keeps: under the store's 65,535-byte column, with
/// room for the request's own fields. The tail is kept, because a failure is said last.
pub const OUTPUT_MAX: usize = 48 * 1024;

const TRUNCATED_NOTE: &str = "[output truncated: the last 49152 bytes follow]\n";
const ATTEMPTS: usize = 16;

pub const QUEUED: &str = "queued";
pub const RUNNING: &str = "running";
pub const DONE: &str = "done";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkRequest {
    pub id: String,
    pub bead: String,
    pub actor: String,
    pub argv: Vec<String>,
    pub stdin: Option<String>,
    /// How long the runner may take, in milliseconds: the verb's own bound.
    pub bound_ms: u64,
    pub state: String,
    pub code: Option<i32>,
    pub output: String,
    pub truncated: bool,
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

/// What `take` found.
#[derive(Debug, PartialEq)]
pub enum Take {
    Taken(Box<WorkRequest>),
    /// Not queued: another runner has it, or it is finished.
    NotQueued(String),
    Absent,
}

/// `output` cut to [`OUTPUT_MAX`] bytes keeping its tail, and whether it was cut.
pub fn capped(output: &str) -> (String, bool) {
    if output.len() <= OUTPUT_MAX {
        return (output.to_string(), false);
    }
    let mut from = output.len() - OUTPUT_MAX;
    while !output.is_char_boundary(from) {
        from += 1;
    }
    (format!("{TRUNCATED_NOTE}{}", &output[from..]), true)
}

fn request(base: &str, part: &str, attempt: usize) -> Request {
    Request::new(&format!("{base}/work-request-{part}/{attempt}"), "spira-lc", "work-request")
}

fn decode(key: &str, v: &Object) -> Result<WorkRequest> {
    serde_json::from_value(v.data.clone()).map_err(|e| state_store::Error::Backend(format!("{key}: {e}")))
}

fn encode(r: &WorkRequest) -> Object {
    Object::new(serde_json::to_value(r).unwrap_or(Value::Null))
}

impl<B: Backend> Store<B> {
    /// Records a request as queued and returns the id the store allocated for it.
    pub fn put_work_request(&self, bead: &str, actor: &str, argv: &[String], stdin: Option<&str>, bound_ms: u64, at: i64, base: &str) -> Result<String> {
        for attempt in 0..ATTEMPTS {
            let seq = self.contract().get(keys::WORK_REQUEST_SEQ)?;
            let n = seq.as_ref().and_then(|i| i.value.data["n"].as_u64()).unwrap_or(0) + 1;
            let id = format!("wr-{n}");
            let rec = WorkRequest {
                id: id.clone(),
                bead: bead.into(),
                actor: actor.into(),
                argv: argv.to_vec(),
                stdin: stdin.map(str::to_string),
                bound_ms,
                state: QUEUED.into(),
                code: None,
                output: String::new(),
                truncated: false,
                queued_at: at,
                started_at: None,
                finished_at: None,
            };
            let t = Txn::new()
                .when(Compare::ModRev(keys::WORK_REQUEST_SEQ.into(), seq.map(|i| i.mod_rev)))
                .put(keys::WORK_REQUEST_SEQ, Object::new(serde_json::json!({ "n": n })))
                .put(&keys::work_request(&id), encode(&rec))
                .put(&format!("{}{id}", keys::work_request_index(QUEUED)), Object::new(Value::Null));
            if self.contract().txn(&t, &request(base, "put", attempt))?.succeeded {
                return Ok(id);
            }
        }
        Err(state_store::Error::Backend(format!("{}: lost {ATTEMPTS} races in a row", keys::WORK_REQUEST_SEQ)))
    }

    pub fn work_request(&self, id: &str) -> Result<Option<WorkRequest>> {
        let key = keys::work_request(id);
        self.contract().get(&key)?.map(|i| decode(&key, &i.value)).transpose()
    }

    /// The requests in `state` (`queued` or `running`), oldest first.
    pub fn work_requests_in(&self, state: &str) -> Result<Vec<WorkRequest>> {
        let prefix = keys::work_request_index(state);
        let (items, _) = self.contract().list(&[prefix.as_str()], &Selector::Everything)?;
        let mut out = Vec::new();
        for item in items {
            let Some(id) = item.key.strip_prefix(&prefix) else { continue };
            if let Some(r) = self.work_request(id)? {
                out.push(r);
            }
        }
        out.sort_by_key(|r| (r.queued_at, r.id.trim_start_matches("wr-").parse::<u64>().unwrap_or(0)));
        Ok(out)
    }

    /// Moves a queued request to running; refused when it is not queued.
    pub fn take_work_request(&self, id: &str, at: i64, base: &str) -> Result<Take> {
        self.move_work_request(id, base, "take", |r| {
            if r.state != QUEUED {
                return Err(r.state.clone());
            }
            r.state = RUNNING.into();
            r.started_at = Some(at);
            Ok(())
        })
    }

    /// Records the exit code and output of a request that has not finished. A finished request is
    /// left as it is and reported as not queued.
    pub fn finish_work_request(&self, id: &str, code: i32, output: &str, at: i64, base: &str) -> Result<Take> {
        let (output, truncated) = capped(output);
        self.move_work_request(id, base, "finish", move |r| {
            if r.state == DONE {
                return Err(DONE.into());
            }
            r.state = DONE.into();
            r.code = Some(code);
            r.output = output.clone();
            r.truncated = truncated;
            r.finished_at = Some(at);
            Ok(())
        })
    }

    fn move_work_request(&self, id: &str, base: &str, part: &str, change: impl Fn(&mut WorkRequest) -> std::result::Result<(), String>) -> Result<Take> {
        let key = keys::work_request(id);
        for attempt in 0..ATTEMPTS {
            let Some(item) = self.contract().get(&key)? else { return Ok(Take::Absent) };
            let mut rec = decode(&key, &item.value)?;
            let from = rec.state.clone();
            if let Err(state) = change(&mut rec) {
                return Ok(Take::NotQueued(state));
            }
            let index = |state: &str| format!("{}{id}", keys::work_request_index(state));
            let mut t = Txn::new().when(Compare::ModRev(key.clone(), Some(item.mod_rev))).put(&key, encode(&rec)).delete(&index(&from));
            if rec.state != DONE {
                t = t.put(&index(&rec.state), Object::new(Value::Null));
            }
            if self.contract().txn(&t, &request(base, part, attempt))?.succeeded {
                return Ok(Take::Taken(Box::new(rec)));
            }
        }
        Err(state_store::Error::Backend(format!("{key}: {part} lost {ATTEMPTS} races in a row")))
    }
}

fn shown(r: &WorkRequest) -> String {
    serde_json::to_string(r).unwrap_or_default()
}

/// `work-request put|take|finish|show|list`: JSON on every path; exit 2 ("cannot tell") with the
/// reason, 3 for a take or finish the request's state refuses.
pub fn run<B: Backend>(store: &Store<B>, args: &[String], base: &str, now: i64) -> (i32, String) {
    let cannot = |e: String| (CANNOT_TELL, format!("cannot tell: {VERB}: {e}"));
    let rest = &args[1..];
    let id = || rest.get(1).filter(|a| !a.starts_with("--")).cloned().ok_or_else(|| "missing <id>".to_string());
    match rest.first().map(String::as_str) {
        Some("put") => {
            let (Some(bead), Some(actor), Some(argv)) = (flag(rest, "--bead"), flag(rest, "--actor"), flag(rest, "--argv-json")) else {
                return cannot("put needs --bead, --actor and --argv-json".into());
            };
            let Ok(argv) = serde_json::from_str::<Vec<String>>(&argv) else { return cannot("--argv-json is not a JSON array of strings".into()) };
            let bound_ms = flag(rest, "--bound-ms").and_then(|v| v.parse().ok()).unwrap_or(0);
            match store.put_work_request(&bead, &actor, &argv, flag(rest, "--stdin").as_deref(), bound_ms, now, base) {
                Ok(id) => (0, serde_json::json!({ "id": id }).to_string()),
                Err(e) => cannot(e.to_string()),
            }
        }
        Some("take") => {
            let id = match id() {
                Ok(i) => i,
                Err(e) => return cannot(e),
            };
            match store.take_work_request(&id, now, base) {
                Ok(Take::Taken(r)) => (0, shown(&r)),
                Ok(Take::NotQueued(state)) => (REFUSED, format!("refused: {VERB} {id} is {state}, not queued")),
                Ok(Take::Absent) => (REFUSED, format!("refused: {VERB} {id}: no such request")),
                Err(e) => cannot(e.to_string()),
            }
        }
        Some("finish") => {
            let id = match id() {
                Ok(i) => i,
                Err(e) => return cannot(e),
            };
            let Some(code) = flag(rest, "--code").and_then(|c| c.parse::<i32>().ok()) else { return cannot("finish needs --code N".into()) };
            match store.finish_work_request(&id, code, &flag(rest, "--stdin").unwrap_or_default(), now, base) {
                Ok(Take::Taken(r)) => (0, shown(&r)),
                Ok(Take::NotQueued(state)) => (REFUSED, format!("refused: {VERB} {id} is already {state}")),
                Ok(Take::Absent) => (REFUSED, format!("refused: {VERB} {id}: no such request")),
                Err(e) => cannot(e.to_string()),
            }
        }
        Some("show") => {
            let id = match id() {
                Ok(i) => i,
                Err(e) => return cannot(e),
            };
            match store.work_request(&id) {
                Ok(Some(r)) => (0, shown(&r)),
                Ok(None) => (REFUSED, format!("refused: {VERB} {id}: no such request")),
                Err(e) => cannot(e.to_string()),
            }
        }
        Some("list") => match flag(rest, "--state").as_deref() {
            Some(s @ (QUEUED | RUNNING)) => match store.work_requests_in(s) {
                Ok(rs) => (0, serde_json::to_string(&rs).unwrap_or_default()),
                Err(e) => cannot(e.to_string()),
            },
            _ => cannot("list needs --state queued|running".into()),
        },
        other => cannot(format!("{other:?} is not put, take, finish, show or list")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports};

    fn store() -> Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(1_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    fn put(s: &Store<MemBackend>, tag: &str) -> String {
        s.put_work_request("sp-a", "builder", &["work".into(), "sp-a".into(), "submit".into()], Some("body"), 300_000, 10, tag).unwrap()
    }

    #[test]
    fn the_store_allocates_each_request_the_next_id() {
        let s = store();
        assert_eq!((put(&s, "r1"), put(&s, "r2")), ("wr-1".to_string(), "wr-2".to_string()));
    }

    #[test]
    fn a_request_is_queued_then_taken_once_then_finished_with_its_output() {
        let s = store();
        let id = put(&s, "r1");
        assert_eq!(s.work_requests_in(QUEUED).unwrap().len(), 1);
        assert!(matches!(s.take_work_request(&id, 11, "r2").unwrap(), Take::Taken(r) if r.state == RUNNING && r.started_at == Some(11)));
        assert_eq!(s.take_work_request(&id, 12, "r3").unwrap(), Take::NotQueued(RUNNING.into()), "a second runner is refused");
        assert!(s.work_requests_in(QUEUED).unwrap().is_empty());
        assert_eq!(s.work_requests_in(RUNNING).unwrap().len(), 1);
        assert!(matches!(s.finish_work_request(&id, 3, "refused: x", 13, "r4").unwrap(), Take::Taken(r) if r.code == Some(3) && r.output == "refused: x"));
        assert!(s.work_requests_in(RUNNING).unwrap().is_empty());
        assert_eq!(s.finish_work_request(&id, 0, "again", 14, "r5").unwrap(), Take::NotQueued(DONE.into()), "a finished request keeps its first answer");
        assert_eq!(s.work_request(&id).unwrap().unwrap().output, "refused: x");
    }

    #[test]
    fn a_request_that_never_started_can_still_be_finished() {
        let s = store();
        let id = put(&s, "r1");
        assert!(matches!(s.finish_work_request(&id, 124, "killed", 20, "r2").unwrap(), Take::Taken(_)));
        assert!(s.work_requests_in(QUEUED).unwrap().is_empty(), "finishing removes it from the queue");
    }

    #[test]
    fn output_over_the_cap_keeps_its_tail_and_is_marked_truncated() {
        let long = format!("{}TAIL", "é".repeat(OUTPUT_MAX));
        let (kept, cut) = capped(&long);
        assert!(cut && kept.ends_with("TAIL") && kept.starts_with("[output truncated"), "{}", &kept[..60]);
        assert!(kept.len() <= OUTPUT_MAX + TRUNCATED_NOTE.len());
        assert_eq!(capped("short"), ("short".to_string(), false));
        let s = store();
        let id = put(&s, "r1");
        s.finish_work_request(&id, 0, &long, 20, "r2").unwrap();
        assert!(s.work_request(&id).unwrap().unwrap().truncated);
    }

    #[test]
    fn the_verb_answers_each_sub_verb_and_refuses_what_the_state_forbids() {
        let s = store();
        let a = |v: &[&str]| std::iter::once(VERB).chain(v.iter().copied()).map(String::from).collect::<Vec<_>>();
        let (code, out) = run(&s, &a(&["put", "--bead", "sp-a", "--actor", "builder", "--argv-json", "[\"work\",\"sp-a\",\"submit\"]", "--bound-ms", "9"]), "q1", 5);
        assert_eq!((code, out.as_str()), (0, "{\"id\":\"wr-1\"}"));
        assert_eq!(run(&s, &a(&["list", "--state", "queued"]), "q2", 5).0, 0);
        assert_eq!(run(&s, &a(&["take", "wr-1"]), "q3", 6).0, 0);
        assert_eq!(run(&s, &a(&["take", "wr-1"]), "q4", 7).0, REFUSED);
        assert_eq!(run(&s, &a(&["finish", "wr-1", "--code", "0", "--stdin", "SUBMITTED"]), "q5", 8).0, 0);
        let (code, out) = run(&s, &a(&["show", "wr-1"]), "q6", 9);
        let shown: WorkRequest = serde_json::from_str(&out).unwrap();
        assert_eq!((code, shown.state.as_str(), shown.output.as_str(), shown.code), (0, DONE, "SUBMITTED", Some(0)));
        assert_eq!(run(&s, &a(&["show", "wr-9"]), "q7", 9).0, REFUSED);
        assert_eq!(run(&s, &a(&["list", "--state", "done"]), "q8", 9).0, CANNOT_TELL);
    }
}
