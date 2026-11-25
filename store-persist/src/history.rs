//! Bounding a database's commit history: the newest commits stay, everything older becomes one base.
//!
//! The rewrite runs on a side branch so the served branch's working set is never touched (the
//! store keeps its uncommitted writes there). Writes that land while the
//! side branch is built are cherry-picked onto it, and the served branch is then soft-reset onto
//! it: the soft reset leaves the working set alone, so a commit that slips in after the last
//! look is dropped from the log but never from the data.

/// Runs SQL against one database; `csv` selects machine-readable output.
pub trait Sql {
    fn run(&self, script: &str, csv: bool) -> Result<String, String>;
}

const SIDE: &str = "store_persist_squash";
const REBASE: &str = "dolt_rebase_store_persist_squash";

/// Log length after a rewrite that keeps `keep`: the root, the base and `keep - 1` replayed.
pub fn length_after(keep: u64) -> u64 {
    keep + 1
}

pub fn needs_squash(len: u64, keep: u64) -> bool {
    keep > 0 && len > length_after(keep)
}

fn csv_rows(out: &str, header: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut inside = false;
    for line in out.lines() {
        if line == header {
            inside = true;
        } else if inside && !line.is_empty() {
            rows.push(line.to_string());
        } else if inside {
            inside = false;
        }
    }
    rows
}

fn hash_ok(h: &str) -> Result<&str, String> {
    if !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric()) {
        Ok(h)
    } else {
        Err(format!("{h:?} is not a commit hash"))
    }
}

pub fn len(sql: &dyn Sql) -> Result<u64, String> {
    let out = sql.run("SELECT COUNT(*) AS n FROM dolt_log;\n", true)?;
    csv_rows(&out, "n").first().and_then(|n| n.parse().ok()).ok_or_else(|| format!("no log length in {out:?}"))
}

fn served(sql: &dyn Sql) -> Result<String, String> {
    let out = sql.run("SELECT active_branch() AS b;\n", true)?;
    let b = csv_rows(&out, "b").first().cloned().ok_or_else(|| format!("no served branch in {out:?}"))?;
    hash_ok(&b.replace(['/', '-', '_', '.'], "x")).map(|_| b)
}

/// Deletes a side branch and a rebase branch an earlier, interrupted rewrite left behind.
pub fn abandon(sql: &dyn Sql) -> Result<(), String> {
    let out = sql.run(&format!("SELECT name FROM dolt_branches WHERE name IN ('{SIDE}', '{REBASE}');\n"), true)?;
    let script: String = csv_rows(&out, "name").iter().map(|b| format!("CALL DOLT_BRANCH('-D', '{b}');\n")).collect();
    if script.is_empty() {
        return Ok(());
    }
    sql.run(&script, false).map(|_| ())
}

/// Builds the side branch: every commit but the root squashed into one base by a soft reset,
/// then the newest `keep - 1` cherry-picked on it. No squashed commit is replayed, so an old
/// schema change cannot fail the build. Returns the served branch's tip the log was read at.
pub fn squash(sql: &dyn Sql, keep: u64) -> Result<String, String> {
    abandon(sql)?;
    let main = served(sql)?;
    let out = sql.run(&format!("SELECT HASHOF('{main}') AS tip;\n"), true)?;
    let tip = csv_rows(&out, "tip").first().cloned().ok_or_else(|| format!("no tip in {out:?}"))?;
    let log = csv_rows(&sql.run(&format!("SELECT commit_hash FROM dolt_log('{}') ORDER BY commit_order ASC;\n", hash_ok(&tip)?), true)?, "commit_hash");
    let mut boundary = log.len().saturating_sub(keep.saturating_sub(1) as usize + 1);
    if log.len() < 3 || boundary == 0 {
        return Err(format!("a log of {} commits has nothing to squash at keep {keep}", log.len()));
    }
    loop {
        match build_side(sql, &log, boundary) {
            Ok(None) => return Ok(tip),
            Ok(Some(stuck)) => boundary = stuck,
            Err(e) => {
                let _ = abandon(sql);
                return Err(e);
            }
        }
    }
}

