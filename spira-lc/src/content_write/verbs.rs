//! The door's writes that are not one bead's row: the store's key-value and memory rows, the
//! blocked flag over every bead, gates, a state change's event bead, and the alert purge's delete.

use super::*;

fn transact(what: &str, w: &Writer, script: &[String]) -> Result<(), String> {
    w.conn.transaction(|tx| apply(what, script, tx)).and_then(|r| r)
}

fn begin(mut body: Vec<String>) -> Vec<String> {
    body.insert(0, "BEGIN".into());
    body.push("COMMIT".into());
    body
}

pub(super) fn standalone(w: &Writer, m: &Mutation) -> Result<Option<String>, String> {
    match m.ops.first() {
        Some(Op::RecomputeBlocked) => recompute_blocked(w).map(Some),
        Some(Op::GateCheck { kind }) => gate_check(w, kind),
        Some(Op::KvSet { key, value }) => kv_set(w, key, value),
        Some(Op::KvClear { key }) => kv_clear(w, key),
        Some(Op::Remember { key, text }) => remember(w, key, text),
        Some(Op::Forget { key }) => forget(w, key),
        _ => Ok(None),
    }
}

fn config_set(db: &str, key: &str, value: &str) -> String {
    format!("REPLACE INTO {db}.config (`key`, value) VALUES ('{}', '{}')", escape(key), escape(value))
}

fn config_delete(db: &str, key: &str) -> String {
    format!("DELETE FROM {db}.config WHERE `key` = '{}'", escape(key))
}

fn config_get(w: &Writer, key: &str) -> Result<Option<String>, String> {
    Ok(rows(w, &format!("SELECT value AS v FROM {}.config WHERE `key` = '{}'", w.db, escape(key)))?.first().map(|r| cell(r, "v")))
}

/// bd's `validateKVKey`: a key it would refuse is left to bd to refuse.
fn kv_key_allowed(key: &str) -> bool {
    const RESERVED: [&str; 8] = ["kv.", "memory.", "sync.", "conflict.", "federation.", "jira.", "linear.", "export."];
    !key.trim().is_empty() && !RESERVED.iter().any(|p| key.starts_with(p)) && !key.starts_with("import.")
}

fn kv_set(w: &Writer, key: &str, value: &str) -> Result<Option<String>, String> {
    if !kv_key_allowed(key) {
        return Ok(None);
    }
    transact("kv set", w, &begin(vec![config_set(&w.db, &format!("kv.{key}"), value)]))?;
    Ok(Some(format!("Set {key} = {value}\n")))
}

fn kv_clear(w: &Writer, key: &str) -> Result<Option<String>, String> {
    if !kv_key_allowed(key) {
        return Ok(None);
    }
    transact("kv clear", w, &begin(vec![config_delete(&w.db, &format!("kv.{key}"))]))?;
    Ok(Some(format!("Cleared {key}\n")))
}

/// bd's `truncateMemory`: newlines as spaces, at most 80 bytes, cut on a character boundary.
fn memory_line(text: &str) -> String {
    let flat = text.replace('\n', " ");
    if flat.len() <= 80 {
        return flat;
    }
    let mut cut = 77;
    while !flat.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &flat[..cut])
}

fn memory_key(key: &str) -> String {
    format!("kv.memory.{key}")
}

fn remember(w: &Writer, key: &str, text: &str) -> Result<Option<String>, String> {
    if key.trim().is_empty() || text.trim().is_empty() {
        return Ok(None);
    }
    let held = config_get(w, &memory_key(key))?.is_some();
    transact("remember", w, &begin(vec![config_set(&w.db, &memory_key(key), text)]))?;
    Ok(Some(format!("{} [{key}]: {}\n", if held { "Updated" } else { "Remembered" }, memory_line(text))))
}

fn forget(w: &Writer, key: &str) -> Result<Option<String>, String> {
    let Some(held) = config_get(w, &memory_key(key))?.filter(|v| !v.is_empty() && !key.trim().is_empty()) else {
        return Ok(None);
    };
    transact("forget", w, &begin(vec![config_delete(&w.db, &memory_key(key))]))?;
    Ok(Some(format!("Forgot [{key}]: {}\n", memory_line(&held))))
}

