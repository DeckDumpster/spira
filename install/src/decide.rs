//! The per-unit apply decision (`systemd/install.sh`'s `_unit_action`, in Rust) — extracted
//! so it is a pure function over six booleans, tested without a rendered `DEST` tree or a
//! recording `systemctl` (docs/test-plan/instance-lifecycle.md, UC-instance-lifecycle-22).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A mask (symlink to `/dev/null`) — never touched.
    Masked,
    /// `ctrl.sh` reports this unit's subject suspended.
    Suspended,
    /// The operator disabled it by hand; left as-is.
    OperatorDisabled,
    /// World is halted: enable but do not start.
    Enable,
    /// Unchanged and already active: nothing to do.
    Skip,
    /// Changed and already active: drain (if a running oneshot), then restart.
    Restart,
    /// New, or changed and not active: enable and start now.
    EnableNow,
}

/// The six inputs `_unit_action` takes, named instead of positional. ORDER IS THE CONTRACT
/// (systemd/install.sh's own comment, preserved): a mask outranks everything, suspended
/// outranks operator-disabled and halted, so neither a stale control-plane read nor a halt
/// can second-guess an operator's explicit mask, and `ctrl.sh` stays the one authority for
/// "why is this not running".
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    pub changed: bool,
    pub masked: bool,
    pub suspended: bool,
    pub disabled: bool,
    pub halted: bool,
    pub active: bool,
}

pub fn decide(s: State) -> Action {
    if s.masked {
        return Action::Masked;
    }
    if s.suspended {
        return Action::Suspended;
    }
    if s.disabled {
        return Action::OperatorDisabled;
    }
    if s.halted {
        return Action::Enable;
    }
    if !s.changed && s.active {
        return Action::Skip;
    }
    if s.changed && s.active {
        return Action::Restart;
    }
    Action::EnableNow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(changed: bool, masked: bool, suspended: bool, disabled: bool, halted: bool, active: bool) -> State {
        State { changed, masked, suspended, disabled, halted, active }
    }

    #[test]
    fn a_mask_outranks_every_other_signal() {
        assert_eq!(decide(s(true, true, true, true, true, true)), Action::Masked);
        assert_eq!(decide(s(false, true, false, false, false, false)), Action::Masked);
    }

    #[test]
    fn suspended_outranks_disabled_and_halted() {
        assert_eq!(decide(s(true, false, true, true, true, true)), Action::Suspended);
    }

    #[test]
    fn operator_disabled_outranks_halted() {
        assert_eq!(decide(s(true, false, false, true, true, true)), Action::OperatorDisabled);
    }

    #[test]
    fn a_halted_world_enables_without_starting() {
        assert_eq!(decide(s(true, false, false, false, true, false)), Action::Enable);
        assert_eq!(decide(s(false, false, false, false, true, true)), Action::Enable);
    }

    #[test]
    fn unchanged_and_active_is_skipped() {
        assert_eq!(decide(s(false, false, false, false, false, true)), Action::Skip);
    }

    #[test]
    fn changed_and_active_is_restarted() {
        assert_eq!(decide(s(true, false, false, false, false, true)), Action::Restart);
    }

    #[test]
    fn everything_else_is_enable_now() {
        assert_eq!(decide(s(false, false, false, false, false, false)), Action::EnableNow);
        assert_eq!(decide(s(true, false, false, false, false, false)), Action::EnableNow);
    }
}
