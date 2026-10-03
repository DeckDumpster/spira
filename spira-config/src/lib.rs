//! spira-config — the typed schema behind `spira.toml`.
//!
//! One document, three kinds of table: `[spira]` (host-wide keys), `[repo.<name>]` (a
//! checkout the harness may work in) and `[persona.<name>]` (an aeon's fayth, cut down to
//! the fields the harness itself reads rather than a persona's prose). Every struct here
//! carries `deny_unknown_fields`, so a typo is a hard error instead of a setting nobody is
//! reading — the same failure mode `spira.conf`'s own allowlist exists to catch, now
//! enforced by the type system instead of a hand-maintained string.
//!
//! `[spira]` covers the keys a real install actually sets, not every key `conf.sh` allows —
//! conf.sh's own allowlist runs past 200, most of them derived defaults nothing overrides.
//! Widening this table is scoped to the cutover bead, where each consumer's own reads say
//! which of the rest still need a home.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod admission;
pub mod build;
pub mod chamber;
pub mod containment;
pub mod convert;
pub mod deps;
pub mod env_bootstrap;
pub mod eval;
pub mod legacy_map;
pub mod locate;
pub mod registry;
pub mod release_env;
pub mod release_skew;
pub mod repos;
pub mod resolve;
pub mod room;
pub mod scratch;
pub mod unit;
pub mod writeback;

pub use locate::LocateOutcome;

/// THE ONE CRATE-WIDE ENV LOCK (sp-dh4fv). `cargo test`'s threads share this binary's
/// process environment — `HOME`, `XDG_CONFIG_HOME`, `SPIRA_TOML`, `SPIRA_CONF`,
/// `SPIRA_REPO`, and anything else a test sets or clears. Every test in this crate
/// (including its `tests/` integration binaries, which each get their OWN copy of this
/// static — see their own lock below) that reads `env::set_var`/`env::remove_var` must
/// take this lock for its whole body, no exceptions. Before sp-dh4fv, `lib.rs` and
/// `locate.rs` each had their own private `ENV_LOCK`, which serialized tests against others
/// in the same module but not against the other module's tests — two locks that never
/// contend is not a lock at all. `cargo test -p spira-config` (default, 32-wide thread
/// pool) flaked on exactly that race; `--test-threads=1` hid it by accident. A function
/// that only *reads* env outside of a test doesn't need this — only a test that mutates
/// the process env does. Prefer not needing it at all: give the function a pure variant
/// that takes the values as an argument (`chamber::persona_model_from_doc` is the pattern)
/// and test that instead of the env-reading wrapper.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The one filename this schema's document is ever named on disk — every path-resolution
/// function below builds on this instead of a caller spelling `"spira.toml"` itself.
pub const FILE_NAME: &str = "spira.toml";

/// `dir/spira.toml`, if it is a file — the check a per-checkout caller (`spira-lc`'s
/// migration detector) uses instead of naming the file itself.
pub fn find_under(dir: &Path) -> Option<PathBuf> {
    let p = dir.join(FILE_NAME);
    p.is_file().then_some(p)
}

/// `dir/spira.toml`, named but not necessarily present — for a writer at a root other than
/// this process's own (`install`'s cross-checkout instance seed, sp-31dm0), which must
/// resolve the path without naming it itself (config-fence: only spira-config names
/// `spira.toml` or `repo-map`).
pub fn toml_path_at(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// The conventional `repo-map` filename, checked at `conf_dir` (if given) then as
/// `<home>/repo-map.example` — the same lookup conf.sh's `_spira_repo_map_candidate` makes
/// for a root other than this process's own `SPIRA_HOME`. `None` when neither exists.
pub fn repo_map_candidate(conf_dir: Option<&Path>, home: &Path) -> Option<PathBuf> {
    const REPO_MAP: &str = "repo-map";
    conf_dir
        .map(|d| d.join(REPO_MAP))
        .into_iter()
        .chain(std::iter::once(home.join(format!("{REPO_MAP}.example"))))
        .find(|c| c.is_file())
}

/// `spira-config convert --conf <conf> --home <home> --out <out> [--repo-map <repo_map>]
/// [--fayth <f>]...`, built (not run) — for a writer at a root other than this process's
/// own (`install`'s cross-checkout instance seed, sp-31dm0), which must not name this
/// binary's own flags itself (config-fence: only spira-config names `repo-map`).
pub fn convert_command(conf: &Path, home: &Path, out: &Path, repo_map: Option<&Path>, fayth: &[PathBuf]) -> std::process::Command {
    let mut cmd = std::process::Command::new("spira-config");
    cmd.arg("convert").arg("--conf").arg(conf).arg("--home").arg(home).arg("--out").arg(out);
    if let Some(rm) = repo_map {
        cmd.arg("--repo-map").arg(rm);
    }
    for f in fayth {
        cmd.arg("--fayth").arg(f);
    }
    cmd
}

/// The search a host-wide reader with no explicit path resolves one from: `explicit` if
/// given (a caller's own `--config`/`$SPIRA_TOML` precedence), else `$SPIRA_TOML` (exclusive —
/// a pinned path that is not a file means "no config", not "keep looking"), else
/// `$XDG_CONFIG_HOME/spira/spira.toml` (`$HOME/.config` when `XDG_CONFIG_HOME` is unset), else
/// `/etc/spira/spira.toml` — first of these that exists. The one search order every host-wide
/// reader (`queue-watch`) shares, so two daemons can never disagree about which file is in
/// force on the same host, and the same search `conf.sh`'s `spira_toml_file` runs (sp-hconl;
/// **no `$SPIRA_REPO` tier** — removed from bash by sp-9hwim, "the running system must not
/// read [from beside the checkout] at all"; this function still offered one until this bead).
///
/// `None` collapses every reason nothing resolved (truly absent, or only a legacy
/// `spira.conf`) into the one answer every existing caller already treats as "use derived
/// defaults" — see [`locate::locate`] for the richer [`LocateOutcome`] a diagnostic needs.
pub fn discover(explicit: Option<PathBuf>) -> Option<PathBuf> {
    locate::locate(explicit).found()
}

/// Reads and [`validate`]s the document at `path` — the one place a caller turns a resolved
/// path into a [`SpiraToml`], instead of pairing its own `fs::read_to_string` with `validate`.
pub fn load(path: &Path) -> Result<SpiraToml, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    validate(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The environment variable that pins `lifecycle_enforce` (a unit's `Environment=`, a
/// fixture, or conf.sh, which exports it with a default of `0`).
/// The one variable a launcher builds PATH from (brain `runtime-is-a-release-2026-09-29`):
/// the root of the release the running system executes — `spira-releases/<sha>`, and until
/// the release deploy lands (sp-gkfg1) the harness checkout's root, which has `bin/` and
/// `spira/` in the same places.
pub const RELEASE_ENV: &str = "SPIRA_RELEASE";

/// The system directories every launcher's PATH ends with (the release's name-clash rule
/// checks these, `release::SYSTEM_DIRS`).
pub const SYSTEM_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The launcher's PATH, set outright — never appended to an inherited one (sp-31gtu):
/// `<release>/bin:<release>/spira:/usr/local/bin:/usr/bin:/bin`.
pub fn release_path(release: &str) -> String {
    format!("{release}/bin:{release}/spira:{SYSTEM_PATH}")
}

/// [`release_path`] of `$SPIRA_RELEASE`, or the refusal naming it: unset (or empty) is a hard
/// failure, never a fallback to an inherited PATH.
pub fn release_path_from_env(value: Option<&str>) -> Result<String, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(r) => Ok(release_path(r.trim_end_matches('/'))),
        None => Err(format!(
            "{RELEASE_ENV} is not set — the launcher sets it to the release the running system executes, and PATH is built from it; there is no fallback"
        )),
    }
}

/// A single colon-separated segment of `tail` (sp-c7b85's fix to sp-31gtu's launcher PATH,
/// which carried the release and the system directories but never the box's own —
/// `spira.path` — so a Rust binary launched directly by a unit, rather than through
/// `conf.sh`, resolved `bd`/`claude`/`dolt`/`gh`/`cargo` by bare name against nothing) that
/// resolves inside a release (any path with a `spira-releases` component) or inside a
/// checkout (a directory that is, or is enclosed by, one tracking `.git`) — the two shapes
/// "ABSOLUTELY ZERO ambiguity about which binaries to use" (runtime-is-a-release) exists to
/// rule out. `None` when the segment is neither.
fn tail_segment_refusal(seg: &str) -> Option<String> {
    let p = Path::new(seg);
    if p.components().any(|c| c.as_os_str() == "spira-releases") {
        return Some(format!(
            "spira.path entry {seg:?} is inside spira-releases — a launcher's PATH tail may name only the box's own tool directories, never a release"
        ));
    }
    let mut cur = Some(p);
    while let Some(c) = cur {
        if c.join(".git").exists() {
            return Some(format!(
                "spira.path entry {seg:?} is inside a checkout ({} tracks .git) — a launcher's PATH tail may name only the box's own tool directories, never a checkout",
                c.display()
            ));
        }
        cur = c.parent();
    }
    None
}

/// Every colon-separated entry of `tail` that [`tail_segment_refusal`] flags, in order — the
/// pure check [`release_path_with_tail`] applies before it will append anything.
pub fn tail_refusals(tail: &str) -> Vec<String> {
    tail.split(':').filter(|s| !s.is_empty()).filter_map(tail_segment_refusal).collect()
}

/// [`release_path`] plus the box's own tool-directory tail (`spira.path`: `~/.local/bin` for
/// `bd`, `dolt`, `gh`, `claude` and `duckdb`, `~/.cargo/bin` for an aeon's `cargo test`),
/// appended after the system directories — never before them, so a bare Spira tool name still
/// means only the release's own copy (sp-c7b85, amending sp-31gtu's rendering, which omitted
/// this tail for every launcher that invokes a Rust binary directly instead of sourcing
/// `conf.sh`, whose own PATH-append already carried it). An empty tail changes nothing.
/// Refuses, naming the offending entry, when [`tail_refusals`] finds one.
pub fn release_path_with_tail(release: &str, tail: &str) -> Result<String, String> {
    let base = release_path(release);
    let tail = tail.trim();
    if tail.is_empty() {
        return Ok(base);
    }
    let refusals = tail_refusals(tail);
    if !refusals.is_empty() {
        return Err(refusals.join("; "));
    }
    Ok(format!("{base}:{tail}"))
}

/// [`release_path_with_tail`] of `$SPIRA_RELEASE`, or the refusal naming it — the tail-aware
/// counterpart to [`release_path_from_env`], for a launcher that resets its environment
/// (`env -i`) and so cannot lean on an inherited PATH for either half.
pub fn release_path_from_env_with_tail(release: Option<&str>, tail: &str) -> Result<String, String> {
    let base = release_path_from_env(release)?;
    let tail = tail.trim();
    if tail.is_empty() {
        return Ok(base);
    }
    let refusals = tail_refusals(tail);
    if !refusals.is_empty() {
        return Err(refusals.join("; "));
    }
    Ok(format!("{base}:{tail}"))
}

pub const LIFECYCLE_ENFORCE_ENV: &str = "SPIRA_LIFECYCLE_ENFORCE";

/// THE lifecycle switch's resolution rule (operator decision 2026-09-28: `lifecycle_enforce`
/// is the one switch for everything that touches the lifecycle machine), identical to the
/// aeon crate's `conf::lifecycle_enforce`: an environment value, when present, wins — `1` or
/// `true` is on, anything else (including empty) is off; else the typed
/// `spira.lifecycle_enforce`; else **off**. Whether a `spira-lc` binary exists is never an
/// input.
pub fn resolve_lifecycle_enforce(env_value: Option<&str>, configured: Option<bool>) -> bool {
    match env_value {
        Some(v) => v == "1" || v == "true",
        None => configured.unwrap_or(false),
    }
}

/// [`resolve_lifecycle_enforce`] for this process: `$SPIRA_LIFECYCLE_ENFORCE`, else
/// `spira.lifecycle_enforce` in `toml_file` (or, when `None`, the document [`discover`]
/// finds), else off. An unreadable or invalid document is off, as in the aeon crate. A
/// non-UTF-8 environment value is present-but-not-`1`, so off.
pub fn lifecycle_enforce(toml_file: Option<&Path>) -> bool {
    if let Some(v) = std::env::var_os(LIFECYCLE_ENFORCE_ENV) {
        return resolve_lifecycle_enforce(Some(v.to_str().unwrap_or("")), None);
    }
    let path = toml_file.map(Path::to_path_buf).or_else(|| discover(None));
    let configured = path
        .filter(|p| p.is_file())
        .and_then(|p| load(&p).ok())
        .and_then(|d| d.spira)
        .and_then(|s| s.lifecycle_enforce);
    resolve_lifecycle_enforce(None, configured)
}

/// The root of `spira.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpiraToml {
    pub spira: Option<SpiraSection>,
    #[serde(default)]
    pub repo: BTreeMap<String, RepoSection>,
    #[serde(default)]
    pub persona: BTreeMap<String, PersonaSection>,
}

