//! The control store's `section`, `intent`, `lock` and `pass` verbs and its key layout, as a
//! library: `spira-lc` serves them, and an in-process caller (the simulator's replay) reaches
//! the same dispatch on its own store instead of a hand-written copy of it.

pub mod keys;
pub mod sections;

pub const CANNOT_TELL: i32 = 2;
pub const REFUSED: i32 = 3;

pub fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}
