//! `admin-drop-lifecycle --database NAME --archive FILE [--dry-run]`: the one-time end of the
//! dolt lifecycle database, for the Concierge to run once on production.
//!
//! The database's facts and the runtime's journal rows have no other home, so they are carried
//! into the control store first, each only where the store does not already hold it, and the
//! drop follows only when the store then holds every row the database does. The administrator's
//! credential is read here, from the environment or the file install provisioned, and never
//! arrives on argv.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::control::facts::Fact;
use crate::control::journal::{Carried, EffectRow, PoisonRow, ScheduleRow, StateRow};
use crate::db::{Conn, DbError};
use crate::{flag, CANNOT_TELL, REFUSED};

/// The administrator: `SPIRA_LC_ADMIN_USER` with the password in `SPIRA_LC_ADMIN_PASSWORD_FILE` (or
/// `SPIRA_LC_ADMIN_PASSWORD`), else the user install provisioned.
pub(crate) fn administrator(get: impl Fn(&str) -> Option<String>, provisioned: impl Fn() -> Result<Option<(String, String)>, String>) -> Result<(String, String), String> {
    if let Some(user) = get("SPIRA_LC_ADMIN_USER").filter(|u| !u.is_empty()) {
        let password = crate::db::password_from(get("SPIRA_LC_ADMIN_PASSWORD_FILE"), get("SPIRA_LC_ADMIN_PASSWORD"), |p| std::fs::read_to_string(p))?;
        return Ok((user, password));
    }
    provisioned()?.ok_or_else(|| "no administrator credential: set SPIRA_LC_ADMIN_USER and SPIRA_LC_ADMIN_PASSWORD_FILE".to_string())
}

pub(crate) fn provisioned() -> Result<Option<(String, String)>, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let Some(path) = spira_config::resolve::lc_admin_password_file(&env) else { return Ok(None) };
    let password = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
    Ok(Some(("root".to_string(), password.trim().to_string())))
}

/// [`administrator`] from this process's environment, else root with the password in the admin
/// credential file, or none where there is no file: what a verb on the local beads server logs in as.
pub(crate) fn local_administrator() -> Result<(String, String), String> {
    administrator(|k| std::env::var(k).ok(), || Ok(Some(provisioned()?.unwrap_or_else(|| ("root".to_string(), String::new())))))
}

/// A database name safe to put between backticks.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn text(r: &Value, k: &str) -> String {
    match r.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    }
}

fn num(r: &Value, k: &str) -> u64 {
    text(r, k).parse().unwrap_or(0)
}

fn flagged(r: &Value, k: &str) -> bool {
    num(r, k) != 0
}

/// The server as the drop sees it.
pub(crate) trait Dolt {
    fn rows(&self, sql: &str) -> Result<Vec<Value>, String>;
    fn execute(&self, sql: &str) -> Result<(), String>;
}

impl Dolt for Conn {
    fn rows(&self, sql: &str) -> Result<Vec<Value>, String> {
        self.query(sql).map_err(|DbError::CannotTell(m)| m)
    }

    fn execute(&self, sql: &str) -> Result<(), String> {
        Conn::execute(self, sql).map_err(|DbError::CannotTell(m)| m)
    }
}

/// Everything the database holds that the control store does not keep elsewhere.
struct Held {
    facts: Vec<Fact>,
    journal: Carried,
}