/// on/off, spelled as an enum rather than a bool so a TOML reader sees the word `spira.conf`
/// already used (`SPIRA_CERTIFY_SUITES = off`) instead of learning a second spelling for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OnOff {
    On,
    Off,
}

/// A czar stage: shadow investigates and writes CZAR-WOULD notes; act permits mutation.
/// One representative key (`czar_stage_deadlock`) ships here — the other six
/// `SPIRA_CZAR_STAGE_*` classes are the same enum and widen with the rest of `[spira]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CzarStage {
    Shadow,
    Act,
}

/// One key of the config key registry (`spira/conf.d`, `spira/conf.toml.d`), as generated
/// into [`SPIRA_KEYS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiraKey {
    pub key: &'static str,
    pub field: &'static str,
    pub ty: &'static str,
    pub toml_only: bool,
}

include!(concat!(env!("OUT_DIR"), "/spira_section.rs"));

/// How a landed branch reaches its base — see `repo-map.example`'s own `land` column.
/// `Queue` and `QueueForge` are the same mode under two spellings — `queue` is the alias a
/// row has always been able to write, `queue.forge` names it explicitly now that
/// `QueueLocal` exists to contrast it with. `QueueLocal` lands on a local branch (`local/main`
/// in the `base` column) with no forge round trip on the critical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LandMode {
    Push,
    Pr,
    Hold,
    Queue,
    #[serde(rename = "queue.forge")]
    QueueForge,
    #[serde(rename = "queue.local")]
    QueueLocal,
}

/// A persona lane a repository admits — the explicit form of `repo-map.example`'s `lanes`
/// column. A row's shorthand mode (`consume`/`develop`/`self`) is expanded to this array by
/// the converter; the schema itself only ever sees the expanded list.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    Plan,
    Incident,
    Groom,
    MaechenSweep,
    Spike,
    CzarTrigger,
}

/// `[repo.<name>]` — a checkout the harness may work in. `path` and `mode` are the two
/// facts nothing can derive: where the checkout is, and what a green gate does with a
/// branch. Everything else has a sensible empty reading (no format, no explicit base to
/// resolve, no extra lanes).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoSection {
    pub path: String,
    pub mode: LandMode,
    pub base: Option<String>,
    pub format: Option<String>,
    // `gate` is retired (sp-quu2w, RETIRED_REPO_KEYS): the gate string lives in the tree
    // under test (`gate.steps`), which the gate reads; this key never had a reader.
    #[serde(default)]
    pub lanes: Vec<Lane>,
    /// Overrides the host's default forge script for this one repository. Absent means
    /// "use `[spira]`'s own", which is every repository today.
    pub forge: Option<String>,
    /// What the certification gate runs for this repository (design
    /// gate-unit-round-integration-2026-09-29, item 4, sp-2ghui). Absent means
    /// [`GateMode::Suites`], today's behaviour exactly. Read it with [`RepoSection::gate_mode`].
    pub gate_mode: Option<GateMode>,
}

impl RepoSection {
    /// The gate mode in force: the typed key, else [`GateMode::Suites`].
    pub fn gate_mode(&self) -> GateMode {
        self.gate_mode.unwrap_or_default()
    }
}

/// `[repo.<name>] gate_mode` — how the gate composes a trial (gate/DESIGN.md "Composition").
///
/// * `suites` (the default): the repository's gate string, whole — fences, build, and the
///   budgeted suite selection.
/// * `unit`: the composition follows what the branch touches. A branch that touches any
///   bash (or other script) component still runs the gate string whole; one that touches
///   only Rust crates (plus docs, config, suites) runs the fences with suites off, then
///   `cargo test -p` for each touched crate and its reverse dependents on the host; one
///   that touches nothing buildable runs the fences only.
///
/// One command reverts it: `spira-config set repo.<name>.gate_mode suites <file>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GateMode {
    Unit,
    #[default]
    Suites,
}

impl GateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GateMode::Unit => "unit",
            GateMode::Suites => "suites",
        }
    }
}

/// `[repo.<name>] gate_mode` of `doc`, the default when the repository or the key is absent.
pub fn repo_gate_mode(doc: &SpiraToml, repo: &str) -> GateMode {
    doc.repo.get(repo).map(RepoSection::gate_mode).unwrap_or_default()
}

/// `append` keeps Claude Code's own coding guidance underneath the persona layer;
/// `replace` is for a persona narrow enough that the default guidance would mislead it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SystemPromptMode {
    Append,
    Replace,
}

/// The claim TTL and heartbeat cadence a persona's lease runs on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub minutes: Option<u32>,
    pub heartbeat_seconds: Option<u32>,
}

/// `[persona.<name>]` — a fayth, cut down to what the harness itself reads to summon and
/// dispatch it. `model` is the one field with no sensible default: an aeon summoned under
/// no model is not a cheaper aeon, it is a dead one, so it is required.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PersonaSection {
    pub model: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub lane: Option<String>,
    #[serde(default)]
    pub lease: Option<Lease>,
    pub system_prompt: Option<SystemPromptMode>,
}

