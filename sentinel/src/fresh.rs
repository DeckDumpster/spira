//! Re-read before a write (DESIGN.md §2.10, sp-du8bv).
//!
//! Every check decides from the pass's one snapshot, read at pass start, and a pass can run
//! for minutes. A decision is only as current as that read; a WRITE must not be. So each
//! mutating action on a bead re-reads that bead live — one `bd show <id>… --json` for all
//! the beads a check is about to touch — and skips a bead whose lifecycle state is no longer what the
//! snapshot showed (bd's status is never the fence). A bead the re-read cannot find, or a re-read that fails, is skipped too:
//! the next pass decides it again from a fresh snapshot, and no write here is so urgent that
//! it is better made blind.
//!
//! Lifecycle writes are exempt: `spira-lc apply` carries the row version and the machine
//! refuses a stale one, which is the same guarantee made structurally.

use std::collections::HashMap;

use crate::model::{parse_beads, Bead};
use crate::pass::Sentinel;

impl<'a> Sentinel<'a> {
    /// One live read of `ids`. None when bd itself failed; a missing id is simply absent.
    pub fn reread(&self, ids: &[&str]) -> Option<HashMap<String, Bead>> {
        if ids.is_empty() {
            return Some(HashMap::new());
        }
        let mut a = vec!["show".to_string()];
        a.extend(ids.iter().map(|s| s.to_string()));
        let json = self.bd().json(self.h, &a).ok()?;
        let rows = parse_beads(&json).ok()?;
        Some(rows.into_iter().map(|b| (b.id.clone(), b)).collect())
    }

    /// The live row of `id` when its lifecycle state is still `was` (None: rowless); otherwise
    /// log why this write is skipped and return None. A state the machine cannot be asked for
    /// is not `was`.
    pub fn still<'m>(
        &self,
        check: &str,
        id: &str,
        was: Option<&str>,
        live: Option<&'m HashMap<String, Bead>>,
    ) -> Option<&'m Bead> {
        let now = match live {
            None => {
                self.log(&format!("{check} {id}: could not re-read it before writing — skipped, the next pass decides it again"));
                return None;
            }
            Some(m) => m.get(id),
        };
        match now {
            Some(b) => {
                let live_state = self.lc_live_state(id);
                if live_state.as_deref() == was {
                    return Some(b);
                }
                self.log(&format!(
                    "{check} {id}: {} in this pass's snapshot, {} now — skipped, the next pass decides it again",
                    was.unwrap_or("rowless"),
                    live_state.as_deref().unwrap_or("rowless or unreadable")
                ));
                None
            }
            None => {
                self.log(&format!("{check} {id}: no longer in the store — skipped"));
                None
            }
        }
    }
}
