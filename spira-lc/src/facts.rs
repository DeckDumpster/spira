//! The attempt and poison history: facts that change no bead's state but that the attempt
//! count and the poison decision fold, kept on the control store (`control::facts`).

use serde_json::Value;

use crate::control::facts::Fact;
use crate::db;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

/// Closed: a kind outside this list is refused, so a typo cannot start a new history.
pub const KINDS: &[&str] = &["claimed", "requeued", "reopen", "reclaimed", "poison.cleared", "recurred", "lapsed", "session", "checkpointed", "fast-tier-red", "eject-failure", "send-back", "send-back-logs", "blind-rework", "rejection", "escalated", "no-progress", "gate-start", "build-dir", "suite-dropped", "__doctor_probe__"];

pub const CAUSE_MAX: usize = 200;
pub const READ_MAX: usize = 120;
/// Kinds whose cause is a multi-line digest rather than a one-line token.
pub const LONG_KINDS: &[&str] = &["fast-tier-red", "eject-failure", "send-back", "rejection"];
pub const LONG_MAX: usize = 6000;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub(crate) fn bounded(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(CAUSE_MAX).collect()
}

fn bounded_tabbed(s: &str) -> String {
    s.chars().filter(|c| *c == '\t' || !c.is_control()).take(CAUSE_MAX).collect()
}

fn bounded_long(s: &str) -> String {
    s.chars().filter(|c| *c == '\n' || !c.is_control()).take(LONG_MAX).collect()
}

pub fn fact_of(key: &str, kind: &str, cause: &str, actor: &str, at: i64) -> Fact {
    Fact { bead: key.to_string(), kind: kind.to_string(), cause: cause.to_string(), actor: actor.to_string(), at }
}

fn store() -> Result<std::sync::Arc<crate::control::store::Store<crate::control::served::Served>>, String> {
    crate::control::served::store()
}

/// Appends `facts` in one txn.
pub fn append(facts: &[Fact]) -> Result<(), String> {
    if crate::plane::remote() {
        return facts.iter().try_for_each(|f| match crate::plane::call(&["fact".into(), "add".into(), f.bead.clone(), "--kind".into(), f.kind.clone(), "--actor".into(), f.actor.clone(), "--cause".into(), f.cause.clone()]) {
            (0, _) => Ok(()),
            (_, out) => Err(out),
        });
    }
    append_on(&*store()?, facts)
}

pub(crate) fn append_on<B: state_store::Backend>(store: &crate::control::store::Store<B>, facts: &[Fact]) -> Result<(), String> {
    let req = state_store::Request::new(&crate::control::served::request_id(), "spira-lc", "fact");
    store.append_facts(facts, &req).map_err(|e| e.to_string())
}

const USAGE: &str = "usage: spira-lc fact add <bead-id> --kind K --actor A [--cause C] | list [--ids a,b] [--kinds x,y] [--since EPOCH]";

/// `fact add …` appends a fact; `fact list …` reads them.
pub fn cmd(args: &[String]) -> (i32, String) {
    cmd_with(args, &append, &read)
}

type Put<'a> = &'a dyn Fn(&[Fact]) -> Result<(), String>;
type Get<'a> = &'a dyn Fn(&[String], &[String], Option<i64>) -> Result<Vec<Value>, String>;

fn cmd_with(args: &[String], put: Put, get: Get) -> (i32, String) {
    match args.first().map(String::as_str) {
        Some("add") => cmd_fact(&args[1..], put),
        Some("list") => cmd_facts(&args[1..], get),
        _ => (CANNOT_TELL, USAGE.into()),
    }
}

