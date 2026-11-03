//! The control store's `section`, `intent`, `lock`, `pass` and `rc` verbs and its key layout, as a
//! library: `spira-lc` serves them, and an in-process caller (the simulator's replay) reaches
//! the same dispatch on its own store instead of a hand-written copy of it.

pub mod keys;
pub mod rc;
pub mod sections;

pub const CANNOT_TELL: i32 = 2;
pub const REFUSED: i32 = 3;

pub fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// `--after <id> --limit <n>` on a list: the rows after `id` in id order, at most `n`, or
/// `None` when neither flag is given.
pub fn page(args: &[String]) -> Result<Option<(Option<String>, Option<usize>)>, String> {
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
