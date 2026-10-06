//! Typed views of the snapshot: the persona (`Fayth`) and conf.sh's resolution (`Conf`),
//! with aeon.sh's defaults for every key it defaulted inline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::seam::Snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPrompt {
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fayth {
    pub name: String,
    pub labels: String,
    pub exclude_labels: String,
    pub max_concurrent: u32,
    pub elastic: bool,
    pub lease_minutes: Option<u32>,
    pub heartbeat_seconds: u64,
    pub timeout_seconds: Option<u64>,
    pub memory_prefixes: String,
    pub statute_core: String,
    pub tools: String,
    pub project_instructions: String,
    pub system_prompt: SystemPrompt,
    pub groom_escalation_check: bool,
    pub graph_only: bool,
}

fn get<'a>(m: &'a BTreeMap<String, String>, k: &str) -> Option<&'a str> {
    m.get(k).map(|s| s.as_str()).filter(|s| !s.is_empty())
}

fn num<T: std::str::FromStr>(m: &BTreeMap<String, String>, k: &str) -> Option<T> {
    get(m, k).and_then(|s| s.trim().parse().ok())
}

impl Fayth {
    pub fn from_vars(name: &str, v: &BTreeMap<String, String>) -> Fayth {
        Fayth {
            name: name.to_string(),
            labels: get(v, "FAYTH_LABELS").unwrap_or("").to_string(),
            exclude_labels: get(v, "FAYTH_EXCLUDE_LABELS").unwrap_or("").to_string(),
            max_concurrent: num(v, "FAYTH_MAX_CONCURRENT").unwrap_or(1),
            elastic: get(v, "FAYTH_ELASTIC") == Some("1"),
            lease_minutes: num(v, "FAYTH_LEASE_MINUTES"),
            heartbeat_seconds: num(v, "FAYTH_HEARTBEAT_SECONDS").unwrap_or(30),
            timeout_seconds: num(v, "FAYTH_TIMEOUT_SECONDS"),
            memory_prefixes: get(v, "FAYTH_MEMORY_PREFIXES").unwrap_or("law-").to_string(),
            statute_core: get(v, "FAYTH_STATUTE_CORE").unwrap_or("").to_string(),
            tools: get(v, "FAYTH_TOOLS").unwrap_or("Bash,Read,Edit,Write,Glob,Grep").to_string(),
            project_instructions: get(v, "FAYTH_PROJECT_INSTRUCTIONS").unwrap_or("").to_string(),
            system_prompt: if get(v, "FAYTH_SYSTEM_PROMPT") == Some("replace") { SystemPrompt::Replace } else { SystemPrompt::Append },
            groom_escalation_check: get(v, "FAYTH_GROOM_ESCALATION_CHECK") == Some("1"),
            graph_only: get(v, "FAYTH_GRAPH_ONLY") == Some("1"),
        }
    }

    /// `fayth_lease_seconds`: minutes × 60, 10 minutes when undeclared.
    pub fn lease_seconds(&self) -> i64 {
        self.lease_minutes.unwrap_or(10) as i64 * 60
    }

    /// "Builder <builder@spira>" — `${FAYTH^}`.
    pub fn mail_from(&self) -> String {
        let mut c = self.name.chars();
        let cap = match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => String::new(),
        };
        format!("{cap} <{}@spira>", self.name)
    }
}

/// `fayth_fenced`: Ok, or the two log lines it prints before refusing.
pub fn fenced(name: &str, labels: &str, scope: &str) -> Result<(), Vec<String>> {
    if labels.is_empty() {
        return Err(vec![format!("FENCE {name}: FAYTH_LABELS is empty — that predicate selects the whole database.")]);
    }
    if scope.is_empty() || labels.split(',').any(|l| l == scope) {
        return Ok(());
    }
    Err(vec![
        format!("FENCE {name}: FAYTH_LABELS='{labels}' does not require '{scope}' (SPIRA_SCOPE_LABEL)."),
        format!("FENCE {name}: Add '{scope}' to it, or set SPIRA_SCOPE_LABEL= to allow unrestricted scope."),
    ])
}

