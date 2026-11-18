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
use crate::db::Conn;
use crate::{flag, CANNOT_TELL, REFUSED};

const CARRY: &str = "carried-from-lifecycle";

/// The administrator: `SPIRA_LC_ADMIN_USER` with the password in `SPIRA_LC_ADMIN_PASSWORD_FILE` (or
/// `SPIRA_LC_ADMIN_PASSWORD`), else the user install provisioned.
fn administrator(get: impl Fn(&str) -> Option<String>, provisioned: impl Fn() -> Result<Option<(String, String)>, String>) -> Result<(String, String), String> {
    if let Some(user) = get("SPIRA_LC_ADMIN_USER").filter(|u| !u.is_empty()) {
        let password = crate::db::password_from(get("SPIRA_LC_ADMIN_PASSWORD_FILE"), get("SPIRA_LC_ADMIN_PASSWORD"), |p| std::fs::read_to_string(p))?;
        return Ok((user, password));
    }
    provisioned()?.ok_or_else(|| "no administrator credential: set SPIRA_LC_ADMIN_USER and SPIRA_LC_ADMIN_PASSWORD_FILE".to_string())
}

fn provisioned() -> Result<Option<(String, String)>, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let Some(path) = spira_config::resolve::lc_admin_password_file(&env) else { return Ok(None) };
    let password = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
    Ok(Some(("root".to_string(), password.trim().to_string())))
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

/// Everything the database holds that the control store does not keep elsewhere.
struct Held {
    facts: Vec<Fact>,
    journal: Carried,
}

fn read_held(admin: &Conn, db: &str) -> Result<Held, String> {
    let q = |sql: String| admin.query(&sql).map_err(|e| format!("{e:?}"));
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
    let facts = store.carry_facts(CARRY, &held.facts, "admin-drop-lifecycle").map_err(e)?;
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
    let admin = Conn::administrator(&host, port, &user, password);
    let present = match admin.query(&format!("SELECT SCHEMA_NAME AS name FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = '{db}'")) {
        Ok(rows) => !rows.is_empty(),
        Err(e) => return cannot(format!("{e:?}")),
    };
    if !present {
        return (0, format!("admin-drop-lifecycle: {db} is not on the server; nothing to drop"));
    }
    let held = match read_held(&admin, &db) {
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
    let (facts, rows) = match carry(&store, &held) {
        Ok(c) => c,
        Err(e) => return cannot(e),
    };
    match missing(&store, &held) {
        Ok(gaps) if gaps.is_empty() => {}
        Ok(gaps) => return (REFUSED, format!("admin-drop-lifecycle: {db} is kept: the control store lacks {}", gaps.join("; "))),
        Err(e) => return cannot(e),
    }
    match admin.execute(&format!("DROP DATABASE `{db}`")) {
        Ok(()) => (0, format!("admin-drop-lifecycle: carried {facts} facts and {rows} journal rows ({summary} held); dropped {db}\nadmin-drop-lifecycle: spira.lc_backup_db must no longer name {db}: set it to the databases still kept")),
        Err(e) => cannot(format!("dropping {db}: {e:?}")),
    }
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

    #[test]
    fn a_carry_leaves_the_store_holding_everything_the_database_held_and_a_second_adds_nothing() {
        let s = store();
        let h = held();
        assert_eq!(missing(&s, &h).unwrap().len(), 7, "before the carry the store lacks it all");
        assert_eq!(carry(&s, &h).unwrap(), (3, 5));
        assert_eq!(missing(&s, &h).unwrap(), Vec::<String>::new());
        assert_eq!(carry(&s, &h).unwrap(), (0, 0), "a second carry writes nothing");
        assert_eq!(s.facts(&[]).unwrap().len(), 3);
        assert_eq!(s.journal_open_effects().unwrap().len(), 1, "the resolved effect carried over resolved");
    }

    #[test]
    fn a_carry_never_overwrites_what_the_runtime_wrote_since() {
        let s = store();
        s.journal_turn(&crate::control::journal::Turn { state: vec![StateRow { task: "t".into(), skey: "k".into(), value: "newer".into(), at: 9 }], ..Default::default() }, "runtime").unwrap();
        carry(&s, &held()).unwrap();
        assert_eq!(s.journal_state().unwrap()[0].value, "newer");
    }

    #[test]
    fn the_carried_facts_sort_before_newer_ones_of_the_same_bead() {
        let s = store();
        s.append_facts(&[Fact { bead: "sp-a".into(), kind: "reclaimed".into(), cause: String::new(), actor: "a".into(), at: 100 }], &state_store::Request::new("r", "t", "t")).unwrap();
        carry(&s, &held()).unwrap();
        let kinds: Vec<String> = s.facts(&["sp-a".into()]).unwrap().into_iter().map(|f| f.kind).collect();
        assert_eq!(kinds, ["claimed", "requeued", "reclaimed"]);
    }

    #[test]
    fn a_missing_row_keeps_the_database() {
        let s = store();
        let h = held();
        carry(&s, &h).unwrap();
        let mut more = held();
        more.journal.poison.push(PoisonRow { stream: "other".into(), event: "e".into(), handler: "h".into(), message: "m".into() });
        assert_eq!(missing(&s, &more).unwrap(), ["poison of stream other"]);
    }

    #[test]
    fn the_administrator_is_the_environments_or_what_install_provisioned_and_never_a_guess() {
        let env = |pairs: &'static [(&str, &str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(administrator(env(&[("SPIRA_LC_ADMIN_USER", "root"), ("SPIRA_LC_ADMIN_PASSWORD", "pw")]), || Ok(None)).unwrap(), ("root".into(), "pw".into()));
        assert_eq!(administrator(env(&[]), || Ok(Some(("root".into(), "from-install".into())))).unwrap().1, "from-install");
        assert!(administrator(env(&[("SPIRA_LC_ADMIN_USER", "")]), || Ok(None)).unwrap_err().contains("SPIRA_LC_ADMIN_USER"));
    }

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