/// The JSON Schema for [`SpiraToml`], generated from the same types `validate` deserializes
/// into, so the schema and the actual hard errors can never name different fields. A key's
/// `MAX=` in the registry is patched in after generation: schemars 0.8 has no derive spelling
/// for a range, and a `schema_with` override would lose the derive's `Option` handling.
pub fn json_schema() -> schemars::schema::RootSchema {
    let mut root = schemars::schema_for!(SpiraToml);
    if let Some(schemars::schema::Schema::Object(spira_section)) = root.definitions.get_mut("SpiraSection") {
        for (field, max) in SPIRA_FIELD_MAX {
            if let Some(schemars::schema::Schema::Object(prop)) = spira_section.object().properties.get_mut(*field) {
                prop.number().maximum = Some(*max);
            }
        }
    }
    root
}

/// A `[spira]` key dropped from `SpiraSection` whose presence in a live config must not
/// break loading it. `validate` accepts these fields with a warning instead of the hard
/// "unknown field" error a genuine typo gets.
pub struct RetiredKey {
    pub key: &'static str,
    pub bead: &'static str,
}

/// Removing a field from `SpiraSection` requires adding it here, or
/// `retiring_a_key_requires_updating_the_history` (tests/validate.rs) fails: it diffs the
/// schema's current field set against `schema/spira-key-history.txt`, the all-time set, and
/// a key present in history but neither active nor listed here is reported as silently
/// dropped.
pub const RETIRED_SPIRA_KEYS: &[RetiredKey] = &[
    // Spira works the whole backlog continuously; there is no single goal bead (per Ryan,
    // 2026-09-30, sp-2f9sa). The id prefix it used to imply is its own key now: id_prefix.
    RetiredKey { key: "goal", bead: "sp-k6m1m" },
    RetiredKey { key: "queue_local_gate", bead: "sp-vsob2" },
    RetiredKey { key: "queue_batch_idle_cut", bead: "sp-vsob2" },
    RetiredKey { key: "cert_idle_skip", bead: "sp-6d9th" },
    RetiredKey { key: "hook_lines", bead: "sp-o9nkc" },
    RetiredKey { key: "answer_state", bead: "sp-xsl8i" },
    RetiredKey { key: "answer_mark", bead: "sp-xsl8i" },
    RetiredKey { key: "answer_comment_mark", bead: "sp-xsl8i" },
    RetiredKey { key: "self_closed", bead: "sp-xsl8i" },
    RetiredKey { key: "wake_watchers", bead: "sp-xsl8i" },
    RetiredKey { key: "reclaim_skip_label", bead: "sp-i2m7y" },
    // No explicit CPU quotas anywhere (law-isolate-greedy-work-in-vms): the aeon launch and the
    // sentinel's landing dispatch no longer pass CPUQuota, so these keys have no consumer.
    RetiredKey { key: "aeon_cpu_quota", bead: "sp-b4oct" },
    RetiredKey { key: "land_cpu_quota", bead: "sp-b4oct" },
    // Every Spira tool is invoked by bare name on the launcher's PATH (sp-gypjk, design
    // runtime-is-a-release): a key naming a tool's path has no consumer. `batcher_bin` is
    // retired too, but a value that is not the batcher (prod's "/bin/true") is honoured as
    // `batcher_enable = "0"` by [`validate_with_warnings`], so the batcher stays off.
    RetiredKey { key: "lc_bin", bead: "sp-gypjk" },
    RetiredKey { key: "panel", bead: "sp-gypjk" },
    RetiredKey { key: "broker_bin", bead: "sp-gypjk" },
    RetiredKey { key: "czar_pass_bin", bead: "sp-gypjk" },
    RetiredKey { key: "queue_watch_bin", bead: "sp-gypjk" },
    RetiredKey { key: "supervise_bin", bead: "sp-gypjk" },
    RetiredKey { key: "landing_pass_bin", bead: "sp-gypjk" },
    RetiredKey { key: "tsd_bin", bead: "sp-gypjk" },
    RetiredKey { key: "tsd_lifecycle_export_bin", bead: "sp-gypjk" },
    RetiredKey { key: "reconciler_bin", bead: "sp-gypjk" },
    RetiredKey { key: "test_plan_bin", bead: "sp-gypjk" },
    RetiredKey { key: "reconciler_flow_bin", bead: "sp-gypjk" },
    RetiredKey { key: "loom_bin", bead: "sp-gypjk" },
    RetiredKey { key: "batcher_bin", bead: "sp-gypjk" },
];

/// A retired `batcher_bin` value that is not the batcher itself (e.g. "/bin/true", the old
/// way to switch automatic cuts off) — read as `batcher_enable = "0"`.
pub fn batcher_bin_means_off(val: &str) -> bool {
    let v = val.trim();
    !v.is_empty() && std::path::Path::new(v).file_name().and_then(|n| n.to_str()) != Some("batcher")
}

/// A `[repo.<name>]` key dropped from `RepoSection`, accepted from a live config with a
/// warning instead of the hard "unknown field" error, exactly as [`RETIRED_SPIRA_KEYS`] is.
pub const RETIRED_REPO_KEYS: &[RetiredKey] = &[
    // The gate string moved into the tree under test (`gate.steps`, gate/DESIGN.md "The tree
    // owns its gate"): a branch that ports a fence edits it in the same commit. The legacy
    // repo-map column still gates a repository whose landing ref has never carried one.
    RetiredKey { key: "gate", bead: "sp-quu2w" },
];

/// Every key in `history` that is neither an active `[spira]` field (`active`) nor listed in
/// [`RETIRED_SPIRA_KEYS`] — a key the schema dropped without retiring it.
pub fn missing_from_retirement(
    history: &std::collections::BTreeSet<String>,
    active: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let retired: std::collections::BTreeSet<&str> =
        RETIRED_SPIRA_KEYS.iter().map(|k| k.key).collect();
    history
        .iter()
        .filter(|k| !active.contains(k.as_str()) && !retired.contains(k.as_str()))
        .cloned()
        .collect()
}

/// Parses `text` as `spira.toml` and reports the first error at the TOML path it occurred
/// on (`spira.max_aeons`, `repo.service.mode`, ...) rather than a bare line/column, so a
/// hard error names the thing to fix instead of the place the parser gave up. A
/// [`RETIRED_SPIRA_KEYS`] member under `[spira]` is dropped before deserializing rather than
/// refused: see [`validate_with_warnings`] for the warning that names it.
pub fn validate(text: &str) -> Result<SpiraToml, String> {
    validate_with_warnings(text).map(|(doc, _)| doc)
}

/// [`validate_with_warnings`] plus [`require_id_prefix`] — the check `spira-config validate`
/// runs (doctor, pre-activate): the document is well-formed AND names this installation's
/// id prefix.
pub fn validate_strict(text: &str) -> Result<(SpiraToml, Vec<String>), String> {
    let (doc, warnings) = validate_with_warnings(text)?;
    require_id_prefix(&doc)?;
    Ok((doc, warnings))
}

