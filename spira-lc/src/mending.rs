//! MENDING rows: opened by the batch's `Failed` event in the same transaction, then driven by
//! `mending pickup|event|sweep|list`. Every timestamp is stamped here from the clock, never
//! taken from the caller, so a mender cannot move its own deadline; `sweep` is how the
//! machine applies `Expire` to an overdue row.

use lifecycle::mending::MendingRow;

use crate::CANNOT_TELL;

pub fn fetch(batch: &str, pass: u32, suite: &str) -> Result<Option<MendingRow>, String> {
    let store = crate::control::served::store()?;
    Ok(store.mending(batch, pass, suite).map_err(|e| e.to_string())?.map(|m| m.row))
}

const BATCH_MAX: usize = 64;
const SUITE_MAX: usize = 160;
const KEY_MAX: usize = 256;

/// Refuse an over-long field by name, before the machine sees it.
pub fn check_widths(batch: &str, pass: u32, suite: &str) -> Result<(), (i32, String)> {
    let key = lifecycle::mending::key(batch, pass, suite);
    for (name, value, max) in [("batch_id", batch, BATCH_MAX), ("suite", suite, SUITE_MAX), ("lc_key", key.as_str(), KEY_MAX)] {
        let n = value.chars().count();
        if n > max {
            return Err((CANNOT_TELL, format!("mending: refused: {name} is {n} chars (max {max}); {name}={value}")));
        }
    }
    Ok(())
}
