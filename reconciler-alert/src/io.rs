//! The alert path's IO seam: persists which streak each invariant last alerted for (one
//! JSON file, keyed by invariant), asks whether the Concierge is running, and hands mail to
//! `mail`. Every function here does exactly one read, one write or one subprocess call —
//! the dedup and classification logic lives in `reconciler_engine::alert`, not here.

use std::process::Stdio;

// Persistence for which streak each invariant last alerted for lives in reconciler-engine::io
// now — reconciler-flow shares the exact same file format instead of a second copy of it.
pub use reconciler_engine::io::{load_alerted, save_alerted};

/// True if tmux has a session of this name — false for "not running" AND for "cannot tell"
/// (tmux missing, or the call itself failed): either way there is nobody to wake, so the
/// alert must fall back to operator mail rather than being typed into a session that will
/// never read it.
pub fn session_is_running(tmux: &str, session: &str) -> bool {
    spira_config::bounded::bounded(tmux)
        .args(["has-session", "-t", &format!("={session}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub use reconciler_engine::mail::send as mail_send;
