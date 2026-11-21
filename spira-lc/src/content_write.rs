//! The content door's label, note, comment, metadata and priority writes as one SQL transaction
//! on the beads database, so lc-serve applies them without forking `bd`. Each verb makes the
//! row changes bd makes for it: the row itself, `updated_at`, and the `events` row bd files
//! (none for a comment).

use crate::rows::escape;

const ACTOR: &str = "spira-lc";

#[derive(Debug, Clone, PartialEq)]
enum Op {
    LabelAdd(String),
    LabelRemove(String),
    Note(String),
    Comment(String),
    Metadata(String, String),
    Priority(u8),
}

/// A door write lc-serve applies itself; `argv` and `body` are kept for the bd fork a store
/// with no writer configured still takes.
#[derive(Debug, Clone, PartialEq)]
pub struct Mutation {
    pub argv: Vec<String>,
    pub body: Option<String>,
    id: String,
    ops: Vec<Op>,
}

impl Mutation {
    /// The mutation `args` (as `guard` left them) and `body` are, when they are exactly one of
    /// the verbs served here.
    pub fn of(args: &[String], body: Option<&str>) -> Option<Mutation> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let (id, ops) = match (a.as_slice(), body) {
            (["label", "add", id, l], None) => (*id, vec![Op::LabelAdd((*l).into())]),
            (["label", "remove", id, l], None) => (*id, vec![Op::LabelRemove((*l).into())]),
            (["note", id, "--stdin"], Some(b)) => (*id, vec![Op::Note(b.trim_end_matches('\n').into())]),
            (["note", id, t], None) if *t != "--stdin" => (*id, vec![Op::Note((*t).into())]),
            (["comments", "add", id, t], None) => (*id, vec![Op::Comment((*t).into())]),
            (["update", id, flags @ ..], None) if !flags.is_empty() => (*id, update_ops(flags)?),
            _ => return None,
        };
        Some(Mutation { argv: args.to_vec(), body: body.map(str::to_string), id: id.into(), ops })
    }

    pub fn bead(&self) -> &str {
        &self.id
    }

    /// The read that finds the bead, which bd refuses a write to when it is absent.
    pub fn exists_sql(&self, db: &str) -> String {
        format!("SELECT id FROM {db}.issues WHERE id = '{}'", escape(&self.id))
    }

    /// The transaction: `BEGIN`, the row changes, `COMMIT`.
    pub fn script(&self, db: &str) -> Vec<String> {
        let id = escape(&self.id);
        let event = |kind: &str, comment: &str, guard: &str| {
            format!("INSERT INTO {db}.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) SELECT UUID(), '{id}', '{kind}', '{ACTOR}', NULL, NULL, {comment}, UTC_TIMESTAMP() FROM DUAL{guard}")
        };
        let lit = |s: &str| format!("'{}'", escape(s));
        let stamp = |column: &str| format!("DATE_FORMAT({column}, '%Y-%m-%dT%H:%i:%sZ')");
        let before = format!(
            "(SELECT JSON_MERGE_PATCH('{{}}', JSON_OBJECT('id', id, 'title', title, 'description', NULLIF(description, ''), 'design', NULLIF(design, ''), 'acceptance_criteria', NULLIF(acceptance_criteria, ''), 'notes', NULLIF(notes, ''), 'spec_id', NULLIF(spec_id, ''), 'status', status, 'priority', priority, 'issue_type', issue_type, 'assignee', NULLIF(assignee, ''), 'estimated_minutes', estimated_minutes, 'owner', NULLIF(owner, ''), 'created_at', {}, 'created_by', NULLIF(created_by, ''), 'updated_at', {}, 'closed_at', {}, 'close_reason', NULLIF(close_reason, ''), 'external_ref', NULLIF(external_ref, ''), 'metadata', NULLIF(metadata, JSON_OBJECT()), 'labels', NULLIF((SELECT JSON_ARRAYAGG(label) FROM (SELECT label FROM {db}.labels WHERE issue_id = '{id}' ORDER BY label) AS l), JSON_ARRAY()))) FROM {db}.issues WHERE id = '{id}')",
            stamp("created_at"),
            stamp("updated_at"),
            stamp("closed_at")
        );
        let mut s = vec!["BEGIN".to_string()];
        for (n, op) in self.ops.iter().enumerate() {
            let settled = |column: &str| {
                format!("UPDATE {db}.events SET new_value = (SELECT JSON_OBJECT('{column}', {column}) FROM {db}.issues WHERE id = '{id}') WHERE id = @event{n}")
            };
            let filed = |old: &str| format!("INSERT INTO {db}.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) VALUES (@event{n}, '{id}', 'updated', '{ACTOR}', {old}, NULL, NULL, UTC_TIMESTAMP())");
            match op {
                Op::LabelAdd(l) => {
                    let e = escape(l);
                    s.push(event("label_added", &lit(&format!("Added label: {l}")), &format!(" WHERE NOT EXISTS (SELECT 1 FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}')")));
                    s.push(format!("INSERT IGNORE INTO {db}.labels (issue_id, label) VALUES ('{id}', '{e}')"));
                }
                Op::LabelRemove(l) => {
                    let e = escape(l);
                    s.push(event("label_removed", &lit(&format!("Removed label: {l}")), &format!(" WHERE EXISTS (SELECT 1 FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}')")));
                    s.push(format!("DELETE FROM {db}.labels WHERE issue_id = '{id}' AND label = '{e}'"));
                }
                Op::Note(t) => {
                    s.push(format!("SET @event{n} = UUID()"));
                    s.push(filed(&before));
                    s.push(format!("UPDATE {db}.issues SET notes = CASE WHEN notes IS NULL OR notes = '' THEN {t} ELSE CONCAT(notes, CHAR(10), {t}) END WHERE id = '{id}'", t = lit(t)));
                    s.push(settled("notes"));
                }
                Op::Comment(t) => {
                    s.push(format!("INSERT INTO {db}.comments (id, issue_id, author, text, created_at) VALUES (UUID(), '{id}', '{ACTOR}', {}, UTC_TIMESTAMP())", lit(t)));
                }
                Op::Metadata(k, v) => {
                    s.push(format!("SET @event{n} = UUID()"));
                    s.push(filed(&before));
                    s.push(format!("UPDATE {db}.issues SET metadata = JSON_SET(COALESCE(metadata, JSON_OBJECT()), '$.\"{}\"', {}) WHERE id = '{id}'", escape(k), metadata_value(v)));
                    s.push(settled("metadata"));
                }
                Op::Priority(p) => {
                    s.push(format!("SET @event{n} = UUID()"));
                    s.push(filed(&before));
                    s.push(format!("UPDATE {db}.issues SET priority = {p} WHERE id = '{id}'"));
                    s.push(settled("priority"));
                }
            }
        }
        if self.ops.iter().any(|op| !matches!(op, Op::LabelAdd(_) | Op::LabelRemove(_) | Op::Comment(_))) {
            s.push(format!("UPDATE {db}.issues SET updated_at = UTC_TIMESTAMP(), row_lock = CAST((RAND() - 0.5) * 1.8e19 AS SIGNED) WHERE id = '{id}'"));
        }
        s.push("COMMIT".into());
        s
    }
}

