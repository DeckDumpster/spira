//! The statute book, reached only through the content door; statute text travels as the
//! request body.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;

use spira_store::lc_call::{LcClient, Reply};

use crate::vocab::Entry;
use crate::{find_duplicate, is_in_force, mirror};

/// `content memories --json` as parsed: every key and value in the book.
pub type Memories = BTreeMap<String, serde_json::Value>;

pub struct Book {
    lc: LcClient,
}

/// What became of one write, proved by reading the book back when the store reported an error.
#[derive(Debug, PartialEq, Eq)]
pub enum Put {
    Written,
    InForceDespite(String),
    NotWritten(String),
    Unknown { write: String, read: String },
}

impl Put {
    pub fn in_force(&self) -> bool {
        matches!(self, Put::Written | Put::InForceDespite(_))
    }

    /// What a write that reported an error tells the caller; nothing for a clean one.
    pub fn notice(&self, key: &str) -> Option<String> {
        match self {
            Put::Written => None,
            Put::InForceDespite(e) => Some(format!("rule: the store reported an error but {key} IS in force (verified against the book): {e}")),
            Put::NotWritten(e) => Some(format!("rule: failed to write {key} to the statute book: {e}\nrule: verified against the book — nothing written, {key} is not in force.")),
            Put::Unknown { write, read } => Some(format!(
                "rule: the store reported an error writing {key} and the book cannot be read back to say whether it landed: {write}\n{read}\nrule: run `rule.sh show {key}` before retrying; do not enact it again under another slug."
            )),
        }
    }
}

/// Why `enact` wrote nothing before its `remember`.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// The caller's gate ended it, with this exit code.
    Gate(i32),
    Unreachable(String),
    Duplicate(String),
}

/// An `enact` that sent its `remember`: the gate's value, the book as it stood, and the outcome.
#[derive(Debug)]
pub struct Enacted<T> {
    pub gate: T,
    pub before: Memories,
    pub put: Put,
}

/// What `rename_apply` wrote.
#[derive(Debug, Default)]
pub struct Renamed {
    pub after: Memories,
    pub written: usize,
    pub statutes: usize,
    pub runbooks: usize,
    /// Each renamed statute: old slug, new slug, whether the old one holds a section.
    pub statute_renames: Vec<(String, String, bool)>,
    pub failed: bool,
}

/// `json_only`: keep the first line (and everything after) that starts, at column 1, with
/// `[` or `{`; no match at all is empty.
fn json_only(input: &str) -> &str {
    let mut offset = 0usize;
    for line in input.split_inclusive('\n') {
        if line.starts_with('[') || line.starts_with('{') {
            return &input[offset..];
        }
        offset += line.len();
    }
    ""
}

fn why(r: &Reply) -> String {
    format!("{}{}", r.out, r.err).trim().to_string() + &format!(" (exit {})", r.code)
}

impl Book {
    pub fn configured() -> Result<Self, String> {
        Ok(Book { lc: LcClient::configured()? })
    }

    pub fn over(lc: LcClient) -> Self {
        Book { lc }
    }

