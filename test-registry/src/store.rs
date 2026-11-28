//! Schema, transitions and the deny-list snapshot. Every function takes the connection.

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::id::{check_test_id, valid_bead_id};

const SCHEMA_VERSION: i64 = 2;
const COLUMNS: &str = "test, state, since, until, bead, reason, actor";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Disabled,
    Quarantined,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub test: String,
    pub state: String,
    pub since: i64,
    pub until: i64,
    pub bead: String,
    pub reason: String,
    pub actor: String,
}

pub struct Set<'a> {
    pub test: &'a str,
    pub bead: &'a str,
    pub reason: &'a str,
    pub actor: &'a str,
    pub until: Option<i64>,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Disabled => "disabled",
            State::Quarantined => "quarantined",
        }
    }
}

fn sql(e: rusqlite::Error) -> String {
    format!("registry: {e}")
}

fn exec(conn: &Connection, sql_text: &str, p: impl rusqlite::Params) -> Result<usize, String> {
    conn.prepare_cached(sql_text)
        .and_then(|mut s| s.execute(p))
        .map_err(sql)
}

fn scalar(conn: &Connection, sql_text: &str) -> Result<i64, String> {
    conn.prepare_cached(sql_text)
        .and_then(|mut s| s.query_row([], |r| r.get(0)))
        .map_err(sql)
}

pub fn open(path: &str) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    init(&conn).map_err(|e| format!("cannot open {path}: {e}"))?;
    Ok(conn)
}

pub fn init(conn: &Connection) -> Result<(), String> {
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(sql)?;
    conn.prepare("PRAGMA journal_mode=WAL")
        .and_then(|mut s| s.query_row([], |_| Ok(())))
        .map_err(sql)?;
    let version = scalar(conn, "PRAGMA user_version")?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "registry schema version {version} is newer than this binary's {SCHEMA_VERSION}"
        ));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(sql)?;
    for ddl in [
        "CREATE TABLE IF NOT EXISTS test_state(
            test TEXT PRIMARY KEY,
            state TEXT NOT NULL CHECK (state IN ('disabled','quarantined')),
            since INTEGER NOT NULL, until INTEGER NOT NULL,
            bead TEXT NOT NULL, reason TEXT NOT NULL, actor TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS test_state_history(
            seq INTEGER PRIMARY KEY, at INTEGER NOT NULL, verb TEXT NOT NULL, test TEXT NOT NULL,
            state TEXT NOT NULL, since INTEGER, until INTEGER, bead TEXT,
            reason TEXT NOT NULL, actor TEXT NOT NULL)",
    ] {
        exec(&tx, ddl, [])?;
    }
    migrate_name_keys(&tx)?;
    exec(&tx, "PRAGMA user_version = 2", [])?;
    tx.commit().map_err(sql)
}

/// Rewrites each `unit:`/`integration:` row to `test:<id>` by its path's last segment; a row whose
/// last segment is no id names a path the stamp retired and is activated, with the reason recorded.
fn migrate_name_keys(conn: &Connection) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let old: Vec<String> = {
        let mut st = conn
            .prepare("SELECT test FROM test_state WHERE test LIKE 'unit:%' OR test LIKE 'integration:%'")
            .map_err(sql)?;
        let rows = st.query_map([], |r| r.get(0)).map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)?
    };
    for name in old {
        let id = spira_config::unit_tier::test_id_of(&name);
        match id {
            Some(id) => {
                let new = format!("test:{id}");
                exec(conn, "DELETE FROM test_state WHERE test = ?1 AND EXISTS (SELECT 1 FROM test_state WHERE test = ?2)", params![name, new])?;
                exec(conn, "UPDATE test_state SET test = ?2 WHERE test = ?1", params![name, new])?;
                exec(conn, "UPDATE test_state_history SET test = ?2 WHERE test = ?1", params![name, new])?;
            }
            None => {
                exec(conn, "DELETE FROM test_state WHERE test = ?1", [&name])?;
                exec(conn, "INSERT INTO test_state_history(at, verb, test, state, reason, actor) VALUES (?1,'activate',?2,'active',?3,'test-registry')", params![now, name, format!("retired by the test-id migration: named {name}")])?;
            }
        }
    }
    Ok(())
}