/// How bd stores a `--set-metadata` value: a number, `true`, `false` or `null` as that JSON value,
/// anything else (a quoted string, an object, an array) as the text it was given.
fn metadata_value(v: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(v) {
        Ok(j) if v.trim() == v && (j.is_number() || j.is_boolean() || j.is_null()) => format!("CAST('{}' AS JSON)", escape(v)),
        _ => format!("'{}'", escape(v)),
    }
}

fn update_ops(flags: &[&str]) -> Option<Vec<Op>> {
    let mut ops = Vec::new();
    let mut it = flags.iter();
    while let Some(f) = it.next() {
        let v = it.next()?;
        ops.push(match *f {
            "--add-label" if !v.is_empty() => Op::LabelAdd((*v).into()),
            "--remove-label" if !v.is_empty() => Op::LabelRemove((*v).into()),
            "--set-metadata" => {
                let (k, val) = v.split_once('=')?;
                if k.is_empty() || k.contains(['"', '\\']) {
                    return None;
                }
                Op::Metadata(k.into(), val.into())
            }
            "--priority" | "-p" => Op::Priority(v.parse().ok().filter(|p| *p <= 4)?),
            _ => return None,
        });
    }
    Some(ops)
}

/// Why a transaction did not commit.
#[derive(Debug, PartialEq)]
pub enum Fail {
    /// COMMIT lost a serialization race (40001): nothing was applied.
    Conflict,
    Other(String),
}

