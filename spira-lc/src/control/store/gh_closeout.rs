//! The pending GitHub closeout: `gh-closeout/<bead>`, one row per bead that reached LANDED,
//! SUPERSEDED or DROPPED while carrying a `github:` reference. The row rides in the txn of the
//! transition, so no landing path can reach the state without it; the drain deletes it once the
//! issue is settled. Every write is idempotent on the bead id.

use serde_json::{json, Value};
use state_store::{Backend, Compare, Error, Object, Request, Result, Txn};

use super::{sub_request, Store, MERGE_ATTEMPTS};
use crate::control::keys;
use crate::control::records::{decode, Audit, LifecycleRecord};

/// The reference prefix of a bead that mirrors a GitHub issue.
pub const GITHUB_REF: &str = "github:";

const OUTCOMES: [&str; 3] = ["LANDED", "SUPERSEDED", "DROPPED"];

/// Whether `a` is a bead's first entry into a state whose issue needs settling.
fn enters_outcome(a: &Audit) -> bool {
    a.applied && a.machine == "bead" && OUTCOMES.contains(&a.to_state.as_str()) && !is_terminal_name(&a.from_state)
}

fn is_terminal_name(state: &str) -> bool {
    matches!(state, "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE")
}

impl<B: Backend> Store<B> {
    /// `t` with a pending closeout put for each applied audit that carries a GitHub bead into an
    /// outcome state. A reference the content store cannot answer fails the write: a landing
    /// that cannot say whether it owes an issue a closeout is not recorded as done.
    pub(super) fn enqueue_closeouts(&self, mut t: Txn, then: &[Audit]) -> Result<Txn> {
        let Some(refs) = &self.refs else { return Ok(t) };
        for a in then.iter().filter(|a| enters_outcome(a)) {
            let id = &a.lc_key;
            let key = keys::gh_closeout(id);
            if self.contract().get(&key)?.is_some() {
                continue;
            }
            let reference = refs(id).map_err(|e| Error::Backend(format!("{id}: external reference unreadable: {e}")))?;
            let Some(reference) = reference.filter(|r| r.starts_with(GITHUB_REF)) else { continue };
            let tip = match self.contract().get(&keys::lifecycle(id))? {
                Some(item) => decode::<LifecycleRecord>(&keys::lifecycle(id), &item.value)?.row.tip.unwrap_or_default(),
                None => String::new(),
            };
            let row = json!({
                "external_ref": reference,
                "outcome_state": a.to_state,
                "sha": tip,
                "ask_id": null,
                "enqueued_at": a.at,
                "attempts": 0,
            });
            t = t.put(&key, Object::new(row));
        }
        Ok(t)
    }

    /// Every pending closeout, `(bead id, row)`, in bead order.
    pub fn gh_closeouts(&self) -> Result<Vec<(String, Value)>> {
        let (items, _) = self.contract().list(&[keys::GH_CLOSEOUTS], &state_store::Selector::Everything)?;
        Ok(items
            .into_iter()
            .filter_map(|i| Some((keys::id_of(keys::GH_CLOSEOUTS, &i.key)?.to_string(), i.value.data)))
            .collect())
    }

    pub fn gh_closeout(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.contract().get(&keys::gh_closeout(id))?.map(|i| i.value.data))
    }

    /// Removes the row; an absent row is already done.
    pub fn gh_closeout_done(&self, id: &str, req: &Request) -> Result<()> {
        let key = keys::gh_closeout(id);
        if self.contract().get(&key)?.is_none() {
            return Ok(());
        }
        let t = Txn::new().delete(&key);
        self.contract().txn(&t, &sub_request(req, "gh-closeout-done", Value::Null))?;
        Ok(())
    }

    /// Records the ask the drain opened for the bead; `false` when no row is pending.
    pub fn gh_closeout_asked(&self, id: &str, ask_id: &str, req: &Request) -> Result<bool> {
        self.edit_closeout(id, req, "asked", |row| row["ask_id"] = Value::String(ask_id.to_string()))
    }

    /// Counts a failed attempt; `false` when no row is pending.
    pub fn gh_closeout_failed(&self, id: &str, req: &Request) -> Result<bool> {
        self.edit_closeout(id, req, "failed", |row| {
            row["attempts"] = Value::from(row["attempts"].as_u64().unwrap_or(0) + 1);
        })
    }

    fn edit_closeout(&self, id: &str, req: &Request, part: &str, edit: impl Fn(&mut Value)) -> Result<bool> {
        let key = keys::gh_closeout(id);
        for attempt in 0..MERGE_ATTEMPTS {
            let Some(item) = self.contract().get(&key)? else { return Ok(false) };
            let mut row = item.value.data.clone();
            edit(&mut row);
            let t = Txn::new()
                .when(Compare::ModRev(key.clone(), Some(item.mod_rev)))
                .put(&key, Object { metadata: item.value.metadata.clone(), data: row });
            if self.contract().txn(&t, &sub_request(req, &format!("gh-closeout-{part}/{attempt}"), Value::Null))?.succeeded {
                return Ok(true);
            }
        }
        Err(Error::Backend(format!("{key}: {part} lost {MERGE_ATTEMPTS} races in a row")))
    }
}
