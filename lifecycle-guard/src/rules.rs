use crate::finding::Class;
use serde::Deserialize;
use std::path::Path;

/// Verbs that mutate lifecycle state outright, regardless of any flag.
pub const FORBIDDEN_BARE_VERBS: &[&str] =
    &["close", "reopen", "claim", "reclaim", "unclaim", "supersede"];

/// `update` is otherwise a metadata edit (title, body, assignee, ...) bd still owns; it only
/// becomes a lifecycle write when paired with one of these flags.
pub const FORBIDDEN_UPDATE_FLAGS: &[&str] = &["--status", "--claim"];

/// `ready` is a read, except `ready --claim`, which claims the bead it picks (sp-voip5).
pub const FORBIDDEN_READY_FLAGS: &[&str] = &["--claim"];

/// bd's global flags that take a separate value word (`bd --help`): `-C <dir>`, `--db <path>`,
/// … . Every other leading word that starts with `-` is a boolean global (`--json`, `-q`,
/// `--readonly`, …) or carries its value joined (`--db=…`, `-Cdir`), and is one word. The verb
/// is the first word after these, so `bd -C "$DB" close …` is judged as `close` (sp-voip5).
pub const BD_GLOBAL_VALUE_FLAGS: &[&str] = &[
    "-C",
    "--directory",
    "--db",
    "--database",
    "--actor",
    "--dolt-auto-commit",
    "--mem-profile",
];

/// Subcommands whose output is a lifecycle read, not a write.
pub const READ_VERBS: &[&str] = &["show", "list", "status"];

/// The machine's own credential/DSN surface (spira_lc SQL user, spira-lc Unix user and
/// socket): a reference to any of these outside the lifecycle crate is itself a finding
/// because it means something else learned how to reach the row.
pub const CREDENTIAL_TOKENS: &[&str] = &["spira_lc", "spira-lc"];

/// landing-pass's retired ledger and landed-oracle subcommands: `mark` and
/// `state` wrote and read the landstate ledger, `landed`/`cited-commit` answered "is it
/// landed" from commit subjects, `close-on-land` closed a bead on that answer. Invoking any
/// of them is a landstate-call, so a caller cannot reach the oracle through the binary.
pub const ORACLE_SUBCOMMANDS: &[&str] = &["mark", "state", "landed", "cited-commit", "close-on-land"];

/// The finding classes the landing gate refuses (`lifecycle-guard --gate`, gate.steps): the
/// landstate ledger and the landed oracles — the lifecycle machine is the only route to
/// "is this bead landed", and any reintroduction is a red — and every way around the machine to a bead's state (sp-hyo5e):
/// a bd/bdq lifecycle write, directly or through a wrapper, a verb the analyser cannot
/// resolve, and a bd status read feeding a decision. Each was cleared by routing it through
/// spira-lc (`unclaim`, `close-epic`, `show`) before it joined this list.
///
/// The Rust bd-status read (`bd-status-read`, sp-mve9i, design §3.4: bd holds content,
/// spira-lc holds state; `bd_status.rs`) joined this list in the commit that cleared its last
/// findings — spira-claim's select, epic and holder paths (sp-mve9i) and the incident
/// family's dedup (sp-jgjvh). Every Rust decision on a work bead reads
/// `spira_config::lc_state`; a bead that is not a work bead names its kind through
/// `spira_config::nonwork`; the rule's named exceptions (the rowless controls) are argued
/// in DESIGN.md. The rest — the credential rule (over-broad: it
/// flags every `spira-lc` CLI call and comment) and bd named in a brief — are still reported
/// by a plain run, counted on the gate's one summary line, and join this list in the commit
/// that clears them.
pub const GATE_CLASSES: &[Class] = &[
    Class::LandstateCall,
    Class::LandstatePath,
    Class::DirectWrite,
    Class::WrapperWrite,
    Class::DynamicVerb,
    Class::LifecycleRead,
    Class::BdStatusRead,
    // sp-3fue0j: a bd close from Rust, joined in the commit that routed every caller through
    // `spira-lc close`.
    Class::BdCloseRust,
    // sp-psztcc: a hold kept as a bd label, joined in the commit that moved every claim
    // predicate and writer onto the row's hold.
    Class::HoldLabel,
    // sp-swh8b8: a bd reopen (or claim-clearing assign) from Rust, joined in the commit that
    // routed every caller through `spira-lc reopen`.
    Class::BdReopenRust,
];

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