const RECOMPUTE_PASSES: usize = 8;

fn blocked_flags(w: &Writer) -> Result<Vec<(String, String)>, String> {
    Ok(rows(w, &format!("SELECT id, is_blocked FROM {}.issues ORDER BY id", w.db))?.iter().map(|r| (cell(r, "id"), cell(r, "is_blocked"))).collect())
}

fn recompute_blocked(w: &Writer) -> Result<String, String> {
    let before = blocked_flags(w)?;
    transact("recompute-blocked", w, &begin(blocked_passes(&w.db, None, RECOMPUTE_PASSES)))?;
    let changed = blocked_flags(w)?.iter().zip(&before).filter(|(a, b)| a != b).count();
    Ok(match changed {
        0 => "is_blocked already consistent — nothing to recompute.\n".to_string(),
        n => format!("Recomputed is_blocked: {n} row(s) corrected.\n"),
    })
}

/// Go's `time.Duration` text for a whole number of seconds.
fn duration_text(secs: i128) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m{s}s"),
        _ => format!("{h}h{m}m{s}s"),
    }
}

/// `ns` rounded to whole seconds, halves away from zero, as `time.Duration.Round(time.Second)`.
fn round_seconds(ns: i128) -> i128 {
    let half = 500_000_000;
    if ns >= 0 {
        (ns + half) / 1_000_000_000
    } else {
        -((-ns + half) / 1_000_000_000)
    }
}

/// `gate check` as bd runs it over the gates that need no program to judge: timer gates and bead
/// gates on a bead of this store. A gate that asks GitHub, or that bd would report an error for,
/// leaves the whole check to bd.
fn gate_check(w: &Writer, kind: &str) -> Result<Option<String>, String> {
    let db = &w.db;
    let open = rows(w, &format!("SELECT id, await_type, await_id, timeout_ns, TIMESTAMPDIFF(MICROSECOND, created_at, UTC_TIMESTAMP(6)) AS age_us FROM {db}.issues WHERE issue_type = 'gate' AND status <> 'closed' ORDER BY created_at LIMIT 101"))?;
    if open.len() > 100 {
        return Ok(None);
    }
    let wanted = |kind_of: &str| match kind {
        "" | "all" => true,
        "gh" => kind_of.starts_with("gh:"),
        other => kind_of == other,
    };
    let checked: Vec<&serde_json::Value> = open.iter().filter(|g| wanted(&cell(g, "await_type"))).collect();
    if checked.is_empty() {
        return Ok(Some(if kind.is_empty() { "No open gates found.\n".to_string() } else { format!("No open gates of type '{kind}' found.\n") }));
    }
    let (mut lines, mut closing, mut judged) = (String::new(), Vec::new(), 0);
    for g in checked {
        let (id, await_type) = (cell(g, "id"), cell(g, "await_type"));
        match await_type.as_str() {
            "timer" => {
                let timeout: i128 = cell(g, "timeout_ns").parse().unwrap_or(0);
                if timeout == 0 {
                    return Ok(None);
                }
                let late = cell(g, "age_us").parse::<i128>().unwrap_or(0) * 1000 - timeout;
                if late > 0 {
                    let reason = format!("timer expired {} ago", duration_text(round_seconds(late)));
                    lines.push_str(&format!("✓ {id}: resolved - {reason}\n"));
                    closing.push((id, reason));
                } else {
                    lines.push_str(&format!("○ {id}: pending - expires in {}\n", duration_text(round_seconds(-late))));
                }
            }
            "bead" => {
                let target = cell(g, "await_id");
                if target.is_empty() || target.contains(':') {
                    return Ok(None);
                }
                let Some(held) = rows(w, &format!("SELECT status, status = 'closed' AS closed FROM {db}.issues WHERE id = '{}'", escape(&target)))?.into_iter().next() else {
                    return Ok(None);
                };
                if cell(&held, "closed") == "1" {
                    let reason = format!("bead {target} closed");
                    lines.push_str(&format!("✓ {id}: resolved - {reason}\n"));
                    closing.push((id, reason));
                } else {
                    lines.push_str(&format!("○ {id}: pending - bead {target} is {}\n", cell(&held, "status")));
                }
            }
            t if t.starts_with("gh:run") || t.starts_with("gh:pr") => return Ok(None),
            _ => continue,
        }
        judged += 1;
    }
    if judged == 0 {
        return Ok(Some("\nChecked 0 gates: 0 resolved, 0 escalated, 0 errors\n".to_string()));
    }
    if !closing.is_empty() {
        let env = env_of(w)?;
        let script: Vec<String> = closing.iter().flat_map(|(id, reason)| close_sql(db, id, reason, &env)).collect();
        transact("gate check", w, &begin(script))?;
    }
    Ok(Some(format!("{lines}\nChecked {judged} gates: {} resolved, 0 escalated, 0 errors\n", closing.len())))
}

