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
    pub sop_required: bool,
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
            sop_required: get(v, "FAYTH_SOP_REQUIRED") == Some("1"),
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
    pub fn s(&self, k: &str) -> String {
        self.v.get(k).cloned().unwrap_or_default()
    }
    pub fn or(&self, k: &str, d: &str) -> String {
        get(&self.v, k).unwrap_or(d).to_string()
    }
    pub fn n(&self, k: &str, d: i64) -> i64 {
        num(&self.v, k).unwrap_or(d)
    }
    pub fn set_nonempty(&self, k: &str) -> bool {
        get(&self.v, k).is_some()
    }
    pub fn db(&self) -> String {
        self.s("SPIRA_DB")
    }
    pub fn ask_label(&self) -> String {
        self.s("SPIRA_ASK_LABEL")
    }
    pub fn submitted_label(&self) -> String {
        self.or("SPIRA_SUBMITTED_LABEL", "spira-submitted")
    }
    pub fn overlay(&self) -> PathBuf {
        PathBuf::from(self.s("SPIRA_CHAMBER_OVERLAY"))
    }
    pub fn trace_mark(&self) -> String {
        self.or("SPIRA_TRACE_MARK", "=== spira attempt")
    }
    pub fn agent(&self) -> String {
        self.or("SPIRA_AGENT", "claude")
    }
    pub fn ledger(&self) -> PathBuf {
        self.run.join("aeon-ledger.log")
    }
    pub fn landstate(&self) -> PathBuf {
        match get(&self.v, "LANDSTATE") {
            Some(l) => PathBuf::from(l),
            None => self.run.join("landstate"),
        }
    }
}

/// `lifecycle_enforce`, resolved as conf.sh resolves it: the unit's environment wins (how a
/// fixture pins it), else `spira.lifecycle_enforce` in the spira.toml conf.sh resolved, read
/// through the spira-config library; else off. Binary presence is never consulted here.
pub fn lifecycle_enforce(original_env: &BTreeMap<String, String>, toml_file: Option<&Path>) -> bool {
    if let Some(v) = original_env.get("SPIRA_LIFECYCLE_ENFORCE") {
        return v == "1" || v == "true";
    }
    let Some(p) = toml_file.filter(|p| p.is_file()) else { return false };
    match spira_config::load(p) {
        Ok(doc) => doc.spira.and_then(|s| s.lifecycle_enforce).unwrap_or(false),
        Err(_) => false,
    }
}

