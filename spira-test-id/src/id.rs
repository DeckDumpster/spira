//! The id grammar, a plain module so a test can include it (a proc-macro lib's own test binary cannot load).

pub const GRAMMAR: &str = "a test id is `t` plus six characters of [0-9a-z], e.g. t7k3m9q; run `test-registry stamp --path <file>` to mint one";

pub fn check_id(id: &str) -> Result<(), String> {
    let b = id.as_bytes();
    let ok = b.len() == 7 && b[0] == b't' && b[1..].iter().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase());
    if ok {
        Ok(())
    } else {
        Err(format!("invalid test id {id:?}: {GRAMMAR}"))
    }
}