/// Cuts the side branch at `log[boundary]`, makes its tree the one base and picks what follows.
/// A commit that cannot be picked is returned: it becomes the next boundary, strictly newer.
fn build_side(sql: &dyn Sql, log: &[String], boundary: usize) -> Result<Option<usize>, String> {
    abandon(sql)?;
    sql.run(
        &format!(
            "CALL DOLT_BRANCH('{SIDE}', '{}');\nCALL DOLT_CHECKOUT('{SIDE}');\nCALL DOLT_RESET('--soft', '{}');\nCALL DOLT_COMMIT('--allow-empty', '-Am', 'history squashed');\n",
            hash_ok(&log[boundary])?,
            hash_ok(&log[0])?
        ),
        false,
    )?;
    for (i, h) in log.iter().enumerate().skip(boundary + 1) {
        if sql.run(&format!("CALL DOLT_CHECKOUT('{SIDE}');\nCALL DOLT_CHERRY_PICK('--allow-empty', '{}');\n", hash_ok(h)?), false).is_err() {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

/// Replays what the served branch gained since `tip` onto the side branch, until it is caught
/// up, then moves the served branch onto it and deletes the side branch.
pub fn graft(sql: &dyn Sql, tip: &str) -> Result<(), String> {
    let result = graft_inner(sql, tip);
    if result.is_err() {
        let _ = abandon(sql);
    }
    result
}

fn graft_inner(sql: &dyn Sql, tip: &str) -> Result<(), String> {
    let mut from = hash_ok(tip)?.to_string();
    let main = served(sql)?;
    loop {
        let out = sql.run(&format!("SELECT commit_hash FROM dolt_log('{from}..{main}') ORDER BY commit_order ASC;\n"), true)?;
        let pending = csv_rows(&out, "commit_hash");
        let Some(last) = pending.last().cloned() else {
            break;
        };
        let mut script = format!("CALL DOLT_CHECKOUT('{SIDE}');\n");
        for h in &pending {
            script.push_str(&format!("CALL DOLT_CHERRY_PICK('--allow-empty', '{}');\n", hash_ok(h)?));
        }
        sql.run(&script, false)?;
        from = last;
    }
    sql.run(&format!("CALL DOLT_RESET('--soft', HASHOF('{SIDE}'));\nCALL DOLT_BRANCH('-D', '{SIDE}');\n"), false).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tjz2fyx)]
    #[test]
    fn csv_rows_reads_the_named_result_set_and_nothing_else() {
        let out = "status\n0\n\ntip\nabc\n\nn\n5\n";
        assert_eq!(csv_rows(out, "tip"), ["abc"]);
        assert_eq!(csv_rows(out, "n"), ["5"]);
        assert!(csv_rows(out, "absent").is_empty());
    }

    #[test_id(tuz3rmg)]
    #[test]
    fn a_log_at_or_under_the_bound_is_left_alone() {
        assert!(!needs_squash(101, 100));
        assert!(needs_squash(102, 100));
        assert!(!needs_squash(1_000_000, 0), "0 disables the bound");
    }

    #[test_id(tlpup70)]
    #[test]
    fn a_hash_with_sql_in_it_is_refused() {
        assert!(hash_ok("abc123").is_ok());
        assert!(hash_ok("a'); DROP TABLE t; --").is_err());
    }

    struct Stub {
        scripts: std::cell::RefCell<Vec<String>>,
        unpickable: &'static str,
    }

    impl Sql for Stub {
        fn run(&self, script: &str, _csv: bool) -> Result<String, String> {
            self.scripts.borrow_mut().push(script.to_string());
            if script.contains("active_branch") {
                Ok("b\nmain\n".into())
            } else if script.contains("HASHOF") {
                Ok("tip\nc9\n".into())
            } else if script.contains("FROM dolt_log") {
                Ok("commit_hash\nc0\nc1\nc2\nc3\nc4\nc5\nc6\nc7\nc8\nc9\n".into())
            } else if script.contains(&format!("CHERRY_PICK('--allow-empty', '{}')", self.unpickable)) {
                Err("cannot merge".into())
            } else {
                Ok(String::new())
            }
        }
    }

    #[test_id(tyd8u9x)]
    #[test]
    fn a_kept_commit_that_cannot_be_picked_becomes_the_base_and_the_build_restarts_newer() {
        let stub = Stub { scripts: Default::default(), unpickable: "c8" };
        assert_eq!(squash(&stub, 4).unwrap(), "c9");
        let scripts = stub.scripts.borrow();
        let bases: Vec<_> = scripts.iter().filter(|s| s.contains("DOLT_RESET('--soft'")).collect();
        assert!(bases[0].contains("DOLT_BRANCH('store_persist_squash', 'c6')"), "keep 4 cuts at c6: {}", bases[0]);
        assert!(bases[1].contains("DOLT_BRANCH('store_persist_squash', 'c8')"), "the stuck commit is the new boundary: {}", bases[1]);
        assert_eq!(bases.len(), 2);
    }

    #[test_id(tew3eyk)]
    #[test]
    fn a_log_with_nothing_to_squash_is_refused() {
        let stub = Stub { scripts: Default::default(), unpickable: "none" };
        assert!(squash(&stub, 10).is_err());
    }
}