/// `set-state`: a closed event bead under the bead, then the dimension's label swapped, as one
/// transaction.
pub(super) fn set_state(w: &Writer, bead: &str, dimension: &str, value: &str) -> Result<Option<String>, String> {
    let db = &w.db;
    let prefix = format!("{dimension}:");
    let labels = rows(w, &format!("SELECT label FROM {db}.labels WHERE issue_id = '{}' ORDER BY label", escape(bead)))?;
    let old = labels.iter().map(|r| cell(r, "label")).find_map(|l| l.strip_prefix(&prefix).map(str::to_string));
    if old.as_deref() == Some(value) {
        return Ok(Some(format!("(no change: {dimension} already set to {value})\n")));
    }
    let description = match &old {
        Some(was) => format!("Changed {dimension} from {was} to {value}"),
        None => format!("Set {dimension} to {value}"),
    };
    let event = Create {
        title: format!("State change: {dimension} → {value}"),
        issue_type: "event".into(),
        status: "closed".into(),
        priority: 4,
        labels: Vec::new(),
        parent: Some(bead.into()),
        deps: Vec::new(),
        description,
        external_ref: String::new(),
        assignee: String::new(),
        id: None,
        inherit_labels: false,
        shape: Shape::Id,
    };
    let env = env_of(w)?;
    for _ in 0..ID_ATTEMPTS {
        let n = next_child(w, bead)?;
        let event_id = format!("{bead}.{n}");
        let mut script = create_script(db, &env, &event, &event_id, &[], Some(&(bead.to_string(), n)));
        script.pop();
        script.push(format!("UPDATE {db}.issues SET closed_at = UTC_TIMESTAMP() WHERE id = '{}'", escape(&event_id)));
        if let Some(was) = &old {
            script.extend(label_remove_sql(db, bead, &format!("{prefix}{was}")));
        }
        script.extend(label_add_sql(db, bead, &format!("{prefix}{value}")));
        script.push("COMMIT".into());
        match transact("set-state", w, &script) {
            Ok(()) => {
                let previous = old.as_ref().map(|was| format!("  Previous: {was}\n")).unwrap_or_default();
                return Ok(Some(format!("✓ Set {dimension} = {value} on {bead}\n{previous}  Event: {event_id}\n")));
            }
            Err(e) if e.to_lowercase().contains("duplicate") || e.contains("conflicted with another writer") => continue,
            Err(e) => return Err(e),
        }
    }
    Err("could not allocate an unused event id".into())
}

/// The statements that delete `ids` and what hangs on them, then settle the beads that depended
/// on them; `dependents` are those beads, read before the rows go.
fn delete_script(db: &str, env: &Env, ids: &[&str], dependents: &[&str]) -> Vec<String> {
    let list = in_list(ids);
    let mut s = Vec::new();
    for (table, column) in [("labels", "issue_id"), ("comments", "issue_id"), ("events", "issue_id"), ("dependencies", "issue_id"), ("child_counters", "parent_id")] {
        s.push(format!("DELETE FROM {db}.{table} WHERE {column} IN ({list})"));
    }
    if env.leases {
        s.push(format!("DELETE FROM {db}.leases WHERE issue_id IN ({list})"));
    }
    s.push(format!("DELETE FROM {db}.issues WHERE id IN ({list})"));
    if !dependents.is_empty() {
        s.extend(settle_blocked(db, dependents));
    }
    begin(s)
}

