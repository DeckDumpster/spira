//! The knobs the section's two actions move, under the controller's own
//! `knobs/RoundController/<repo>/`: the repo's stack depth, and the holds that keep one member at
//! one tip out of every cut. Only an applied intent moves either; the cut reads both.

use std::collections::BTreeSet;

use serde::Deserialize;
use spira_desired_state::sections::round_controller::NAME;
use state_store::Selector;

use crate::ports::{Member, Ports};
use crate::refusal::Refusal;

pub fn prefix(repo: &str) -> String {
    format!("knobs/{NAME}/{repo}/")
}

pub fn concurrency_key(repo: &str) -> String {
    format!("{}concurrency", prefix(repo))
}

pub fn holds_prefix(repo: &str) -> String {
    format!("{}hold/", prefix(repo))
}

pub fn hold_key(repo: &str, m: &Member) -> String {
    format!("{}{}/{}", holds_prefix(repo), m.id, m.tip)
}

/// The member at the tip a hold key names, `None` for any other key.
pub fn member_of_hold(repo: &str, key: &str) -> Option<Member> {
    let (id, tip) = key.strip_prefix(&holds_prefix(repo))?.split_once('/')?;
    let ok = [id, tip].iter().all(|p| !p.is_empty() && !p.contains('/'));
    ok.then(|| Member {
        id: id.into(),
        tip: tip.into(),
    })
}

/// A hold's value: who lifts it (law-every-hold-names-who-lifts-it).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Hold {
    pub lifted_by: String,
}

impl Hold {
    pub fn of(v: &serde_json::Value) -> Option<Hold> {
        serde_json::from_value::<Hold>(v.clone())
            .ok()
            .filter(|h| !h.lifted_by.trim().is_empty())
    }
}

/// The stack depth the concurrency knob holds; `None` while it has never been set.
pub fn concurrency(p: &Ports, repo: &str) -> Result<Option<usize>, Refusal> {
    let key = concurrency_key(repo);
    let Some(item) = p
        .store
        .get(&key)
        .map_err(|e| Refusal::unreadable(&key, e))?
    else {
        return Ok(None);
    };
    item.value
        .data
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .map(Some)
        .ok_or_else(|| {
            Refusal::unreadable(&key, format!("{} is not a stack depth", item.value.data))
        })
}

/// Every (member, tip) a hold keeps out of the cut.
pub fn held(p: &Ports, repo: &str) -> Result<BTreeSet<Member>, Refusal> {
    let prefix = holds_prefix(repo);
    let (items, _) = p
        .store
        .list(&[prefix.as_str()], &Selector::Everything)
        .map_err(|e| Refusal::unreadable(&prefix, e))?;
    Ok(items
        .iter()
        .filter(|i| Hold::of(&i.value.data).is_some())
        .filter_map(|i| member_of_hold(repo, &i.key))
        .collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::sim::bench::Bench;

    fn m(id: &str, tip: &str) -> Member {
        Member {
            id: id.into(),
            tip: tip.into(),
        }
    }

    #[test]
    fn a_hold_key_names_one_member_at_one_tip_and_nothing_else_reads_as_one() {
        let k = hold_key("lane", &m("sp-a", "a1"));
        assert_eq!(k, "knobs/RoundController/lane/hold/sp-a/a1");
        assert_eq!(member_of_hold("lane", &k), Some(m("sp-a", "a1")));
        for bad in [
            "knobs/RoundController/lane/hold/sp-a",
            "knobs/RoundController/lane/hold/sp-a/a1/x",
            "knobs/RoundController/lane/hold//a1",
            "knobs/RoundController/other/hold/sp-a/a1",
            "knobs/RoundController/lane/concurrency",
        ] {
            assert_eq!(member_of_hold("lane", bad), None, "{bad}");
        }
        assert_eq!(
            concurrency_key("lane"),
            "knobs/RoundController/lane/concurrency"
        );
    }

    #[test]
    fn the_depth_is_the_knob_while_set_and_a_value_that_is_no_depth_is_unreadable() {
        let b = Bench::new();
        let p = b.ports();
        assert_eq!(concurrency(&p, "lane").unwrap(), None);
        b.seed(&concurrency_key("lane"), json!(3));
        assert_eq!(concurrency(&p, "lane").unwrap(), Some(3));
        for bad in [json!(0), json!("3"), json!(2.5)] {
            let b = Bench::new();
            b.seed(&concurrency_key("lane"), bad.clone());
            let e = concurrency(&b.ports(), "lane").unwrap_err();
            assert_eq!(e.word(), "unreadable", "{bad}");
        }
        let u = Bench::unserved();
        assert_eq!(
            concurrency(&u.ports(), "lane").unwrap_err().word(),
            "unreadable"
        );
    }

    #[test]
    fn only_a_hold_that_names_who_lifts_it_keeps_its_member_out() {
        let b = Bench::new();
        b.seed(
            &hold_key("lane", &m("sp-a", "a1")),
            json!({ "lifted_by": "reconciler" }),
        );
        b.seed(
            &hold_key("lane", &m("sp-b", "b1")),
            json!({ "lifted_by": " " }),
        );
        b.seed(&hold_key("lane", &m("sp-c", "c1")), json!(true));
        b.seed(
            "knobs/RoundController/lane/hold/stray",
            json!({ "lifted_by": "x" }),
        );
        let held = held(&b.ports(), "lane").unwrap();
        assert_eq!(held.into_iter().collect::<Vec<_>>(), vec![m("sp-a", "a1")]);
        assert_eq!(held_unserved(), "unreadable");
    }

    fn held_unserved() -> &'static str {
        let u = Bench::unserved();
        let p = u.ports();
        held(&p, "lane").unwrap_err().word()
    }
}