/// conf.sh's resolution, with aeon.sh's inline defaults.
#[derive(Debug, Clone, Default)]
pub struct Conf {
    pub v: BTreeMap<String, String>,
    pub home: PathBuf,
    pub run: PathBuf,
    /// The repo registry (`spira_config::repos`, sp-37rmg "wave 4.11"), built once from this
    /// same snapshot instead of shelling to `repo_root`/`repo_land`/`spira_home_repo` per
    /// call — the biggest wall-clock win the whole family offers, per wave4-decomposition.md's
    /// own cost note. `SPIRA_REPO_MAP`'s file is read here, in-process, rather than by `awk`
    /// inside a fresh `bash` seam call.
    pub repos: spira_config::repos::Registry,
}

impl Conf {
    pub fn new(snap: &Snapshot, home: &Path) -> Conf {
        let run = PathBuf::from(snap.vars.get("SPIRA_RUN").cloned().unwrap_or_default());
        let map_text = snap
            .vars
            .get("SPIRA_REPO_MAP")
            .filter(|p| !p.is_empty())
            .and_then(|p| std::fs::read_to_string(p).ok());
        let repos = spira_config::repos::Registry::new(map_text.as_deref(), &snap.vars, home);
        Conf { v: snap.vars.clone(), home: home.to_path_buf(), run, repos }
    }
    /// A REGISTERED key's declared value — never a default (per Ryan 2026-10-05, round 2:
    /// a missing key is an error, never a value — `Conf::i`'s old `unwrap_or(0)` silently
    /// turned a missing `SPIRA_CAPACITY_PROBE_WINDOW` into 0 and broke a `left >
    /// probe_window` check in production shape; that was a default in disguise). `Conf`
    /// may only ever be built from a COMPLETE resolved config — production's
    /// `merge_resolved_config` (every registered key, via `resolve_for_process`), tests'
    /// `complete_vars()` — so a key truly ABSENT here is an invariant violation in how
    /// this `Conf` was built, not a legitimate "unset", and panics naming it. A key
    /// PRESENT with a declared empty string (a real, resolved "" — several registered
    /// keys mean exactly that, e.g. SPIRA_SCOPE_LABEL's "no scope exclusion") is not
    /// missing and passes straight through. A NON-registered key (SPIRA_TOML_FILE,
    /// SPIRA_WORKFLOW_RUN_CONSIDERED, ...) must go through [`Conf::or`]/[`Conf::n`]
    /// instead — those are the ones that may legitimately be absent.
    pub fn s(&self, k: &str) -> String {
        match self.v.get(k) {
            Some(v) => v.clone(),
            None => panic!("Conf::s({k:?}): key is absent from a Conf that must be built from a complete config — this is an invariant violation, not a value"),
        }
    }
    /// A key that MAY legitimately be absent (never registered, or an ad hoc override) —
    /// `d` on absence or on a declared-but-empty value alike, matching conf.sh's own
    /// `${VAR:=d}` (unset or empty both take the default). Never use this for a
    /// registered key just to dodge [`Conf::s`]'s panic — register the gap in the test
    /// fixture instead.
    pub fn or(&self, k: &str, d: &str) -> String {
        get(&self.v, k).unwrap_or(d).to_string()
    }
    /// [`Conf::or`]'s numeric twin, for a key that may legitimately be absent.
    pub fn n(&self, k: &str, d: i64) -> i64 {
        num(&self.v, k).unwrap_or(d)
    }
    /// [`Conf::s`]'s numeric twin: a REGISTERED key's declared value, parsed — never a
    /// default. Absent is the same invariant violation `Conf::s` panics on; present but
    /// not a valid integer is a malformed config, also named and panicked on, never a
    /// silent 0.
    pub fn i(&self, k: &str) -> i64 {
        match self.v.get(k) {
            Some(v) => v.trim().parse().unwrap_or_else(|e| panic!("Conf::i({k:?}): {v:?} does not parse as an integer: {e}")),
            None => panic!("Conf::i({k:?}): key is absent from a Conf that must be built from a complete config — this is an invariant violation, not a value"),
        }
    }
    /// Whether a key that may legitimately be absent is explicitly set and non-empty —
    /// "false" for absence is this accessor's actual job (an ad hoc override's own
    /// presence check), not a disguised default.
    pub fn set_nonempty(&self, k: &str) -> bool {
        get(&self.v, k).is_some()
    }
    pub fn db(&self) -> String {
        self.s("SPIRA_DB")
    }
    pub fn ask_label(&self) -> String {
        self.s("SPIRA_ASK_LABEL")
    }
    /// No Rust-side default: `spira/conf.d/SPIRA_SUBMITTED_LABEL` already defaults this
    /// to "spira-submitted", supplied through `self.v` the same way every other
    /// registered key is (per Ryan 2026-10-05: one source of config).
    pub fn submitted_label(&self) -> String {
        self.s("SPIRA_SUBMITTED_LABEL")
    }
    pub fn overlay(&self) -> PathBuf {
        PathBuf::from(self.s("SPIRA_CHAMBER_OVERLAY"))
    }
    pub fn trace_mark(&self) -> String {
        self.or("SPIRA_TRACE_MARK", "=== spira attempt")
    }
    /// No Rust-side default: SPIRA_AGENT is registered (its `spira/conf.d` entry is
    /// PROCEDURAL — conf-gen.sh could not auto-extract conf.sh's own inline default — but
    /// the resolved document still carries a real value, same as production's own
    /// spira.toml).
    pub fn agent(&self) -> String {
        self.s("SPIRA_AGENT")
    }
    pub fn ledger(&self) -> PathBuf {
        self.run.join("aeon-ledger.log")
    }
    // ---- capacity pause (family K, wave 4.26) -----------------------------------------
    //
    // SPIRA_CAPACITY_PAUSE/_BACKOFF/_PROBE_LAST/_WITHDRAWN are lib.sh literals, not
    // spira-config registry keys (wave4-decomposition.md (b): "ad hoc overrides... never
    // conf.sh's"), same footing as SPIRA_TRACE_MARK above. An operator override reaches
    // `Conf` because `main.rs` merges these four names out of the process environment
    // explicitly (`merge_capacity_env`) — there is no bash seam round trip to ask for them
    // any more now that this crate owns the logic.