/// Where a transaction's statements go, in order, on one session.
pub trait Tx {
    fn send(&mut self, sql: &str) -> Result<(), Fail>;
}

fn once(script: &[String], tx: &mut dyn Tx) -> Result<(), Fail> {
    script.iter().try_for_each(|s| tx.send(s))
}

/// Applies `script`; a conflict is retried once. A statement that fails ends the transaction
/// with no `COMMIT` sent.
pub fn apply(script: &[String], tx: &mut dyn Tx) -> Result<(), String> {
    match once(script, tx) {
        Err(Fail::Conflict) => match once(script, tx) {
            Err(Fail::Conflict) => Err("the write conflicted with another writer twice; nothing was applied".into()),
            other => other.map_err(|f| format!("{f:?}")),
        },
        Err(Fail::Other(e)) => Err(e),
        Ok(()) => Ok(()),
    }
}

/// The Dolt commit the writes owe, taken off the request: any number of writes before the
/// committer wakes cost one commit.
pub struct Committer {
    owed: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
}

impl Committer {
    pub const fn new() -> Committer {
        Committer { owed: std::sync::Mutex::new(false), wake: std::sync::Condvar::new() }
    }

    pub fn owe(&self) {
        *self.owed.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.wake.notify_one();
    }

    /// Blocks until a commit is owed, then settles every write owed so far.
    pub fn take(&self) {
        let mut g = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        while !*g {
            g = self.wake.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        *g = false;
    }
}

static COMMITTER: Committer = Committer::new();

struct Writer {
    conn: crate::db::Conn,
    db: String,
}

/// Keeps only a value that was built: a failed build leaves nothing, so the next call builds again.
struct Built<T> {
    value: std::sync::OnceLock<T>,
    building: std::sync::Mutex<()>,
}

impl<T> Built<T> {
    const fn new() -> Self {
        Built { value: std::sync::OnceLock::new(), building: std::sync::Mutex::new(()) }
    }

    fn get_or_build(&self, build: impl FnOnce() -> Result<T, String>) -> Result<&T, String> {
        if let Some(v) = self.value.get() {
            return Ok(v);
        }
        let _one_builder = self.building.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = self.value.get() {
            return Ok(v);
        }
        let built = build()?;
        Ok(self.value.get_or_init(|| built))
    }
}

static WRITER: Built<Writer> = Built::new();

fn build_writer(file: &str) -> Result<Writer, String> {
    let s = crate::content_sql::Settings::from_config()?;
    let user = s.user.strip_suffix("_ro").unwrap_or(&s.user).to_string();
    let db = crate::plan::content_db()?.to_string();
    let conn = crate::db::Conn::writer(&s.host, s.port, &user, file, db.trim_matches('`'))?;
    Ok(Writer { conn, db })
}

/// The writer connection, or `None` when no writer credential is configured: the door then
/// forks bd for these verbs as it always did.
fn writer() -> Option<Result<&'static Writer, String>> {
    let file = match spira_config::process::cfg("SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE") {
        Ok(f) => f,
        Err(e) => return Some(Err(e)),
    };
    if file.trim().is_empty() {
        return None;
    }
    Some(WRITER.get_or_build(|| build_writer(&file)))
}

/// Applies `m` to the beads database in one transaction. `None` when no writer is configured.
pub fn run(m: &Mutation) -> Option<Result<String, String>> {
    let w = match writer()? {
        Ok(w) => w,
        Err(e) => return Some(Err(e)),
    };
    let found = w.conn.query(&m.exists_sql(&w.db)).map_err(|e| format!("{e:?}"));
    Some(match found {
        Err(e) => Err(e),
        Ok(rows) if rows.is_empty() => Err(format!("no issue found matching {:?}", m.bead())),
        Ok(_) => w.conn.transaction(|tx| apply(&m.script(&w.db), tx)).and_then(|r| r).map(|()| {
            COMMITTER.owe();
            String::new()
        }),
    })
}