fn read_held(admin: &dyn Dolt, db: &str) -> Result<Held, String> {
    let q = |sql: String| admin.rows(&sql);
    let facts = q(format!("SELECT lc_key, event, JSON_UNQUOTE(JSON_EXTRACT(evidence, '$.cause')) AS cause, actor, `at` FROM `{db}`.`event` WHERE machine = 'fact' ORDER BY seq"))?
        .iter()
        .map(|r| Fact { bead: text(r, "lc_key"), kind: text(r, "event"), cause: text(r, "cause"), actor: text(r, "actor"), at: num(r, "at") as i64 })
        .collect();
    let state = q(format!("SELECT task, skey, value, `at` FROM `{db}`.runtime_task_state"))?
        .iter()
        .map(|r| StateRow { task: text(r, "task"), skey: text(r, "skey"), value: text(r, "value"), at: num(r, "at") })
        .collect();
    let effects = q(format!("SELECT task, ekey, request_id, stream, spec, requested_at, deadline_at, resolved FROM `{db}`.runtime_effect"))?
        .iter()
        .map(|r| {
            let spec = serde_json::from_str(&text(r, "spec")).map_err(|e| format!("effect {}/{} has an unreadable spec: {e}", text(r, "task"), text(r, "ekey")))?;
            Ok((EffectRow { task: text(r, "task"), ekey: text(r, "ekey"), request_id: text(r, "request_id"), stream: text(r, "stream"), spec, requested_at: num(r, "requested_at"), deadline_at: num(r, "deadline_at") }, flagged(r, "resolved")))
        })
        .collect::<Result<_, String>>()?;
    let schedules = q(format!("SELECT row_id, `at`, stream, kind_tag, kind_body, cause, correlation, fired FROM `{db}`.runtime_schedule"))?
        .iter()
        .map(|r| (ScheduleRow { id: text(r, "row_id"), at: num(r, "at"), stream: text(r, "stream"), tag: text(r, "kind_tag"), body: text(r, "kind_body"), cause: r.get("cause").and_then(Value::as_str).map(str::to_string), correlation: text(r, "correlation") }, flagged(r, "fired")))
        .collect();
    let poison = q(format!("SELECT stream, event_id, handler, message FROM `{db}`.runtime_poison"))?
        .iter()
        .map(|r| PoisonRow { stream: text(r, "stream"), event: text(r, "event_id"), handler: text(r, "handler"), message: text(r, "message") })
        .collect();
    Ok(Held { facts, journal: Carried { state, effects, schedules, poison } })
}

fn carry<B: state_store::Backend>(store: &crate::control::store::Store<B>, held: &Held) -> Result<(usize, usize), String> {
    let e = |e: state_store::Error| e.to_string();
    let facts = store.carry_facts(crate::control::facts::CARRIED, &held.facts, "admin-drop-lifecycle").map_err(e)?;
    let rows = store.journal_carry(&held.journal, "admin-drop-lifecycle").map_err(e)?;
    Ok((facts, rows))
}

/// What the control store lacks of `held` after the carry: the drop proceeds only on none.
fn missing<B: state_store::Backend>(store: &crate::control::store::Store<B>, held: &Held) -> Result<Vec<String>, String> {
    let e = |e: state_store::Error| e.to_string();
    let mut gaps = Vec::new();
    let mut have: BTreeMap<(String, String), usize> = BTreeMap::new();
    for f in store.facts(&[]).map_err(e)? {
        *have.entry((f.bead, f.kind)).or_default() += 1;
    }
    let mut want: BTreeMap<(String, String), usize> = BTreeMap::new();
    for f in &held.facts {
        *want.entry((f.bead.clone(), f.kind.clone())).or_default() += 1;
    }
    for (k, n) in want {
        if have.get(&k).copied().unwrap_or(0) < n {
            gaps.push(format!("facts {} of {}: the store holds fewer than the database's {n}", k.1, k.0));
        }
    }
    let state: BTreeMap<(String, String), String> = store.journal_state().map_err(e)?.into_iter().map(|r| ((r.task, r.skey), r.value)).collect();
    for r in &held.journal.state {
        if !state.contains_key(&(r.task.clone(), r.skey.clone())) {
            gaps.push(format!("task state {}/{}", r.task, r.skey));
        }
    }
    let open: std::collections::BTreeSet<String> = store.journal_open_effects().map_err(e)?.into_iter().map(|r| r.request_id).collect();
    for (r, resolved) in &held.journal.effects {
        if !resolved && !open.contains(&r.request_id) {
            gaps.push(format!("open effect {}/{}", r.task, r.ekey));
        }
    }
    let due: std::collections::BTreeSet<String> = store.journal_due(None).map_err(e)?.into_iter().map(|r| r.id).collect();
    for (r, fired) in &held.journal.schedules {
        if !fired && !due.contains(&r.id) {
            gaps.push(format!("schedule row {}", r.id));
        }
    }
    for p in &held.journal.poison {
        if store.journal_poisoned(&p.stream).map_err(e)?.is_none() {
            gaps.push(format!("poison of stream {}", p.stream));
        }
    }
    Ok(gaps)
}

