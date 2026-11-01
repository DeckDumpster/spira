//! The one blocking rule: whether a planning-plane edge holds its dependent back, and the
//! projection of a blocker's lifecycle record that a claim compares. Every reader that asks
//! "does this edge block?" asks [`blocks`]; none keeps its own list of edge types.

use lifecycle::bead::BeadState;
use serde::{Deserialize, Serialize};

/// The dependency types that hold the dependent until the target finishes. bd stores its
/// `blocked-by` and `depends-on` aliases as `blocks`; a row from another reader may carry them.
pub const BLOCKING_TYPES: [&str; 3] = ["blocks", "blocked-by", "depends-on"];

/// One edge from a dependent to the bead it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge<'a> {
    /// A dependency of the given bd type.
    Dep(&'a str),
    /// The dependent's parent. `parent` is the parent's lifecycle record as `(state, has_tip)`,
    /// `None` when it has none; `split` is whether the dependent carries the split label.
    Parent {
        parent: Option<(BeadState, bool)>,
        split: bool,
    },
}

/// Whether `edge` holds its dependent back. A parent does not when it is a container with no
/// record, or a coordination bead (READY, no tip) that itself waits on its children — unless the
/// child was split from it, when the parent's own work comes first.
pub fn blocks(edge: Edge) -> bool {
    match edge {
        Edge::Dep(kind) => BLOCKING_TYPES.contains(&kind),
        Edge::Parent { parent: None, .. } => false,
        Edge::Parent {
            parent: Some((BeadState::Ready, false)),
            split: false,
        } => false,
        Edge::Parent { .. } => true,
    }
}

/// How a blocker stands toward the beads that wait on it, as the claim rule acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateClass {
    /// READY with no tip: the one unfinished state in which a parent does not hold its child.
    Idle,
    Unfinished,
    /// Certified work a dependent of the same repo may stack on.
    Stackable,
    Finished,
}

/// The blocker-facing projection of a lifecycle record. It changes only when the record's
/// class does, or its tip while it is stackable, so a claim that compares it conflicts with a
/// blocker that regressed and with no other write to that blocker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub class: GateClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
}

pub fn gate_class(state: BeadState, has_tip: bool) -> GateClass {
    match state {
        BeadState::Ready if !has_tip => GateClass::Idle,
        BeadState::Certified | BeadState::InDelivery => GateClass::Stackable,
        s if s.is_terminal() => GateClass::Finished,
        _ => GateClass::Unfinished,
    }
}

/// The gate of a record in `state` at `tip`. The tip is kept only while it is stackable: a
/// dependent stacks on it then, and at no other time does a blocker's tip decide a claim.
pub fn gate(state: BeadState, tip: Option<&str>) -> Gate {
    let class = gate_class(state, tip.is_some());
    Gate {
        class,
        tip: (class == GateClass::Stackable)
            .then(|| tip.map(str::to_string))
            .flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every type `bd dep add --type` accepts, its two aliases, and the empty type a row may
    /// carry: only the blocking three block.
    #[test]
    fn blocks_counts_the_blocking_types_and_no_other_type_bd_emits() {
        let table = [
            ("blocks", true),
            ("blocked-by", true),
            ("depends-on", true),
            ("parent-child", false),
            ("tracks", false),
            ("related", false),
            ("relates-to", false),
            ("discovered-from", false),
            ("until", false),
            ("caused-by", false),
            ("validates", false),
            ("supersedes", false),
            ("", false),
        ];
        for (kind, want) in table {
            assert_eq!(blocks(Edge::Dep(kind)), want, "{kind:?}");
        }
    }

    #[test]
    fn a_parent_blocks_unless_it_is_a_container_or_a_coordination_bead_of_an_unsplit_child() {
        let parent = |p, split| blocks(Edge::Parent { parent: p, split });
        assert!(!parent(None, false));
        assert!(!parent(None, true));
        assert!(!parent(Some((BeadState::Ready, false)), false));
        assert!(parent(Some((BeadState::Ready, false)), true));
        assert!(parent(Some((BeadState::Ready, true)), false));
        assert!(parent(Some((BeadState::Working, false)), false));
        assert!(parent(Some((BeadState::Landed, true)), false));
    }

    #[test]
    fn the_gate_moves_with_the_class_and_a_stackable_tip_and_nothing_else() {
        let tip = Some("t1");
        assert_eq!(gate(BeadState::Ready, tip), gate(BeadState::Working, None));
        assert_eq!(
            gate(BeadState::Submitted, tip),
            gate(BeadState::Rework, None)
        );
        assert_eq!(gate(BeadState::Open, None).class, GateClass::Unfinished);
        assert_eq!(gate(BeadState::Ready, None).class, GateClass::Idle);
        assert_ne!(
            gate(BeadState::Ready, None),
            gate(BeadState::Working, None),
            "a coordination parent claimed holds its child"
        );
        assert_eq!(
            gate(BeadState::Certified, tip),
            gate(BeadState::InDelivery, tip)
        );
        assert_ne!(
            gate(BeadState::Certified, tip),
            gate(BeadState::Certified, Some("t2"))
        );
        assert_ne!(
            gate(BeadState::Certified, tip),
            gate(BeadState::Rework, tip)
        );
        for done in [
            BeadState::Landed,
            BeadState::Done,
            BeadState::Superseded,
            BeadState::Dropped,
        ] {
            assert_eq!(
                gate(done, tip),
                Gate {
                    class: GateClass::Finished,
                    tip: None
                }
            );
        }
        assert_eq!(
            serde_json::to_value(gate(BeadState::Certified, tip)).unwrap(),
            serde_json::json!({ "class": "stackable", "tip": "t1" })
        );
    }
}
