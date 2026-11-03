//! Which tasks act and which only observe: `SPIRA_TASK_MODES`, a `name=observe|act` list read
//! once at boot. Every registered task must be named and every name must be registered; there
//! is no default mode (law-a-config-value-has-no-fallback).

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
pub fn apply(regs: Vec<Registration>, table: &BTreeMap<String, Mode>) -> Result<Vec<Registration>, String> {
    if let Some(stray) = table.keys().find(|k| !regs.iter().any(|r| &r.name == *k)) {
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
    fn malformed_entries_are_refused() {
        for bad in ["heartbeat", "heartbeat=", "heartbeat=Act", "heartbeat=shadow", "=act", "a=act,a=observe"] {
            assert!(parse(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(parse("").unwrap().is_empty());
    }
}