/// `fact add <bead-id> --kind K --actor A [--cause C]`.
fn cmd_fact(args: &[String], put: Put) -> (i32, String) {
    let Some(key) = args.first().filter(|a| !a.starts_with("--")) else {
        return (CANNOT_TELL, "fact add: missing <bead-id>".into());
    };
    let (Some(kind), Some(actor)) = (flag(args, "--kind"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "fact add: --kind and --actor are required".into());
    };
    if !KINDS.contains(&kind.as_str()) {
        return (REFUSED, format!("fact add: unknown kind {kind:?} (want one of {})", KINDS.join(", ")));
    }
    if key.is_empty() || actor.is_empty() {
        return (CANNOT_TELL, "fact add: empty <bead-id> or --actor".into());
    }
    let raw = flag(args, "--cause").unwrap_or_default();
    let cause = match kind.as_str() {
        k if LONG_KINDS.contains(&k) => bounded_long(&raw),
        "build-dir" => bounded_tabbed(&raw),
        _ => bounded(&raw),
    };
    match put(&[fact_of(key, &kind, &cause, &bounded(&actor), db::now_epoch())]) {
        Ok(()) => (0, String::new()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

/// The fact of a submit's `build-dir`: the tip submitted and the directory it was built in.
pub fn build_dir(bead: &str, tip: &str, dir: &str, actor: &str, at: i64) -> Fact {
    fact_of(bead, "build-dir", &format!("{tip}\t{dir}"), &bounded(actor), at)
}

/// The facts of `ids` of `kinds` after `since`, each bead's oldest first, as rows of
/// `issue_id`, `event_type`, `new_value` (clipped as a read clips), `actor` and `at`.
pub fn read(ids: &[String], kinds: &[String], since: Option<i64>) -> Result<Vec<Value>, String> {
    if crate::plane::remote() {
        let mut argv = vec!["fact".to_string(), "list".into(), "--raw".into(), "--ids".into(), ids.join(","), "--kinds".into(), kinds.join(",")];
        argv.extend(since.into_iter().flat_map(|t| ["--since".to_string(), t.to_string()]));
        return match crate::plane::call(&argv) {
            (0, out) => serde_json::from_str(&out).map_err(|e| format!("fact list: {e}")),
            (_, out) => Err(out),
        };
    }
    read_on(&*store()?, ids, kinds, since)
}

pub(crate) fn read_on<B: state_store::Backend>(store: &crate::control::store::Store<B>, ids: &[String], kinds: &[String], since: Option<i64>) -> Result<Vec<Value>, String> {
    let facts = if ids.is_empty() && !kinds.is_empty() { store.facts_of_kinds(kinds, since) } else { store.facts(ids) }.map_err(|e| e.to_string())?;
    Ok(facts
        .into_iter()
        .filter(|f| kinds.is_empty() || kinds.contains(&f.kind))
        .filter(|f| since.is_none_or(|t| f.at > t))
        .map(|f| {
            let max = if LONG_KINDS.contains(&f.kind.as_str()) { LONG_MAX } else { READ_MAX };
            serde_json::json!({ "issue_id": f.bead, "event_type": f.kind, "new_value": f.cause.chars().take(max).collect::<String>(), "actor": f.actor, "at": f.at.to_string() })
        })
        .collect())
}

fn csv(args: &[String], name: &str) -> Vec<String> {
    flag(args, name).map(|v| v.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

/// Each row as bd's `events` rows read: `issue_id`, `event_type`, `new_value`, `actor`, `created_at`.
pub fn shape(rows: Vec<Value>) -> Vec<Value> {
    rows.into_iter()
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let at = s("at").parse::<i64>().unwrap_or(0);
            serde_json::json!({
                "issue_id": s("issue_id"),
                "event_type": s("event_type"),
                "new_value": s("new_value"),
                "actor": s("actor"),
                "created_at": crate::callers::fmt_utc(at),
            })
        })
        .collect()
}

/// `fact list [--ids a,b] [--kinds x,y] [--since EPOCH]`: the facts, oldest first per bead.
fn cmd_facts(args: &[String], get: Get) -> (i32, String) {
    let since = match flag(args, "--since").map(|v| v.parse::<i64>()) {
        Some(Err(_)) => return (CANNOT_TELL, "fact list: --since must be an epoch second".into()),
        Some(Ok(t)) => Some(t),
        None => None,
    };
    match get(&csv(args, "--ids"), &csv(args, "--kinds"), since) {
        Ok(r) if args.iter().any(|a| a == "--raw") => (0, serde_json::to_string(&Value::Array(r)).unwrap()),
        Ok(r) => (0, serde_json::to_string_pretty(&Value::Array(shape(r))).unwrap()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports};

    fn store() -> crate::control::store::Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(1_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    fn put(s: &crate::control::store::Store<MemBackend>, bead: &str, kind: &str, cause: &str, at: i64) {
        append_on(s, &[fact_of(bead, kind, cause, "harness", at)]).unwrap();
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(tu4o8uy)]
    #[test]
    fn fact_add_then_fact_list_round_trips_one_row() {
        let s = store();
        let (put, get): (Put, Get) = (&|f| append_on(&s, f), &|i, k, t| read_on(&s, i, k, t));
        assert_eq!(cmd_with(&argv(&["add", "sp-a", "--kind", "claimed", "--actor", "dwarf-1", "--cause", "first"]), put, get), (0, String::new()));
        let (rc, out) = cmd_with(&argv(&["list", "--ids", "sp-a"]), put, get);
        assert_eq!(rc, 0, "{out}");
        let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0]["event_type"].as_str(), rows[0]["new_value"].as_str(), rows[0]["actor"].as_str()), (Some("claimed"), Some("first"), Some("dwarf-1")));
    }

    #[test_id(tuzviow)]
    #[test]
    fn fact_list_raw_answers_the_rows_unshaped_so_a_process_outside_the_service_reads_what_the_service_does() {
        let s = store();
        put(&s, "sp-a", "claimed", "first", 1_700_000_000);
        let get: Get = &|i, k, t| read_on(&s, i, k, t);
        let (rc, out) = cmd_with(&argv(&["list", "--ids", "sp-a", "--raw"]), &|f| append_on(&s, f), get);
        assert_eq!(rc, 0, "{out}");
        let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
        assert_eq!((rows[0]["event_type"].as_str(), rows[0]["at"].as_str()), (Some("claimed"), Some("1700000000")));
        assert!(rows[0].get("created_at").is_none(), "shaped rows carry created_at, raw ones carry at");
    }

    #[test]
    fn fact_without_a_known_sub_verb_answers_the_usage() {
        for a in [&["frobnicate"][..], &[]] {
            let (rc, out) = cmd(&argv(a));
            assert_eq!(rc, CANNOT_TELL);
            assert!(out.starts_with("usage: spira-lc fact add"), "{out}");
        }
    }

    #[test_id(tgzg9le)]
    #[test]
    fn the_send_back_kind_is_accepted_and_is_a_long_kind() {
        assert!(KINDS.contains(&spira_config::send_back::KIND));
        assert!(LONG_KINDS.contains(&spira_config::send_back::KIND));
    }

    #[test_id(t13x1m6)]
    #[test]
    fn the_gate_start_kind_the_gate_sends_is_one_the_fact_verb_accepts() {
        assert!(KINDS.contains(&"gate-start"));
    }

    #[test_id(tmbemi9)]
    #[test]
    fn a_build_dir_fact_keeps_the_tab_between_the_tip_and_the_directory() {
        assert!(KINDS.contains(&"build-dir"));
        assert_eq!(bounded_tabbed("abc\tb/dir.1\n"), "abc\tb/dir.1");
        assert_eq!(bounded("abc\tdef"), "abcdef", "no other kind keeps a tab");
        assert_eq!(build_dir("sp-a", "abc", "b/dir.3", "builder", 7).cause, "abc\tb/dir.3");
    }

    #[test_id(tz4jwa1)]
    #[test]
    fn a_cause_is_bounded_and_single_line() {
        assert_eq!(bounded(&format!("a\nb{}", "x".repeat(300))).len(), CAUSE_MAX);
        assert!(!bounded("a\nb").contains('\n'));
    }

    #[test_id(toyhnf2)]
    #[test]
    fn a_fast_tier_red_keeps_its_lines_and_reads_back_whole() {
        assert!(KINDS.contains(&"fast-tier-red"));
        assert!(bounded_long("a\nb").contains('\n'));
        assert_eq!(bounded_long(&"x".repeat(LONG_MAX + 50)).len(), LONG_MAX);
        let s = store();
        let long = "x\n".repeat(LONG_MAX / 2);
        put(&s, "sp-a", "fast-tier-red", &long, 5);
        put(&s, "sp-a", "requeued", &"y".repeat(300), 6);
        let rows = read_on(&s, &["sp-a".into()], &[], None).unwrap();
        assert_eq!(rows[0]["new_value"].as_str().unwrap().len(), LONG_MAX);
        assert_eq!(rows[1]["new_value"].as_str().unwrap().len(), READ_MAX);
    }

    fn counting_store() -> crate::control::store::Store<crate::control::store::counting::Counting> {
        let c = Contract::open(
            crate::control::store::counting::Counting::default(),
            Ports::virtual_clock(&VirtualClock::new(1_000)),
            Config {
                min_lock_ttl_ms: 1_000,
            },
        )
        .unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    #[test_id(t7ws7ut)]
    #[test]
    fn listing_a_kind_since_a_floor_reads_the_index_and_never_pages_the_facts_prefix() {
        let s = counting_store();
        let floor = 5_000;
        let mut batch = Vec::new();
        for n in 0..2_000i64 {
            let (kind, at) = match n {
                n if n % 100 == 0 && n < 1_500 => ("eject-failure", floor + n + 1),
                n if n % 100 == 50 && n < 1_000 => ("eject-failure", floor - 1 - n),
                n => ("claimed", floor + n),
            };
            batch.push(fact_of(&format!("sp-{:03}", n % 200), kind, "c", "harness", at));
        }
        for chunk in batch.chunks(100) {
            append_on(&s, chunk).unwrap();
        }
        let expect = batch
            .iter()
            .filter(|f| f.kind == "eject-failure" && f.at > floor)
            .count();
        assert_eq!(expect, 15);
        s.contract().backend().take();
        let rows = read_on(&s, &[], &["eject-failure".into()], Some(floor)).unwrap();
        assert_eq!(rows.len(), expect);
        let reads = s.contract().backend().take();
        assert!(
            !reads.is_empty()
                && reads.iter().all(|(k, p, limit)| *k == "range_kind_since"
                    && p.starts_with("facts/")
                    && *limit <= crate::control::facts::KIND_CHUNK),
            "{reads:?}"
        );
    }

    #[test_id(tykszo8)]
    #[test]
    fn listing_kinds_answers_what_filtering_every_fact_answers_carried_facts_included() {
        let s = store();
        let carried: Vec<Fact> = (0..3)
            .map(|n| {
                fact_of(
                    "sp-b",
                    if n == 1 { "reclaimed" } else { "requeued" },
                    "old",
                    "harness",
                    5 + n,
                )
            })
            .collect();
        s.carry_facts("carried-from-lifecycle", &carried, "harness")
            .unwrap();
        put(&s, "sp-c", "requeued", "x", 90);
        put(&s, "sp-b", "reclaimed", "y", 60);
        put(&s, "sp-a", "eject-failure", "z", 70);
        put(&s, "sp-b", "requeued", "w", 50);
        put(&s, "sp-a", "requeued", "v", 20);
        put(&s, "sp-a", "claimed", "u", 80);
        put(&s, "sp-a", "eject-failure", "t", 10);
        for (kinds, since) in [
            (vec!["requeued", "reclaimed"], Some(5)),
            (vec!["requeued", "reclaimed"], None),
            (vec!["eject-failure"], None),
            (vec!["eject-failure"], Some(10)),
            (vec!["requeued", "requeued"], Some(i64::MAX)),
        ] {
            let kinds: Vec<String> = kinds.into_iter().map(String::from).collect();
            let old: Vec<Fact> = s
                .facts(&[])
                .unwrap()
                .into_iter()
                .filter(|f| kinds.contains(&f.kind) && since.is_none_or(|t| f.at > t))
                .collect();
            let want: Vec<Value> = old.iter().map(|f| serde_json::json!({ "issue_id": f.bead, "event_type": f.kind, "new_value": f.cause, "actor": f.actor, "at": f.at.to_string() })).collect();
            assert_eq!(
                read_on(&s, &[], &kinds, since).unwrap(),
                want,
                "{kinds:?} {since:?}"
            );
        }
    }

    // covers: UC-ops-detection-remediation-12
    #[test_id(t337c93)]
    #[test]
    fn facts_read_back_per_bead_in_the_order_they_were_written_and_scope_to_ids_kinds_and_since() {
        let s = store();
        put(&s, "sp-b", "claimed", "w1", 10);
        put(&s, "sp-a", "claimed", "w2", 20);
        put(&s, "sp-a", "requeued", "gate-red", 30);
        put(&s, "sp-b", "requeued", "it's", 40);
        let all = read_on(&s, &[], &[], None).unwrap();
        let order: Vec<(&str, &str)> = all.iter().map(|r| (r["issue_id"].as_str().unwrap(), r["event_type"].as_str().unwrap())).collect();
        assert_eq!(order, [("sp-a", "claimed"), ("sp-a", "requeued"), ("sp-b", "claimed"), ("sp-b", "requeued")]);
        assert_eq!(read_on(&s, &["sp-a".into()], &["requeued".into()], None).unwrap().len(), 1);
        let since = read_on(&s, &[], &[], Some(30)).unwrap();
        assert_eq!(since.len(), 1, "a fact at the watermark itself is excluded: {since:?}");
        assert_eq!(since[0]["new_value"], "it's");
        assert_eq!(shape(all)[0]["created_at"], "1970-01-01T00:00:20Z");
    }

    #[test_id(tg46f96)]
    #[test]
    fn a_later_fact_with_an_earlier_time_still_reads_after_the_earlier_write() {
        let s = store();
        put(&s, "sp-a", "claimed", "first", 100);
        put(&s, "sp-a", "requeued", "second", 50);
        let kinds: Vec<String> = read_on(&s, &[], &[], None).unwrap().iter().map(|r| r["event_type"].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, ["claimed", "requeued"], "the order is the order they were written in, whatever their clocks said");
    }
}
