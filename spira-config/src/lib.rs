//! spira-config — the typed schema behind `spira.toml`.
//!
//! One document, three kinds of table: `[spira]` (host-wide keys), `[repo.<name>]` (a
//! checkout the harness may work in) and `[persona.<name>]` (a dwarf's rune, cut down to
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

pub mod amendment;
pub mod lc_call;
pub mod admission;
pub mod bounded;
pub mod build;
pub mod runes;
pub mod containment;
pub mod convert;
pub mod deps;
pub mod env_bootstrap;
pub mod hostaddr;
pub mod eval;
pub mod rune_file;
pub mod init;
pub mod legacy_map;
pub mod lc_state;
pub mod lifecycle_row;
pub mod local_pass;
pub mod escalation;
pub mod locate;
pub mod nonwork;
pub mod process;
pub mod proof;
pub mod recipe;
pub mod rename;
pub mod registry;
pub mod release_env;
pub mod release_skew;
pub mod release_tag;
pub mod repo_admin;
pub mod repos;
pub mod resolve;
pub mod room;
pub mod scoped_stop;
pub mod scratch;
pub mod session;
pub mod stack_conflict;
pub mod target;
pub mod unit;
pub mod vtime;
pub mod writeback;

pub use locate::LocateOutcome;


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

/// `spira-config convert --conf <conf> --home <home> --out <out> [--repo-map <repo_map>]
/// [--rune <f>]...`, built (not run) — for a writer at a root other than this process's
/// own (`install`'s cross-checkout instance seed, sp-31dm0), which must not name this
/// binary's own flags itself (config-fence: only spira-config names `repo-map`).
pub fn convert_command(conf: &Path, home: &Path, out: &Path, repo_map: Option<&Path>, rune: &[PathBuf]) -> std::process::Command {
    let mut cmd = std::process::Command::new(crate::release_env::tool("spira-config"));
    cmd.arg("convert").arg("--conf").arg(conf).arg("--home").arg(home).arg("--out").arg(out);
    if let Some(rm) = repo_map {
        cmd.arg("--repo-map").arg(rm);
    }
    for f in rune {
        cmd.arg("--rune").arg(f);
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
/// Parse a TOML document that is not spira.toml (a repository's own declaration).
pub fn parse_foreign<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

pub fn load(path: &Path) -> Result<SpiraToml, String> {
    load_layered(&path.to_string_lossy()).map(|(doc, _)| doc)
}

/// Refuses an added value that names a host path through a `placeholder` segment: a delta is
/// written without knowing the host, so such a path is right on none.
pub fn placeholder_paths<'a>(added: impl Iterator<Item = (&'a str, &'a toml::Value)>) -> Result<(), String> {
    for (k, v) in added {
        if let Some(s) = v.as_str().filter(|s| s.starts_with('/') && s.split('/').any(|seg| seg.eq_ignore_ascii_case("placeholder"))) {
            return Err(format!("{k} = {s:?} is a placeholder path; omit the key so its registry default derives from the install's own roots"));
        }
    }
    Ok(())
}

/// `(added, removed)` key paths a `config-delta.toml` declares: `[added]` table keys and the
/// `removed` array.
pub fn parse_config_delta(text: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let v: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    if let Some(t) = v.get("added").and_then(|a| a.as_table()) {
        placeholder_paths(t.iter().map(|(k, v)| (k.as_str(), v)))?;
    }
    let added = v.get("added").and_then(|a| a.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default();
    let removed = v
        .get("removed")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok((added, removed))
}

/// [`load`] plus the warnings and [`require_id_prefix`] — the check `spira-config validate`
/// runs (doctor, pre-activate) over the config in force, layers and all.
pub fn load_strict(path: &Path) -> Result<(SpiraToml, Vec<String>), String> {
    let (doc, warnings) = load_layered(&path.to_string_lossy())?;
    require_id_prefix(&doc)?;
    Ok((doc, warnings))
}

/// THE ONE SOURCE, LAYERED (per Ryan 2026-10-05): `spec` is one file, or a `:`-separated list.
/// The first is the complete base; each later file overrides only the keys it declares. Each
/// layer is parsed once; the layers' tables are unioned key by key (a later layer's leaf
/// replaces the earlier one, nested tables merge); the result is validated once. Every listed
/// file must exist. Callers never see layers — they get one [`SpiraToml`].
fn load_layered(spec: &str) -> Result<(SpiraToml, Vec<String>), String> {
    let mut merged = toml::map::Map::new();
    let mut any = false;
    for p in spec.split(':').filter(|p| !p.is_empty()) {
        any = true;
        let text = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
        let toml::Value::Table(layer) = text.parse::<toml::Value>().map_err(|e| format!("{p}: {e}"))? else {
            return Err(format!("{p}: not a TOML table"));
        };
        union_into(&mut merged, layer);
    }
    if !any {
        return Err("no config file named".to_string());
    }
    validate_value(toml::Value::Table(merged), registry::for_process()?).map_err(|e| format!("{spec}: {e}"))
}

/// `over`'s keys into `base`: a nested table merges, anything else replaces.
fn union_into(base: &mut toml::map::Map<String, toml::Value>, over: toml::map::Map<String, toml::Value>) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => union_into(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

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

/// Where install builds the pinned tool environment (`nix build .#spira-env`), under `$HOME`.
pub const SPIRA_ENV_REL: &str = ".local/lib/spira-env";
/// The spira-env the last rebuild replaced, rooted so `spira-nix-gc` keeps it for rollback.
pub const SPIRA_ENV_PREV_REL: &str = ".local/lib/spira-env.prev";

/// The `bin/` of the pinned tool environment for `home` — what unit PATHs put ahead of the
/// hand-installed directories.
pub fn spira_env_bin(home: &str) -> String {
    format!("{}/{SPIRA_ENV_REL}/bin", home.trim_end_matches('/'))
}

/// Every colon-separated entry of `tail` that [`tail_segment_refusal`] flags, in order — the
/// pure check [`release_path_with_tail`] applies before it will append anything.
pub fn tail_refusals(tail: &str) -> Vec<String> {
    tail.split(':').filter(|s| !s.is_empty()).filter_map(tail_segment_refusal).collect()
}

/// [`release_path`] plus the box's own tool-directory tail (`spira.path`: `~/.local/bin` for
/// `bd`, `dolt`, `gh`, `claude` and `duckdb`, `~/.cargo/bin` for a dwarf's `cargo test`),
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

/// The retired lifecycle switch (sp-v62vn): the lifecycle machine is the only mode, so
/// neither this environment variable nor `spira.lifecycle_enforce` selects anything. Both are
/// still READ, for one purpose — a value saying off is refused by name rather than silently
/// ignored ([`lifecycle_switch_refusal`]), since a box that believes it runs off would be
/// wrong about every decision it makes.
pub const LIFECYCLE_ENFORCE_ENV: &str = "SPIRA_LIFECYCLE_ENFORCE";

/// Whether a value of the retired switch says on (`1`/`true`, the only spellings the old
/// resolution rule accepted as on).
fn lifecycle_switch_says_on(v: &str) -> bool {
    matches!(v.trim(), "1" | "true")
}

/// The refusal for a retired lifecycle switch that says off — `who` names where the value was
/// read (`$SPIRA_LIFECYCLE_ENFORCE`, `spira.lifecycle_enforce`, `spira.conf`) — naming the
/// exit (law-a-refusal-names-its-exit).
pub fn lifecycle_switch_refusal(who: &str, value: &str) -> String {
    format!(
        "{who} = {value:?} is refused: lifecycle_enforce is retired (sp-v62vn) and the lifecycle \
         machine is the only mode, so there is no off. Exit: remove it (unset the variable, or \
         delete the key from the config document); a fixture that needs bead state seeds \
         lifecycle rows instead (spira/testlib/lc-fixture.sh)"
    )
}

/// `$SPIRA_LIFECYCLE_ENFORCE` as `env_value` holds it: `Ok(None)` when absent or empty,
/// `Ok(Some(warning))` when it says on (deprecated: remove it), `Err(refusal)` when it says
/// anything else.
pub fn check_lifecycle_switch_env(env_value: Option<&str>) -> Result<Option<String>, String> {
    match env_value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) if lifecycle_switch_says_on(v) => Ok(Some(format!(
            "{LIFECYCLE_ENFORCE_ENV} is retired (sp-v62vn): the lifecycle machine is always on — remove it"
        ))),
        Some(v) => Err(lifecycle_switch_refusal(&format!("${LIFECYCLE_ENFORCE_ENV}"), v)),
    }
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub publish: BTreeMap<String, PublishSection>,
    /// Enum-valued keys whose value this release does not know (a newer release wrote it),
    /// dropped from the document and kept here by dotted path. A reader that needs the key
    /// refuses through [`repo_gate_mode`] and its like; every other reader never notices.
    #[serde(skip)]
    #[schemars(skip)]
    pub unknown_values: BTreeMap<String, String>,
}

/// `[publish.<name>]` — where a `queue.local` repository's green work is published. `remote`
/// `"none"` declares the repository publishes nowhere. Keyed by repository name, and kept
/// apart from `[repo.<name>]` because that table is only present for a box without a repo-map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishSection {
    pub remote: Option<String>,
    pub branch: Option<String>,
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

/// `[spira]` — host-wide keys, by their `spira.toml` spelling (`field`). Which keys exist, and
/// of what type, is the registry's to say ([`registry::Registry`]); [`validate`] checks the table
/// against it and drops what it does not know. Every key is optional but `id_prefix` (see
/// [`require_id_prefix`]); conf.sh derives each default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpiraSection(pub BTreeMap<String, toml::Value>);

impl JsonSchema for SpiraSection {
    fn schema_name() -> String {
        "SpiraSection".into()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject { instance_type: Some(schemars::schema::InstanceType::Object.into()), ..Default::default() }.into()
    }
}

impl SpiraSection {
    fn value(&self, key: &str, want: &[&str]) -> Result<Option<&toml::Value>, String> {
        let reg = registry::for_process()?;
        let rk = reg.get(key).ok_or_else(|| format!("{key} is not a registered config key — refusing"))?;
        if !want.contains(&rk.ty.as_str()) {
            return Err(format!("{key} is TYPE={}, not {}", rk.ty, want.join("/")));
        }
        Ok(self.0.get(&rk.field))
    }

    pub fn get_str(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.value(key, &["string", "onoff", "czar_stage"])?.and_then(|v| v.as_str()).map(str::to_string))
    }

    pub fn get_u32(&self, key: &str) -> Result<Option<u32>, String> {
        Ok(self.value(key, &["u32"])?.and_then(|v| v.as_integer()).and_then(|n| u32::try_from(n).ok()))
    }

    pub fn get_u64(&self, key: &str) -> Result<Option<u64>, String> {
        Ok(self.value(key, &["u64", "u32"])?.and_then(|v| v.as_integer()).and_then(|n| u64::try_from(n).ok()))
    }

    pub fn get_bool(&self, key: &str) -> Result<Option<bool>, String> {
        Ok(self.value(key, &["bool"])?.and_then(|v| v.as_bool()))
    }

    pub fn get_list(&self, key: &str) -> Result<Option<Vec<String>>, String> {
        Ok(self
            .value(key, &["list"])?
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()))
    }

    /// Whether any key is set at all, an empty list not counting.
    fn sets_anything(&self) -> bool {
        self.0.values().any(|v| !matches!(v, toml::Value::Array(a) if a.is_empty()))
    }
}