/// Whether `p` can be a bead id prefix: non-empty ASCII letters, digits and `_` — never a
/// hyphen, which is the separator between the prefix and the id.
pub fn valid_id_prefix(p: &str) -> bool {
    !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// sp-oppza: the ONE-TIME UPGRADE MIGRATION for a box that last wrote its config before
/// sp-k6m1m retired the goal concept. Such a box's `[spira]` table (or legacy `spira.conf`)
/// names a `goal` shaped like a bead id — `"<prefix>-<rest>"` — the exact prefix this
/// installation's own bead ids carry (`conf.sh` used to derive `SPIRA_ID_PREFIX` from it as
/// `${SPIRA_GOAL%%-*}`). With id_prefix now required and nothing deriving it, that derivation
/// is reproduced here so an existing box migrates instead of failing activation outright.
/// `None` when `goal` is not bead-id-shaped (no hyphen, or a prefix `valid_id_prefix` rejects)
/// — that box gets the ordinary "required" refusal naming the key, same as one with no goal
/// at all.
pub fn id_prefix_from_goal(goal: &str) -> Option<String> {
    let (prefix, _) = goal.split_once('-')?;
    valid_id_prefix(prefix).then(|| prefix.to_string())
}

/// sp-oppza: `spira-config migrate` — the automatic form of `the_migration_is_one_set`
/// (spira-config/tests/validate.rs): read, `set_path spira.id_prefix`, write. Run by the
/// installer (pre-activate, doctor) before either checks `spira.id_prefix` is set, so a box
/// whose config predates sp-k6m1m migrates instead of failing activation on every one of
/// them. `Ok(None)` when there is nothing to do — no `[spira]` table, `id_prefix` ALREADY SET
/// (production's own state, set by hand: a true no-op, checked first), or `goal` is not
/// bead-id-shaped (that box gets the ordinary "required" refusal naming the key, same as one
/// with no goal at all). `Ok(Some((new_text, message)))` otherwise: the migrated document,
/// with `goal` gone (the ordinary retirement already drops it — [`validate`] strips it before
/// this ever reaches `set_path`), and a message for the caller to log.
pub fn migrate_goal_to_id_prefix(text: &str) -> Result<Option<(String, String)>, String> {
    let root: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let prefix = match root.get("spira").and_then(|v| v.as_table()) {
        Some(spira) if !spira.contains_key("id_prefix") => {
            match spira.get("goal").and_then(|v| v.as_str()).and_then(id_prefix_from_goal) {
                Some(p) => p,
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let doc = validate(text)?;
    let migrated = set_path(&doc, "spira.id_prefix", &prefix)?;
    let out = toml::to_string_pretty(&migrated).map_err(|e| e.to_string())?;
    let msg = format!("migrated: goal implied id_prefix = {prefix:?} (sp-k6m1m/sp-oppza, one-time) — goal removed");
    Ok(Some((out, msg)))
}

/// File wrapper for [`migrate_goal_to_id_prefix`]: reads `file` and, when a migration
/// applies, writes the result back atomically (via [`write_atomic`]) and returns the message
/// to log. A missing or empty file is nothing to migrate, not an error — `spira-config
/// migrate` is meant to run unconditionally, ahead of every `validate`, same as this crate's
/// other idempotent seams (`spira_toml_resolve`'s auto-convert).
pub fn migrate_goal_to_id_prefix_in_file(file: &std::path::Path) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    match migrate_goal_to_id_prefix(&text)? {
        Some((out, msg)) => {
            write_atomic(file, &out).map_err(|e| format!("{}: {e}", file.display()))?;
            Ok(Some(msg))
        }
        None => Ok(None),
    }
}

/// Whether `[spira]` sets any key at all. An empty table — the shape `convert` writes when
/// it regenerates only `[persona.*]` — says nothing about the host, so it needs no prefix.
fn spira_sets_anything(s: &SpiraSection) -> bool {
    match toml::Value::try_from(s) {
        Ok(toml::Value::Table(t)) => t.values().any(|v| !matches!(v, toml::Value::Array(a) if a.is_empty())),
        _ => true,
    }
}

/// THE ID PREFIX FAILS CLOSED (sp-k6m1m). A `[spira]` table that sets anything must name
/// `id_prefix`, and it must be a usable prefix. It used to be derived in conf.sh from the
/// goal epic's id (`${SPIRA_GOAL%%-*}`); with the goal retired nothing derives it. A
/// document with no `[spira]` table, or an empty one, passes: it configures no installation.
///
/// WHERE IT REFUSES: [`validate_strict`] — `spira-config validate`, which is what `doctor`
/// and the release's `pre-activate` run against the config in force, so a release cannot
/// be activated over a config without one. The typed READERS ([`validate`], [`load`]) do not
/// refuse a whole document for it: a daemon that cannot parse its config falls back to
/// defaults for every key, which is a far wider failure than the one key being absent.
pub fn require_id_prefix(doc: &SpiraToml) -> Result<(), String> {
    let Some(s) = doc.spira.as_ref() else { return Ok(()) };
    match s.id_prefix.as_deref() {
        Some(p) if valid_id_prefix(p) => Ok(()),
        Some(p) => Err(format!(
            "spira.id_prefix: {p:?} is not a bead id prefix (letters, digits and _ only, no hyphen)"
        )),
        None if !spira_sets_anything(s) => Ok(()),
        None => Err(
            "spira.id_prefix: required — the prefix of this installation's own bead ids, without \
             the hyphen (e.g. \"sp\"); it is no longer derived from the retired goal key (sp-k6m1m)"
                .into(),
        ),
    }
}

/// Like [`validate`], but returns one warning per [`RETIRED_SPIRA_KEYS`] member found under
/// `[spira]` — naming the key and the bead that retired it — instead of silently dropping
/// them. A key `SPIRA_CONF_KEYS`/`SpiraSection` never accepted still hard-errors: only
/// listed retirements are stripped before the deserialize that would otherwise refuse them.
///
/// A `[spira]` key's registry `MAX=` is enforced here too: this function deserializes the
/// TOML directly and never runs the shipped schema against the document.
pub fn validate_with_warnings(text: &str) -> Result<(SpiraToml, Vec<String>), String> {
    let mut root: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let mut warnings = Vec::new();
    if let Some(spira) = root.get_mut("spira").and_then(|v| v.as_table_mut()) {
        // BEFORE the generic strip: a batcher_bin that is not the batcher switched the cuts
        // off, and dropping it silently would switch them back on (sp-gypjk).
        let off = spira.get("batcher_bin").and_then(|v| v.as_str()).map(batcher_bin_means_off).unwrap_or(false);
        if off && !spira.contains_key("batcher_enable") {
            spira.insert("batcher_enable".into(), toml::Value::String("0".into()));
            warnings.push(
                "batcher_bin is retired (sp-gypjk); its non-batcher value is read as batcher_enable = \"0\" — replace it with that".into(),
            );
        }
        for retired in RETIRED_SPIRA_KEYS {
            if spira.remove(retired.key).is_some() {
                warnings.push(format!(
                    "{} is retired ({}) and ignored — remove it",
                    retired.key, retired.bead
                ));
            }
        }
    }
    if let Some(repos) = root.get_mut("repo").and_then(|v| v.as_table_mut()) {
        for (name, table) in repos.iter_mut() {
            let Some(table) = table.as_table_mut() else { continue };
            for retired in RETIRED_REPO_KEYS {
                if table.remove(retired.key).is_some() {
                    warnings.push(format!(
                        "repo.{name}.{} is retired ({}) and ignored — remove it",
                        retired.key, retired.bead
                    ));
                }
            }
        }
    }
    if let Some(spira) = root.get("spira").and_then(|v| v.as_table()) {
        for (field, max) in SPIRA_FIELD_MAX {
            if let Some(n) = spira.get(*field).and_then(|v| v.as_integer()) {
                if n as f64 > *max {
                    return Err(format!("spira.{field}: {n} exceeds hard ceiling {max}"));
                }
            }
        }
    }
    let doc: SpiraToml = serde_path_to_error::deserialize(root).map_err(|e| {
        let path = e.path().to_string();
        if path.is_empty() {
            e.inner().to_string()
        } else {
            format!("{path}: {}", e.inner())
        }
    })?;
    Ok((doc, warnings))
}

/// Whether `new` is a SHRINK of `existing` — fewer `[repo.*]` tables, or fewer
/// `[spira].fayths` entries — the two counts a converted `spira.toml` can lose without any
/// parse error to show for it, if it is regenerated from a narrower source (a repo-map
/// missing rows, a worktree with only some of the real `chamber/*.fayth` files) and written
/// over a fuller document already in force. `Some(reason)` names both counts for the
/// caller to report; `None` means `new` carries at least as much as `existing` in both
/// dimensions.
pub fn shrink_reason(existing: &SpiraToml, new: &SpiraToml) -> Option<String> {
    let existing_repos = existing.repo.len();
    let new_repos = new.repo.len();
    let existing_fayths = existing.spira.as_ref().map(|s| s.fayths.len()).unwrap_or(0);
    let new_fayths = new.spira.as_ref().map(|s| s.fayths.len()).unwrap_or(0);
    if new_repos < existing_repos || new_fayths < existing_fayths {
        Some(format!(
            "existing has {existing_repos} [repo.*] table(s) and {existing_fayths} fayth(s); \
             new document has {new_repos} and {new_fayths}"
        ))
    } else {
        None
    }
}

/// A temp file already written beside its destination, waiting on [`atomic_write_commit`].
/// Splitting the write from the rename is what lets a caller validate the temp file's
/// contents — or back up the destination — before the one step that actually replaces it.
pub struct PendingWrite {
    tmp: std::path::PathBuf,
    dest: std::path::PathBuf,
}

/// Cleans up the temp file when a `PendingWrite` is dropped without being committed — a
/// validation failure between `atomic_write_start` and `atomic_write_commit` should not
/// leave a stray `.tmp.<pid>` file beside the destination. A no-op once committed: the
/// rename has already moved the temp file away, so removing its old path finds nothing.
impl Drop for PendingWrite {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.tmp);
    }
}

/// Writes `contents` to a temp file beside `path`, touching nothing at `path` itself. A
/// process killed before [`atomic_write_commit`] runs leaves `path` exactly as it was — the
/// crash window this two-phase split exists to prove closed.
pub fn atomic_write_start(path: &std::path::Path, contents: &str) -> std::io::Result<PendingWrite> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("spira-config-out");
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    Ok(PendingWrite {
        tmp,
        dest: path.to_path_buf(),
    })
}

/// The one step that makes a [`PendingWrite`] visible at its destination.
pub fn atomic_write_commit(pending: PendingWrite) -> std::io::Result<()> {
    std::fs::rename(&pending.tmp, &pending.dest)
}

/// Writes `contents` to `path` atomically: a temp file beside it, then a rename. Without
/// this a reader racing the writer (`spira_toml_read`, another `validate`) can observe a
/// half-written document — truncated by a writer killed mid-write — as a parse error on a
/// file that was never actually invalid.
pub fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    atomic_write_commit(atomic_write_start(path, contents)?)
}

/// Copies `path` to a sibling `.bak.<unix-seconds>.<pid>` file before a writer replaces it —
/// so a bad write has an undo even after `atomic_write_commit` has already run. A no-op,
/// not an error, when `path` does not exist yet (the first write to a fresh file has nothing
/// to preserve).
pub fn backup_existing(path: &std::path::Path) -> std::io::Result<Option<std::path::PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("spira-config-out");
    let backup = path.with_file_name(format!("{name}.bak.{now}.{}", std::process::id()));
    std::fs::copy(path, &backup)?;
    Ok(Some(backup))
}