    pub fn capacity_pause(&self) -> PathBuf {
        get(&self.v, "SPIRA_CAPACITY_PAUSE").map(PathBuf::from).unwrap_or_else(|| self.run.join("capacity-pause"))
    }
    pub fn capacity_backoff(&self) -> i64 {
        self.n("SPIRA_CAPACITY_BACKOFF", 900)
    }
    pub fn capacity_probe_last(&self) -> PathBuf {
        get(&self.v, "SPIRA_CAPACITY_PROBE_LAST").map(PathBuf::from).unwrap_or_else(|| self.run.join("capacity-probe-last"))
    }
    pub fn capacity_withdrawn(&self) -> PathBuf {
        get(&self.v, "SPIRA_CAPACITY_WITHDRAWN").map(PathBuf::from).unwrap_or_else(|| self.run.join("capacity-withdrawn"))
    }
    /// SPIRA_CAPACITY_PROBE_WINDOW/_INTERVAL/_TIMEOUT: registered spira-config keys
    /// (`spira/conf.d/SPIRA_CAPACITY_PROBE_*`), so these DO come through `resolve()`.
    pub fn capacity_probe_window(&self) -> i64 {
        self.i("SPIRA_CAPACITY_PROBE_WINDOW")
    }
    pub fn capacity_probe_interval(&self) -> i64 {
        self.i("SPIRA_CAPACITY_PROBE_INTERVAL")
    }
    pub fn capacity_probe_timeout(&self) -> u64 {
        self.i("SPIRA_CAPACITY_PROBE_TIMEOUT").max(1) as u64
    }
    /// `capacity_probe`'s own default: no literal fallback — the builder persona's
    /// resolved model, so a model change never needs a second edit here.
    pub fn capacity_probe_model(&self) -> String {
        // `.or(_, "")`, not the strict `Conf::s`: this accessor's own contract is "unset
        // (or empty) falls through to the persona's resolved model" — that fallthrough is
        // this function's explicit business logic, not `Conf` silently defaulting, so an
        // absent SPIRA_CAPACITY_PROBE_MODEL (a registered key, but one some test fixtures
        // narrower than `complete_vars()` still omit on purpose, to exercise exactly this
        // fallthrough) must not panic here. SPIRA_TOML_FILE is not registered at all.
        let m = self.or("SPIRA_CAPACITY_PROBE_MODEL", "");
        if !m.is_empty() {
            return m;
        }
        persona_model("builder", &self.s("SPIRA_TOML")).unwrap_or_else(|e| {
            eprintln!("aeon: FATAL: {e}");
            std::process::exit(1)
        })
    }
}