    /// One content request; `Ok` is its stdout when it exited 0.
    fn content(&self, argv: &[&str], body: Option<&str>) -> Result<String, String> {
        let argv: Vec<&str> = std::iter::once("content").chain(argv.iter().copied()).collect();
        let sent = match body {
            Some(b) => self.lc.call_stdin(&argv, b),
            None => self.lc.call(&argv),
        };
        match sent {
            Ok(r) if r.code == 0 => Ok(r.out),
            Ok(r) => Err(why(&r)),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn memories_text(&self) -> Result<String, String> {
        self.content(&["memories", "--json"], None).map(|out| json_only(&out).trim_end_matches('\n').to_string())
    }

    /// The whole book. An unanswered or unreadable read is an error, never an empty book.
    pub fn memories(&self) -> Result<Memories, String> {
        self.memories_text()
            .and_then(|t| serde_json::from_str(&t).map_err(|e| format!("the answer is not a JSON object: {e}")))
            .map_err(|e| format!("rule: cannot reach the statute book: {e}"))
    }

    /// The mirror in `dir`; only a book never mirrored is asked of the store, and that read
    /// fills the mirror.
    pub fn read(&self, dir: &Path) -> Result<Memories, String> {
        if let Some(book) = mirror::read(dir) {
            return Ok(book);
        }
        let book = self.memories()?;
        let _ = mirror::sync(dir, &book);
        Ok(book)
    }

    pub fn recall(&self, key: &str) -> Option<String> {
        self.content(&["recall", key], None).ok()
    }

    pub fn remember(&self, key: &str, text: &str) -> Result<(), String> {
        self.content(&["remember", "--key", key], Some(text)).map(drop)
    }

    pub fn forget(&self, key: &str) -> Result<(), String> {
        self.content(&["forget", key], None).map(drop)
    }

    /// The titles of the beads not closed.
    pub fn open_bead_titles(&self) -> Result<Vec<String>, String> {
        let unreadable = |e: String| format!("rule: cannot read the open beads: {e}");
        let out = self.content(&["list", "--brief", "--json", "--limit", "0"], None).map_err(unreadable)?;
        let rows: Vec<serde_json::Value> = serde_json::from_str(json_only(&out)).map_err(|e| unreadable(e.to_string()))?;
        Ok(rows.iter().filter_map(|b| b.get("title").and_then(|t| t.as_str()).map(str::to_string)).collect())
    }

    fn put(&self, key: &str, text: &str) -> Put {
        let Err(write) = self.remember(key, text) else { return Put::Written };
        match self.memories() {
            Ok(m) if is_in_force(&m, key, text) => Put::InForceDespite(write),
            Ok(_) => Put::NotWritten(write),
            Err(read) => Put::Unknown { write, read },
        }
    }

    /// The book's half of `enact`. `gate` sees the text already under `key` and decides, before
    /// anything is written, whether to go on.
    pub fn enact<T>(&self, key: &str, text: &str, gate: impl FnOnce(Option<&str>) -> ControlFlow<i32, T>) -> Result<Enacted<T>, Stop> {
        let current = self.recall(key);
        let gate = match gate(current.as_deref()) {
            ControlFlow::Continue(t) => t,
            ControlFlow::Break(rc) => return Err(Stop::Gate(rc)),
        };
        let before = self.memories().map_err(Stop::Unreachable)?;
        if let Some(existing) = find_duplicate(&before, key, text) {
            return Err(Stop::Duplicate(existing));
        }
        let put = self.put(key, text);
        Ok(Enacted { gate, before, put })
    }

    /// Writes the retirement `record` (key, body) when there is one, then forgets `key`; a
    /// refused forget takes the record back out.
    pub fn retire(&self, key: &str, record: Option<(&str, &str)>) -> Result<(), String> {
        if let Some((record_key, body)) = record {
            self.remember(record_key, body).map_err(|e| format!("rule: failed to record the retirement of {key}, nothing forgotten: {e}"))?;
        }
        self.forget(key).map_err(|e| {
            if let Some((record_key, _)) = record {
                let _ = self.forget(record_key);
            }
            format!("rule: failed to forget {key}: {e}")
        })
    }

    /// Puts `key` back as it was: the prior text, or no key at all.
    pub fn restore(&self, key: &str, prior: Option<&str>) -> bool {
        match prior {
            Some(text) => self.remember(key, text),
            None => self.forget(key),
        }
        .is_ok()
    }

    /// Writes every amendment and rename of a vocabulary plan, each proved by a read-back.
    /// `record` is the retirement record body for a key renamed from `.0` to `.1`.
    pub fn rename_apply(&self, entries: &[Entry], before: &Memories, record: impl Fn(&str, &str) -> String) -> Renamed {
        let mut done = Renamed { after: before.clone(), ..Renamed::default() };
        for e in entries {
            let (key, text, from) = match e {
                Entry::Amend { key, text } => (key, text, None),
                Entry::Rename { from, to, text, numbered } => (to, text, Some((from, *numbered))),
                _ => continue,
            };
            let wrote = self.remember(key, text);
            if !self.memories().is_ok_and(|m| is_in_force(&m, key, text)) {
                eprintln!("rule: failed to write {key}: {}", wrote.err().unwrap_or_default());
                done.failed = true;
                continue;
            }
            if let Err(err) = wrote {
                eprintln!("{}", Put::InForceDespite(err).notice(key).unwrap_or_default());
            }
            done.after.insert(key.clone(), serde_json::Value::String(text.clone()));
            let runbook = from.map_or(key, |(f, _)| f).starts_with("sop-");
            if let Some((from, numbered)) = from {
                let record_key = if runbook { sop::shelf::retired_key(from) } else { crate::code::retired_key(from) };
                if let Err(err) = self.remember(&record_key, &record(from, key)) {
                    eprintln!("rule: failed to record the retirement of {from}: {err}");
                    let _ = self.forget(key);
                    done.after.remove(key);
                    done.failed = true;
                    continue;
                }
                if let Err(err) = self.forget(from) {
                    eprintln!("rule: failed to forget {from}: {err}");
                    let _ = self.forget(&record_key);
                    done.failed = true;
                    continue;
                }
                done.after.remove(from);
                if !runbook {
                    done.statute_renames.push((from.clone(), key.clone(), numbered));
                }
                println!("renamed {from} → {key}");
            } else {
                println!("amended {key}");
            }
            if runbook {
                done.runbooks += 1;
            } else {
                done.statutes += 1;
            }
            done.written += 1;
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_store::lc_call::Transport;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    type Sent = (Vec<String>, Option<String>);

    /// Records every request and answers from a script, in order; a request past the script's
    /// end is an unanswering store.
    #[derive(Default)]
    struct Script {
        sent: Mutex<Vec<Sent>>,
        replies: Mutex<VecDeque<(i32, String)>>,
    }

    impl Transport for Script {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.sent.lock().unwrap().push((argv.to_vec(), stdin.map(String::from)));
            let (code, out) = self.replies.lock().unwrap().pop_front().ok_or("store not running")?;
            Ok(Reply { code, out, err: String::new(), read_ended: false })
        }
    }

    fn scripted(replies: &[(i32, &str)]) -> (Arc<Script>, Book) {
        let t = Arc::new(Script::default());
        *t.replies.lock().unwrap() = replies.iter().map(|(c, o)| (*c, o.to_string())).collect();
        (t.clone(), Book::over(LcClient::over(t, true)))
    }

    fn req(argv: &str, body: Option<&str>) -> Sent {
        (argv.split(' ').map(String::from).collect(), body.map(String::from))
    }

    fn sent(t: &Script) -> Vec<Sent> {
        t.sent.lock().unwrap().clone()
    }

    const MEMORIES: &str = "content memories --json";

    fn go(_: Option<&str>) -> ControlFlow<i32, ()> {
        ControlFlow::Continue(())
    }

    #[test_id(tgo5rt9)]
    #[test]
    fn the_memories_read_is_a_content_memories_json_request() {
        let (t, book) = scripted(&[(0, "warning\n{\"law-a\":\"x\"}\n"), (3, "{}")]);
        assert_eq!(book.memories_text().as_deref(), Ok("{\"law-a\":\"x\"}"));
        assert!(book.memories_text().is_err());
        assert_eq!(sent(&t), vec![req(MEMORIES, None), req(MEMORIES, None)]);
    }

    #[test_id(t0h3ibf)]
    #[test]
    fn an_unreachable_book_is_named_and_is_not_an_empty_one() {
        let (t, book) = scripted(&[(0, "{}"), (2, "cannot tell: down"), (0, "no json here")]);
        assert_eq!(book.memories(), Ok(Memories::new()));
        let refused = book.memories().unwrap_err();
        assert!(refused.starts_with("rule: cannot reach the statute book: ") && refused.contains("down"), "{refused}");
        assert!(book.memories().is_err(), "an unreadable answer is not an empty book");
        let silent = book.memories().unwrap_err();
        assert!(silent.contains("cannot reach the statute book") && silent.contains("store not running"), "{silent}");
        assert_eq!(sent(&t), vec![req(MEMORIES, None); 4]);
    }

    #[test_id(tiakx7n)]
    #[test]
    fn enact_sends_recall_memories_and_remember_with_the_text_as_body() {
        let (t, book) = scripted(&[(0, "old\n"), (0, "{\"law-a\":\"old\"}"), (0, "")]);
        let mut seen = None;
        let done = book.enact("law-a", "new text", |current| {
            seen = current.map(String::from);
            go(current)
        });
        assert_eq!(done.unwrap().put, Put::Written);
        assert_eq!(seen.as_deref(), Some("old\n"));
        assert_eq!(sent(&t), vec![req("content recall law-a", None), req(MEMORIES, None), req("content remember --key law-a", Some("new text"))]);
    }

    #[test_id(tmcfxmh)]
    #[test]
    fn enact_stopped_by_its_gate_or_by_a_duplicate_writes_nothing() {
        let (t, book) = scripted(&[(1, "")]);
        let stopped = book.enact("law-a", "t", |current| {
            assert_eq!(current, None);
            ControlFlow::<i32, ()>::Break(0)
        });
        assert_eq!(stopped.unwrap_err(), Stop::Gate(0));
        assert_eq!(sent(&t), vec![req("content recall law-a", None)]);

        let (t, book) = scripted(&[(1, ""), (0, "{\"law-b\":\"same text\"}")]);
        assert_eq!(book.enact("law-a", "same  text", go).unwrap_err(), Stop::Duplicate("law-b".into()));
        assert_eq!(sent(&t), vec![req("content recall law-a", None), req(MEMORIES, None)]);

        let (t, book) = scripted(&[(1, "")]);
        assert!(matches!(book.enact("law-a", "t", go).unwrap_err(), Stop::Unreachable(_)));
        assert_eq!(sent(&t).len(), 2);
    }

    #[test_id(tydpgik)]
    #[test]
    fn a_refused_remember_is_judged_by_reading_the_book_back() {
        let cases: [(&[(i32, &str)], bool, &str); 3] = [
            (&[(1, ""), (0, "{}"), (1, "refused"), (0, "{\"law-a\":\"t x\"}")], true, "IS in force"),
            (&[(1, ""), (0, "{}"), (1, "refused"), (0, "{}")], false, "nothing written"),
            (&[(1, ""), (0, "{}"), (1, "refused")], false, "cannot be read back"),
        ];
        for (replies, in_force, says) in cases {
            let (t, book) = scripted(replies);
            let put = book.enact("law-a", "t x", go).unwrap().put;
            assert_eq!(put.in_force(), in_force, "{put:?}");
            let notice = put.notice("law-a").unwrap();
            assert!(notice.contains(says) && notice.contains("refused"), "{notice}");
            assert_eq!(sent(&t)[2..], [req("content remember --key law-a", Some("t x")), req(MEMORIES, None)]);
        }
    }

    #[test_id(talzhdb)]
    #[test]
    fn retire_records_then_forgets_and_a_refused_forget_removes_the_record() {
        let (t, book) = scripted(&[(0, ""), (0, "")]);
        assert_eq!(book.retire("law-a", Some(("retired-law-a", "the record"))), Ok(()));
        assert_eq!(sent(&t), vec![req("content remember --key retired-law-a", Some("the record")), req("content forget law-a", None)]);

        let (t, book) = scripted(&[(0, "")]);
        assert_eq!(book.retire("law-a", None), Ok(()));
        assert_eq!(sent(&t), vec![req("content forget law-a", None)]);

        let (t, book) = scripted(&[(0, ""), (3, "no"), (0, "")]);
        assert!(book.retire("law-a", Some(("retired-law-a", "r"))).unwrap_err().contains("failed to forget law-a"));
        assert_eq!(sent(&t)[1..], [req("content forget law-a", None), req("content forget retired-law-a", None)]);

        let (t, book) = scripted(&[(3, "no")]);
        assert!(book.retire("law-a", Some(("retired-law-a", "r"))).unwrap_err().contains("nothing forgotten"));
        assert_eq!(sent(&t).len(), 1);
    }

    #[test_id(txa6em6)]
    #[test]
    fn restore_puts_back_the_prior_text_or_forgets_the_key() {
        let (t, book) = scripted(&[(0, ""), (0, ""), (3, "no")]);
        assert!(book.restore("law-a", Some("prior")));
        assert!(book.restore("law-a", None));
        assert!(!book.restore("law-a", None));
        assert_eq!(sent(&t)[..2], [req("content remember --key law-a", Some("prior")), req("content forget law-a", None)]);
    }

    #[test_id(tpou5od)]
    #[test]
    fn rename_writes_reads_back_records_and_forgets_the_old_key() {
        let (t, book) = scripted(&[(0, ""), (0, "{\"law-new\":\"N.\"}"), (0, ""), (0, ""), (1, "late"), (0, "{\"law-b\":\"B.\"}")]);
        let before: Memories = [("law-old".to_string(), "O.".into()), ("law-b".to_string(), "b.".into())].into();
        let entries = [
            Entry::Rename { from: "law-old".into(), to: "law-new".into(), text: "N.".into(), numbered: true },
            Entry::Amend { key: "law-b".into(), text: "B.".into() },
        ];
        let done = book.rename_apply(&entries, &before, |from, to| format!("{from}>{to}"));
        assert_eq!((done.written, done.statutes, done.runbooks, done.failed), (2, 2, 0, false));
        assert_eq!(done.statute_renames, vec![("law-old".to_string(), "law-new".to_string(), true)]);
        assert_eq!(done.after.keys().collect::<Vec<_>>(), ["law-b", "law-new"]);
        assert_eq!(
            sent(&t),
            vec![
                req("content remember --key law-new", Some("N.")),
                req(MEMORIES, None),
                req("content remember --key retired-law-old", Some("law-old>law-new")),
                req("content forget law-old", None),
                req("content remember --key law-b", Some("B.")),
                req(MEMORIES, None),
            ]
        );
    }

    #[test_id(tdm7vso)]
    #[test]
    fn show_reads_the_mirror_and_asks_the_store_only_for_a_book_never_mirrored() {
        let dir = testkit::TempDir::new("book-read");
        let (t, book) = scripted(&[(0, "{\"law-a\":\"Alpha.\"}")]);
        let first = book.read(dir.path()).unwrap();
        assert_eq!(book.read(dir.path()).unwrap(), first);
        assert_eq!(sent(&t), vec![req(MEMORIES, None)]);

        let empty = testkit::TempDir::new("book-read-down");
        let (_, down) = scripted(&[]);
        assert!(down.read(empty.path()).unwrap_err().contains("cannot reach the statute book"));
    }

    #[test_id(taipxz3)]
    #[test]
    fn open_bead_titles_asks_for_the_not_closed_beads_only() {
        let (t, book) = scripted(&[(0, "[{\"title\":\"one\"},{\"title\":\"two\"}]"), (1, "no")]);
        assert_eq!(book.open_bead_titles(), Ok(vec!["one".to_string(), "two".to_string()]));
        assert!(book.open_bead_titles().is_err());
        assert_eq!(sent(&t)[0], req("content list --brief --json --limit 0", None));
    }
}