/// Names this crate treats as secret-shaped and therefore never lets `export --sh` put into
/// a shell's environment — a schema field named like a credential is validated and typed
/// like everything else, but its VALUE is exactly the thing the credential-storage design
/// keeps out of `spira.toml` in the first place.
///
/// Matched case-insensitively, as the WHOLE name or its trailing `_`-separated component
/// (`broker_gh_token` matches on `token`), never a bare substring — `token_window_h` and
/// `token_projects` (a context-budget window and a project list, not a secret) both lead
/// with `token_` rather than end with it, and a substring rule would wrongly catch them too.
pub fn is_secret_shaped(field_name: &str) -> bool {
    let lower = field_name.to_ascii_lowercase();
    ["credential", "token", "password", "dsn"]
        .iter()
        .any(|pat| lower == *pat || lower.ends_with(&format!("_{pat}")))
}

/// `repo.<name>.land` is the name `spira-config set`'s callers reach for — the config's own
/// [`LandMode`] type already carries that meaning — but the field this schema types is
/// `mode`; this is the one alias between them, applied before path resolution ever sees the
/// path. Keeping the on-disk field `mode` (rather than renaming it) means an existing
/// `spira.toml` with `mode = "..."` keeps validating unchanged.
fn alias_repo_land(path: &str) -> String {
    let segs: Vec<&str> = path.split('.').collect();
    if let [rest @ .., last] = segs.as_slice() {
        if rest.first() == Some(&"repo") && rest.len() == 2 && *last == "land" {
            let mut segs = rest.to_vec();
            segs.push("mode");
            return segs.join(".");
        }
    }
    path.to_string()
}