/// A getter's answer for a reader that carries on without the key: a refusal is said on stderr,
/// never read as the key being unset.
pub fn said<T>(answer: Result<Option<T>, String>) -> Option<T> {
    answer.unwrap_or_else(|e| {
        eprintln!("spira-config: {e}");
        None
    })
}

/// Why `v` is not a `ty`, in serde's words; `None` when it is one.
fn type_error(ty: &str, v: &toml::Value) -> Option<String> {
    let got = match v {
        toml::Value::String(s) => format!("string {s:?}"),
        toml::Value::Integer(n) => format!("integer `{n}`"),
        toml::Value::Float(n) => format!("floating point `{n}`"),
        toml::Value::Boolean(b) => format!("boolean `{b}`"),
        toml::Value::Datetime(d) => format!("datetime `{d}`"),
        toml::Value::Array(_) => "sequence".into(),
        toml::Value::Table(_) => "map".into(),
    };
    let invalid_type = |expected: &str| Some(format!("invalid type: {got}, expected {expected}"));
    let one_of = |words: &[&str]| match v.as_str() {
        Some(s) if words.contains(&s) => None,
        Some(s) => Some(format!(
            "unknown variant `{s}`, expected {}",
            words.iter().map(|w| format!("`{w}`")).collect::<Vec<_>>().join(" or ")
        )),
        None => invalid_type(&format!("one of {}", words.join(", "))),
    };
    match ty {
        "string" => v.as_str().map_or_else(|| invalid_type("a string"), |_| None),
        "bool" => v.as_bool().map_or_else(|| invalid_type("a boolean"), |_| None),
        "u32" | "u64" => match v.as_integer() {
            None => invalid_type(ty),
            Some(n) if (ty == "u32" && u32::try_from(n).is_err()) || n < 0 => {
                Some(format!("invalid value: integer `{n}`, expected {ty}"))
            }
            Some(_) => None,
        },
        "list" => match v.as_array() {
            Some(a) if a.iter().all(toml::Value::is_str) => None,
            Some(_) => invalid_type("a list of strings"),
            None => invalid_type("a sequence"),
        },
        "onoff" => one_of(&["on", "off"]),
        "czar_stage" => one_of(&["shadow", "act"]),
        _ => Some(format!("registry TYPE {ty:?} is not understood")),
    }
}

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
    MimirSweep,
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
/// * `fences`: the fences and the build fence (`cargo check` of the merged tree) only — the
///   gate string without its suites step, the `bin` tools those fences read and nothing else;
///   no unit test phase, no suites, no re-entry suites. The full-suite VM round is the test
///   gate (approved 2026-10-28, while the gate's 300 s cap starved the certified pool).
///   `SPIRA_GATE_ALL=1` still runs the gate string whole.
///
/// One command reverts it: `spira-config set repo.<name>.gate_mode suites <file>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GateMode {
    Unit,
    #[default]
    Suites,
    Fences,
}

impl GateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GateMode::Unit => "unit",
            GateMode::Suites => "suites",
            GateMode::Fences => "fences",
        }
    }
}

/// `[repo.<name>] gate_mode` of `doc`, the default when the repository or the key is absent.
pub fn repo_gate_mode(doc: &SpiraToml, repo: &str) -> Result<GateMode, String> {
    let path = format!("repo.{repo}.gate_mode");
    if let Some(v) = doc.unknown_values.get(&path) {
        return Err(format!("{path}: unknown value {v:?}; this release knows unit, suites, fences"));
    }
    Ok(doc.repo.get(repo).map(RepoSection::gate_mode).unwrap_or_default())
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

/// `[persona.<name>]` — a rune, cut down to what the harness itself reads to call and
/// dispatch it. `model` is the one field with no sensible default: a dwarf called under
/// no model is not a cheaper dwarf, it is a dead one, so it is required.
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
    /// The model a bead's claim runs on once it has been rejected twice (`escalation`); unset, the
    /// persona has no stronger model and its claims keep `model`.
    #[serde(default)]
    pub escalation_model: Option<String>,
}