/// Deletes `ids` as one transaction; `None` when no writer is configured.
pub fn delete(ids: &[String]) -> Option<Result<(), String>> {
    let w = match writer()? {
        Ok(w) => w,
        Err(e) => return Some(Err(e)),
    };
    let named: Vec<&str> = ids.iter().map(String::as_str).collect();
    let run = || -> Result<(), String> {
        let db = &w.db;
        let env = env_of(w)?;
        let dependents: Vec<String> = rows(w, &format!("SELECT DISTINCT issue_id FROM {db}.dependencies WHERE depends_on_issue_id IN ({}) AND issue_id NOT IN ({})", in_list(&named), in_list(&named)))?.iter().map(|r| cell(r, "issue_id")).collect();
        let dependents: Vec<&str> = dependents.iter().map(String::as_str).collect();
        transact("delete", w, &delete_script(db, &env, &named, &dependents))?;
        COMMITTER.owe();
        Ok(())
    };
    Some(if named.is_empty() { Ok(()) } else { run() })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(crate) fn standalone_statements(db: &str) -> Vec<String> {
        let env = Env::default();
        let mut all = vec![config_set(db, "kv.k", "v"), config_delete(db, "kv.k")];
        all.extend(blocked_passes(db, None, 1));
        all.extend(delete_script(db, &env, &["sp-a"], &["sp-b"]));
        all.extend(close_sql(db, "sp-a", "r", &env));
        all
    }

    #[test_id(tg86u3k)]
    #[test]
    fn a_duration_is_written_as_go_writes_it() {
        assert_eq!([duration_text(0), duration_text(7), duration_text(300), duration_text(7197), duration_text(7200)], ["0s", "7s", "5m0s", "1h59m57s", "2h0m0s"]);
    }

    #[test_id(tfm8vkd)]
    #[test]
    fn a_duration_rounds_halves_away_from_zero() {
        assert_eq!([round_seconds(499_999_999), round_seconds(500_000_000), round_seconds(1_500_000_000), round_seconds(-500_000_000)], [0, 1, 2, -1]);
    }

    #[test_id(tokf2zl)]
    #[test]
    fn a_memory_line_is_cut_where_bd_cuts_it() {
        assert_eq!(memory_line("a\nb"), "a b");
        let long = "é".repeat(60);
        let cut = memory_line(&long);
        assert!(cut.len() <= 80 && cut.ends_with("...") && cut.is_char_boundary(cut.len() - 3), "{cut}");
    }

    #[test_id(tl2h4iz)]
    #[test]
    fn a_key_bd_would_refuse_is_not_taken() {
        assert!([" ", "kv.x", "memory.x", "sync.x", "import.x"].iter().all(|k| !kv_key_allowed(k)));
        assert!(kv_key_allowed("spira.watch.sp-a"));
    }

    #[test_id(to19f9s)]
    #[test]
    fn a_delete_takes_each_row_that_hangs_on_a_bead_before_the_bead_and_settles_its_dependents() {
        let s = delete_script("`d`", &Env::default(), &["sp-a", "sp-b"], &["sp-c"]);
        let at = |needle: &str| s.iter().position(|q| q.contains(needle)).unwrap_or_else(|| panic!("{needle}: {s:?}"));
        assert_eq!((s[0].as_str(), s.last().unwrap().as_str()), ("BEGIN", "COMMIT"));
        assert!(at("`d`.labels") < at("DELETE FROM `d`.issues") && at("`d`.events") < at("DELETE FROM `d`.issues") && at("`d`.leases") < at("DELETE FROM `d`.issues"));
        assert!(at("DELETE FROM `d`.issues") < at("UPDATE `d`.issues i SET i.is_blocked"));
        assert!(s[at("DELETE FROM `d`.issues")].contains("'sp-a', 'sp-b'"));
    }
}
