//! A "does not merge" mend bead stands for its target's conflict; when the target ends
//! (LANDED, SUPERSEDED or DROPPED) the mend is retired in the same transition's hook.

use crate::bd;
use lifecycle::bead::{BeadEventKind, BeadState};
use lifecycle::reason::DropReason;

pub trait Mends {
    fn open_mends(&mut self, target: &str) -> Result<Vec<String>, String>;
    fn retire(&mut self, mend: &str, reason: &str) -> Result<(), String>;
}

pub fn needle(target: &str) -> String {
    format!("mend {target}: does not merge onto")
}

pub fn ends_target(to: BeadState) -> bool {
    matches!(to, BeadState::Landed | BeadState::Superseded | BeadState::Dropped)
}

pub fn reason(target: &str, to: BeadState) -> String {
    format!("Bead {target} is now {}; the conflict this mend was filed for ended with it.", to.as_str())
}

pub fn retire_for(target: &str, to: BeadState, mends: &mut dyn Mends) -> Vec<String> {
    if !ends_target(to) {
        return Vec::new();
    }
    let open = match mends.open_mends(target) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("spira-lc: mends of {target} not listed: {}", e.trim());
            return Vec::new();
        }
    };
    open.into_iter()
        .filter(|m| match mends.retire(m, &reason(target, to)) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("spira-lc: mend {m} of {target} not retired: {}", e.trim());
                false
            }
        })
        .collect()
}

pub struct Live<'a>(pub &'a crate::db::Conn);

impl Mends for Live<'_> {
    fn open_mends(&mut self, target: &str) -> Result<Vec<String>, String> {
        let needle = needle(target);
        let out = bd::run(&["list", "--title-contains", &needle, "--status", "open,in_progress,blocked,deferred", "--limit", "0", "--json"])?;
        let start = out.find(['[', '{']).ok_or("bd list: no JSON")?;
        let v: serde_json::Value = serde_json::from_str(&out[start..]).map_err(|e| format!("bd list --json: {e}"))?;
        let rows = match v {
            serde_json::Value::Array(a) => a,
            o => vec![o],
        };
        Ok(rows
            .iter()
            .filter(|r| r.get("title").and_then(|t| t.as_str()).is_some_and(|t| t.starts_with(&needle)))
            .filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(str::to_string))
            .collect())
    }
    fn retire(&mut self, mend: &str, reason: &str) -> Result<(), String> {
        let (rc, msg) = crate::apply_bead_event(self.0, mend, "mend-retire", BeadEventKind::Drop { reason: DropReason::Unwanted });
        if rc != 0 {
            return Err(format!("lifecycle drop exited {rc}: {msg}"));
        }
        crate::callers::Bd::close(&mut bd::LiveBd, mend, reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        open: Vec<String>,
        retired: Vec<(String, String)>,
    }
    impl Mends for Fake {
        fn open_mends(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(self.open.clone())
        }
        fn retire(&mut self, m: &str, why: &str) -> Result<(), String> {
            self.retired.push((m.into(), why.into()));
            self.open.retain(|o| o != m);
            Ok(())
        }
    }

    #[test]
    fn a_target_that_lands_supersedes_or_drops_retires_its_open_mends_naming_the_state() {
        for to in [BeadState::Landed, BeadState::Superseded, BeadState::Dropped] {
            let mut f = Fake { open: vec!["sp-m1".into()], ..Default::default() };
            assert_eq!(retire_for("sp-t", to, &mut f), ["sp-m1"]);
            assert!(f.open.is_empty() && f.retired[0].1.contains(to.as_str()), "{:?}", f.retired);
        }
    }

    #[test]
    fn a_target_still_in_flight_keeps_its_mend() {
        for to in [BeadState::Ready, BeadState::Working, BeadState::Submitted, BeadState::Certified, BeadState::InDelivery, BeadState::Rework] {
            let mut f = Fake { open: vec!["sp-m1".into()], ..Default::default() };
            assert!(retire_for("sp-t", to, &mut f).is_empty());
            assert_eq!(f.open, ["sp-m1"], "{to:?}");
        }
    }

    #[test]
    fn the_title_needle_names_one_target_exactly() {
        assert!(needle("sp-t").ends_with("onto") && needle("sp-t").starts_with("mend sp-t:"));
        assert!(!"mend sp-t2: does not merge onto main".starts_with(&needle("sp-t")));
    }
}