/// `persona_model`: `persona.<name>.model` from the resolved spira.toml, default
/// `claude-opus-5`.
pub fn persona_model(name: &str, toml_file: Option<&Path>) -> String {
    const DEF: &str = "claude-opus-5";
    let Some(p) = toml_file.filter(|p| p.is_file()) else { return DEF.into() };
    match spira_config::load(p) {
        Ok(doc) => doc.persona.get(name).map(|p| p.model.clone()).filter(|m| !m.is_empty()).unwrap_or_else(|| DEF.into()),
        Err(_) => DEF.into(),
    }
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
/// still supplies (`seam::SNAPSHOT_VARS` — `LANDSTATE`, every `FAYTH_*`, ...) is ever
/// overridden, matching conf.sh's own `${VAR:=default}` rule.
///
/// `SPIRA_HOME`/`SPIRA_REPO`/`SPIRA_REPO_DERIVED` are inserted explicitly: `resolve()`
/// deliberately never produces them (per-copy facts — see `spira_config::resolve::
/// ResolveInput`'s own doc), but [`Conf::new`] builds this process's `spira_config::repos::
/// Registry` straight out of `snap.vars`, and that registry's own `repo_root`/
/// `spira_home_repo` logic needs all three to tell an explicit `SPIRA_REPO` override apart
/// from one that merely fell out of where this copy of the harness sits.
///
/// Best-effort, same as every other config read in this binary: a containment refusal or
/// an unreadable registry leaves `snap.vars` exactly as the seam call alone produced it.
pub fn merge_resolved_config(snap: &mut crate::seam::Snapshot, home: &Path, env: &BTreeMap<String, String>) {
    let repo_derived = spira_config::resolve::derive_repo_filesystem(home);
    let repo = env
        .get("SPIRA_REPO")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_derived.clone());
    snap.vars.entry("SPIRA_HOME".into()).or_insert_with(|| home.to_string_lossy().into_owned());
    snap.vars.entry("SPIRA_REPO".into()).or_insert_with(|| repo.to_string_lossy().into_owned());
    snap.vars.entry("SPIRA_REPO_DERIVED".into()).or_insert_with(|| repo_derived.to_string_lossy().into_owned());
    if let Ok(resolved) = spira_config::resolve::resolve_for_process(home, &repo, env) {
        for (k, v) in resolved.values {
            snap.vars.entry(k).or_insert(v);
        }
    }
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

    #[test]
    fn enforce_env_wins_and_binary_presence_is_irrelevant() {
        let dir = testkit::TempDir::new("aeon-conf");
        let toml = dir.join("spira.toml");
        std::fs::write(&toml, "[spira]\nlifecycle_enforce = true\n").unwrap();
        assert!(lifecycle_enforce(&BTreeMap::new(), Some(&toml)));
        assert!(!lifecycle_enforce(&vars(&[("SPIRA_LIFECYCLE_ENFORCE", "0")]), Some(&toml)));
        std::fs::write(&toml, "[spira]\n").unwrap();
        assert!(!lifecycle_enforce(&BTreeMap::new(), Some(&toml)));
        assert!(lifecycle_enforce(&vars(&[("SPIRA_LIFECYCLE_ENFORCE", "1")]), Some(&toml)));
        assert!(!lifecycle_enforce(&BTreeMap::new(), None));
        std::fs::write(&toml, "[persona.builder]\nmodel = \"claude-x\"\n").unwrap();
        assert_eq!(persona_model("builder", Some(&toml)), "claude-x");
        assert_eq!(persona_model("ops", Some(&toml)), "claude-opus-5");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ENV VARS ARE PROCESS-GLOBAL (spira-config's own locate.rs/lib.rs tests guard the same
    // hazard): the one test below that resolves config takes this lock, and pins SPIRA_TOML
    // to a nonexistent path — locate()'s own exclusive-pin rule — so it never depends on a
    // real operator spira.toml on the machine running this suite.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn merge_resolved_config_inserts_home_repo_and_registry_keys_without_overriding_the_seam() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = std::env::var("SPIRA_TOML").ok();
        let dir = testkit::TempDir::new("aeon-conf-merge");
        let home = dir.join("spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(
            home.join("conf.d/SPIRA_CI_PARK_MAX"),
            "TYPE=u32\nGROUP=queue\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_CI_PARK_MAX:=9}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        std::env::set_var("SPIRA_TOML", dir.join("no-such-spira.toml"));

        let mut snap = crate::seam::Snapshot {
            vars: vars(&[("LANDSTATE", "/seam/landstate")]),
            ..Default::default()
        };
        merge_resolved_config(&mut snap, &home, &BTreeMap::new());

        match saved {
            Some(v) => std::env::set_var("SPIRA_TOML", v),
            None => std::env::remove_var("SPIRA_TOML"),
        }

        assert_eq!(snap.vars.get("LANDSTATE").map(String::as_str), Some("/seam/landstate"), "the seam's own value must survive the merge");
        assert_eq!(snap.vars.get("SPIRA_HOME").map(String::as_str), Some(home.to_str().unwrap()));
        assert!(snap.vars.contains_key("SPIRA_REPO"));
        assert!(snap.vars.contains_key("SPIRA_REPO_DERIVED"));
        assert_eq!(snap.vars.get("SPIRA_CI_PARK_MAX").map(String::as_str), Some("9"), "a registry key resolve() covers must reach snap.vars in-process");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
