//! spira-claim's public surface for OTHER crates to link against in-process: the
//! `READY_ARGS_BASE` constant (wave 4.25, sp-obhv6, "READY_ARGS as one const") and the
//! `verdicts` file the dispatcher publishes, so a reader asks what the dispatcher decided
//! instead of re-deriving it. Everything else — the bead-attempt accounting, CHECK 4 poison
//! decision, epic-first claim selection and `unpoison` — is CLI-internal; see DESIGN.md and
//! `src/main.rs`. A caller that needs more shells into the `spira-claim` binary by bare name,
//! same as `bdq`/`spira-lc`/`spira-config`.

pub mod verdicts;

/// `READY_ARGS`'s fixed prefix (lib.sh's own array literal) — "the one definition of a bead
/// a dwarf can take" (lib.sh:434). `cockpit-collect`'s queue probe used to keep its own copy
/// of exactly these five tokens (`probes/queue.rs`); importing this one is the fix.
pub const READY_ARGS_BASE: &[&str] = &["ready", "--limit", "0", "--exclude-type", "epic,event", "-u"];