/// The committer thread lc-serve runs.
pub fn run_committer() {
    loop {
        COMMITTER.take();
        let Some(Ok(w)) = writer() else { continue };
        match w.conn.execute("CALL DOLT_COMMIT('-Am', 'spira-lc: content writes')") {
            Ok(()) => {}
            Err(e) if format!("{e:?}").contains("nothing to commit") => {}
            Err(e) => {
                eprintln!("spira-lc serve: dolt commit of content writes failed: {e:?}");
                std::thread::sleep(std::time::Duration::from_secs(1));
                COMMITTER.owe();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_writer_build_is_tried_again_and_a_built_writer_is_kept() {
        let cell: Built<u32> = Built::new();
        let builds = std::cell::Cell::new(0);
        let build = |r: Result<u32, String>| {
            builds.set(builds.get() + 1);
            r
        };
        assert_eq!(cell.get_or_build(|| build(Err("reading /x: No such file".into()))), Err("reading /x: No such file".to_string()));
        assert_eq!(cell.get_or_build(|| build(Ok(7))), Ok(&7));
        assert_eq!(cell.get_or_build(|| build(Ok(8))), Ok(&7));
        assert_eq!(builds.get(), 2);
    }

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn script(args: &[&str], body: Option<&str>) -> Vec<String> {
        Mutation::of(&v(args), body).expect("a served verb").script("`beads`")
    }

    struct Rec {
        sent: Vec<String>,
        fail_at: Option<(usize, Fail)>,
        conflicts: usize,
    }

    impl Tx for Rec {
        fn send(&mut self, sql: &str) -> Result<(), Fail> {
            self.sent.push(sql.into());
            if sql == "COMMIT" && self.conflicts > 0 {
                self.conflicts -= 1;
                return Err(Fail::Conflict);
            }
            match &self.fail_at {
                Some((n, _)) if *n == self.sent.len() => Err(self.fail_at.take().unwrap().1),
                _ => Ok(()),
            }
        }
    }

    #[test]
    fn only_the_served_verbs_are_mutations() {
        for argv in [
            &["close", "sp-a"][..],
            &["set-state", "sp-a", "branch=x"],
            &["dep", "add", "sp-a", "sp-b"],
            &["update", "sp-a", "--assignee", "x"],
            &["update", "sp-a", "--body-file", "-"],
            &["update", "sp-a", "--set-metadata", "a\"b=c"],
            &["update", "sp-a", "--priority", "9"],
            &["update", "sp-a"],
            &["note", "sp-a", "--stdin"],
            &["create", "T"],
        ] {
            assert_eq!(Mutation::of(&v(argv), None), None, "{argv:?}");
        }
        assert!(Mutation::of(&v(&["note", "sp-a", "--stdin"]), Some("b")).is_some());
        assert!(Mutation::of(&v(&["update", "sp-a", "--add-label", "x", "--priority", "1"]), None).is_some());
    }

    #[test]
    fn a_label_add_is_one_transaction_that_records_the_event_before_the_row_and_leaves_the_issue_row_alone() {
        let s = script(&["label", "add", "sp-a", "overseer"], None);
        assert_eq!(
            s,
            vec![
                "BEGIN",
                "INSERT INTO `beads`.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) SELECT UUID(), 'sp-a', 'label_added', 'spira-lc', NULL, NULL, 'Added label: overseer', UTC_TIMESTAMP() FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM `beads`.labels WHERE issue_id = 'sp-a' AND label = 'overseer')",
                "INSERT IGNORE INTO `beads`.labels (issue_id, label) VALUES ('sp-a', 'overseer')",
                "COMMIT",
            ]
        );
    }

    #[test]
    fn a_label_remove_and_an_update_that_carries_it_make_the_same_changes() {
        let a = script(&["label", "remove", "sp-a", "branch:spira/sp-a"], None);
        let b = script(&["update", "sp-a", "--remove-label", "branch:spira/sp-a"], None);
        assert_eq!(a, b);
        assert!(a[2].starts_with("DELETE FROM `beads`.labels WHERE issue_id = 'sp-a' AND label = 'branch:spira/sp-a'"));
    }

    #[test]
    fn a_note_appends_a_comment_inserts_metadata_merges_and_priority_sets() {
        let n = script(&["note", "sp-a", "--stdin"], Some("body\n"));
        assert!(n[3].contains("CONCAT(notes, CHAR(10), 'body')"), "{}", n[3]);
        let c = script(&["comments", "add", "sp-a", "hi"], None);
        assert!(c[1].starts_with("INSERT INTO `beads`.comments (id, issue_id, author, text, created_at) VALUES (UUID(), 'sp-a', 'spira-lc', 'hi',"));
        assert_eq!(c.len(), 3, "a comment files no event and leaves the issue row alone, as bd does: {c:?}");
        let m = script(&["update", "sp-a", "--set-metadata", "k=v"], None);
        assert!(m[3].contains("JSON_SET(COALESCE(metadata, JSON_OBJECT()), '$.\"k\"', 'v')"), "{}", m[3]);
        let p = script(&["update", "sp-a", "-p", "2"], None);
        assert_eq!(p[3], "UPDATE `beads`.issues SET priority = 2 WHERE id = 'sp-a'");
    }

    #[test]
    fn an_update_event_carries_the_issue_as_it_was_and_the_value_it_became() {
        for s in [script(&["note", "sp-a", "t"], None), script(&["update", "sp-a", "--set-metadata", "k=v"], None), script(&["update", "sp-a", "-p", "1"], None)] {
            assert_eq!(s[1], "SET @event0 = UUID()");
            assert!(s[2].starts_with("INSERT INTO `beads`.events (id, issue_id, event_type, actor, old_value, new_value, comment, created_at) VALUES (@event0, 'sp-a', 'updated', 'spira-lc', (SELECT JSON_MERGE_PATCH("), "{}", s[2]);
            assert!(s[2].contains("'labels', NULLIF((SELECT JSON_ARRAYAGG(label)"), "{}", s[2]);
            assert!(s[4].starts_with("UPDATE `beads`.events SET new_value = (SELECT JSON_OBJECT(") && s[4].ends_with("WHERE id = @event0"), "{}", s[4]);
            assert!(s[5].contains("row_lock = CAST(") && s[5].contains("updated_at = UTC_TIMESTAMP()"), "{}", s[5]);
        }
    }

    #[test]
    fn a_metadata_number_boolean_or_null_is_stored_as_that_json_value_and_anything_else_as_text() {
        for (v, json) in [("5", true), ("-3", true), ("1.5", true), ("true", true), ("null", true), ("hi", false), ("\"q\"", false), ("[1,2]", false), ("{\"x\":1}", false), (" 5", false), ("", false)] {
            assert_eq!(metadata_value(v).starts_with("CAST("), json, "{v:?}");
        }
    }

    #[test]
    fn a_quote_in_a_note_cannot_end_the_string() {
        let s = script(&["note", "sp-a", "--stdin"], Some("x'; DROP TABLE issues; --"));
        assert!(s[3].contains(r"'x\'; DROP TABLE issues; --'"), "{}", s[3]);
        assert!(!s[3].contains("x'; DROP"));
    }

    #[test]
    fn a_failure_on_the_second_statement_sends_no_commit() {
        let s = script(&["label", "add", "sp-a", "x"], None);
        let mut tx = Rec { sent: vec![], fail_at: Some((2, Fail::Other("boom".into()))), conflicts: 0 };
        assert_eq!(apply(&s, &mut tx), Err("boom".into()));
        assert_eq!(tx.sent.len(), 2);
        assert!(!tx.sent.iter().any(|q| q == "COMMIT"));
    }

    #[test]
    fn writes_owed_before_the_committer_wakes_cost_one_commit() {
        let c = Committer::new();
        c.owe();
        c.owe();
        c.owe();
        c.take();
        assert!(!*c.owed.lock().unwrap(), "one take settles every write owed so far");
    }

    #[test]
    fn a_conflict_retries_once_then_gives_up() {
        let s = script(&["label", "add", "sp-a", "x"], None);
        let mut tx = Rec { sent: vec![], fail_at: None, conflicts: 1 };
        assert_eq!(apply(&s, &mut tx), Ok(()));
        assert_eq!(tx.sent.len(), s.len() * 2);
        let mut tx = Rec { sent: vec![], fail_at: None, conflicts: 2 };
        assert!(apply(&s, &mut tx).is_err());
        assert_eq!(tx.sent.len(), s.len() * 2);
    }
}