fn set(
    conn: &Connection,
    verb: &str,
    state: State,
    now: i64,
    hold_secs: i64,
    s: &Set,
) -> Result<Row, String> {
    check_test_id(s.test)?;
    if !valid_bead_id(s.bead) {
        return Err(format!(
            "bead {:?} is not a bead id: a row needs one",
            s.bead
        ));
    }
    if s.reason.trim().is_empty() || s.actor.trim().is_empty() {
        return Err("reason and actor must not be empty".into());
    }
    let until = s.until.unwrap_or(now + hold_secs);
    if until <= now {
        return Err(format!("until {until} is not after now {now}"));
    }
    let row = Row {
        test: s.test.into(),
        state: state.as_str().into(),
        since: now,
        until,
        bead: s.bead.into(),
        reason: s.reason.into(),
        actor: s.actor.into(),
    };
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(sql)?;
    exec(&tx, "INSERT OR REPLACE INTO test_state(test, state, since, until, bead, reason, actor) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![row.test, row.state, row.since, row.until, row.bead, row.reason, row.actor])?;
    exec(&tx, "INSERT INTO test_state_history(at, verb, test, state, since, until, bead, reason, actor) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![now, verb, row.test, row.state, row.since, row.until, row.bead, row.reason, row.actor])?;
    tx.commit().map_err(sql)?;
    Ok(row)
}

pub fn disable(conn: &Connection, now: i64, hold_secs: i64, s: &Set) -> Result<Row, String> {
    set(conn, "disable", State::Disabled, now, hold_secs, s)
}

pub fn quarantine(conn: &Connection, now: i64, hold_secs: i64, s: &Set) -> Result<Row, String> {
    set(conn, "quarantine", State::Quarantined, now, hold_secs, s)
}

pub fn activate(
    conn: &Connection,
    now: i64,
    test: &str,
    reason: &str,
    actor: &str,
) -> Result<(), String> {
    check_test_id(test)?;
    if reason.trim().is_empty() || actor.trim().is_empty() {
        return Err("reason and actor must not be empty".into());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(sql)?;
    let gone = exec(&tx, "DELETE FROM test_state WHERE test = ?1", [test])?;
    if gone == 0 {
        return Err(format!("{test} has no row: it is already active"));
    }
    exec(&tx, "INSERT INTO test_state_history(at, verb, test, state, reason, actor) VALUES (?1,'activate',?2,'active',?3,?4)", params![now, test, reason, actor])?;
    tx.commit().map_err(sql)
}

/// Deletes every row whose bead is named, one `cleared-landed` history row each; returns the cleared test ids.
pub fn clear_landed(
    conn: &Connection,
    now: i64,
    beads: &[&str],
    actor: &str,
) -> Result<Vec<String>, String> {
    if actor.trim().is_empty() {
        return Err("actor must not be empty".into());
    }
    if let Some(bad) = beads.iter().find(|b| !valid_bead_id(b)) {
        return Err(format!("bead {bad:?} is not a bead id"));
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(sql)?;
    let mut cleared = Vec::new();
    for row in list(&tx)? {
        if !beads.contains(&row.bead.as_str()) {
            continue;
        }
        exec(&tx, "DELETE FROM test_state WHERE test = ?1", [&row.test])?;
        exec(&tx, "INSERT INTO test_state_history(at, verb, test, state, since, until, bead, reason, actor) VALUES (?1,'cleared-landed',?2,'active',?3,?4,?5,?6,?7)", params![now, row.test, row.since, row.until, row.bead, format!("the landing of {} cleared the row", row.bead), actor])?;
        cleared.push(row.test);
    }
    tx.commit().map_err(sql)?;
    Ok(cleared)
}

/// Moves every row naming `from` to name `to`, one `repointed` history row each; returns the moved test ids.
pub fn repoint(
    conn: &Connection,
    now: i64,
    from: &str,
    to: &str,
    actor: &str,
) -> Result<Vec<String>, String> {
    if actor.trim().is_empty() {
        return Err("actor must not be empty".into());
    }
    for b in [from, to] {
        if !valid_bead_id(b) {
            return Err(format!("bead {b:?} is not a bead id"));
        }
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(sql)?;
    let mut moved = Vec::new();
    for row in list(&tx)? {
        if row.bead != from {
            continue;
        }
        exec(
            &tx,
            "UPDATE test_state SET bead = ?2 WHERE test = ?1",
            params![row.test, to],
        )?;
        exec(&tx, "INSERT INTO test_state_history(at, verb, test, state, since, until, bead, reason, actor) VALUES (?1,'repointed',?2,?3,?4,?5,?6,?7,?8)", params![now, row.test, row.state, row.since, row.until, to, format!("moved from {from} to its fix {to}"), actor])?;
        moved.push(row.test);
    }
    tx.commit().map_err(sql)?;
    Ok(moved)
}

fn row_of(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        test: r.get(0)?,
        state: r.get(1)?,
        since: r.get(2)?,
        until: r.get(3)?,
        bead: r.get(4)?,
        reason: r.get(5)?,
        actor: r.get(6)?,
    })
}

/// Every test ever disabled, with the first time it was: `activate` and a later re-disable do not move it.
pub fn ever_disabled(conn: &Connection) -> Result<Vec<(String, i64)>, String> {
    let mut stmt = conn
        .prepare("SELECT test, MIN(at) FROM test_state_history WHERE verb = 'disable' GROUP BY test ORDER BY test")
        .map_err(sql)?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql)
}

pub fn show(conn: &Connection, test: &str) -> Result<Option<Row>, String> {
    let mut st = conn
        .prepare(&format!("SELECT {COLUMNS} FROM test_state WHERE test = ?1"))
        .map_err(sql)?;
    let mut rows = st.query_map([test], row_of).map_err(sql)?;
    rows.next().transpose().map_err(sql)
}

pub fn list(conn: &Connection) -> Result<Vec<Row>, String> {
    let mut st = conn
        .prepare(&format!("SELECT {COLUMNS} FROM test_state ORDER BY test"))
        .map_err(sql)?;
    let rows = st.query_map([], row_of).map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

pub fn history(conn: &Connection, test: &str) -> Result<Vec<serde_json::Value>, String> {
    let mut st = conn
        .prepare("SELECT seq, at, verb, state, since, until, bead, reason, actor FROM test_state_history WHERE test = ?1 ORDER BY seq")
        .map_err(sql)?;
    let rows = st
        .query_map([test], |r| {
            Ok(serde_json::json!({
                "seq": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "verb": r.get::<_, String>(2)?,
                "test": test, "state": r.get::<_, String>(3)?, "since": r.get::<_, Option<i64>>(4)?,
                "until": r.get::<_, Option<i64>>(5)?, "bead": r.get::<_, Option<String>>(6)?,
                "reason": r.get::<_, String>(7)?, "actor": r.get::<_, String>(8)?,
            }))
        })
        .map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

/// The rows with `since <= at < until`, in the one text every gate and round parses.
pub fn try_deny_list(conn: &Connection, at: i64) -> Result<String, String> {
    let rows: Vec<Row> = list(conn)?
        .into_iter()
        .filter(|r| r.since <= at && at < r.until)
        .collect();
    let mut out = format!("# test-registry deny-list at={at} rows={}\n", rows.len());
    for r in rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            r.state, r.test, r.bead, r.until
        ));
    }
    Ok(out)
}

/// [`try_deny_list`] for a caller with no error path: an unreadable registry panics, never an empty list.
pub fn deny_list(conn: &Connection, at: i64) -> String {
    try_deny_list(conn, at).unwrap_or_else(|e| panic!("deny list unreadable: {e}"))
}

/// A consistent copy of the open file at `out`, written to `<out>.tmp` and renamed. Returns bytes and test_state rows.
pub fn backup(conn: &Connection, out: &str) -> Result<(u64, i64), String> {
    let tmp = format!("{out}.tmp");
    let _ = std::fs::remove_file(&tmp);
    let copied: Result<i64, String> = (|| {
        let mut dst = Connection::open(&tmp).map_err(sql)?;
        rusqlite::backup::Backup::new(conn, &mut dst)
            .and_then(|b| b.run_to_completion(256, std::time::Duration::from_millis(10), None))
            .map_err(sql)?;
        let rows = scalar(&dst, "SELECT count(*) FROM test_state")?;
        dst.close().map_err(|(_, e)| sql(e))?;
        Ok(rows)
    })();
    let rows = copied.inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    std::fs::rename(&tmp, out).map_err(|e| format!("cannot rename {tmp} to {out}: {e}"))?;
    let bytes = std::fs::metadata(out)
        .map_err(|e| format!("cannot stat {out}: {e}"))?
        .len();
    Ok((bytes, rows))
}

pub fn digest(text: &str) -> String {
    let hex: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    hex[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        c
    }

    fn s<'a>(test: &'a str, until: Option<i64>) -> Set<'a> {
        Set {
            test,
            bead: "sp-abc123",
            reason: "flaky",
            actor: "ryan",
            until,
        }
    }

    #[test_id(tgh5i1d)]
    #[test]
    fn ever_disabled_gives_each_test_once_at_its_first_disable_and_ignores_activate() {
        let c = db();
        disable(&c, 100, 50, &s("test:t0oneaa", None)).unwrap();
        disable(&c, 200, 50, &s("test:t0twoaa", None)).unwrap();
        activate(&c, 300, "test:t0oneaa", "fixed", "ryan").unwrap();
        disable(&c, 400, 50, &s("test:t0oneaa", None)).unwrap();
        activate(&c, 500, "test:t0twoaa", "fixed", "ryan").unwrap();
        assert_eq!(ever_disabled(&c).unwrap(), vec![("test:t0oneaa".to_string(), 100), ("test:t0twoaa".to_string(), 200)]);
    }

    #[test_id(tvqh1ej)]
    #[test]
    fn deny_list_includes_a_live_row_and_excludes_an_expired_one() {
        let c = db();
        disable(&c, 1000, 600, &s("suite:test-a.sh", Some(2000))).unwrap();
        disable(&c, 1000, 600, &s("suite:test-b.sh", Some(1500))).unwrap();
        assert!(deny_list(&c, 1499).contains("suite:test-b.sh"));
        let at = deny_list(&c, 1500);
        assert!(
            at.contains("suite:test-a.sh") && !at.contains("suite:test-b.sh"),
            "{at}"
        );
        assert!(!deny_list(&c, 2000).contains("suite:test-a.sh"));
        assert!(
            !deny_list(&c, 999).contains("suite:test-a.sh"),
            "before since"
        );
    }

    #[test_id(toounrv)]
    #[test]
    fn a_row_with_no_until_expires_exactly_hold_secs_after_since() {
        let c = db();
        let r = disable(&c, 1000, 600, &s("suite:test-a.sh", None)).unwrap();
        assert_eq!((r.since, r.until), (1000, 1600));
        assert!(deny_list(&c, 1599).contains("suite:test-a.sh"));
        assert!(!deny_list(&c, 1600).contains("suite:test-a.sh"));
    }

    #[test_id(t35qqc3)]
    #[test]
    fn an_empty_registry_prints_the_header_alone() {
        assert_eq!(
            deny_list(&db(), 42),
            "# test-registry deny-list at=42 rows=0\n"
        );
    }

    #[test_id(tl5krtj)]
    #[test]
    fn three_rows_print_sorted_with_tabs() {
        let c = db();
        disable(&c, 10, 600, &s("test:t0aaaaa", Some(900))).unwrap();
        quarantine(&c, 10, 600, &s("test:t0bbbbb", Some(800))).unwrap();
        disable(&c, 10, 600, &s("suite:test-a.sh", Some(700))).unwrap();
        let want = "# test-registry deny-list at=50 rows=3\n\
                    disabled\tsuite:test-a.sh\tsp-abc123\t700\n\
                    disabled\ttest:t0aaaaa\tsp-abc123\t900\n\
                    quarantined\ttest:t0bbbbb\tsp-abc123\t800\n";
        assert_eq!(deny_list(&c, 50), want);
    }

    #[test_id(tm1g4t2)]
    #[test]
    fn a_v1_file_opens_as_v2_with_ids_rewritten_and_retired_paths_activated() {
        let c = Connection::open_in_memory().unwrap();
        for ddl in [
            "CREATE TABLE test_state(test TEXT PRIMARY KEY, state TEXT NOT NULL CHECK (state IN ('disabled','quarantined')), since INTEGER NOT NULL, until INTEGER NOT NULL, bead TEXT NOT NULL, reason TEXT NOT NULL, actor TEXT NOT NULL)",
            "CREATE TABLE test_state_history(seq INTEGER PRIMARY KEY, at INTEGER NOT NULL, verb TEXT NOT NULL, test TEXT NOT NULL, state TEXT NOT NULL, since INTEGER, until INTEGER, bead TEXT, reason TEXT NOT NULL, actor TEXT NOT NULL)",
            "INSERT INTO test_state VALUES ('unit:loom a::b::t0aaaaa','disabled',1,9999999999,'sp-x','flaky','a')",
            "INSERT INTO test_state VALUES ('unit:loom a::old','disabled',1,9999999999,'sp-y','flaky','a')",
            "INSERT INTO test_state VALUES ('suite:x.sh','disabled',1,9999999999,'sp-z','flaky','a')",
            "INSERT INTO test_state_history(at,verb,test,state,reason,actor) VALUES (1,'disable','unit:loom a::b::t0aaaaa','disabled','flaky','a')",
            "PRAGMA user_version = 1",
        ] {
            exec(&c, ddl, []).unwrap();
        }
        init(&c).unwrap();
        assert_eq!(scalar(&c, "PRAGMA user_version").unwrap(), 2);
        let rows: Vec<(String, String)> = list(&c).unwrap().into_iter().map(|r| (r.test, r.state)).collect();
        assert_eq!(rows, [("suite:x.sh".into(), "disabled".into()), ("test:t0aaaaa".into(), "disabled".into())]);
        assert_eq!(history(&c, "test:t0aaaaa").unwrap().len(), 1, "history follows the rename");
        let old = history(&c, "unit:loom a::old").unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0]["verb"], "activate");
        assert_eq!(old[0]["reason"], "retired by the test-id migration: named unit:loom a::old");
    }

    #[test_id(tcwkruk)]
    #[test]
    fn digest_is_twelve_hex_of_the_sha256() {
        assert_eq!(digest("abc"), "ba7816bf8f01");
    }

    #[test_id(tgiu45g)]
    #[test]
    fn disable_on_an_existing_row_updates_it_and_keeps_two_history_rows() {
        let c = db();
        disable(&c, 10, 600, &s("suite:test-a.sh", Some(100))).unwrap();
        quarantine(&c, 20, 600, &s("suite:test-a.sh", Some(200))).unwrap();
        assert_eq!(list(&c).unwrap().len(), 1);
        let r = show(&c, "suite:test-a.sh").unwrap().unwrap();
        assert_eq!(
            (r.state.as_str(), r.since, r.until),
            ("quarantined", 20, 200)
        );
        assert_eq!(history(&c, "suite:test-a.sh").unwrap().len(), 2);
    }

    #[test_id(t4ibtnr)]
    #[test]
    fn activate_deletes_the_row_and_appends_an_active_history_row() {
        let c = db();
        disable(&c, 10, 600, &s("suite:test-a.sh", Some(100))).unwrap();
        activate(&c, 30, "suite:test-a.sh", "fixed", "ryan").unwrap();
        assert!(show(&c, "suite:test-a.sh").unwrap().is_none());
        let h = history(&c, "suite:test-a.sh").unwrap();
        assert_eq!(
            (h.len(), h[1]["state"].as_str(), h[1]["verb"].as_str()),
            (2, Some("active"), Some("activate"))
        );
    }

    #[test_id(t54jvwm)]
    #[test]
    fn activate_with_no_row_is_refused() {
        let c = db();
        assert!(activate(&c, 30, "suite:test-a.sh", "x", "ryan").is_err());
        assert!(history(&c, "suite:test-a.sh").unwrap().is_empty());
    }

    #[test_id(tyv5mau)]
    #[test]
    fn refused_writes_leave_the_tables_unchanged() {
        let c = db();
        let mut bad = s("suite:test-a.sh", None);
        bad.bead = "";
        assert!(disable(&c, 10, 600, &bad).is_err());
        bad.bead = "../x";
        assert!(disable(&c, 10, 600, &bad).is_err());
        assert!(disable(&c, 10, 600, &s("nonsense", None)).is_err());
        assert!(
            disable(&c, 10, 600, &s("suite:test-a.sh", Some(10))).is_err(),
            "until == now"
        );
        assert!(
            disable(&c, 10, 600, &s("suite:test-a.sh", Some(5))).is_err(),
            "past until"
        );
        assert!(list(&c).unwrap().is_empty());
        let n = scalar(&c, "SELECT count(*) FROM test_state_history").unwrap();
        assert_eq!(n, 0);
    }

    #[test_id(tt3p2ue)]
    #[test]
    fn an_unreadable_registry_is_an_error_never_an_empty_list() {
        let c = Connection::open_in_memory().unwrap();
        assert!(try_deny_list(&c, 1).is_err());
    }

    #[test_id(tp74g1h)]
    #[test]
    fn init_is_idempotent_and_refuses_a_newer_schema() {
        let c = db();
        disable(&c, 10, 600, &s("suite:test-a.sh", Some(100))).unwrap();
        init(&c).unwrap();
        assert_eq!(list(&c).unwrap().len(), 1);
        exec(&c, "PRAGMA user_version = 3", []).unwrap();
        assert!(init(&c).unwrap_err().contains("newer"));
    }

    #[test_id(tb4b2cp)]
    #[test]
    fn the_schema_is_version_two() {
        let v = scalar(&db(), "PRAGMA user_version").unwrap();
        assert_eq!(v, 2);
    }

    #[test_id(t87dto4)]
    #[test]
    fn clear_landed_deletes_the_rows_naming_the_bead_and_keeps_the_rest() {
        let c = db();
        let named = |test, bead| Set {
            bead,
            ..s(test, Some(9000))
        };
        disable(&c, 100, 600, &named("suite:a.sh", "sp-a")).unwrap();
        quarantine(&c, 100, 600, &named("suite:b.sh::t", "sp-a")).unwrap();
        disable(&c, 100, 600, &named("suite:c.sh", "sp-b")).unwrap();
        let cleared = clear_landed(&c, 200, &["sp-a"], "batcher").unwrap();
        assert_eq!(cleared, ["suite:a.sh", "suite:b.sh::t"]);
        let left: Vec<String> = list(&c).unwrap().into_iter().map(|r| r.test).collect();
        assert_eq!(left, ["suite:c.sh"]);
        for t in &cleared {
            let h = history(&c, t).unwrap();
            let last = h.last().unwrap();
            assert_eq!(
                (last["verb"].as_str(), last["state"].as_str()),
                (Some("cleared-landed"), Some("active"))
            );
            assert_eq!(last["actor"], "batcher");
        }
        assert_eq!(history(&c, "suite:c.sh").unwrap().len(), 1);
    }

    #[test_id(tui6h8d)]
    #[test]
    fn clear_landed_for_a_bead_with_no_rows_changes_nothing() {
        let c = db();
        disable(&c, 100, 600, &s("suite:a.sh", Some(9000))).unwrap();
        assert!(clear_landed(&c, 200, &["sp-zzz"], "batcher")
            .unwrap()
            .is_empty());
        assert_eq!(list(&c).unwrap().len(), 1);
        assert_eq!(history(&c, "suite:a.sh").unwrap().len(), 1);
    }

    #[test_id(tt1l4eb)]
    #[test]
    fn repoint_moves_the_rows_naming_from_so_the_fixs_landing_clears_them() {
        let c = db();
        let named = |test, bead| Set {
            bead,
            ..s(test, Some(9000))
        };
        disable(&c, 100, 600, &named("suite:a.sh", "sp-i")).unwrap();
        quarantine(&c, 100, 600, &named("suite:b.sh::t", "sp-i")).unwrap();
        disable(&c, 100, 600, &named("suite:c.sh", "sp-other")).unwrap();
        assert!(clear_landed(&c, 150, &["sp-f"], "batcher")
            .unwrap()
            .is_empty());
        let moved = repoint(&c, 200, "sp-i", "sp-f", "bead").unwrap();
        assert_eq!(moved, ["suite:a.sh", "suite:b.sh::t"]);
        let beads: Vec<(String, String)> = list(&c)
            .unwrap()
            .into_iter()
            .map(|r| (r.test, r.bead))
            .collect();
        assert_eq!(beads[2], ("suite:c.sh".into(), "sp-other".into()));
        assert_eq!(beads[0].1, "sp-f");
        for t in &moved {
            let h = history(&c, t).unwrap();
            assert_eq!(h.len(), 2);
            assert_eq!(h[1]["verb"], "repointed");
            assert_eq!(h[1]["reason"], "moved from sp-i to its fix sp-f");
        }
        assert_eq!(history(&c, "suite:c.sh").unwrap().len(), 1);
        assert_eq!(
            clear_landed(&c, 300, &["sp-f"], "batcher").unwrap(),
            ["suite:a.sh", "suite:b.sh::t"]
        );
    }

    #[test_id(tz4ar31)]
    #[test]
    fn repoint_from_a_bead_with_no_rows_changes_nothing_and_a_bad_id_is_refused() {
        let c = db();
        disable(&c, 100, 600, &s("suite:a.sh", Some(9000))).unwrap();
        assert!(repoint(&c, 200, "sp-zzz", "sp-f", "bead")
            .unwrap()
            .is_empty());
        assert_eq!(history(&c, "suite:a.sh").unwrap().len(), 1);
        assert!(repoint(&c, 200, "bad id", "sp-f", "bead").is_err());
        assert!(repoint(&c, 200, "sp-i", "", "bead").is_err());
        assert!(repoint(&c, 200, "sp-i", "sp-f", " ").is_err());
    }
}