pub fn run(args: &[String]) -> (i32, String) {
    let cannot = |why: String| (CANNOT_TELL, format!("admin-drop-lifecycle: {why}"));
    let (Some(db), Some(archive)) = (flag(args, "--database"), flag(args, "--archive")) else {
        return (CANNOT_TELL, "admin-drop-lifecycle: usage admin-drop-lifecycle --database NAME --archive FILE [--dry-run]".into());
    };
    if !plain_name(&db) {
        return (REFUSED, format!("admin-drop-lifecycle: {db:?} is not a plain database name"));
    }
    match std::fs::metadata(&archive) {
        Ok(m) if m.is_file() && m.len() > 0 => {}
        Ok(_) => return (REFUSED, format!("admin-drop-lifecycle: {archive} is not a non-empty file: the database is dropped only once archived")),
        Err(e) => return (REFUSED, format!("admin-drop-lifecycle: the archive {archive}: {e}: the database is dropped only once archived")),
    }
    let dry = args.iter().any(|a| a == "--dry-run");
    let get = |k: &str| std::env::var(k).ok();
    let (user, password) = match administrator(get, provisioned) {
        Ok(c) => c,
        Err(e) => return cannot(e),
    };
    let setting = |k: &str| spira_config::process::cfg(k);
    let (host, port) = match (setting("SPIRA_BEADS_SQL_HOST"), setting("SPIRA_BEADS_SQL_PORT")) {
        (Ok(h), Ok(p)) => match p.trim().parse::<u16>() {
            Ok(p) => (h, p),
            Err(e) => return cannot(format!("SPIRA_BEADS_SQL_PORT {p:?}: {e}")),
        },
        (Err(e), _) | (_, Err(e)) => return cannot(e),
    };
    let store = match crate::control::served::store() {
        Ok(s) => s,
        Err(e) => return cannot(e),
    };
    drop_on(&Conn::administrator(&host, port, &user, password), &db, &store, dry)
}