/// The JSON Schema for [`SpiraToml`] against this process's registry ([`json_schema_with`]).
pub fn json_schema() -> Result<schemars::schema::RootSchema, String> {
    Ok(json_schema_with(registry::for_process()?))
}

/// The JSON Schema for [`SpiraToml`]. `SpiraSection` has one property per registry key: its TYPE,
/// its `SCHEMA=` as the description, its `MAX=` as `maximum`.
pub fn json_schema_with(reg: &registry::Registry) -> schemars::schema::RootSchema {
    use schemars::schema::{InstanceType, Metadata, Schema, SchemaObject, SingleOrVec};
    let mut root = schemars::schema_for!(SpiraToml);
    let Some(Schema::Object(section)) = root.definitions.get_mut("SpiraSection") else { return root };
    let mut keys: Vec<&registry::RegistryKey> = reg.keys.values().collect();
    keys.sort_by(|a, b| a.field.cmp(&b.field));
    for k in keys {
        let strings = || SchemaObject { instance_type: Some(InstanceType::String.into()), ..Default::default() };
        let mut prop = match k.ty.as_str() {
            "u32" | "u64" => SchemaObject { instance_type: Some(InstanceType::Integer.into()), ..Default::default() },
            "bool" => SchemaObject { instance_type: Some(InstanceType::Boolean.into()), ..Default::default() },
            "list" => SchemaObject {
                instance_type: Some(InstanceType::Array.into()),
                array: Some(Box::new(schemars::schema::ArrayValidation {
                    items: Some(SingleOrVec::Single(Box::new(strings().into()))),
                    ..Default::default()
                })),
                ..Default::default()
            },
            "onoff" => SchemaObject { enum_values: Some(vec!["on".into(), "off".into()]), ..strings() },
            "czar_stage" => SchemaObject { enum_values: Some(vec!["shadow".into(), "act".into()]), ..strings() },
            _ => strings(),
        };
        if let Some(d) = &k.schema_doc {
            prop.metadata = Some(Box::new(Metadata { description: Some(d.clone()), ..Default::default() }));
        }
        if let Some(m) = k.max {
            prop.number().maximum = Some(m);
        }
        section.object().properties.insert(k.field.clone(), prop.into());
    }
    root
}

/// A `[spira]` key dropped from the registry whose presence in a live config must not
/// break loading it. `validate` accepts these fields with a warning.
pub struct RetiredKey {
    pub key: &'static str,
    pub bead: &'static str,
}

