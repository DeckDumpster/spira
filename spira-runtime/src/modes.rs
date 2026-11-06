//! Which tasks act and which only observe: `SPIRA_TASK_MODES`, a `name=observe|act` list read
//! once at boot. Every registered task must be named and every name must be registered or a
//! pending row (`tasks::pending()`); there is no default mode (law-a-config-value-has-no-fallback).

use std::collections::BTreeMap;

use runtime::{Mode, Registration};

pub const KEY: &str = "SPIRA_TASK_MODES";

/// Entries are separated by commas or whitespace.
pub fn parse(value: &str) -> Result<BTreeMap<String, Mode>, String> {
    let mut table = BTreeMap::new();
    for entry in value.split(|c: char| c == ',' || c.is_whitespace()).filter(|e| !e.is_empty()) {
        let (name, mode) = entry.split_once('=').ok_or_else(|| format!("{KEY}: {entry:?} is not name=observe|act"))?;
        let mode = match mode {
            "observe" => Mode::Observe,
            "act" => Mode::Act,
            _ => return Err(format!("{KEY}: {entry:?} names mode {mode:?}; the modes are observe and act")),
        };
        if name.is_empty() {
            return Err(format!("{KEY}: {entry:?} names no task"));
        }
        if table.insert(name.to_string(), mode).is_some() {
            return Err(format!("{KEY}: {name} is named twice"));
        }
    }
    Ok(table)
}

/// Every registration with its configured mode, or the first way the table disagrees with them.
/// A name may be a registration or a row in `tasks::pending()`: the table names every pending
/// row ahead of its port, so moving a row into `TASKS` touches no config line. A pending name
/// boots nothing; a name in neither is refused.
pub fn apply(regs: Vec<Registration>, table: &BTreeMap<String, Mode>) -> Result<Vec<Registration>, String> {
    let pending: Vec<&str> = crate::tasks::pending().map(|t| t.name).collect();
    apply_with(regs, table, &pending)
}

/// [`apply`] against an explicit set of pending names, so the rule holds and is tested whether or
/// not any row is still pending.
fn apply_with(regs: Vec<Registration>, table: &BTreeMap<String, Mode>, pending: &[&str]) -> Result<Vec<Registration>, String> {
    let known = |k: &str| regs.iter().any(|r| r.name == k) || pending.contains(&k);
    if let Some(stray) = table.keys().find(|k| !known(k)) {
        return Err(format!("{KEY} names {stray}, which no task registers"));
    }
    regs.into_iter()
        .map(|r| match table.get(&r.name) {
            Some(m) => Ok(r.with_mode(*m)),
            None => Err(format!("{KEY} gives no mode for task {}; every task needs one", r.name)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::{Command, Ctx, Event, EventKindTag, Handler, Projection};

    struct Idle;
    impl Handler for Idle {
        fn interests(&self) -> &[EventKindTag] {
            &[]
        }
        fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            vec![]
        }
    }

    fn regs(names: &[&str]) -> Vec<Registration> {
        names.iter().map(|n| Registration::new(n, Idle)).collect()
    }

    #[test]
    fn each_task_takes_its_configured_mode() {
        let table = parse("heartbeat=act, round-duty=observe\nview=observe").unwrap();
        let out = apply(regs(&["heartbeat", "round-duty", "view"]), &table).unwrap();
        let got: Vec<(&str, Mode)> = out.iter().map(|r| (r.name.as_str(), r.mode)).collect();
        assert_eq!(got, vec![("heartbeat", Mode::Act), ("round-duty", Mode::Observe), ("view", Mode::Observe)]);
    }

    #[test]
    fn a_task_missing_from_the_table_is_a_config_error_not_a_default() {
        let table = parse("heartbeat=observe").unwrap();
        let err = apply(regs(&["heartbeat", "view"]), &table).err().expect("a task with no mode refuses to boot");
        assert!(err.contains("view") && err.contains(KEY), "{err}");
    }

    #[test]
    fn a_name_no_task_registers_is_an_error() {
        let table = parse("heartbeat=act,hearbeat=observe").unwrap();
        let err = apply(regs(&["heartbeat"]), &table).err().expect("a misspelt name is not ignored");
        assert!(err.contains("hearbeat"), "{err}");
    }

    #[test]
    fn a_pending_row_may_be_named_before_it_is_ported_and_is_not_booted() {
        // Every family's rows are ported, so the pending set is planted rather than read.
        let pending = "planted.pending_row";
        let table = parse(&format!("heartbeat=act {pending}=act")).unwrap();
        let out = apply_with(regs(&["heartbeat"]), &table, &[pending]).expect("a pending name is known, not stray");
        assert_eq!(out.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["heartbeat"], "a pending row is named, not registered");
        assert!(apply(regs(&["heartbeat"]), &table).is_err(), "the same name is stray once no row is pending under it");
        let err = apply_with(regs(&["heartbeat"]), &parse(&format!("heartbeat=act {pending}x=act")).unwrap(), &[pending]).err().expect("a name in neither table is refused");
        assert!(err.contains(&format!("{pending}x")), "{err}");
    }

    /// The `SPIRA_TASK_MODES` value in the shipped default (`spira/conf.d`) and in the complete
    /// fixture (`spira/testlib/complete.toml`).
    fn declared_tables() -> Vec<(&'static str, String)> {
        let conf = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d").join(KEY)).unwrap();
        let default = conf.split(&format!("${{{KEY}:=")).nth(1).and_then(|r| r.split("}\"").next()).expect("conf.d default").to_string();
        let toml = spira_config::process::complete_fixture_text();
        let fixture = toml.lines().find_map(|l| l.trim().strip_prefix("task_modes = \"")).and_then(|v| v.strip_suffix('"')).expect("complete.toml task_modes").to_string();
        vec![("spira/conf.d", default), ("spira/testlib/complete.toml", fixture)]
    }

    #[test]
    fn the_shipped_default_and_the_complete_fixture_name_every_core_and_pending_task_in_act() {
        let mut want: Vec<&str> = crate::tasks::all().chain(crate::tasks::pending()).map(|t| t.name).collect();
        want.sort();
        for (place, value) in declared_tables() {
            let table = parse(&value).unwrap_or_else(|e| panic!("{place}: {e}"));
            assert_eq!(table.keys().map(String::as_str).collect::<Vec<_>>(), want, "{place} must name every CORE and PENDING task");
            let observe: Vec<&String> = table.iter().filter(|(_, m)| **m != Mode::Act).map(|(n, _)| n).collect();
            assert!(observe.is_empty(), "{place}: every task lands in Act mode (D4): {observe:?}");
        }
    }

    #[test]
    fn malformed_entries_are_refused() {
        for bad in ["heartbeat", "heartbeat=", "heartbeat=Act", "heartbeat=shadow", "=act", "a=act,a=observe"] {
            assert!(parse(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(parse("").unwrap().is_empty());
    }
}