/// The carry and the drop of `db` on `admin`, into `store`.
fn drop_on<B: state_store::Backend>(admin: &dyn Dolt, db: &str, store: &crate::control::store::Store<B>, dry: bool) -> (i32, String) {
    let cannot = |why: String| (CANNOT_TELL, format!("admin-drop-lifecycle: {why}"));
    let present = match admin.rows(&format!("SELECT SCHEMA_NAME AS name FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = '{db}'")) {
        Ok(rows) => !rows.is_empty(),
        Err(e) => return cannot(e),
    };
    if !present {
        return (0, format!("admin-drop-lifecycle: {db} is not on the server; nothing to drop"));
    }
    let held = match read_held(admin, db) {
        Ok(h) => h,
        Err(e) => return cannot(e),
    };
    let summary = format!(
        "{} facts, {} task state, {} effects, {} schedule rows, {} poisons",
        held.facts.len(),
        held.journal.state.len(),
        held.journal.effects.len(),
        held.journal.schedules.len(),
        held.journal.poison.len()
    );
    if dry {
        return (0, format!("admin-drop-lifecycle: would carry {summary} into the control store, then drop {db}"));
    }
    let (facts, rows) = match carry(store, &held) {
        Ok(c) => c,
        Err(e) => return cannot(e),
    };
    match missing(store, &held) {
        Ok(gaps) if gaps.is_empty() => {}
        Ok(gaps) => return (REFUSED, format!("admin-drop-lifecycle: {db} is kept: the control store lacks {}", gaps.join("; "))),
        Err(e) => return cannot(e),
    }
    if let Err(e) = admin.execute(&format!("DROP DATABASE `{db}`")) {
        return cannot(format!("dropping {db}: {e}"));
    }
    if let Err(e) = admin.execute("CALL dolt_purge_dropped_databases()") {
        return cannot(format!("{db} is dropped but its files are not purged: {e}"));
    }
    (0, format!("admin-drop-lifecycle: carried {facts} facts and {rows} journal rows ({summary} held); dropped {db} and purged the dropped databases\nadmin-drop-lifecycle: spira.lc_backup_db must no longer name {db}: set it to the databases still kept"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports};

    fn store() -> crate::control::store::Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    fn held() -> Held {
        let fact = |bead: &str, kind: &str, at: i64| Fact { bead: bead.into(), kind: kind.into(), cause: "c".into(), actor: "a".into(), at };
        Held {
            facts: vec![fact("sp-a", "claimed", 1), fact("sp-a", "requeued", 2), fact("sp-b", "claimed", 3)],
            journal: Carried {
                state: vec![StateRow { task: "t".into(), skey: "k".into(), value: "v".into(), at: 1 }],
                effects: vec![
                    (EffectRow { task: "t".into(), ekey: "open".into(), request_id: "r1".into(), stream: "s".into(), spec: serde_json::json!({}), requested_at: 1, deadline_at: 2 }, false),
                    (EffectRow { task: "t".into(), ekey: "done".into(), request_id: "r2".into(), stream: "s".into(), spec: serde_json::json!({}), requested_at: 1, deadline_at: 2 }, true),
                ],
                schedules: vec![(ScheduleRow { id: "s1".into(), at: 5, stream: "s".into(), tag: "tick".into(), body: String::new(), cause: None, correlation: "c".into() }, false)],
                poison: vec![PoisonRow { stream: "st".into(), event: "e".into(), handler: "h".into(), message: "m".into() }],
            },
        }
    }

    #[test_id(tdtmedr)]
    #[test]
    fn a_carry_leaves_the_store_holding_everything_the_database_held_and_a_second_adds_nothing() {
        let s = store();
        let h = held();
        assert_eq!(missing(&s, &h).unwrap().len(), 7, "before the carry the store lacks it all");
        assert_eq!(carry(&s, &h).unwrap(), (3, 4), "the resolved effect is not written");
        assert_eq!(missing(&s, &h).unwrap(), Vec::<String>::new());
        assert_eq!(carry(&s, &h).unwrap(), (0, 0), "a second carry writes nothing");
        assert_eq!(s.facts(&[]).unwrap().len(), 3);
        assert_eq!(s.journal_open_effects().unwrap().len(), 1, "the resolved effect is not open");
    }

    #[test_id(t61ybim)]
    #[test]
    fn a_carry_never_overwrites_what_the_runtime_wrote_since() {
        let s = store();
        s.journal_turn(&crate::control::journal::Turn { state: vec![StateRow { task: "t".into(), skey: "k".into(), value: "newer".into(), at: 9 }], ..Default::default() }, "runtime").unwrap();
        carry(&s, &held()).unwrap();
        assert_eq!(s.journal_state().unwrap()[0].value, "newer");
    }

    #[test_id(tn8ybzz)]
    #[test]
    fn a_turn_that_clears_a_state_key_deletes_its_row() {
        let s = store();
        let row = |k: &str| StateRow { task: "t".into(), skey: k.into(), value: "v".into(), at: 1 };
        s.journal_turn(&crate::control::journal::Turn { state: vec![row("a"), row("b")], ..Default::default() }, "runtime").unwrap();
        s.journal_turn(&crate::control::journal::Turn { cleared: vec![("t".into(), "a".into())], ..Default::default() }, "runtime").unwrap();
        let left: Vec<String> = s.journal_state().unwrap().into_iter().map(|r| r.skey).collect();
        assert_eq!(left, vec!["b".to_string()]);
    }

    /// A server that answers each read from a script keyed by the table it names, and records every statement.
    struct Scripted {
        present: bool,
        tables: Vec<(&'static str, Vec<Value>)>,
        seen: std::cell::RefCell<Vec<String>>,
        drop_fails: bool,
    }

    impl Scripted {
        fn new() -> Scripted {
            let row = |pairs: &[(&str, &str)]| Value::Object(pairs.iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect());
            Scripted {
                present: true,
                tables: vec![
                    ("`event`", vec![row(&[("lc_key", "sp-a"), ("event", "claimed"), ("cause", "x"), ("actor", "a"), ("at", "10")]), row(&[("lc_key", "sp-a"), ("event", "requeued"), ("cause", "y"), ("actor", "a"), ("at", "20")])]),
                    ("runtime_task_state", vec![row(&[("task", "t"), ("skey", "k"), ("value", "v"), ("at", "1")])]),
                    ("runtime_effect", vec![row(&[("task", "t"), ("ekey", "e"), ("request_id", "r1"), ("stream", "s"), ("spec", "{\"n\":1}"), ("requested_at", "1"), ("deadline_at", "2"), ("resolved", "0")])]),
                    ("runtime_schedule", vec![row(&[("row_id", "s1"), ("at", "5"), ("stream", "s"), ("kind_tag", "tick"), ("kind_body", ""), ("correlation", "c"), ("fired", "0")])]),
                    ("runtime_poison", vec![row(&[("stream", "st"), ("event_id", "e"), ("handler", "h"), ("message", "m")])]),
                ],
                seen: Default::default(),
                drop_fails: false,
            }
        }
    }

    impl Dolt for Scripted {
        fn rows(&self, sql: &str) -> Result<Vec<Value>, String> {
            self.seen.borrow_mut().push(sql.to_string());
            if sql.contains("information_schema.SCHEMATA") {
                return Ok(if self.present { vec![serde_json::json!({"name": "lifedb"})] } else { vec![] });
            }
            self.tables.iter().find(|(t, _)| sql.contains(&format!("`lifedb`.{t}")) || sql.contains(&format!("`lifedb`.`{}`", t.trim_matches('`')))).map(|(_, r)| r.clone()).ok_or_else(|| format!("no such table in: {sql}"))
        }

        fn execute(&self, sql: &str) -> Result<(), String> {
            self.seen.borrow_mut().push(sql.to_string());
            if self.drop_fails { Err("refused".into()) } else { Ok(()) }
        }
    }

    #[test_id(tesvnnt)]
    #[test]
    fn the_database_is_read_whole_carried_and_dropped_in_that_order() {
        let (s, server) = (store(), Scripted::new());
        let (rc, out) = drop_on(&server, "lifedb", &s, false);
        assert_eq!(rc, 0, "{out}");
        assert!(out.contains("2 facts") && out.contains("1 task state") && out.contains("dropped lifedb") && out.contains("spira.lc_backup_db"), "{out}");
        let seen = server.seen.borrow();
        let drop_at = seen.iter().position(|q| q == "DROP DATABASE `lifedb`").expect("the drop ran");
        assert_eq!(seen.get(drop_at + 1).map(String::as_str), Some("CALL dolt_purge_dropped_databases()"), "the purge follows the drop");
        assert!(out.contains("purged"), "{out}");
        drop(seen);
        assert_eq!(server.seen.borrow().iter().filter(|q| q.starts_with("DROP")).count(), 1);
        assert_eq!(s.facts(&[]).unwrap().len(), 2);
        assert_eq!(s.journal_open_effects().unwrap().len(), 1);
    }

    #[test_id(thid990)]
    #[test]
    fn a_dry_run_reads_and_reports_and_changes_nothing() {
        let (s, server) = (store(), Scripted::new());
        let (rc, out) = drop_on(&server, "lifedb", &s, true);
        assert_eq!(rc, 0, "{out}");
        assert!(out.contains("would carry 2 facts"), "{out}");
        assert!(s.facts(&[]).unwrap().is_empty() && !server.seen.borrow().iter().any(|q| q.starts_with("DROP")));
    }

    #[test_id(tbr6hfp)]
    #[test]
    fn a_database_already_gone_is_nothing_to_do_and_a_server_that_fails_the_drop_says_so() {
        let s = store();
        let mut gone = Scripted::new();
        gone.present = false;
        let (rc, out) = drop_on(&gone, "lifedb", &s, false);
        assert_eq!((rc, out.contains("nothing to drop")), (0, true), "{out}");
        let mut stuck = Scripted::new();
        stuck.drop_fails = true;
        let (rc, out) = drop_on(&stuck, "lifedb", &s, false);
        assert_eq!(rc, CANNOT_TELL, "{out}");
        assert!(!s.facts(&[]).unwrap().is_empty(), "the carry stands though the drop was refused, and a second run carries nothing twice");
        assert_eq!(drop_on(&Scripted::new(), "lifedb", &s, false).0, 0);
        assert_eq!(s.facts(&[]).unwrap().len(), 2);
    }

    #[test_id(t68g2wo)]
    #[test]
    fn an_unreadable_table_keeps_the_database() {
        let (s, mut server) = (store(), Scripted::new());
        server.tables.retain(|(t, _)| *t != "runtime_poison");
        let (rc, out) = drop_on(&server, "lifedb", &s, false);
        assert_eq!(rc, CANNOT_TELL, "{out}");
        assert!(!server.seen.borrow().iter().any(|q| q.starts_with("DROP")), "{:?}", server.seen.borrow());
        assert!(s.facts(&[]).unwrap().is_empty(), "nothing is carried from a database that could not be read whole");
    }

    #[test_id(t2n51x0)]
    #[test]
    fn the_carried_facts_sort_before_newer_ones_of_the_same_bead() {
        let s = store();
        s.append_facts(&[Fact { bead: "sp-a".into(), kind: "reclaimed".into(), cause: String::new(), actor: "a".into(), at: 100 }], &state_store::Request::new("r", "t", "t")).unwrap();
        carry(&s, &held()).unwrap();
        let kinds: Vec<String> = s.facts(&["sp-a".into()]).unwrap().into_iter().map(|f| f.kind).collect();
        assert_eq!(kinds, ["claimed", "requeued", "reclaimed"]);
    }

    #[test_id(t0281xq)]
    #[test]
    fn a_missing_row_keeps_the_database() {
        let s = store();
        let h = held();
        carry(&s, &h).unwrap();
        let mut more = held();
        more.journal.poison.push(PoisonRow { stream: "other".into(), event: "e".into(), handler: "h".into(), message: "m".into() });
        assert_eq!(missing(&s, &more).unwrap(), ["poison of stream other"]);
    }

    #[test_id(tw0c2s4)]
    #[test]
    fn the_administrator_is_the_environments_or_what_install_provisioned_and_never_a_guess() {
        let env = |pairs: &'static [(&str, &str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(administrator(env(&[("SPIRA_LC_ADMIN_USER", "root"), ("SPIRA_LC_ADMIN_PASSWORD", "pw")]), || Ok(None)).unwrap(), ("root".into(), "pw".into()));
        assert_eq!(administrator(env(&[]), || Ok(Some(("root".into(), "from-install".into())))).unwrap().1, "from-install");
        assert!(administrator(env(&[("SPIRA_LC_ADMIN_USER", "")]), || Ok(None)).unwrap_err().contains("SPIRA_LC_ADMIN_USER"));
    }

    #[test_id(tuozoem)]
    #[test]
    fn only_a_plain_database_name_is_dropped_and_an_unarchived_one_is_refused_first() {
        assert!(plain_name("db_1") && !plain_name("") && !plain_name("a`; DROP DATABASE spira; --") && !plain_name("a b"));
        let dir = testkit::TempDir::new("spira-lc-drop-archive");
        let missing = dir.join("archive.jsonl").display().to_string();
        let (rc, out) = run(&["--database".into(), "x`y".into(), "--archive".into(), missing.clone()]);
        assert_eq!(rc, REFUSED, "{out}");
        let (rc, out) = run(&["--database".into(), "sptest_x".into(), "--archive".into(), missing]);
        assert_eq!(rc, REFUSED, "{out}");
        assert!(out.contains("archived"), "{out}");
        assert_eq!(run(&[]).0, CANNOT_TELL);
    }
}