/// Wave 4.26: an explicit env override for the four `SPIRA_CAPACITY_*` lib.sh literals
/// reaches `Conf` the same way `SPIRA_TRACE_MARK` always has, without asking the bash
/// seam for them (there is no bash implementation left to ask).
pub fn merge_capacity_env(snap: &mut crate::seam::Snapshot, env: &BTreeMap<String, String>) {
    for k in ["SPIRA_CAPACITY_PAUSE", "SPIRA_CAPACITY_BACKOFF", "SPIRA_CAPACITY_PROBE_LAST", "SPIRA_CAPACITY_WITHDRAWN"] {
        if let Some(v) = env.get(k) {
            snap.vars.entry(k.to_string()).or_insert_with(|| v.clone());
        }
    }
}

/// `persona.<name>.model`, from the one source of config: the spec `$SPIRA_TOML` names, layers
/// and all (per Ryan 2026-10-05). Undeclared is a refusal naming the key — never a built-in
/// model, which once launched every persona on `claude-opus-5` whatever its config said.
pub fn persona_model(name: &str, spec: &str) -> Result<String, String> {
    if spec.is_empty() {
        return Err("SPIRA_TOML is not set — it names the one source of config".into());
    }
    let doc = spira_config::load(Path::new(&spec))?;
    doc.persona
        .get(name)
        .map(|p| p.model.clone())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| format!("persona.{name}.model is not declared in {spec}"))
}

