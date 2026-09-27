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

/// Where a call into the landstate ledger, or a direct read of its files, is still the
/// sanctioned path rather than a finding: the lifecycle crate that models the ledger, and
/// spira-lc, the ledger's one reader/writer — which also holds the migration classifier
/// (`spira-lc/src/classify_cmd.rs`, the one place the legacy ledger shape is read to seed the
/// new store). Everything else is a caller sp-wenrl's cutover has yet to move.
pub const LANDSTATE_ALLOWED_PREFIXES: &[&str] = &["lifecycle/", "spira-lc/"];

pub fn landstate_path_allowed(rel_path: &str) -> bool {
    LANDSTATE_ALLOWED_PREFIXES.iter().any(|p| rel_path.starts_with(p))
}

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
