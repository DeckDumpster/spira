//! spira-claim's public surface for OTHER crates to link against in-process: just the one
//! constant cockpit-collect used to keep its own byte-identical copy of (wave 4.25,
//! sp-obhv6, "READY_ARGS as one const"). Everything else — the bead-attempt accounting,
//! CHECK 4 poison decision, epic-first claim selection and `unpoison` — is CLI-internal;
//! see DESIGN.md and `src/main.rs`. A caller that needs more than this constant shells into
//! the `spira-claim` binary by bare name, same as `bdq`/`spira-lc`/`spira-config`.

/// `READY_ARGS`'s fixed prefix (lib.sh's own array literal) — "the one definition of a bead
/// an aeon can take" (lib.sh:434). `cockpit-collect`'s queue probe used to keep its own copy
/// of exactly these five tokens (`probes/queue.rs`); importing this one is the fix.
pub const READY_ARGS_BASE: &[&str] = &["ready", "--limit", "0", "--exclude-type", "epic,event", "-u"];
