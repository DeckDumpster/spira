//! The control store's `section`, `intent`, `lock`, `pass` and `rc` verbs and its key layout, as a
//! library: `spira-lc` serves them, and an in-process caller (the simulator's replay) reaches
//! the same dispatch on its own store instead of a hand-written copy of it.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod goodput;
pub mod keys;
pub mod rc;
pub mod sections;

pub const CANNOT_TELL: i32 = 2;
pub const REFUSED: i32 = 3;

/// The refusal for a verb served only over the socket, when it arrives as the command line's own argv.
pub fn cli_refusal(args: &[String]) -> Option<(i32, String)> {
    let verb = args.first().filter(|v| matches!(v.as_str(), "dep-add" | "dep-remove"))?;
    Some((
        REFUSED,
        format!(
            "refused: spira-lc {verb} only mirrors an edge the content door has already written; a dependency edge is written through that door. \
             Run `bead.sh dep add <id> <depends-on-id> [--type T]` or `bead.sh dep remove <id> <depends-on-id>` (from a lane persona, `work dep-add`)."
        ),
    ))
}

pub fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Rows per page when an admin verb reads the whole lifecycle table.
pub const PAGE_ROWS: usize = 1_000;

/// Every lifecycle row `filter` keeps, read as pages by cursor through `call` until a short page.
pub fn list_pages(filter: &[&str], mut call: impl FnMut(&[String]) -> (i32, String)) -> Result<Vec<serde_json::Value>, String> {
    let mut all: Vec<serde_json::Value> = Vec::new();
    loop {
        let mut args: Vec<String> = std::iter::once("list").chain(filter.iter().copied()).map(String::from).collect();
        if let Some(last) = all.last().and_then(|r| r.get("bead_id")).and_then(|i| i.as_str()) {
            args.extend(["--after".into(), last.to_string()]);
        } else if !all.is_empty() {
            return Err("lifecycle list is not a JSON array of rows".into());
        }
        args.extend(["--limit".into(), PAGE_ROWS.to_string()]);
        let (rc, out) = call(&args);
        let rows = match serde_json::from_str::<serde_json::Value>(out.trim()) {
            Ok(serde_json::Value::Array(rows)) if rc == 0 => rows,
            _ => return Err(format!("lifecycle list failed: {}", out.trim())),
        };
        let short = rows.len() < PAGE_ROWS;
        all.extend(rows);
        if short {
            return Ok(all);
        }
    }
}

/// `--after <id> --limit <n>` on a list: the rows after `id` in id order, at most `n`.
pub type Page = (Option<String>, Option<usize>);

/// The page a list asks for, or `None` when neither flag is given.
pub fn page(args: &[String]) -> Result<Option<Page>, String> {
    let after = flag(args, "--after");
    let limit = match flag(args, "--limit") {
        None => None,
        Some(n) => match n.parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            _ => return Err(format!("--limit must be a positive integer, not {n:?}")),
        },
    };
    Ok((after.is_some() || limit.is_some()).then_some((after, limit)))
}

#[cfg(test)]
mod list_pages_tests {
    use super::*;

    #[test_id(tyircxm)]
    #[test]
    fn a_whole_table_read_is_pages_by_cursor_until_a_short_page() {
        let ids: Vec<String> = (0..PAGE_ROWS + 5).map(|i| format!("sp-{i:05}")).collect();
        let mut sent: Vec<Vec<String>> = Vec::new();
        let rows = list_pages(&[], |args| {
            sent.push(args.to_vec());
            let after = flag(args, "--after");
            let page: Vec<_> = ids
                .iter()
                .filter(|i| after.as_ref().is_none_or(|a| *i > a))
                .take(PAGE_ROWS)
                .map(|i| serde_json::json!({"bead_id": i}))
                .collect();
            (0, serde_json::Value::Array(page).to_string())
        })
        .unwrap();
        assert_eq!(rows.len(), PAGE_ROWS + 5);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], ["list", "--limit", "1000"]);
        assert_eq!(sent[1], ["list", "--after", "sp-00999", "--limit", "1000"]);
        assert!(list_pages(&[], |_| (3, "refused".into())).is_err());
    }
}

#[cfg(test)]
mod cli_refusal_tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(tcn2jbm)]
    #[test]
    fn a_hand_typed_edge_verb_is_refused_and_names_the_content_door() {
        for a in [&["dep-add", "sp-a", "sp-b"][..], &["dep-add", "sp-a", "sp-b", "--type", "blocks"], &["dep-remove", "sp-a", "sp-b"]] {
            let got = cli_refusal(&argv(a));
            assert!(matches!(&got, Some((REFUSED, m)) if m.contains("bead.sh dep add")), "{a:?}: {got:?}");
        }
        for a in [&["show", "sp-a"][..], &["content", "dep", "add", "sp-a", "sp-b"], &["create-bead", "sp-a"]] {
            assert_eq!(cli_refusal(&argv(a)), None, "{a:?}");
        }
    }
}

#[cfg(test)]
mod watch_hold_tests {
    #[test_id(thug090)]
    #[test]
    fn the_clients_watch_hold_is_the_services_cap() {
        assert_eq!(spira_store::lc_call::WATCH_HOLD_MS, state_store::MAX_WATCH_WAIT_MS);
    }
}