/// Removing a key from the registry requires adding it here, or the history test
/// (tests/validate.rs) fails: it diffs the schema's field set against `spira/conf.key-history`,
/// the all-time set, and a key present in history but neither active nor listed here is
/// reported as silently dropped.
pub const RETIRED_SPIRA_KEYS: &[RetiredKey] = &[
    // Spira works the whole backlog continuously; there is no single goal bead (per Ryan,
    // 2026-09-30, sp-2f9sa). The id prefix it used to imply is its own key now: id_prefix.
    RetiredKey { key: "goal", bead: "sp-k6m1m" },
    RetiredKey { key: "maechen_label", bead: "sp-vt8lbr" }, // ffx-ok
    RetiredKey { key: "maechen_landing_interval", bead: "sp-vt8lbr" }, // ffx-ok
    RetiredKey { key: "maechen_max_beads", bead: "sp-vt8lbr" }, // ffx-ok
    RetiredKey { key: "maechen_max_gap_seconds", bead: "sp-vt8lbr" }, // ffx-ok
    RetiredKey { key: "maechen_remedy_label", bead: "sp-vt8lbr" }, // ffx-ok
    RetiredKey { key: "auron_restarts", bead: "sp-acp3lx" }, // ffx-ok
    RetiredKey { key: "auron_restart_window", bead: "sp-acp3lx" }, // ffx-ok
    RetiredKey { key: "chamber", bead: "sp-yyuunv" }, // ffx-ok
    RetiredKey { key: "chamber_overlay", bead: "sp-yyuunv" }, // ffx-ok
    RetiredKey { key: "lanes_reserve", bead: "sp-6evjfc" }, // ffx-ok
    RetiredKey { key: "fayths", bead: "sp-yyuunv" }, // ffx-ok
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
    // No explicit CPU quotas anywhere (law-isolate-greedy-work-in-vms): the dwarf launch and the
    // sentinel's landing dispatch no longer pass CPUQuota, so these keys have no consumer.
    RetiredKey { key: "aeon_cpu_quota", bead: "sp-b4oct" }, // ffx-ok
    RetiredKey { key: "max_aeons", bead: "sp-vti1l1" }, // ffx-ok
    RetiredKey { key: "max_live_aeons", bead: "sp-vti1l1" }, // ffx-ok
    RetiredKey { key: "aeon_warm_base", bead: "sp-vti1l1" }, // ffx-ok
    RetiredKey { key: "aeon_warm_cap_mib", bead: "sp-vti1l1" }, // ffx-ok
    RetiredKey { key: "summon_jitter", bead: "sp-vti1l1" }, // ffx-ok
    RetiredKey { key: "summon_lock_wait", bead: "sp-vti1l1" }, // ffx-ok
    RetiredKey { key: "land_cpu_quota", bead: "sp-b4oct" },
    // Every Spira tool is invoked by bare name on the launcher's PATH (sp-gypjk, design
    // runtime-is-a-release): a key naming a tool's path has no consumer. `batcher_bin` is
    // retired too, but a value that is not the batcher (prod's "/bin/true") is honoured as
    // `batcher_enable = "0"` by [`validate_with_warnings`], so the batcher stays off.
    RetiredKey { key: "lc_bin", bead: "sp-gypjk" },
    RetiredKey { key: "panel", bead: "sp-gypjk" },
    RetiredKey { key: "broker_bin", bead: "sp-gypjk" },
    // The broker intent queue is gone; only `broker token` and `broker read` remain.
    RetiredKey { key: "broker_enable", bead: "sp-1tvphb" },
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
    // Flaky suites are deleted, not quarantined, so nothing reactivates on clean runs.
    RetiredKey { key: "quarantine_clean_runs", bead: "sp-op2c2" },
    // The lifecycle machine is the only mode (sp-v62vn): `true` is accepted with this warning;
    // any other value is refused by [`validate_with_warnings`] before the strip.
    RetiredKey { key: "lifecycle_enforce", bead: "sp-v62vn" },
    // Express is lifecycle state (`spira-lc list --express`); no consumer reads a bd label.
    RetiredKey { key: "express_label", bead: "sp-38yq9j" },
    // Rework is never deferred behind another bead's branch: rounds merge and rebase, Sift screens.
    RetiredKey { key: "overlap_defer_label", bead: "sp-dzwj7w" },
    // Registered but read by no code under any spelling.
    RetiredKey { key: "round_marker_max_age", bead: "sp-dm7mjb" },
    RetiredKey { key: "liveness_model", bead: "sp-dm7mjb" },
    RetiredKey { key: "tier_budget_window", bead: "sp-dm7mjb" },
    RetiredKey { key: "attribute_maxpar", bead: "sp-dm7mjb" },
    RetiredKey { key: "batch_bins_ttl", bead: "sp-dm7mjb" },
    RetiredKey { key: "gate_select_cap", bead: "sp-dm7mjb" },
    RetiredKey { key: "preflight_suite_max_secs", bead: "sp-dm7mjb" },
    RetiredKey { key: "queue_repro_ci_maxsec", bead: "sp-dm7mjb" },
    RetiredKey { key: "queue_repro_ci_pollsec", bead: "sp-dm7mjb" },
    RetiredKey { key: "queue_stuck_age", bead: "sp-dm7mjb" },
    RetiredKey { key: "reconciler_label", bead: "sp-dm7mjb" },
    RetiredKey { key: "round_vm_provider", bead: "sp-dm7mjb" },
    RetiredKey { key: "suites_budget", bead: "sp-dm7mjb" },
    RetiredKey { key: "suite_times_log", bead: "sp-dm7mjb" },
    RetiredKey { key: "tier_allowlist", bead: "sp-dm7mjb" },
    RetiredKey { key: "tier_area_allowlist", bead: "sp-dm7mjb" },
    // The promote/review flow is gone; review.sh was its only consumer.
    RetiredKey { key: "reviewer_model", bead: "sp-fm92p0" },
    RetiredKey { key: "reviewer_timeout", bead: "sp-fm92p0" },
    RetiredKey { key: "reviewer_verdicts", bead: "sp-fm92p0" },
    RetiredKey { key: "reviewer_diff_limit", bead: "sp-fm92p0" },
    RetiredKey { key: "queue_batcher", bead: "sp-qu3k0s" },
    RetiredKey { key: "mirror", bead: "sp-qu3k0s" },
    RetiredKey { key: "qa_depth", bead: "sp-qu3k0s" },
    RetiredKey { key: "self_test", bead: "sp-qu3k0s" },
    RetiredKey { key: "prefix_map", bead: "sp-qu3k0s" },
    RetiredKey { key: "check5_max_resolve", bead: "sp-qu3k0s" },
    RetiredKey { key: "delivers_check_timeout", bead: "sp-qu3k0s" },
    RetiredKey { key: "cert_prices", bead: "sp-qu3k0s" },
    RetiredKey { key: "dolt_maintenance_units", bead: "sp-qu3k0s" },
    // Every fixture runs a private server of its own; no shared test server has a data directory or a port.
    RetiredKey { key: "testdb_data", bead: "sp-iulew8" },
    RetiredKey { key: "testdb_port", bead: "sp-iulew8" },
    RetiredKey { key: "lc_testdb_data", bead: "sp-iulew8" },
    RetiredKey { key: "lc_testdb_port", bead: "sp-iulew8" },
    // rust-toolchain.toml is the one pin; every consumer reads it.
    RetiredKey { key: "release_rust_toolchain", bead: "sp-nx83lb" },
    RetiredKey { key: "sift_mend_hold_secs", bead: "sp-luwtqz" },
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
/// on (`spira.max_dwarves`, `repo.service.mode`, ...) rather than a bare line/column, so a
/// hard error names the thing to fix instead of the place the parser gave up. A
/// [`RETIRED_SPIRA_KEYS`] member under `[spira]` is dropped before deserializing rather than
/// refused: see [`validate_with_warnings`] for the warning that names it.
pub fn validate(text: &str) -> Result<SpiraToml, String> {
    validate_with_warnings(text).map(|(doc, _)| doc)
}

/// [`validate_with_warnings`] against a registry the caller already holds.
pub fn validate_with(text: &str, reg: &registry::Registry) -> Result<(SpiraToml, Vec<String>), String> {
    let root: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    validate_value(root, reg)
}

/// [`validate_with_warnings`] plus [`require_id_prefix`] — the check `spira-config validate`
/// runs (doctor, pre-activate): the document is well-formed AND names this installation's
/// id prefix.
pub fn validate_strict(text: &str) -> Result<(SpiraToml, Vec<String>), String> {
    let (doc, warnings) = validate_with_warnings(text)?;
    require_id_prefix(&doc)?;
    let root: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    require_roster_covers_lanes(&root)?;
    require_roster_declares_models(&root)?;
    Ok((doc, warnings))
}

/// A rostered persona with no `persona.<name>.model` is called and dies before its session
/// starts: refused here, naming the key, rather than at every call.
fn require_roster_declares_models(root: &toml::Value) -> Result<(), String> {
    let Some(spira) = root.get("spira").and_then(|v| v.as_table()) else { return Ok(()) };
    let roster: Vec<&str> = match spira.get("runes") {
        Some(toml::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
        Some(toml::Value::String(s)) => s.split_whitespace().collect(),
        _ => return Ok(()),
    };
    for p in roster {
        let model = root.get("persona").and_then(|v| v.get(p)).and_then(|v| v.get("model")).and_then(|v| v.as_str());
        if model.is_none_or(str::is_empty) {
            return Err(format!(
                "spira.runes rosters {p:?} but persona.{p}.model is not declared — every call of it would die before its session starts; run `spira-config set persona.{p}.model <model>`"
            ));
        }
    }
    Ok(())
}

const TIMER_PARTITION_PERSONAS: &[&str] = &["groomer", "mimir", "czar", "warden", "mender", "pilgrim"];

/// A declared lane, or a timer-filed partition label, whose persona is not in an explicit
/// `spira.runes` roster is work nothing can claim: refused, naming both keys.
fn require_roster_covers_lanes(root: &toml::Value) -> Result<(), String> {
    let Some(spira) = root.get("spira").and_then(|v| v.as_table()) else { return Ok(()) };
    let roster: Vec<&str> = match spira.get("runes") {
        Some(toml::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
        Some(toml::Value::String(s)) => s.split_whitespace().collect(),
        _ => return Ok(()),
    };
    if roster.is_empty() {
        return Ok(());
    }
    let lanes: Vec<&str> = match spira.get("lanes") {
        Some(toml::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
        Some(toml::Value::String(s)) => s.split_whitespace().collect(),
        _ => Vec::new(),
    };
    let persona_lane = |l: &str| {
        roster.iter().any(|p| {
            root.get("persona").and_then(|v| v.get(*p)).and_then(|v| v.get("lane")).and_then(|v| v.as_str()) == Some(l)
        })
    };
    if let Some(l) = lanes.iter().find(|l| !roster.contains(l) && !persona_lane(l)) {
        return Err(format!(
            "spira.lanes declares {l:?} but spira.runes does not roster it — nothing can claim that lane's beads; add {l:?} to spira.runes or drop it from spira.lanes"
        ));
    }
    for p in TIMER_PARTITION_PERSONAS {
        let key = format!("{p}_label");
        if spira.contains_key(&key) && !roster.contains(p) {
            return Err(format!(
                "spira.{key} is set but spira.runes does not roster {p:?} — beads filed under that label can never be claimed; add {p:?} to spira.runes"
            ));
        }
    }
    Ok(())
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
    match s.get_str("SPIRA_ID_PREFIX")?.as_deref() {
        Some(p) if valid_id_prefix(p) => Ok(()),
        Some(p) => Err(format!(
            "spira.id_prefix: {p:?} is not a bead id prefix (letters, digits and _ only, no hyphen)"
        )),
        None if !s.sets_anything() => Ok(()),
        None => Err(
            "spira.id_prefix: required — the prefix of this installation's own bead ids, without \
             the hyphen (e.g. \"sp\"); it is no longer derived from the retired goal key (sp-k6m1m)"
                .into(),
        ),
    }
}

/// Like [`validate`], but returns one warning per [`RETIRED_SPIRA_KEYS`] member found under
/// `[spira]` — naming the key and the bead that retired it — instead of silently dropping
/// them. A key this release does not know at all (a newer release wrote it) is warned and
/// ignored, so a predecessor can read its successor's config; a malformed value of a known key
/// still errors.
///
/// A `[spira]` key's registry `MAX=` is enforced here too: the shipped schema is never run
/// against the document.
pub fn validate_with_warnings(text: &str) -> Result<(SpiraToml, Vec<String>), String> {
    validate_with(text, registry::for_process()?)
}

/// The checks and the typed deserialize, on an already-parsed document — one parse per layer,
/// never a text round trip after a merge.
fn validate_value(mut root: toml::Value, reg: &registry::Registry) -> Result<(SpiraToml, Vec<String>), String> {
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
        // BEFORE the generic strip: the retired lifecycle switch saying off is refused, not
        // dropped — a document that believes the box runs off is wrong (sp-v62vn).
        if let Some(v) = spira.get("lifecycle_enforce") {
            if v.as_bool() != Some(true) {
                return Err(lifecycle_switch_refusal("spira.lifecycle_enforce", &v.to_string()));
            }
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
    if let Some(spira) = root.get_mut("spira").and_then(|v| v.as_table_mut()) {
        let fields: Vec<String> = spira.keys().cloned().collect();
        for field in fields {
            let Some(rk) = reg.by_field(&field) else {
                spira.remove(&field);
                warnings.push(format!("spira.{field} is not known to this release (written by a newer release?) and ignored"));
                continue;
            };
            let v = &spira[&field];
            if let Some(e) = type_error(&rk.ty, v) {
                return Err(format!("spira.{field}: {e}"));
            }
            if let (Some(n), Some(max)) = (v.as_integer(), rk.max) {
                if n as f64 > max {
                    return Err(format!("spira.{field}: {n} exceeds hard ceiling {max}"));
                }
            }
        }
    }
    let mut unknown_values = BTreeMap::new();
    let mut doc = loop {
        let attempt: Result<SpiraToml, _> = serde_path_to_error::deserialize(root.clone());
        let e = match attempt {
            Ok(doc) => break doc,
            Err(e) => e,
        };
        let path = e.path().to_string();
        let msg = e.inner().to_string();
        if let Some(value) = unknown_variant(&msg) {
            let required = path.starts_with("repo.") && path.ends_with(".mode");
            if !required && remove_at(&mut root, &path) {
                warnings.push(format!(
                    "{path}: unknown value {value:?} (written by a newer release?); ignored until a reader needs it"
                ));
                unknown_values.insert(path, value);
                continue;
            }
        }
        match unknown_key_path(&path, &msg).and_then(|p| remove_key(&mut root, &p).then_some(p)) {
            Some(p) => warnings.push(format!(
                "{p} is not known to this release (written by a newer release?) and ignored"
            )),
            None if path.is_empty() => return Err(msg),
            None => return Err(format!("{path}: {msg}")),
        }
    };
    doc.unknown_values = unknown_values;
    Ok((doc, warnings))
}

/// The value serde refused, from `unknown variant \`v\`, expected ...`.
fn unknown_variant(msg: &str) -> Option<String> {
    Some(msg.strip_prefix("unknown variant `")?.split("`, expected").next()?.to_string())
}

/// Removes the table key or array element at a serde path (`repo.r.gate_mode`, `repo.r.lanes[2]`).
fn remove_at(root: &mut toml::Value, path: &str) -> bool {
    let mut cur = root;
    let mut segs = path.split('.').peekable();
    while let Some(seg) = segs.next() {
        let (name, idx) = match seg.split_once('[') {
            Some((n, rest)) => (n, rest.trim_end_matches(']').parse::<usize>().ok()),
            None => (seg, None),
        };
        let last = segs.peek().is_none();
        if last && idx.is_none() {
            return cur.as_table_mut().is_some_and(|t| t.remove(name).is_some());
        }
        let Some(next) = cur.get_mut(name) else { return false };
        match idx {
            Some(i) if last => {
                return next.as_array_mut().is_some_and(|a| i < a.len() && { a.remove(i); true });
            }
            Some(i) => match next.get_mut(i) {
                Some(v) => cur = v,
                None => return false,
            },
            None => cur = next,
        }
    }
    false
}

/// The dotted path of the key serde refused as an unknown field, from the error's path and
/// message (`unknown field \`k\`, expected ...`). The path may or may not already end in the key.
fn unknown_key_path(path: &str, msg: &str) -> Option<String> {
    let key = msg.strip_prefix("unknown field `")?.split('`').next()?;
    if path.is_empty() {
        Some(key.to_string())
    } else if path == key || path.ends_with(&format!(".{key}")) {
        Some(path.to_string())
    } else {
        Some(format!("{path}.{key}"))
    }
}

fn remove_key(root: &mut toml::Value, path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(last) = parts.pop() else { return false };
    let mut cur = root;
    for p in parts {
        match cur.get_mut(p) {
            Some(v) => cur = v,
            None => return false,
        }
    }
    cur.as_table_mut().is_some_and(|t| t.remove(last).is_some())
}

/// Whether `new` is a SHRINK of `existing` — fewer `[repo.*]` tables, or fewer
/// `[spira].runes` entries — the two counts a converted `spira.toml` can lose without any
/// parse error to show for it, if it is regenerated from a narrower source (a repo-map
/// missing rows, a worktree with only some of the real `runes/*.rune` files) and written
/// over a fuller document already in force. `Some(reason)` names both counts for the
/// caller to report; `None` means `new` carries at least as much as `existing` in both
/// dimensions.
pub fn shrink_reason(existing: &SpiraToml, new: &SpiraToml) -> Option<String> {
    let existing_repos = existing.repo.len();
    let new_repos = new.repo.len();
    let runes = |d: &SpiraToml| d.spira.as_ref().and_then(|s| crate::said(s.get_list("SPIRA_RUNES"))).map_or(0, |r| r.len());
    let (existing_runes, new_runes) = (runes(existing), runes(new));
    if new_repos < existing_repos || new_runes < existing_runes {
        Some(format!(
            "existing has {existing_repos} [repo.*] table(s) and {existing_runes} rune(s); \
             new document has {new_repos} and {new_runes}"
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
/// own copy of (sp-6onps: `dwarves.sh`, now the `dwarves` binary in `spira-world`, is exactly
/// that caller). Kept here, not duplicated at the call site, so `config-fence`'s "only
/// spira-config names or parses the config" contract holds without a new allow-list entry.
pub fn serialize_and_write(path: &std::path::Path, doc: &SpiraToml) -> Result<(), String> {
    if let Some((k, v)) = doc.unknown_values.iter().next() {
        return Err(format!("{}: refusing to rewrite a file holding {k} = {v:?}, a value this release does not know; edit it with set_paths_in_file", path.display()));
    }
    let text = toml::to_string_pretty(doc).map_err(|e| format!("{}: {e}", path.display()))?;
    write_atomic(path, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads one dotted path out of an already-validated document — `spira.max_dwarves`,
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
    spira_value_map(doc, false)
}

/// Every DECLARED `[spira]` value as a string — an explicitly empty string or list is
/// declared (""), only an absent field is not. `include_secrets`: `resolve` reads the whole
/// document (a value it cannot see is a value it would refuse as undeclared); the shell
/// export ([`spira_string_map`]) still leaves secret-shaped keys out.
pub fn spira_value_map(doc: &SpiraToml, include_secrets: bool) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(spira) = &doc.spira else {
        return out;
    };
    for (key, val) in &spira.0 {
        if !include_secrets && is_secret_shaped(key) {
            continue;
        }
        let shell_val = match val {
            toml::Value::String(s) => s.clone(),
            toml::Value::Boolean(b) => b.to_string(),
            toml::Value::Integer(n) => n.to_string(),
            toml::Value::Array(items) => {
                if items.is_empty() && !include_secrets {
                    continue;
                }
                items
                    .iter()
                    .map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            _ => continue,
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

/// Sets one dotted path (`spira.prod`, `spira.max_live_dwarves`) to a leaf JSON value on `doc`.
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
    if let ["spira", field] = segs.as_slice() {
        let reg = registry::for_process()?;
        let rk = reg.by_field(field).ok_or_else(|| format!("spira.{field}: unknown field `{field}`"))?;
        let table = json.as_object_mut().expect("a document is an object").entry("spira").or_insert_with(|| serde_json::Value::Object(Default::default()));
        if table.is_null() {
            *table = serde_json::Value::Object(Default::default());
        }
        let table = table.as_object_mut().ok_or_else(|| "spira: not a table".to_string())?;
        if leaf_value.is_null() {
            table.remove(*field);
        } else {
            let v: toml::Value = serde_json::from_value(leaf_value.clone()).map_err(|e| format!("spira.{field}: {e}"))?;
            if let Some(e) = type_error(&rk.ty, &v) {
                return Err(format!("spira.{field}: {e}"));
            }
            table.insert(field.to_string(), leaf_value);
        }
        return serde_json::from_value(json).map_err(|e| e.to_string());
    }
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

/// `text` with only the keys that differ between `old` and `new` changed: every other line —
/// comments, order, quoting, empty strings — stays byte-identical. A whole-file re-serialise
/// rewrites keys the caller never named.
pub fn edit_text(text: &str, old: &SpiraToml, new: &SpiraToml) -> Result<String, String> {
    let mut edited: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let old_v = toml::Value::try_from(old).map_err(|e| e.to_string())?;
    let new_v = toml::Value::try_from(new).map_err(|e| e.to_string())?;
    let mut path = Vec::new();
    diff_apply(edited.as_table_mut(), &mut path, Some(&old_v), Some(&new_v))?;
    Ok(edited.to_string())
}

fn diff_apply(
    root: &mut toml_edit::Table,
    path: &mut Vec<String>,
    old: Option<&toml::Value>,
    new: Option<&toml::Value>,
) -> Result<(), String> {
    if old == new {
        return Ok(());
    }
    if let (Some(toml::Value::Table(o)), Some(toml::Value::Table(n))) = (old, new) {
        let keys: std::collections::BTreeSet<&String> = o.keys().chain(n.keys()).collect();
        for k in keys {
            path.push(k.clone());
            diff_apply(root, path, o.get(k), n.get(k))?;
            path.pop();
        }
        return Ok(());
    }
    let Some((leaf, parents)) = path.split_last() else { return Ok(()) };
    let mut cur: &mut dyn toml_edit::TableLike = root;
    for seg in parents {
        if new.is_none() && cur.get(seg).is_none() {
            return Ok(());
        }
        let entry = cur.entry(seg).or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        });
        cur = entry.as_table_like_mut().ok_or_else(|| format!("{}: not a table", parents.join(".")))?;
    }
    let Some(new) = new else {
        cur.remove(leaf);
        return Ok(());
    };
    let mut wrap = toml::map::Map::new();
    wrap.insert(leaf.clone(), new.clone());
    let doc: toml_edit::DocumentMut =
        toml::to_string(&wrap).map_err(|e| e.to_string())?.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let mut item = doc.as_table().get(leaf).cloned().ok_or("lost the value")?;
    if let (Some(prev), toml_edit::Item::Value(v)) = (cur.get(leaf).and_then(|i| i.as_value()), &mut item) {
        *v.decor_mut() = prev.decor().clone();
    }
    cur.insert(leaf, item);
    Ok(())
}

/// Sets one dotted path (`spira.prod`, `spira.instance`, `spira.max_live_dwarves`) to a value
/// given as a plain command-line string, for `spira-config set` — the writer `deploy.sh`,
/// `install.sh` and `conf.sh`'s own `spira_config_set` shell helper use once nothing writes
/// `spira.conf`'s `KEY=value` lines anymore. Goes through a JSON round-trip rather than a
/// hand-written per-field match arm, so a field this schema already knows needs no writer of
/// its own; `deny_unknown_fields` still refuses a path this schema does not carry, via the
/// same `serde` deserialize the rest of this crate validates through.
///
/// TRIES THE VALUE AS A STRING FIRST, because most fields here are paths and names, and a
/// literal like `"3"` must stay the string `"3"` when the field is one of those — only a
/// field the schema itself types as a number or bool (`max_live_dwarves`, `queue_batch_max`, ...)
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
/// retiring a key (`dwarves.sh unset`'s fleet ceiling) without leaving an empty string behind.
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

/// `text` without the retired key at `path`, every other line untouched.
pub fn remove_retired_text(text: &str, path: &str) -> Result<String, String> {
    let mut edited: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let mut cur: &mut dyn toml_edit::TableLike = edited.as_table_mut();
    let segs: Vec<&str> = path.split('.').collect();
    let Some((leaf, parents)) = segs.split_last() else { return Ok(text.to_string()) };
    for seg in parents {
        match cur.get_mut(seg).and_then(|i| i.as_table_like_mut()) {
            Some(t) => cur = t,
            None => return Ok(text.to_string()),
        }
    }
    cur.remove(leaf);
    Ok(edited.to_string())
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
    let before = if text.trim().is_empty() { SpiraToml::default() } else { validate(&text)? };
    let mut doc = before.clone();
    for (path, value) in pairs {
        doc = set_path(&doc, path, value)?;
    }
    let out = edit_text(&text, &before, &doc).map_err(|e| format!("{}: {e}", file.display()))?;
    validate(&out)?;
    write_atomic(file, &out).map_err(|e| format!("{}: {e}", file.display()))
}

/// The complete fixture `spira.toml` (`spira/testlib/complete.toml`: every key declared, with
/// fixture paths) — what a test resolves against now that nothing is defaulted. `SPIRA_<KEY>`
/// pairs in `declare` are written as `spira.<key>`, the only place a value can come from.
#[cfg(test)]
pub(crate) fn fixture_doc(declare: &std::collections::BTreeMap<String, String>) -> SpiraToml {
    let mut d = validate(&process::complete_fixture_text()).expect("the complete fixture validates");
    for (k, v) in declare {
        if let Some(rest) = k.strip_prefix("SPIRA_") {
            if let Ok(n) = set_path(&d, &format!("spira.{}", rest.to_ascii_lowercase()), v) {
                d = n;
            }
        }
    }
    d
}

/// [`fixture_doc`] written to `<dir>/spira.toml`, its path returned: for a test that resolves
/// through a process-level entry point, which reads only the file `SPIRA_TOML` names.
#[cfg(test)]
pub(crate) fn fixture_toml_file(dir: &std::path::Path, declare: &std::collections::BTreeMap<String, String>) -> PathBuf {
    let p = dir.join(FILE_NAME);
    serialize_and_write(&p, &fixture_doc(declare)).expect("the fixture writes");
    p
}

#[cfg(test)]
mod tests {

    // ---- layered config (one source, base:override) ----
    fn layer(dir: &std::path::Path, name: &str, text: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p.display().to_string()
    }

    #[test]
    fn an_override_changes_only_its_own_keys() {
        let d = testkit::TempDir::new("spira-config-layer-keys");
        let base = layer(d.path(), "base.toml", "[spira]\nid_prefix = \"sp\"\nlanes_max_live = 2\n");
        let over = layer(d.path(), "t.override.toml", "[spira]\nlanes_max_live = 5\n");
        let doc = load(std::path::Path::new(&format!("{base}:{over}"))).unwrap();
        let m = spira_string_map(&doc);
        assert_eq!(m.get("LANES_MAX_LIVE").map(String::as_str), Some("5"), "the override wins");
        assert_eq!(m.get("ID_PREFIX").map(String::as_str), Some("sp"), "the base fills the rest");
    }

    #[test]
    fn nested_tables_merge_key_by_key() {
        let d = testkit::TempDir::new("spira-config-layer-nested");
        let base = layer(d.path(), "base.toml", "[repo.alpha]\npath = \"/a\"\nmode = \"queue.local\"\nbase = \"local/main\"\n");
        let over = layer(d.path(), "o.toml", "[repo.alpha]\npath = \"/b\"\n");
        let doc = load(std::path::Path::new(&format!("{base}:{over}"))).unwrap();
        let rows = repos::rows_from_toml(&doc);
        assert_eq!(rows.iter().find(|r| r.name == "alpha").map(|r| r.path.as_str()), Some("/b"));
        assert_eq!(rows.iter().find(|r| r.name == "alpha").map(|r| r.base.as_str()), Some("local/main"), "unset keys keep the base's value");
    }

    #[test]
    fn a_missing_layer_is_refused_naming_it() {
        let d = testkit::TempDir::new("spira-config-layer-missing");
        let base = layer(d.path(), "base.toml", "[spira]\n");
        let gone = d.path().join("gone.toml").display().to_string();
        let err = load(std::path::Path::new(&format!("{base}:{gone}"))).unwrap_err();
        assert!(err.contains("gone.toml"), "{err}");
    }
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
    fn the_retired_lifecycle_switch_refuses_off_and_warns_on() {
        assert_eq!(check_lifecycle_switch_env(None), Ok(None));
        assert_eq!(check_lifecycle_switch_env(Some("")), Ok(None));
        assert!(check_lifecycle_switch_env(Some("1")).unwrap().unwrap().contains("retired"));
        assert!(check_lifecycle_switch_env(Some("true")).unwrap().is_some());
        for off in ["0", "false", "yes", "TRUE"] {
            let e = check_lifecycle_switch_env(Some(off)).unwrap_err();
            assert!(e.contains("sp-v62vn") && e.contains("Exit: remove it"), "{e}");
        }
        let (_, w) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nlifecycle_enforce = true\n").unwrap();
        assert!(w.iter().any(|w| w.contains("lifecycle_enforce is retired (sp-v62vn)")), "{w:?}");
        let e = validate("[spira]\nid_prefix = \"sp\"\nlifecycle_enforce = false\n").unwrap_err();
        assert!(e.contains("spira.lifecycle_enforce") && e.contains("no off") && e.contains("Exit:"), "{e}");
        assert!(validate("[spira]\nlifecycle_enforce = \"off\"\n").is_err());
    }

    #[test]
    fn valid_minimal_document() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmax_dwarves = 4\n").expect("valid");
        assert_eq!(doc.spira.unwrap().get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
    }

    #[test]
    fn unknown_key_names_its_path() {
        let (_, w) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nbogus = 1\n").unwrap();
        assert!(w.iter().any(|m| m.starts_with("spira.bogus") && m.contains("newer release")), "{w:?}");
        let err = validate("[spira]\nid_prefix = \"sp\"\nmax_dwarves = \"four\"\nbogus = 1\n").unwrap_err();
        assert!(err.starts_with("spira.max_dwarves"), "{err}");
    }

    #[test]
    fn wrong_type_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nmax_dwarves = \"four\"\n").unwrap_err();
        assert!(err.starts_with("spira.max_dwarves"), "{err}");
    }

    #[test]
    fn a_wrongly_typed_enum_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nczar_stage_deadlock = 3\n").unwrap_err();
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
        assert_eq!(doc.spira.unwrap().get_str("SPIRA_DB").unwrap(), Some("/home/x".to_string()));
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
        let doc = validate("[spira]\nid_prefix = \"sp\"\nhome_repo = \"a b\"\nmax_dwarves = 4\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("HOME_REPO='a b'\n"), "{out}");
        assert!(out.contains("MAX_DWARVES='4'\n"), "{out}");
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
        assert_eq!(doc.spira.unwrap().get_str("SPIRA_PROD").unwrap(), Some("/srv/x".to_string()));
    }

    #[test]
    fn set_path_replaces_without_disturbing_siblings() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nprod = \"/old\"\nmax_dwarves = 4\n").unwrap();
        let doc = set_path(&doc, "spira.prod", "/new").unwrap();
        let spira = doc.spira.unwrap();
        assert_eq!(spira.get_str("SPIRA_PROD").unwrap(), Some("/new".to_string()));
        assert_eq!(spira.get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
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
        let (_, w) = validate_with_warnings("[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngatee = 1\n").unwrap();
        assert!(w.iter().any(|m| m.starts_with("repo.spira.gatee") && !m.contains("retired")), "{w:?}");
    }

    #[test]
    fn unsetting_a_retired_key_removes_it_instead_of_refusing() {
        let text = "[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngate = \"bash spira/x.sh\"\n";
        let doc = validate(text).unwrap();
        let out = toml::to_string_pretty(&unset_path(&doc, "repo.spira.gate").unwrap()).unwrap();
        assert!(!out.contains("gate"), "{out}");
        assert!(out.contains("path = \"/p\""), "{out}");
        assert!(unset_path(&doc, "spira.aeon_cpu_quota").is_ok()); // ffx-ok
        // Positive control: an unknown, never-retired key is still refused.
        assert!(unset_path(&doc, "repo.spira.gatee").is_err());
    }

    #[test]
    fn retired_cpu_quota_keys_warn_and_are_ignored() {
        // sp-b4oct: a live spira.toml still carrying the dwarf/landing CPU quota validates,
        // with one warning per key naming the retiring bead, and nothing exports them.
        let (doc, warnings) = validate_with_warnings(
            "[spira]\nid_prefix = \"sp\"\naeon_cpu_quota = \"400\"\nland_cpu_quota = \"70\"\n",
        )
        .expect("a retired CPU quota key must not be a hard error");
        for key in ["aeon_cpu_quota", "land_cpu_quota"] { // ffx-ok
            assert!(
                warnings.iter().any(|w| w.contains(key) && w.contains("sp-b4oct")),
                "no warning for {key}: {warnings:?}"
            );
        }
        let sh = export_sh(&doc);
        assert!(!sh.contains("CPU_QUOTA"), "a retired key leaked into export --sh: {sh}");
    }

    #[test]
    fn an_unknown_enum_value_loads_with_a_warning_and_refuses_only_its_reader() {
        let doc_text = "[spira]\nid_prefix = \"sp\"\nczar_stage_deadlock = \"someday\"\n[repo.r]\npath = \"/p\"\nmode = \"push\"\nlanes = [\"plan\", \"future-lane\"]\ngate_mode = \"some-future-mode\"\n";
        let (doc, w) = validate_with_warnings(doc_text).unwrap();
        for k in ["repo.r.gate_mode", "repo.r.lanes[1]", "spira.czar_stage_deadlock"] {
            assert!(w.iter().any(|m| m.starts_with(k)), "{k}: {w:?}");
        }
        assert_eq!(doc.repo["r"].lanes, vec![Lane::Plan]);
        let err = repo_gate_mode(&doc, "r").unwrap_err();
        assert!(err.contains("repo.r.gate_mode") && err.contains("some-future-mode"), "{err}");
        assert_eq!(repo_gate_mode(&doc, "other").unwrap(), GateMode::Suites);
        let err = validate("[repo.r]\npath = \"/p\"\nmode = \"teleport\"\n").unwrap_err();
        assert!(err.contains("repo.r.mode") && err.contains("teleport"), "{err}");
    }

    #[test]
    fn a_rewrite_never_drops_a_value_this_release_does_not_know() {
        let text = format!("{REPO_R}gate_mode = \"some-future-mode\"\n");
        let doc = validate(&text).unwrap();
        let edited = edit_text(&text, &doc, &set_path(&doc, "repo.r.format", "x").unwrap()).unwrap();
        assert!(edited.contains("some-future-mode") && edited.contains("format"), "{edited}");
        let dir = testkit::TempDir::new("unknown-value");
        let p = dir.path().join("spira.toml");
        let err = serialize_and_write(&p, &doc).unwrap_err();
        assert!(err.contains("repo.r.gate_mode") && !p.exists(), "{err}");
    }

    #[test]
    fn a_misspelt_key_warns_as_unknown_but_a_bad_known_value_errors() {
        let (_, w) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nqueue_local_gatee = 1\n").unwrap();
        assert!(w.iter().any(|m| m.starts_with("spira.queue_local_gatee") && !m.contains("retired")), "{w:?}");
        let err = validate("[spira]\nid_prefix = \"sp\"\nqueue_local_gatee = 1\nmax_dwarves = true\n").unwrap_err();
        assert!(err.starts_with("spira.max_dwarves"), "{err}");
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
        let doc = set_path(&SpiraToml::default(), "spira.max_live_dwarves", "3").unwrap();
        assert_eq!(doc.spira.unwrap().get_u32("SPIRA_MAX_LIVE_DWARVES").unwrap(), Some(3));
    }

    #[test]
    fn set_path_coerces_a_bool_field() {
        let doc = set_path(&SpiraToml::default(), "spira.mail_mute", "true").unwrap();
        assert_eq!(doc.spira.unwrap().get_bool("SPIRA_MAIL_MUTE").unwrap(), Some(true));
    }

    #[test]
    fn set_path_keeps_a_numeric_looking_string_field_a_string() {
        // `instance` is a string field; a value that happens to parse as JSON must not be
        // coerced away from the string the schema actually wants.
        let doc = set_path(&SpiraToml::default(), "spira.instance", "123").unwrap();
        assert_eq!(doc.spira.unwrap().get_str("SPIRA_INSTANCE").unwrap(), Some("123".to_string()));
    }

    #[test]
    fn unset_path_removes_the_key_and_keeps_siblings() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmax_live_dwarves = 3\nmax_dwarves = 4\n").unwrap();
        let doc = unset_path(&doc, "spira.max_live_dwarves").unwrap();
        // omitted entirely from the serialized document, not written as an empty value.
        let out = toml::to_string_pretty(&doc).unwrap();
        assert!(!out.contains("max_live_dwarves"), "{out}");
        let spira = doc.spira.unwrap();
        assert_eq!(spira.get_u32("SPIRA_MAX_LIVE_DWARVES").unwrap(), None);
        assert_eq!(spira.get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
    }

    // Env edits go through `testkit::env`, which serializes every test in this binary.
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
        let _env = testkit::env(&[("SPIRA_TOML", Some("/should-not-be-used/spira.toml"))]);
        let explicit = PathBuf::from("/explicit/spira.toml");
        assert_eq!(discover(Some(explicit.clone())), Some(explicit));
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
        let dir = scratch_dir("discover-repo");
        let repo_dir = dir.join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(repo_dir.join(FILE_NAME), "[spira]\n").unwrap();
        let empty_home = dir.join("home-empty");
        std::fs::create_dir_all(&empty_home).unwrap();
        let _env = testkit::env(&[
            ("SPIRA_TOML", None),
            ("XDG_CONFIG_HOME", None),
            ("SPIRA_REPO", repo_dir.to_str()),
            ("HOME", empty_home.to_str()),
        ]);
        assert_eq!(discover(None), None);
    }

    #[test]
    fn load_reads_and_validates() {
        let dir = scratch_dir("load-ok");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[spira]\nid_prefix = \"sp\"\nmax_dwarves = 4\n").unwrap();
        let doc = load(&path).expect("valid document");
        assert_eq!(doc.spira.unwrap().get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
    }

    #[test]
    fn load_names_the_path_on_a_parse_error() {
        let dir = scratch_dir("load-bad");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[spira]\nid_prefix = \"sp\"\nmax_dwarves = true\n").unwrap();
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
        assert_eq!(repo_gate_mode(&doc, "r").unwrap(), GateMode::Suites);
        assert_eq!(repo_gate_mode(&doc, "no-such-repo").unwrap(), GateMode::Suites);
        assert_eq!(get_path(&doc, "repo.r.gate_mode"), None);
    }

    #[test]
    fn gate_mode_reads_every_value() {
        for (v, want) in [("unit", GateMode::Unit), ("suites", GateMode::Suites), ("fences", GateMode::Fences)] {
            let doc = validate(&format!("{REPO_R}gate_mode = \"{v}\"\n")).unwrap();
            assert_eq!(repo_gate_mode(&doc, "r").unwrap(), want);
            assert_eq!(want.as_str(), v);
            assert_eq!(get_path(&doc, "repo.r.gate_mode").as_deref(), Some(v));
        }
    }

    #[test]
    fn gate_mode_refuses_anything_else() {
        for bad in ["\"Unit\"", "\"fast\"", "\"Fences\"", "\"fence\"", "\"\""] {
            let doc = validate(&format!("{REPO_R}gate_mode = {bad}\n")).unwrap();
            let err = repo_gate_mode(&doc, "r").unwrap_err();
            assert!(err.contains("repo.r.gate_mode"), "{bad}: {err}");
        }
        for bad in ["true", "1"] {
            let err = validate(&format!("{REPO_R}gate_mode = {bad}\n")).unwrap_err();
            assert!(err.contains("repo.r.gate_mode"), "{bad}: {err}");
        }
    }

    #[test]
    fn gate_mode_is_set_and_reverted_through_set_path() {
        let doc = validate(REPO_R).unwrap();
        let on = set_path(&doc, "repo.r.gate_mode", "unit").unwrap();
        assert_eq!(repo_gate_mode(&on, "r").unwrap(), GateMode::Unit);
        let text = toml::to_string_pretty(&on).unwrap();
        assert!(text.contains("gate_mode = \"unit\""), "{text}");
        assert!(set_path(&doc, "repo.r.gate_mode", "fast").is_err());
        let off = set_path(&on, "repo.r.gate_mode", "suites").unwrap();
        assert_eq!(repo_gate_mode(&off, "r").unwrap(), GateMode::Suites);
        let gone = unset_path(&on, "repo.r.gate_mode").unwrap();
        assert_eq!(repo_gate_mode(&gone, "r").unwrap(), GateMode::Suites);
        let fences = set_path(&doc, "repo.r.gate_mode", "fences").unwrap();
        assert_eq!(repo_gate_mode(&fences, "r").unwrap(), GateMode::Fences);
        assert!(toml::to_string_pretty(&fences).unwrap().contains("gate_mode = \"fences\""));
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
        assert_eq!(s.get_str("SPIRA_ID_PREFIX").unwrap().as_deref(), Some("sp"));
        assert_eq!(s.get_str("SPIRA_DB").unwrap().as_deref(), Some("/db"), "the rest of [spira] survives");
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
        for name in ["home_repo", "max_dwarves", "gate", "path"] {
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