/// Serializes `doc` and writes it to `path` atomically — [`write_atomic`] plus the one
/// `toml::to_string_pretty` call every writer outside this crate would otherwise need its
/// own copy of (sp-6onps: `aeons.sh`, now the `aeons` binary in `spira-world`, is exactly
/// that caller). Kept here, not duplicated at the call site, so `config-fence`'s "only
/// spira-config names or parses the config" contract holds without a new allow-list entry.
pub fn serialize_and_write(path: &std::path::Path, doc: &SpiraToml) -> Result<(), String> {
    let text = toml::to_string_pretty(doc).map_err(|e| format!("{}: {e}", path.display()))?;
    write_atomic(path, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads one dotted path out of an already-validated document — `spira.max_aeons`,
/// `repo.service.mode`, `persona.builder.lease.minutes` — for `spira-config get`.
pub fn get_path(doc: &SpiraToml, path: &str) -> Option<String> {
    let path = alias_repo_land(path);
    let value = serde_json::to_value(doc).ok()?;
    let mut cur = &value;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    match cur {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// `[spira]`'s scalar/list fields as `UPPER_KEY -> value` — the one place a toml field name
/// becomes the `SPIRA_*`/`COCKPIT_*` key spelling, shared by [`export_sh`] (bash-facing) and
/// [`resolve::resolve`] (in-process: this IS the "config file" tier of its env > toml >
/// derived precedence). A `None` field serialises to JSON `null` and is skipped — so a key
/// present here means the toml document set it, even to an explicit empty string, which is
/// exactly the "is this key spoken for" test `resolve` needs to tell apart from "unset,
/// consult the derived default".
pub fn spira_string_map(doc: &SpiraToml) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(spira) = &doc.spira else {
        return out;
    };
    let value = serde_json::to_value(spira).unwrap_or(serde_json::Value::Null);
    let serde_json::Value::Object(map) = value else {
        return out;
    };
    for (key, val) in map {
        if is_secret_shaped(&key) {
            continue;
        }
        let shell_val = match val {
            serde_json::Value::Null => continue,
            serde_json::Value::String(s) => s,
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Array(items) => {
                if items.is_empty() {
                    continue;
                }
                items
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            serde_json::Value::Object(_) => continue,
        };
        out.insert(key.to_uppercase(), shell_val);
    }
    out
}

/// Renders `[spira]` as quoted `KEY=value` lines — `spira-config export --sh` — for the
/// bash callers this schema has not replaced yet. Only scalar and list fields have a bash
/// shape; tables (`repo`, `persona`) are not exported.
pub fn export_sh(doc: &SpiraToml) -> String {
    let mut out = String::new();
    for (key, val) in spira_string_map(doc) {
        out.push_str(&key);
        out.push('=');
        out.push_str(&shell_quote(&val));
        out.push('\n');
    }
    out
}

pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Sets one dotted path (`spira.prod`, `spira.max_live_aeons`) to a leaf JSON value on `doc`.
/// Shared by `set_path`'s two attempts, below.
fn set_path_leaf(
    doc: &SpiraToml,
    path: &str,
    leaf_value: serde_json::Value,
) -> Result<SpiraToml, String> {
    let path = alias_repo_land(path);
    let segs: Vec<&str> = path.split('.').collect();
    let Some((leaf, parents)) = segs.split_last() else {
        return Err("set: empty path".to_string());
    };
    let mut json = serde_json::to_value(doc).map_err(|e| e.to_string())?;
    let mut cur = &mut json;
    for seg in parents {
        if !cur.is_object() {
            *cur = serde_json::Value::Object(Default::default());
        }
        cur = cur
            .as_object_mut()
            .expect("just made an object")
            .entry(seg.to_string())
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
    }
    if !cur.is_object() {
        *cur = serde_json::Value::Object(Default::default());
    }
    cur.as_object_mut()
        .expect("just made an object")
        .insert(leaf.to_string(), leaf_value);
    serde_path_to_error::deserialize(json).map_err(|e| {
        let p = e.path().to_string();
        if p.is_empty() {
            e.inner().to_string()
        } else {
            format!("{p}: {}", e.inner())
        }
    })
}

/// Sets one dotted path (`spira.prod`, `spira.instance`, `spira.max_live_aeons`) to a value
/// given as a plain command-line string, for `spira-config set` — the writer `deploy.sh`,
/// `install.sh` and `conf.sh`'s own `spira_config_set` shell helper use once nothing writes
/// `spira.conf`'s `KEY=value` lines anymore. Goes through a JSON round-trip rather than a
/// hand-written per-field match arm, so a field this schema already knows needs no writer of
/// its own; `deny_unknown_fields` still refuses a path this schema does not carry, via the
/// same `serde` deserialize the rest of this crate validates through.
///
/// TRIES THE VALUE AS A STRING FIRST, because most fields here are paths and names, and a
/// literal like `"3"` must stay the string `"3"` when the field is one of those — only a
/// field the schema itself types as a number or bool (`max_live_aeons`, `queue_batch_max`, ...)
/// ever takes the second attempt, which parses the same text as JSON and retries. Reports the
/// first (string) attempt's error when both fail, since "expected u32" names the fix; the
/// generic JSON-parse failure on a bare word does not.
pub fn set_path(doc: &SpiraToml, path: &str, value: &str) -> Result<SpiraToml, String> {
    let as_string_err = match set_path_leaf(doc, path, serde_json::Value::String(value.to_string()))
    {
        Ok(d) => return Ok(d),
        Err(e) => e,
    };
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) {
        if !parsed.is_string() {
            if let Ok(d) = set_path_leaf(doc, path, parsed) {
                return Ok(d);
            }
        }
    }
    Err(as_string_err)
}

/// Removes one dotted path's value from `doc`, for `spira-config unset` — the writer side of
/// retiring a key (`aeons.sh unset`'s fleet ceiling) without leaving an empty string behind.
/// A JSON null deserializes to `None` for every `Option<T>` field this schema has, and `toml`
/// omits a `None` field entirely on serialization, so the key simply stops appearing in the
/// document — never a leftover `key = ""` a reader would have to know means "unset".
///
/// A RETIRED key (`spira.<key>` in [`RETIRED_SPIRA_KEYS`], `repo.<name>.<key>` in
/// [`RETIRED_REPO_KEYS`]) is not a field any more, so it cannot be nulled; `validate` already
/// dropped it from `doc`, and writing `doc` back is what removes it from the file. Unsetting
/// one is therefore the document unchanged, never an "unknown field" refusal — that refusal
/// would leave the retired line in the live file with no CLI way to take it out.
pub fn unset_path(doc: &SpiraToml, path: &str) -> Result<SpiraToml, String> {
    if is_retired_path(path) {
        return Ok(doc.clone());
    }
    set_path_leaf(doc, path, serde_json::Value::Null)
}

/// Whether `path` names a retired key (see [`unset_path`]).
pub fn is_retired_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('.').collect();
    match parts.as_slice() {
        ["spira", key] => RETIRED_SPIRA_KEYS.iter().any(|r| r.key == *key),
        ["repo", _, key] => RETIRED_REPO_KEYS.iter().any(|r| r.key == *key),
        _ => false,
    }
}

/// `spira-config set` for several paths at once, as a library call: read and validate
/// `file`, apply every `(path, value)` with [`set_path`], re-validate the result, and write
/// it with [`write_atomic`] — all or nothing. For a Rust caller that must change two keys
/// together (queue's land-mode transition writes `mode` and `base` as one fact), so the
/// document is never observed with one written and not the other.
pub fn set_paths_in_file(file: &std::path::Path, pairs: &[(&str, &str)]) -> Result<(), String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut doc = if text.trim().is_empty() { SpiraToml::default() } else { validate(&text)? };
    for (path, value) in pairs {
        doc = set_path(&doc, path, value)?;
    }
    let out = toml::to_string_pretty(&doc).map_err(|e| format!("{}: {e}", file.display()))?;
    validate(&out)?;
    write_atomic(file, &out).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_path_is_the_release_then_the_system_dirs_and_unset_is_refused() {
        assert_eq!(
            release_path("/r/spira-releases/abc"),
            "/r/spira-releases/abc/bin:/r/spira-releases/abc/spira:/usr/local/bin:/usr/bin:/bin"
        );
        assert_eq!(release_path_from_env(Some("/h/")).unwrap(), "/h/bin:/h/spira:/usr/local/bin:/usr/bin:/bin");
        for v in [None, Some(""), Some("  ")] {
            assert!(release_path_from_env(v).unwrap_err().contains("SPIRA_RELEASE is not set"));
        }
    }

    #[test]
    fn release_path_with_tail_appends_after_the_system_dirs() {
        assert_eq!(
            release_path_with_tail("/r/spira-releases/abc", "/h/.local/bin:/h/.cargo/bin").unwrap(),
            "/r/spira-releases/abc/bin:/r/spira-releases/abc/spira:/usr/local/bin:/usr/bin:/bin:/h/.local/bin:/h/.cargo/bin"
        );
        // An empty (or all-whitespace) tail changes nothing — same as before this bead.
        for empty in ["", "   "] {
            assert_eq!(release_path_with_tail("/r/spira-releases/abc", empty).unwrap(), release_path("/r/spira-releases/abc"));
        }
    }

    #[test]
    fn release_path_with_tail_refuses_a_tail_entry_inside_a_release() {
        let e = release_path_with_tail("/r/spira-releases/abc", "/r/spira-releases/def/bin").unwrap_err();
        assert!(e.contains("spira-releases"), "{e}");
        assert!(e.contains("/r/spira-releases/def/bin"), "{e}");
    }

    #[test]
    fn release_path_with_tail_refuses_a_tail_entry_inside_a_checkout() {
        let dir = testkit::TempDir::new("spira-config-tail-checkout");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let e = release_path_with_tail("/r/spira-releases/abc", bin.to_str().unwrap()).unwrap_err();
        assert!(e.contains("checkout"), "{e}");
        assert!(e.contains(".git"), "{e}");
    }

    #[test]
    fn release_path_from_env_with_tail_composes_both_refusals() {
        assert_eq!(
            release_path_from_env_with_tail(Some("/h/"), "/x/y").unwrap(),
            "/h/bin:/h/spira:/usr/local/bin:/usr/bin:/bin:/x/y"
        );
        assert!(release_path_from_env_with_tail(None, "/x/y").unwrap_err().contains("SPIRA_RELEASE is not set"));
        assert!(release_path_from_env_with_tail(Some("/h/"), "/r/spira-releases/def").unwrap_err().contains("spira-releases"));
    }

    #[test]
    fn lifecycle_enforce_resolution_matches_aeon() {
        // The environment wins, both ways; set-but-empty is off.
        assert!(resolve_lifecycle_enforce(Some("1"), Some(false)));
        assert!(resolve_lifecycle_enforce(Some("true"), None));
        assert!(!resolve_lifecycle_enforce(Some("0"), Some(true)));
        assert!(!resolve_lifecycle_enforce(Some(""), Some(true)));
        assert!(!resolve_lifecycle_enforce(Some("yes"), None));
        assert!(!resolve_lifecycle_enforce(Some("TRUE"), None));
        // No environment: the typed key, else off.
        assert!(resolve_lifecycle_enforce(None, Some(true)));
        assert!(!resolve_lifecycle_enforce(None, Some(false)));
        assert!(!resolve_lifecycle_enforce(None, None));
    }

    #[test]
    fn lifecycle_enforce_reads_the_typed_key_from_a_document() {
        // Only meaningful when the process environment does not pin the switch.
        if std::env::var_os(LIFECYCLE_ENFORCE_ENV).is_some() {
            return;
        }
        let dir = testkit::TempDir::new("spira-config-lce");
        let p = dir.join("spira.toml");
        std::fs::write(&p, "[spira]\nid_prefix = \"sp\"\nlifecycle_enforce = true\n").unwrap();
        assert!(lifecycle_enforce(Some(&p)));
        std::fs::write(&p, "[spira]\nid_prefix = \"sp\"\nlifecycle_enforce = false\n").unwrap();
        assert!(!lifecycle_enforce(Some(&p)));
        std::fs::write(&p, "[spira]\nid_prefix = \"sp\"\nnot_a_key = 1\n").unwrap();
        assert!(!lifecycle_enforce(Some(&p)), "an invalid document is off");
        assert!(!lifecycle_enforce(Some(&dir.join("absent.toml"))), "a named but absent document is off");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_minimal_document() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmax_aeons = 4\n").expect("valid");
        assert_eq!(doc.spira.unwrap().max_aeons, Some(4));
    }

    #[test]
    fn unknown_key_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nbogus = 1\n").unwrap_err();
        assert!(err.starts_with("spira.bogus"), "{err}");
    }

    #[test]
    fn wrong_type_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nmax_aeons = \"four\"\n").unwrap_err();
        assert!(err.starts_with("spira.max_aeons"), "{err}");
    }

    #[test]
    fn bad_enum_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nczar_stage_deadlock = \"sometimes\"\n").unwrap_err();
        assert!(err.starts_with("spira.czar_stage_deadlock"), "{err}");
    }

    #[test]
    fn missing_required_repo_field() {
        let err = validate("[repo.home]\nmode = \"push\"\n").unwrap_err();
        assert!(err.starts_with("repo.home"), "{err}");
    }

    #[test]
    fn missing_required_persona_field() {
        let err = validate("[persona.builder]\ntools = [\"Bash\"]\n").unwrap_err();
        assert!(err.starts_with("persona.builder"), "{err}");
    }

    #[test]
    fn the_inline_comment_scar_is_refused_unquoted() {
        // sp-upkae's own scar: an inline `#` comment after a bare, unquoted value used to
        // be silently absorbed into that value by conf.sh's KEY=value reader. TOML has no
        // such ambiguity to inherit: a bare word is not a legal value at all, so the same
        // line is a hard parse error here rather than a value nobody refused.
        let err = validate("[spira]\nid_prefix = \"sp\"\ndb = /home/x # a trailing comment\n").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn the_inline_comment_scar_is_harmless_quoted() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\ndb = \"/home/x\" # a trailing comment\n").expect("valid");
        assert_eq!(doc.spira.unwrap().db, Some("/home/x".to_string()));
    }

    #[test]
    fn get_path_reads_nested_tables() {
        let doc =
            validate("[repo.home]\npath = \"/srv/checkouts/home\"\nmode = \"push\"\n").unwrap();
        assert_eq!(get_path(&doc, "repo.home.mode"), Some("push".to_string()));
        assert_eq!(get_path(&doc, "repo.home.base"), None);
    }

    #[test]
    fn export_sh_quotes_and_uppercases() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nhome_repo = \"a b\"\nmax_aeons = 4\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("HOME_REPO='a b'\n"), "{out}");
        assert!(out.contains("MAX_AEONS='4'\n"), "{out}");
    }

    #[test]
    fn export_sh_renders_a_toml_bool_as_the_word_true_not_1() {
        // sp-9hwim: a caller reading a bool key off export --sh (mail.sh's SPIRA_MAIL_MUTE)
        // must match on "1|true", the same spelling every other [spira] bool key already
        // uses — never a bare `= "1"`, which a TOML `true` would silently fail.
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmail_mute = true\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("MAIL_MUTE='true'\n"), "{out}");
    }

    #[test]
    fn set_path_on_empty_document_creates_the_table() {
        let doc = set_path(&SpiraToml::default(), "spira.prod", "/srv/x").unwrap();
        assert_eq!(doc.spira.unwrap().prod, Some("/srv/x".to_string()));
    }

    #[test]
    fn set_path_replaces_without_disturbing_siblings() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nprod = \"/old\"\nmax_aeons = 4\n").unwrap();
        let doc = set_path(&doc, "spira.prod", "/new").unwrap();
        let spira = doc.spira.unwrap();
        assert_eq!(spira.prod, Some("/new".to_string()));
        assert_eq!(spira.max_aeons, Some(4));
    }

    #[test]
    fn set_path_refuses_an_unknown_field() {
        let err = set_path(&SpiraToml::default(), "spira.bogus", "x").unwrap_err();
        assert!(err.starts_with("spira.bogus"), "{err}");
    }

    #[test]
    fn a_retired_key_warns_instead_of_erroring() {
        let (doc, warnings) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nqueue_local_gate = 1\n")
            .expect("a retired key must not be a hard error");
        assert!(doc.spira.is_some());
        assert!(
            warnings.iter().any(|w| w.contains("queue_local_gate") && w.contains("sp-vsob2")),
            "{warnings:?}"
        );
    }

    #[test]
    fn the_retired_repo_gate_key_warns_and_is_ignored() {
        let (doc, warnings) = validate_with_warnings(
            "[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngate = \"bash spira/x.sh\"\n",
        )
        .expect("a retired repo key must not be a hard error");
        assert!(doc.repo.contains_key("spira"));
        assert!(
            warnings.iter().any(|w| w.contains("repo.spira.gate") && w.contains("sp-quu2w")),
            "{warnings:?}"
        );
        // Positive control: a misspelt repo key is still refused.
        let err = validate("[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngatee = 1\n").unwrap_err();
        assert!(err.contains("repo.spira"), "{err}");
    }

    #[test]
    fn unsetting_a_retired_key_removes_it_instead_of_refusing() {
        let text = "[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngate = \"bash spira/x.sh\"\n";
        let doc = validate(text).unwrap();
        let out = toml::to_string_pretty(&unset_path(&doc, "repo.spira.gate").unwrap()).unwrap();
        assert!(!out.contains("gate"), "{out}");
        assert!(out.contains("path = \"/p\""), "{out}");
        assert!(unset_path(&doc, "spira.aeon_cpu_quota").is_ok());
        // Positive control: an unknown, never-retired key is still refused.
        assert!(unset_path(&doc, "repo.spira.gatee").is_err());
    }

    #[test]
    fn retired_cpu_quota_keys_warn_and_are_ignored() {
        // sp-b4oct: a live spira.toml still carrying the aeon/landing CPU quota validates,
        // with one warning per key naming the retiring bead, and nothing exports them.
        let (doc, warnings) = validate_with_warnings(
            "[spira]\nid_prefix = \"sp\"\naeon_cpu_quota = \"400\"\nland_cpu_quota = \"70\"\n",
        )
        .expect("a retired CPU quota key must not be a hard error");
        for key in ["aeon_cpu_quota", "land_cpu_quota"] {
            assert!(
                warnings.iter().any(|w| w.contains(key) && w.contains("sp-b4oct")),
                "no warning for {key}: {warnings:?}"
            );
        }
        let sh = export_sh(&doc);
        assert!(!sh.contains("CPU_QUOTA"), "a retired key leaked into export --sh: {sh}");
    }

    #[test]
    fn a_misspelt_key_still_errors() {
        // Positive control for a_retired_key_warns_instead_of_erroring: a key that looks
        // like a retired one but isn't must still be refused, not silently accepted.
        let err = validate("[spira]\nid_prefix = \"sp\"\nqueue_local_gatee = 1\n").unwrap_err();
        assert!(err.starts_with("spira.queue_local_gatee"), "{err}");
    }

    #[test]
    fn missing_from_retirement_catches_a_dropped_key() {
        use std::collections::BTreeSet;
        let active: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let mut history = active.clone();
        history.insert("queue_local_gate".to_string());
        // Dropped, but listed in RETIRED_SPIRA_KEYS — not an offender.
        assert!(missing_from_retirement(&history, &active).is_empty());

        // Planted offender: dropped from `active` and never retired.
        history.insert("never_retired".to_string());
        assert_eq!(
            missing_from_retirement(&history, &active),
            vec!["never_retired".to_string()]
        );
    }

    #[test]
    fn set_path_coerces_a_numeric_field() {
        let doc = set_path(&SpiraToml::default(), "spira.max_live_aeons", "3").unwrap();
        assert_eq!(doc.spira.unwrap().max_live_aeons, Some(3));
    }

    #[test]
    fn set_path_coerces_a_bool_field() {
        let doc = set_path(&SpiraToml::default(), "spira.lifecycle_enforce", "true").unwrap();
        assert_eq!(doc.spira.unwrap().lifecycle_enforce, Some(true));
    }

    #[test]
    fn set_path_keeps_a_numeric_looking_string_field_a_string() {
        // `instance` is a string field; a value that happens to parse as JSON must not be
        // coerced away from the string the schema actually wants.
        let doc = set_path(&SpiraToml::default(), "spira.instance", "123").unwrap();
        assert_eq!(doc.spira.unwrap().instance, Some("123".to_string()));
    }

    #[test]
    fn unset_path_removes_the_key_and_keeps_siblings() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmax_live_aeons = 3\nmax_aeons = 4\n").unwrap();
        let doc = unset_path(&doc, "spira.max_live_aeons").unwrap();
        // omitted entirely from the serialized document, not written as an empty value.
        let out = toml::to_string_pretty(&doc).unwrap();
        assert!(!out.contains("max_live_aeons"), "{out}");
        let spira = doc.spira.unwrap();
        assert_eq!(spira.max_live_aeons, None);
        assert_eq!(spira.max_aeons, Some(4));
    }

    // ENV VARS ARE PROCESS-GLOBAL: every `discover` test holding one of these keys takes
    // the crate-wide `ENV_LOCK` (declared near `FILE_NAME` above, sp-dh4fv) for its whole
    // body, so it also serializes against `locate`'s own env-mutating tests, not just its
    // siblings here.
    static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> testkit::TempDir {
        let n = SCRATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        testkit::TempDir::new(&format!("spira-config-lib-test-{tag}-{n}"))
    }

    #[test]
    fn find_under_locates_the_file() {
        let dir = scratch_dir("find-under-present");
        std::fs::write(dir.join(FILE_NAME), "[spira]\n").unwrap();
        assert_eq!(find_under(&dir), Some(dir.join(FILE_NAME)));
    }

    #[test]
    fn find_under_is_none_when_absent() {
        // POSITIVE CONTROL for find_under_locates_the_file: an empty scratch dir must not
        // report a file that was never written.
        let dir = scratch_dir("find-under-absent");
        assert_eq!(find_under(&dir), None);
    }

    #[test]
    fn discover_prefers_the_explicit_path_over_the_environment() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("SPIRA_TOML").ok();
        std::env::set_var("SPIRA_TOML", "/should-not-be-used/spira.toml");
        let explicit = PathBuf::from("/explicit/spira.toml");
        assert_eq!(discover(Some(explicit.clone())), Some(explicit));
        match saved {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }
    }

    #[test]
    fn discover_never_falls_back_through_spira_repo() {
        // REGRESSION (sp-hconl): discover() used to offer $SPIRA_REPO/spira.toml as a
        // candidate (sp-cx0mj); sp-9hwim's bash rewrite deliberately dropped that tier from
        // conf.sh a day later ("the running system must not read [from beside the checkout]
        // at all") and this function was never updated to match. A spira.toml sitting right
        // there, with nothing at the XDG/etc tiers, must resolve to None, not that file. The
        // tier logic itself is exercised in full in `locate`'s own tests; this is the
        // public-wrapper regression guard.
        if Path::new("/etc/spira/spira.toml").is_file() {
            eprintln!("skipping: this machine has a real /etc/spira/spira.toml");
            return;
        }
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_toml = std::env::var("SPIRA_TOML").ok();
        let saved_repo = std::env::var("SPIRA_REPO").ok();
        let saved_home = std::env::var("HOME").ok();
        let saved_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::remove_var("SPIRA_TOML");
        std::env::remove_var("XDG_CONFIG_HOME");
        let dir = scratch_dir("discover-repo");
        let repo_dir = dir.join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(repo_dir.join(FILE_NAME), "[spira]\n").unwrap();
        let empty_home = dir.join("home-empty");
        std::fs::create_dir_all(&empty_home).unwrap();
        std::env::set_var("SPIRA_REPO", &repo_dir);
        std::env::set_var("HOME", &empty_home);
        assert_eq!(discover(None), None);
        match saved_toml {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }
        match saved_repo {
            Some(v) => std::env::set_var("SPIRA_REPO", v),
            None => std::env::remove_var("SPIRA_REPO"),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn load_reads_and_validates() {
        let dir = scratch_dir("load-ok");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[spira]\nid_prefix = \"sp\"\nmax_aeons = 4\n").unwrap();
        let doc = load(&path).expect("valid document");
        assert_eq!(doc.spira.unwrap().max_aeons, Some(4));
    }

    #[test]
    fn load_names_the_path_on_a_parse_error() {
        let dir = scratch_dir("load-bad");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[spira]\nid_prefix = \"sp\"\nbogus = 1\n").unwrap();
        let err = load(&path).unwrap_err();
        assert!(err.contains(&path.display().to_string()), "{err}");
    }

    #[test]
    fn set_paths_in_file_writes_both_or_neither() {
        let dir = testkit::TempDir::new("spira-config-setpaths");
        let path = dir.join("spira.toml");
        std::fs::write(&path, "[repo.r]\npath = \"/x\"\nmode = \"queue.local\"\nbase = \"local/main\"\n").unwrap();
        set_paths_in_file(&path, &[("repo.r.mode", "queue.forge"), ("repo.r.base", "origin/main")]).unwrap();
        let doc = load(&path).unwrap();
        assert_eq!(get_path(&doc, "repo.r.mode").as_deref(), Some("queue.forge"));
        assert_eq!(get_path(&doc, "repo.r.base").as_deref(), Some("origin/main"));
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(set_paths_in_file(&path, &[("repo.r.base", "local/main"), ("repo.r.mode", "bogus")]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------ gate_mode (sp-2ghui)

    const REPO_R: &str = "[repo.r]\npath = \"/x\"\nmode = \"queue.local\"\n";

    #[test]
    fn gate_mode_defaults_to_suites_when_absent() {
        let doc = validate(REPO_R).unwrap();
        assert_eq!(repo_gate_mode(&doc, "r"), GateMode::Suites);
        assert_eq!(repo_gate_mode(&doc, "no-such-repo"), GateMode::Suites);
        assert_eq!(get_path(&doc, "repo.r.gate_mode"), None);
    }

    #[test]
    fn gate_mode_reads_both_values() {
        for (v, want) in [("unit", GateMode::Unit), ("suites", GateMode::Suites)] {
            let doc = validate(&format!("{REPO_R}gate_mode = \"{v}\"\n")).unwrap();
            assert_eq!(repo_gate_mode(&doc, "r"), want);
            assert_eq!(want.as_str(), v);
            assert_eq!(get_path(&doc, "repo.r.gate_mode").as_deref(), Some(v));
        }
    }

    #[test]
    fn gate_mode_refuses_anything_else() {
        for bad in ["\"Unit\"", "\"fast\"", "\"\"", "true", "1"] {
            let err = validate(&format!("{REPO_R}gate_mode = {bad}\n")).unwrap_err();
            assert!(err.contains("repo.r.gate_mode"), "{bad}: {err}");
        }
    }

    #[test]
    fn gate_mode_is_set_and_reverted_through_set_path() {
        let doc = validate(REPO_R).unwrap();
        let on = set_path(&doc, "repo.r.gate_mode", "unit").unwrap();
        assert_eq!(repo_gate_mode(&on, "r"), GateMode::Unit);
        let text = toml::to_string_pretty(&on).unwrap();
        assert!(text.contains("gate_mode = \"unit\""), "{text}");
        assert!(set_path(&doc, "repo.r.gate_mode", "fast").is_err());
        let off = set_path(&on, "repo.r.gate_mode", "suites").unwrap();
        assert_eq!(repo_gate_mode(&off, "r"), GateMode::Suites);
        let gone = unset_path(&on, "repo.r.gate_mode").unwrap();
        assert_eq!(repo_gate_mode(&gone, "r"), GateMode::Suites);
    }

    // ---- sp-oppza: migrate_goal_to_id_prefix — the automatic one-time upgrade migration ----

    #[test]
    fn id_prefix_from_goal_reads_the_prefix_and_refuses_what_is_not_bead_shaped() {
        assert_eq!(id_prefix_from_goal("sp-spira").as_deref(), Some("sp"));
        assert_eq!(id_prefix_from_goal("tt-own-thing").as_deref(), Some("tt"));
        for bad in ["nohyphen", "-leadinghyphen", "sp with space-x", ""] {
            assert_eq!(id_prefix_from_goal(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn migrate_derives_id_prefix_from_a_bead_shaped_goal_and_drops_goal() {
        let (out, msg) = migrate_goal_to_id_prefix("[spira]\ngoal = \"sp-spira\"\ndb = \"/db\"\n")
            .expect("parses")
            .expect("a migration applies");
        assert!(msg.contains("sp-k6m1m") && msg.contains("sp-oppza"), "{msg}");
        let (doc, _) = validate_strict(&out).expect("the migrated document passes the same check doctor/pre-activate run");
        let s = doc.spira.unwrap();
        assert_eq!(s.id_prefix.as_deref(), Some("sp"));
        assert_eq!(s.db.as_deref(), Some("/db"), "the rest of [spira] survives");
        assert!(!out.contains("goal"), "{out}");
    }

    #[test]
    fn migrate_is_a_true_no_op_once_id_prefix_is_set_by_hand() {
        // Production's own state: id_prefix already set BY HAND. Checked first, so a goal
        // left behind alongside it changes nothing — no rewrite, same as the retired-key
        // warning path already covers.
        assert_eq!(migrate_goal_to_id_prefix("[spira]\nid_prefix = \"sp\"\ngoal = \"sp-spira\"\n").unwrap(), None);
        assert_eq!(migrate_goal_to_id_prefix("[spira]\nid_prefix = \"tt\"\n").unwrap(), None);
    }

    #[test]
    fn migrate_is_a_no_op_with_nothing_to_migrate() {
        assert_eq!(migrate_goal_to_id_prefix("[spira]\ndb = \"/db\"\n").unwrap(), None, "no goal at all");
        assert_eq!(migrate_goal_to_id_prefix("[spira]\ngoal = \"nohyphen\"\n").unwrap(), None, "goal not bead-shaped");
        assert_eq!(migrate_goal_to_id_prefix("[repo.a]\npath = \"/a\"\nmode = \"push\"\n").unwrap(), None, "no [spira] table at all");
        assert_eq!(migrate_goal_to_id_prefix("").unwrap(), None, "empty document");
    }

    #[test]
    fn migrate_in_file_writes_atomically_and_a_second_run_is_silent() {
        let dir = testkit::TempDir::new("spira-config-migrate");
        let path = dir.join("spira.toml");
        std::fs::write(&path, "[spira]\ngoal = \"sp-spira\"\ndb = \"/db\"\n").unwrap();

        let msg = migrate_goal_to_id_prefix_in_file(&path).unwrap().expect("first run migrates");
        assert!(msg.contains("sp-oppza"), "{msg}");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("id_prefix = \"sp\""), "{after}");
        assert!(!after.contains("goal"), "{after}");

        // SECOND RUN IS SILENT: id_prefix is there now, so this is the true no-op path —
        // proof the migration ran exactly once, not on every invocation.
        assert_eq!(migrate_goal_to_id_prefix_in_file(&path).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after, "unchanged on the second run");
    }

    #[test]
    fn migrate_in_file_is_a_no_op_on_a_missing_or_empty_file() {
        let dir = testkit::TempDir::new("spira-config-migrate-missing");
        assert_eq!(migrate_goal_to_id_prefix_in_file(&dir.join("nonexistent.toml")).unwrap(), None);
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(migrate_goal_to_id_prefix_in_file(&empty).unwrap(), None);
    }

    #[test]
    fn set_path_repo_land_writes_the_mode_field() {
        let doc = validate("[repo.home]\npath = \"/srv/home\"\nmode = \"push\"\n").unwrap();
        let doc = set_path(&doc, "repo.home.land", "queue.local").unwrap();
        assert_eq!(doc.repo["home"].mode, LandMode::QueueLocal);
        assert_eq!(get_path(&doc, "repo.home.land"), Some("queue.local".to_string()));
        assert_eq!(get_path(&doc, "repo.home.mode"), Some("queue.local".to_string()));
    }

    #[test]
    fn gate_string_with_quotes_dollar_and_newlines_round_trips_through_toml() {
        let gate = "echo \"hi\" && $(rm -rf /) # not really\nnext line\n\t'quoted'";
        let doc = set_path(
            &validate("[repo.home]\npath = \"/srv/home\"\nmode = \"push\"\n").unwrap(),
            "repo.home.base",
            gate,
        )
        .unwrap();
        let out = toml::to_string_pretty(&doc).expect("serializes");
        let reparsed = validate(&out).expect("the serialized document must still validate");
        assert_eq!(reparsed.repo["home"].base.as_deref(), Some(gate));
    }

    #[test]
    fn is_secret_shaped_matches_credential_shaped_names_case_insensitively() {
        for name in ["token", "BROKER_GH_TOKEN", "db_password", "Credential", "some_dsn"] {
            assert!(is_secret_shaped(name), "{name} should be secret-shaped");
        }
        for name in ["home_repo", "max_aeons", "gate", "path"] {
            assert!(!is_secret_shaped(name), "{name} should not be secret-shaped");
        }
    }

    #[test]
    fn export_sh_omits_a_secret_shaped_field() {
        let doc = validate("[spira]\nhome_repo = \"a\"\nbroker_gh_token = \"ghp_x\"\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("HOME_REPO="), "{out}");
        assert!(!out.contains("ghp_x"), "{out}");
        assert!(!out.to_uppercase().contains("TOKEN"), "{out}");
    }

    #[test]
    fn atomic_write_start_leaves_the_destination_untouched_until_commit() {
        let tmp = testkit::TempDir::new("spira-config-test");
        let dir = tmp.path().to_path_buf();
        let dest = dir.join("spira.toml");
        std::fs::write(&dest, "original").unwrap();

        let pending = atomic_write_start(&dest, "replacement").expect("temp write succeeds");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "original");

        atomic_write_commit(pending).expect("commit succeeds");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "replacement");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_dropped_without_commit_leaves_no_temp_file_and_the_original_intact() {
        let tmp = testkit::TempDir::new("spira-config-test-drop");
        let dir = tmp.path().to_path_buf();
        let dest = dir.join("spira.toml");
        std::fs::write(&dest, "original").unwrap();

        {
            let _pending = atomic_write_start(&dest, "never committed").expect("temp write succeeds");
            // simulates a crash between the temp write and the rename: dropped, never committed
        }
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "original");
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty(), "{leftover:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backup_existing_copies_the_old_file_and_is_a_noop_when_absent() {
        let tmp = testkit::TempDir::new("spira-config-test-backup");
        let dir = tmp.path().to_path_buf();
        let dest = dir.join("spira.toml");

        assert_eq!(backup_existing(&dest).unwrap(), None);

        std::fs::write(&dest, "before").unwrap();
        let backup = backup_existing(&dest).unwrap().expect("a backup path");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "before");
        std::fs::remove_dir_all(&dir).ok();
    }
}
