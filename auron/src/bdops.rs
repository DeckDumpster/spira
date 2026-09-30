//! The bead-store operations auron needs, above the raw seam: list-by-label, create,
//! update, close, label add/remove, reopen. A trait so the reconciliation logic
//! (`alerts.rs`) can be driven by an in-memory fake in tests, exactly aeon's own `Bd` port.

use serde::Deserialize;
use serde_json::Value;

use crate::seam::{self, Seam};

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct BeadRow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub labels: Vec<String>,
}

/// Whether a failed call was a timeout (lock contention — "saturated", not "down") or an
/// ordinary failure. auron.sh's own distinction: exit 124 from `timeout` means another
/// dolt process held the lock longer than `BD_TIMEOUT`; anything else means the database
/// is actually unreachable or the write was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Saturated,
    Other,
}

pub type R<T> = Result<T, Failure>;

pub trait BdOps: Send + Sync {
    fn list_by_label(&self, label: &str) -> R<Vec<BeadRow>>;
    fn create(&self, title: &str, labels: &str, body: &str) -> R<String>;
    fn update_title_body(&self, id: &str, title: &str, body: &str) -> R<()>;
    fn update_body(&self, id: &str, body: &str) -> R<()>;
    fn close(&self, id: &str, reason: &str) -> R<()>;
    fn label_add(&self, id: &str, label: &str) -> R<()>;
    fn label_remove(&self, id: &str, label: &str) -> R<()>;
    fn reopen(&self, id: &str, cause: &str, note: &str) -> R<()>;
}

pub struct SeamBdOps<'a> {
    pub seam: &'a dyn Seam,
}

fn fail_of(code: i32) -> Failure {
    if code == 124 {
        Failure::Saturated
    } else {
        Failure::Other
    }
}

/// `bd create --json`'s id, from the FIRST `{` in stdout — never from a grep over human
/// output (law-never-derive-an-id-from-output): `bd create` prepends an advisory that can
/// itself contain id-shaped text.
fn id_from_create_output(text: &str) -> Option<String> {
    let i = text.find('{')?;
    let v: Value = serde_json::from_str(&text[i..]).ok()?;
    v.get("id")?.as_str().map(|s| s.to_string())
}

impl BdOps for SeamBdOps<'_> {
    fn list_by_label(&self, label: &str) -> R<Vec<BeadRow>> {
        let o = seam::bdq(self.seam, &["list", "--all", "--limit", "0", "--label", label, "--json"]);
        if !o.success() {
            return Err(fail_of(o.code));
        }
        let text = crate::util::json_only(&o.stdout);
        if text.trim().is_empty() {
            return Err(Failure::Other);
        }
        let v: Value = serde_json::from_str(&text).map_err(|_| Failure::Other)?;
        let items = match v {
            Value::Array(a) => a,
            other => vec![other],
        };
        Ok(items.into_iter().filter_map(|i| serde_json::from_value(i).ok()).collect())
    }

    fn create(&self, title: &str, labels: &str, body: &str) -> R<String> {
        let o = seam::bdq(self.seam, &["create", "--title", title, "--type", "event", "-p", "1", "--labels", labels, "--body", body, "--json"]);
        if !o.success() {
            return Err(fail_of(o.code));
        }
        id_from_create_output(&o.stdout).ok_or(Failure::Other)
    }

    fn update_title_body(&self, id: &str, title: &str, body: &str) -> R<()> {
        let o = seam::bdq(self.seam, &["update", id, "--title", title, "--body", body]);
        if o.success() {
            Ok(())
        } else {
            Err(fail_of(o.code))
        }
    }

    fn update_body(&self, id: &str, body: &str) -> R<()> {
        let o = seam::bdq(self.seam, &["update", id, "--body", body]);
        if o.success() {
            Ok(())
        } else {
            Err(fail_of(o.code))
        }
    }

    fn close(&self, id: &str, reason: &str) -> R<()> {
        let o = seam::bdq(self.seam, &["close", id, "--reason", reason]);
        if o.success() {
            Ok(())
        } else {
            Err(fail_of(o.code))
        }
    }

    fn label_add(&self, id: &str, label: &str) -> R<()> {
        let o = seam::bdq(self.seam, &["label", "add", id, label]);
        if o.success() {
            Ok(())
        } else {
            Err(fail_of(o.code))
        }
    }

    fn label_remove(&self, id: &str, label: &str) -> R<()> {
        let o = seam::bdq(self.seam, &["label", "remove", id, label]);
        if o.success() {
            Ok(())
        } else {
            Err(fail_of(o.code))
        }
    }

    fn reopen(&self, id: &str, cause: &str, note: &str) -> R<()> {
        let o = seam::bead_reopen(self.seam, id, cause, note);
        if o.success() {
            Ok(())
        } else {
            Err(fail_of(o.code))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_from_create_output_skips_bds_own_advisory() {
        let out = "note: this repo defaults to spira,plan\n{\"id\":\"sp-x1\",\"title\":\"t\"}\n";
        assert_eq!(id_from_create_output(out), Some("sp-x1".to_string()));
    }

    #[test]
    fn id_from_create_output_none_when_no_json() {
        assert_eq!(id_from_create_output("just an error\n"), None);
    }

    #[test]
    fn fail_of_124_is_saturated() {
        assert_eq!(fail_of(124), Failure::Saturated);
        assert_eq!(fail_of(1), Failure::Other);
    }
}