/// Where the harness's `spira/` directory is (DESIGN.md §2.1).
pub fn resolve_home(flag: Option<&str>, env: &BTreeMap<String, String>, exe: Option<&Path>) -> Option<PathBuf> {
    let ok = |p: &Path| p.join("lib.sh").is_file() && p.join("chamber").is_dir();
    if let Some(f) = flag {
        let p = PathBuf::from(f);
        return ok(&p).then_some(p);
    }
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(h) = env.get("SPIRA_HOME").filter(|s| !s.is_empty()) {
        cands.push(PathBuf::from(h));
    }
    let cfg = env
        .get("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| env.get("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(c) = cfg {
        if let Ok(t) = std::fs::read_to_string(c.join("spira").join("harness")) {
            let t = t.trim();
            if !t.is_empty() {
                cands.push(PathBuf::from(t).join("spira"));
            }
        }
    }
    if let Some(dir) = exe.and_then(|e| e.parent()) {
        cands.push(dir.join("../spira"));
        cands.push(dir.join("../../spira"));
    }
    cands.into_iter().find(|p| ok(p)).map(|p| p.canonicalize().unwrap_or(p))
}

/// Wave 4.8 ("retire conf re-import seams in Rust"): `_aeon_snapshot`'s bash seam used to
/// read every `seam::RETIRED_SNAPSHOT_VARS` name back out of the subprocess that had just
/// sourced `lib.sh`/`conf.sh` — a second, bash-shaped derivation of values
/// `spira_config::resolve()` already computes in-process. This merges that in-process
/// answer into `snap.vars` instead, via `entry().or_insert()` so nothing the seam itself
/// still supplies (`seam::SNAPSHOT_VARS` — every `FAYTH_*`, ...) is ever
/// overridden, matching conf.sh's own `${VAR:=default}` rule.
///
/// `SPIRA_HOME`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED` are inserted explicitly: `resolve()`
/// deliberately never produces them (per-copy facts — see `spira_config::resolve::
/// ResolveInput`'s own doc), but [`Conf::new`] builds this process's `spira_config::repos::
/// Registry` straight out of `snap.vars`, and that registry's own `repo_root`/
/// `spira_home_repo` logic needs all three to tell an explicit `SPIRA_REPO` override apart
/// from one that merely fell out of where this copy of the harness sits.
///
/// NOT BEST-EFFORT ANY MORE (sp-1cdgq round 3): a containment refusal or an unreadable
/// registry used to leave `snap.vars` exactly as the seam call alone produced it — silently,
/// with no error anywhere. That "best-effort" was what let a missing `conf.d` (production
/// never has one; only a test fixture's `--home` ever does) resolve every `RETIRED_SNAPSHOT_
/// VARS` key to nothing while looking like a clean run (sp-8qm8g, then sp-1cdgq itself, same
/// defect twice). `resolve_for_process`'s `Err` is now the caller's problem: surfaced here,
/// not swallowed, so `main.rs` can refuse to start rather than run an aeon short the config
/// it believes it has.
pub fn merge_resolved_config(snap: &mut crate::seam::Snapshot, home: &Path, env: &BTreeMap<String, String>) -> Result<(), String> {
    let repo_derived = spira_config::resolve::derive_repo_filesystem(home, env);
    let repo = env
        .get("SPIRA_REPO")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_derived.clone());
    snap.vars.entry("SPIRA_HOME".into()).or_insert_with(|| home.to_string_lossy().into_owned());
    snap.vars.entry("SPIRA_REPO".into()).or_insert_with(|| repo.to_string_lossy().into_owned());
    snap.vars.entry("SPIRA_REPO_DERIVED".into()).or_insert_with(|| repo_derived.to_string_lossy().into_owned());
    // Unconditional insert, not `.entry().or_insert()` (per Ryan 2026-10-05: one source of
    // config). `resolved.values` is spira-config's own resolution of $SPIRA_TOML for every
    // key spira/conf.d registers — today this never collides with anything the bash seam
    // call above actually supplies (`seam::SNAPSHOT_VARS`, the live allow-list, carries no
    // registered key since wave 4.8; every registered name moved to
    // `RETIRED_SNAPSHOT_VARS`, resolved only here), but an `or_insert` left a stale or
    // accidentally-reintroduced seam value free to outrank the resolved one if that ever
    // changed. The registered keys this resolves are the ONE source; they win outright.
    let resolved = spira_config::resolve::resolve_for_process(home, &repo, env)?;
    // The spec itself travels with the config it resolved (persona tables are read from it).
    if let Some(t) = env.get("SPIRA_TOML") {
        snap.vars.insert("SPIRA_TOML".into(), t.clone());
    }
    for (k, v) in resolved.values {
        snap.vars.insert(k, v);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn fayth_defaults_match_aeon_sh() {
        let f = Fayth::from_vars("builder", &vars(&[("FAYTH_LABELS", "spira,plan")]));
        assert_eq!(f.max_concurrent, 1);
        assert_eq!(f.heartbeat_seconds, 30);
        assert_eq!(f.lease_seconds(), 600);
        assert_eq!(f.memory_prefixes, "law-");
        assert_eq!(f.tools, "Bash,Read,Edit,Write,Glob,Grep");
        assert_eq!(f.system_prompt, SystemPrompt::Append);
        assert_eq!(f.mail_from(), "Builder <builder@spira>");
        let g = Fayth::from_vars("x", &vars(&[("FAYTH_LEASE_MINUTES", "90"), ("FAYTH_SYSTEM_PROMPT", "replace")]));
        assert_eq!(g.lease_seconds(), 5400);
        assert_eq!(g.system_prompt, SystemPrompt::Replace);
    }

    #[test]
    fn fence_rules() {
        assert!(fenced("b", "", "").is_err());
        assert!(fenced("b", "plan", "").is_ok());
        assert!(fenced("b", "spira,plan", "spira").is_ok());
        let e = fenced("b", "plan,spirax", "spira").unwrap_err();
        assert_eq!(e.len(), 2);
        assert!(e[0].contains("does not require 'spira'"));
    }

    /// capacity_probe's own "defaults to the builder's own resolved model" case (wave
    /// 4.26, moved from test-persona-model.sh's bash source-grep).
    #[test]
    fn capacity_probe_model_defaults_to_persona_builder_when_unset() {
        let dir = testkit::TempDir::new("aeon-conf-capacity-model");
        let toml = dir.join("spira.toml");
        std::fs::write(&toml, "[persona.builder]\nmodel = \"toml-override-model\"\n").unwrap();
        // The builder's model comes from the spec SPIRA_TOML names (the one source).
        let snap = crate::seam::Snapshot { vars: vars(&[("SPIRA_TOML", toml.to_str().unwrap())]), ..Default::default() };
        let conf = Conf::new(&snap, &dir);
        assert_eq!(conf.capacity_probe_model(), "toml-override-model");

        let snap2 = crate::seam::Snapshot {
            vars: vars(&[("SPIRA_TOML", toml.to_str().unwrap()), ("SPIRA_CAPACITY_PROBE_MODEL", "operator-override")]),
            ..Default::default()
        };
        let conf2 = Conf::new(&snap2, &dir);
        assert_eq!(conf2.capacity_probe_model(), "operator-override", "an explicit SPIRA_CAPACITY_PROBE_MODEL wins");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persona_model_reads_the_declared_model_and_refuses_an_undeclared_one() {
        let dir = testkit::TempDir::new("aeon-conf");
        let base = dir.join("base.toml");
        let over = dir.join("over.toml");
        std::fs::write(&base, "[persona.builder]\nmodel = \"claude-x\"\n").unwrap();
        std::fs::write(&over, "[persona.builder]\nmodel = \"claude-y\"\n").unwrap();
        let spec = format!("{}:{}", base.display(), over.display());
        assert_eq!(persona_model("builder", &spec), Ok("claude-y".to_string()), "the layered spec, later layer wins");
        let e = persona_model("ops", &spec).unwrap_err();
        assert!(e.contains("persona.ops.model is not declared"), "{e}");
    }

    // ENV VARS ARE PROCESS-GLOBAL (spira-config's own locate.rs/lib.rs tests guard the same
    // hazard): the tests below that resolve config take this lock. `resolve_for_process`
    // now hard-refuses without a real `SPIRA_TOML` (per Ryan 2026-10-05: one source of
    // config — "SPIRA_TOML is not set" is a refusal, not "no config, defaults only"), so
    // each one hands `merge_resolved_config` an `env` map naming a real `fixture_toml`
    // file rather than pinning the ambient `SPIRA_TOML` to a nonexistent path: that map is
    // the `env` PARAMETER `resolve_for_process` actually reads, never the process's own
    // environment.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn merge_resolved_config_inserts_home_repo_and_registry_keys_without_overriding_the_seam() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = testkit::TempDir::new("aeon-conf-merge");
        // The real, checked-in spira/conf.d (this crate's own repo layout: `aeon/` sits
        // beside `spira/`) — SPIRA_CI_PARK_MAX is one of its real registered keys.
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_CI_PARK_MAX", "9")]);
        let env = BTreeMap::from([("SPIRA_TOML".to_string(), toml.to_string_lossy().into_owned())]);

        let mut snap = crate::seam::Snapshot {
            vars: vars(&[("SPIRA_TRACE_MARK", "/seam/mark")]),
            ..Default::default()
        };
        merge_resolved_config(&mut snap, &home, &env).unwrap();

        assert_eq!(snap.vars.get("SPIRA_TRACE_MARK").map(String::as_str), Some("/seam/mark"), "the seam's own value must survive the merge");
        assert_eq!(snap.vars.get("SPIRA_HOME").map(String::as_str), Some(home.to_str().unwrap()));
        assert!(snap.vars.contains_key("SPIRA_REPO"));
        assert!(snap.vars.contains_key("SPIRA_REPO_DERIVED"));
        assert_eq!(
            snap.vars.get("SPIRA_CI_PARK_MAX").map(String::as_str),
            Some("9"),
            "a declared registry key value must reach snap.vars in-process"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// sp-1cdgq's own regression (SPIRA_SUMMON_JITTER unreachable because SNAPSHOT_VARS
    /// never carried it) is superseded by this bead, not re-broken: the name moved to
    /// RETIRED_SNAPSHOT_VARS (seam.rs), resolved in-process here instead. Proves a
    /// synthetic, no-default registry entry (matching the real one's own shape) still
    /// reaches `snap.vars` once it is DECLARED in `spira.toml` — "an env var can still
    /// override" is no longer the thing to prove (per Ryan 2026-10-05: one source of
    /// config; `resolve_for_process`'s only input is the file `$SPIRA_TOML` names).
    #[test]
    fn merge_resolved_config_reaches_a_retired_registry_key() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = testkit::TempDir::new("aeon-conf-jitter");
        let home = dir.join("spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(
            home.join("conf.d/SPIRA_SUMMON_JITTER"),
            "TYPE=string\nGROUP=summon\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    # no default, matches the real registry entry\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_SUMMON_JITTER", "0")]);
        let env = BTreeMap::from([("SPIRA_TOML".to_string(), toml.to_string_lossy().into_owned())]);

        let mut snap = crate::seam::Snapshot::default();
        merge_resolved_config(&mut snap, &home, &env).unwrap();

        assert_eq!(
            snap.vars.get("SPIRA_SUMMON_JITTER").map(String::as_str),
            Some("0"),
            "a declared SPIRA_SUMMON_JITTER never reached snap.vars"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// sp-1cdgq, round 2: the test above proves the WIRING using a synthetic `conf.d` it
    /// writes itself — which is exactly why it did not catch the second collapse. Round 161
    /// (sp-mz7dn) moved SPIRA_SUMMON_JITTER's resolution onto `--home`'s OWN `conf.d/`, not
    /// wherever `lib.sh`/`conf.sh` happen to live (the old bash seam's decoupling), so a test
    /// fixture `--home` with no `conf.d` at all resolves the whole generic registry pass to
    /// nothing — `test-thrash-teardown.sh`'s fixture, still red with the identical
    /// "pre-session death" signature even after `SPIRA_SUMMON_JITTER=0` was exported,
    /// because no `conf.d` was ever copied into its `$SPIRA_HOME`.
    ///
    /// This test uses the REAL, checked-in `spira/conf.d` (this crate's own repo layout:
    /// `aeon/` sits beside `spira/`) instead of fabricating one, so a future regression in
    /// either direction — the registry file disappearing, `resolve()`'s generic pass
    /// breaking, or a fixture that forgets to copy `conf.d` in — is caught the same way
    /// production would actually hit it: through `Conf`, the way `run.rs`'s summon jitter
    /// reads it, not just `snap.vars`.
    #[test]
    fn summon_jitter_reaches_conf_through_the_real_registry() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
        assert!(home.join("conf.d").is_dir(), "this crate's own ../spira/conf.d must exist for this test to mean anything");

        let dir = testkit::TempDir::new("aeon-conf-real-jitter");
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_SUMMON_JITTER", "0")]);
        let env = BTreeMap::from([("SPIRA_TOML".to_string(), toml.to_string_lossy().into_owned())]);

        // No SPIRA_SUMMON_JITTER in snap.vars going in — matching production: wave 4.8
        // retired it from the bash seam's own SNAPSHOT_VARS allowlist, so only
        // merge_resolved_config can ever supply it now.
        let mut snap = crate::seam::Snapshot::default();
        merge_resolved_config(&mut snap, &home, &env).unwrap();
        let conf = Conf::new(&snap, &home);

        assert_eq!(
            conf.i(spira_config::admission::JITTER_ENV),
            0,
            "a declared SPIRA_SUMMON_JITTER did not reach Conf through the real spira/conf.d registry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
