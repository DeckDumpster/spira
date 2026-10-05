use crate::finding::Class;
use serde::Deserialize;
use std::path::Path;

/// Verbs that mutate lifecycle state outright, regardless of any flag.
pub const FORBIDDEN_BARE_VERBS: &[&str] =
    &["close", "reopen", "claim", "reclaim", "unclaim", "supersede"];

/// `update` is otherwise a metadata edit (title, body, assignee, ...) bd still owns; it only
/// becomes a lifecycle write when paired with one of these flags.
pub const FORBIDDEN_UPDATE_FLAGS: &[&str] = &["--status", "--claim"];

/// Subcommands whose output is a lifecycle read, not a write.
pub const READ_VERBS: &[&str] = &["show", "list", "status"];

/// The machine's own credential/DSN surface (spira_lc SQL user, spira-lc Unix user and
/// socket): a reference to any of these outside the lifecycle crate is itself a finding
/// because it means something else learned how to reach the row.
pub const CREDENTIAL_TOKENS: &[&str] = &["spira_lc", "spira-lc"];

/// The lifecycle machine itself, which is the route rather than a caller of it: the
/// `lifecycle` crate that models the record, and in spira-lc only the migration classifier's
/// reader of the legacy ledger (`spira-lc/src/legacy_files.rs`, design §3.8(3): the one
/// place the old shape is read, once, to seed the new store at the cutover deploy; it goes
/// when the legacy files are removed, the release after). This is not an allow-list — there
/// is none (sp-ts2qr, design §3.8(2): "the static analyser with an **empty** allowlist"): no
/// caller outside the machine may reach the ledger, whatever it is, and nothing here names a
/// legacy writer kept on. The rest of spira-lc is held to the same rule as everything else,
/// so the machine cannot grow a second, ledger-backed answer either.
pub fn landstate_path_allowed(rel_path: &str) -> bool {
    rel_path.starts_with("lifecycle/")
        || (rel_path.starts_with("spira-lc/") && rel_path.ends_with("/legacy_files.rs"))
        || rel_path == "spira-lc/legacy_files.rs"
}

/// landing-pass's ledger and landed-oracle subcommands (sp-2c1n0 deletes them): `mark` and
/// `state` wrote and read the landstate ledger, `landed`/`cited-commit` answered "is it
/// landed" from commit subjects, `close-on-land` closed a bead on that answer. Invoking any
/// of them is a landstate-call, so a caller cannot reach the oracle through the binary
/// either once the shell `landed`/`land_mark` functions are gone.
pub const ORACLE_SUBCOMMANDS: &[&str] = &["mark", "state", "landed", "cited-commit", "close-on-land"];

/// The finding classes the landing gate refuses (`lifecycle-guard --gate`, gate.steps): the
/// landstate ledger and the landed oracles, whose removal completes the cutover (sp-2c1n0) —
/// after it, the lifecycle machine is the only route to "is this bead landed", and any
/// reintroduction is a red. Every other class is still reported by a plain run (and counted
/// on the gate's one summary line) and joins this list in the commit that clears it.
pub const GATE_CLASSES: &[Class] = &[Class::LandstateCall, Class::LandstatePath];

/// Cutover-specific and therefore empty until the cutover round actually retires a label or
/// deletes a state path — see this bead's guardrail against touching legacy lifecycle paths.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub retired_labels: Vec<String>,
    pub deleted_state_paths: Vec<String>,
}

impl Rules {
    pub fn load(path: &Path) -> Result<Rules, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading rules file {}: {e}", path.display()))?;
        serde_json::from_str(&text)
            .map_err(|e| format!("parsing rules file {}: {e}", path.display()))
    }
}
