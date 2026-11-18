//! The filing contract `bead.sh` enforces, as one library both it and the content door call, so a
//! filing refused through one is refused through the other with the same text: the identity and
//! statute guards on a decision's text, a persona's build-spec and lane requirements, and the
//! refusal of a hand-set partition label.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use spira_config::RepoSection;

use crate::guard::Refusal;
use crate::{bdq, identity, lane_check, repos_by_name, statute, LaneCheck};

pub type Vars<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// The caller variables the contract reads, which a content request must carry. The lane
/// override is `bead.sh`'s alone: the door has none, so a lane the repo does not admit stays refused.
pub const VARS: &[&str] = &[identity::OVERRIDE_ENV, statute::OVERRIDE_ENV, CONTRACT_OVERRIDE_ENV];

/// Admits a filing for a repo that breaks the landing contract, and records why on the bead.
pub const CONTRACT_OVERRIDE_ENV: &str = "BEAD_CONTRACT_CONSIDERED";

/// Exit code `bead.sh` refuses a filing with.
const REFUSED: i32 = 2;

fn refuse(code: i32, text: impl Into<String>) -> Refusal {
    Refusal { code, text: format!("{}\n", text.into()) }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Persona {
    pub name: String,
    pub labels: Vec<String>,
    pub requires_build_spec: bool,
    /// Why the persona's predicate cannot be trusted; a filing for it is refused with this.
    pub unresolved: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Statutes {
    Off,
    Registry { path: String, text: String },
    Unreadable(String),
}

impl Statutes {
    /// The registry at `path`; an empty path declares none, and an unreadable one refuses.
    pub fn at(path: &str) -> Statutes {
        if path.is_empty() {
            return Statutes::Off;
        }
        match std::fs::read_to_string(path) {
            Ok(text) => Statutes::Registry { path: path.to_string(), text },
            Err(e) => Statutes::Unreadable(format!("bead: refusing: cannot read the statute registry {path}: {e}")),
        }
    }
}

/// What `repo-contract check <repo>` answered: its exit (`None` when it was killed) and output.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckRun {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

type Checker = Arc<dyn Fn(&str) -> Result<CheckRun, String> + Send + Sync>;

/// The home repo, which is never checked, and the check run on any other.
#[derive(Clone)]
pub struct Contract {
    pub home_repo: String,
    check: Checker,
}

impl std::fmt::Debug for Contract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contract").field("home_repo", &self.home_repo).finish_non_exhaustive()
    }
}

impl Contract {
    pub fn new(home_repo: impl Into<String>, check: impl Fn(&str) -> Result<CheckRun, String> + Send + Sync + 'static) -> Contract {
        Contract { home_repo: home_repo.into(), check: Arc::new(check) }
    }

    /// The installation's: the release's `repo-contract`, every clause, bounded.
    pub fn real(home_repo: impl Into<String>) -> Contract {
        Contract::new(home_repo, |repo| {
            let exe = spira_config::release_env::tool("repo-contract")?;
            let out = spira_config::bounded::bounded_for(exe, "300").args(["check", repo]).output().map_err(|e| format!("cannot run repo-contract: {e}"))?;
            Ok(CheckRun { code: out.status.code(), stdout: String::from_utf8_lossy(&out.stdout).into_owned(), stderr: String::from_utf8_lossy(&out.stderr).into_owned() })
        })
    }

    /// Admits everything.
    pub fn none() -> Contract {
        Contract::new("", |_| Ok(CheckRun { code: Some(0), ..Default::default() }))
    }
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub personas: Vec<Persona>,
    pub repos: BTreeMap<String, RepoSection>,
    pub identity: BTreeMap<String, Vec<String>>,
    pub statutes: Statutes,
    pub contract: Contract,
}

impl Policy {
    /// No persona, so nothing is a filing.
    pub fn none() -> Policy {
        Policy { personas: vec![], repos: BTreeMap::new(), identity: BTreeMap::new(), statutes: Statutes::Off, contract: Contract::none() }
    }

    /// The installation's policy: the personas under `home`'s runes, and the files the
    /// registered keys `SPIRA_REPO_MAP`, `SPIRA_REPO_IDENTITY` and `SPIRA_STATUTE_GUARDS` name.
    pub fn load(home: &Path, cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<Policy, String> {
        let path = |key: &str| cfg(key).map(|v| (!v.is_empty()).then_some(v));
        let repos = path("SPIRA_REPO_MAP")?.and_then(|p| std::fs::read_to_string(p).ok()).map(|t| repos_by_name(&t)).unwrap_or_default();
        let identity = path("SPIRA_REPO_IDENTITY")?.and_then(|p| std::fs::read_to_string(p).ok()).map(|t| identity::parse(&t)).unwrap_or_default();
        let statutes = Statutes::at(&cfg("SPIRA_STATUTE_GUARDS")?);
        Ok(Policy { personas: personas(home), repos, identity, statutes, contract: Contract::real(cfg("SPIRA_HOME_REPO")?) })
    }

    /// The persona a create is filed for: the most specific one whose whole predicate the
    /// labels carry. `None` when the labels name no persona's partition.
    pub fn persona_of(&self, labels: &[String]) -> Option<&Persona> {
        self.personas
            .iter()
            .filter(|p| !p.labels.is_empty() && p.labels.iter().all(|l| labels.contains(l)))
            .max_by_key(|p| p.labels.len())
    }

    /// Every partition label some persona's predicate carries.
    pub fn partitions(&self) -> Vec<String> {
        let rows: Vec<(String, String)> = self.personas.iter().map(|p| (p.name.clone(), p.labels.join(","))).collect();
        crate::runes_partitions(&rows, "")
    }
}

fn personas(home: &Path) -> Vec<Persona> {
    let dir = spira_config::runes::runes_dir(home);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().and_then(|x| x.to_str()) == Some("rune")).then(|| p.file_stem().and_then(|s| s.to_str()).map(str::to_string)).flatten()
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let requires_build_spec = spira_config::runes::rune_get(home, &name, "RUNE_REQUIRES_BUILD_SPEC", "") == "1";
            let (labels, unresolved) = match spira_config::runes::rune_predicate(home, &name) {
                Ok(p) => (p.labels, None),
                Err(e) => (spira_config::runes::rune_get(home, &name, "RUNE_LABELS", ""), Some(e.to_string())),
            };
            Persona { labels: labels.split(',').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(), name, requires_build_spec, unresolved }
        })
        .collect()
}

/// The labels `bead.sh file --for <persona> --repo <repo>` files a work bead under: the
/// persona's predicate and the repo. A persona that does not resolve is refused as `bead.sh` refuses it.
pub fn work_labels(home: &Path, persona: &str, repo: &str) -> Result<String, String> {
    if !spira_config::runes::runes_dir(home).join(format!("{persona}.rune")).is_file() {
        return Err(format!("bead: no such persona: {persona}"));
    }
    let predicate = spira_config::runes::rune_predicate(home, persona).map_err(|e| format!("bead: {e}"))?;
    if predicate.labels.is_empty() {
        return Err(format!("bead: persona {persona} has no partition labels"));
    }
    Ok(crate::work_labels(&predicate.labels, repo))
}

/// The identity guard on `text` filed for `repo`: `Ok(Some(line))` is the line to record on
/// the bead when the named override admitted another repo's terms.
pub fn identity_guard(registry: &BTreeMap<String, Vec<String>>, repo: &str, text: &str, vars: &Vars) -> Result<Option<String>, Refusal> {
    let hits = identity::foreign_terms(registry, repo, text);
    if hits.is_empty() {
        return Ok(None);
    }
    match identity::override_reason(vars(identity::OVERRIDE_ENV)) {
        Some(why) => Ok(Some(identity::recorded_line(&why))),
        None => Err(refuse(REFUSED, identity::refusal(repo, &hits))),
    }
}

/// The contract guard on a filing for `repo` carrying `labels`: `Ok(Some(line))` is the line to
/// record on the bead when the named override admitted a repo that breaks the contract.
pub fn contract_guard(repo: &str, labels: &[String], contract: &Contract, vars: &Vars) -> Result<Option<String>, Refusal> {
    if repo == contract.home_repo || labels.iter().any(|l| l == spira_config::scoped_stop::CONTRACT_FIX_LABEL) {
        return Ok(None);
    }
    let exits = format!(
        "bring it in line with `repo-contract scaffold {repo}` filed as `bead.sh file ... --contract-fix`, or set {CONTRACT_OVERRIDE_ENV}=\"<why>\" to file anyway"
    );
    let (headline, detail) = match (contract.check)(repo) {
        Err(e) => (format!("cannot tell whether {repo} meets the landing contract: {e}"), String::new()),
        Ok(run) => {
            let not_met: Vec<&str> = run.stdout.lines().filter(|l| l.starts_with("not met ")).collect();
            match run.code {
                Some(0) => return Ok(None),
                Some(1) if !not_met.is_empty() => (format!("{repo} does not meet the landing contract"), format!("{}\n", not_met.join("\n"))),
                other => {
                    let code = match other {
                        None => "no exit code".to_string(),
                        Some(1) => "exit 1 naming no clause".to_string(),
                        Some(c) => format!("exit {c}"),
                    };
                    (format!("cannot tell whether {repo} meets the landing contract: repo-contract check gave {code}"), format!("{}{}", run.stdout, run.stderr))
                }
            }
        }
    };
    match identity::override_reason(vars(CONTRACT_OVERRIDE_ENV)) {
        Some(why) => Ok(Some(format!("Contract considered: {why}\n{headline}\n{detail}").trim_end().to_string())),
        None => Err(refuse(REFUSED, format!("bead: refusing: {headline}\n{detail}bead: {exits}"))),
    }
}

/// What the statute guard made of a text it admitted: the line to record, and what to show.
#[derive(Debug, PartialEq, Default)]
pub struct StatuteNote {
    pub line: Option<String>,
    pub shown: String,
}

pub fn statute_guard(statutes: &Statutes, text: &str, vars: &Vars) -> Result<StatuteNote, Refusal> {
    let (path, registry) = match statutes {
        Statutes::Off => return Ok(StatuteNote::default()),
        Statutes::Unreadable(why) => return Err(refuse(1, why)),
        Statutes::Registry { path, text } => (path, text),
    };
    let hits = statute::check(registry, text).map_err(|e| refuse(1, format!("bead: refusing: statute registry {path}: {e}")))?;
    if hits.is_empty() {
        return Ok(StatuteNote::default());
    }
    if !hits.iter().any(|h| h.refuses) {
        return Ok(StatuteNote { line: Some(statute::recorded_line(&hits, None)), shown: statute::shown(&hits) });
    }
    match statute::override_reason(vars(statute::OVERRIDE_ENV)) {
        Some(why) => Ok(StatuteNote { line: Some(statute::recorded_line(&hits, Some(&why))), shown: statute::shown(&hits) }),
        None => Err(refuse(REFUSED, statute::refusal(&hits))),
    }
}

/// A persona that requires a build spec is refused a body that names none.
pub fn build_spec_guard(persona: &str, body: &str) -> Result<(), Refusal> {
    if crate::has_build_spec(body) {
        return Ok(());
    }
    Err(refuse(
        REFUSED,
        format!(
            "bead: refusing: a bead for {persona} must name what to build under a `{}` heading (the artifact, component or change, and where). An outcome with no named deliverable is design work: file it --for spike, which returns a proposed build spec for approval.",
            crate::BUILD_HEADING
        ),
    ))
}

pub fn lane_guard(persona: &Persona, repo: &str, repos: &BTreeMap<String, RepoSection>, override_set: bool) -> Result<(), Refusal> {
    match lane_check(&persona.labels.join(","), repo, repos, override_set) {
        LaneCheck::Admitted => Ok(()),
        LaneCheck::Refused(msg) => Err(refuse(REFUSED, msg)),
    }
}

/// `bead.sh label`'s refusal of a hand-set partition label: it derives those from the
/// persona's own predicate, so a hand-set one can disagree with it.
pub fn partition_label(verb: &str, label: &str, partitions: &[String]) -> Result<(), Refusal> {
    if partitions.iter().any(|p| p == label) || label.starts_with("rune:") {
        return Err(refuse(1, format!("bead: label {verb}: refusing — {label} is a partition label; `bead.sh file --for <persona>` derives those from the persona's own predicate, so a hand-set one can disagree with it")));
    }
    Ok(())
}

/// `label add|remove <id> <label>` on the door.
pub fn label_write(args: &[String], policy: &Policy) -> Result<(), Refusal> {
    match args {
        [l, verb, _, label, ..] if l == "label" && (verb == "add" || verb == "remove") => partition_label(verb, label, &policy.partitions()),
        _ => Ok(()),
    }
}

fn value<'a>(args: &'a [String], long: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if long.contains(&a.as_str()) {
            out.extend(it.next().map(String::as_str));
        } else if let Some(v) = long.iter().find_map(|l| a.strip_prefix(&format!("{l}="))) {
            out.push(v);
        }
    }
    out
}

fn labels_of(args: &[String]) -> Vec<String> {
    value(args, &["--labels", "--label", "-l"]).iter().flat_map(|v| v.split(',')).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// The text ahead of a write's body: the lines the guards record, blank-line separated.
fn prefixed(lines: &[String], body: &str) -> String {
    lines.iter().map(|l| format!("{l}\n\n")).collect::<String>() + body
}

/// The text `args` writes: the request body, else the `--description` argument.
fn text_of(args: &[String], body: &Option<String>) -> String {
    body.clone().unwrap_or_else(|| value(args, &["--description", "-d"]).last().map(|s| s.to_string()).unwrap_or_default())
}

fn put_text(args: &mut [String], body: &mut Option<String>, text: String) {
    if body.is_some() {
        *body = Some(text);
    } else if let Some(i) = args.iter().position(|a| a == "--description" || a == "-d") {
        if let Some(v) = args.get_mut(i + 1) {
            *v = text;
        }
    }
}

/// What a persona's filing must satisfy once its text is admitted: the build spec, then the lane.
pub fn persona_guards(persona: &Persona, repo: &str, body: &str, repos: &BTreeMap<String, RepoSection>, lane_override: bool) -> Result<(), Refusal> {
    if persona.requires_build_spec {
        build_spec_guard(&persona.name, body)?;
    }
    lane_guard(persona, repo, repos, lane_override)
}

/// The contract on a create that files a persona's work, in `bead.sh file`'s order: identity,
/// statute, a repo, the build spec, the lane. Lines the guards record are put ahead of the
/// description; the returned text is what to log. A create that names no persona's partition is
/// not a filing and is let through.
pub fn create(args: &mut [String], body: &mut Option<String>, policy: &Policy, vars: &Vars) -> Result<String, Refusal> {
    if !bdq::is_create(args) {
        return Ok(String::new());
    }
    let labels = labels_of(args);
    let Some(persona) = policy.persona_of(&labels) else { return Ok(String::new()) };
    let repo = labels.iter().find_map(|l| l.strip_prefix("repo:")).map(str::to_string);
    let (title, _, _) = bdq::create_fields(args);
    let text = text_of(args, body);
    let mut recorded = Vec::new();

    if let Some(repo) = &repo {
        if let Some(line) = identity_guard(&policy.identity, repo, &format!("{title}\n{text}"), vars)? {
            recorded.insert(0, line);
        }
    }
    let judged = prefixed(&recorded, &text);
    let note = statute_guard(&policy.statutes, &format!("{title}\n{judged}"), vars)?;
    if let Some(line) = note.line {
        recorded.insert(0, line);
    }
    if let Some(why) = &persona.unresolved {
        return Err(refuse(REFUSED, format!("bead: {why}")));
    }
    let Some(repo) = repo else { return Err(refuse(REFUSED, "bead: --repo <name> required")) };
    persona_guards(persona, &repo, &text, &policy.repos, false)?;
    if let Some(line) = contract_guard(&repo, &labels, &policy.contract, vars)? {
        recorded.insert(0, line);
    }
    if !recorded.is_empty() {
        put_text(args, body, prefixed(&recorded, &text));
    }
    Ok(note.shown)
}

/// The guards on text written to an existing bead, `bead.sh amend`'s: a `note` or a
/// description replacement. `repo` is the bead's own repo label.
pub fn amend_text(args: &mut [String], body: &mut Option<String>, repo: &dyn Fn(&str) -> Option<String>, policy: &Policy, vars: &Vars) -> Result<String, Refusal> {
    let writes_description = args.first().is_some_and(|v| v == "update") && args.iter().any(|a| a == "--body-file");
    let (id, text, in_arg) = match args {
        [v, id, text, ..] if v == "note" && body.is_none() => (id.clone(), text.clone(), true),
        [v, id, ..] if v == "note" || writes_description => (id.clone(), body.clone().unwrap_or_default(), false),
        _ => return Ok(String::new()),
    };
    let mut recorded = Vec::new();
    if let Some(repo) = repo(&id) {
        if let Some(line) = identity_guard(&policy.identity, &repo, &text, vars)? {
            recorded.push(line);
        }
    }
    let note = statute_guard(&policy.statutes, &prefixed(&recorded, &text), vars)?;
    if let Some(line) = note.line {
        recorded.insert(0, line);
    }
    if !recorded.is_empty() {
        let text = prefixed(&recorded, &text);
        if in_arg {
            args[2] = text;
        } else {
            *body = Some(text);
        }
    }
    Ok(note.shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(name: &str, labels: &[&str], spec: bool) -> Persona {
        Persona { name: name.into(), labels: labels.iter().map(|s| s.to_string()).collect(), requires_build_spec: spec, unresolved: None }
    }

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_most_specific_persona_whose_whole_predicate_the_labels_carry_is_the_one_filed_for() {
        let p = Policy { personas: vec![persona("a", &["x"], false), persona("b", &["x", "y"], true)], ..Policy::none() };
        assert_eq!(p.persona_of(&a(&["x", "y", "repo:r"])).map(|p| p.name.as_str()), Some("b"));
        assert_eq!(p.persona_of(&a(&["x"])).map(|p| p.name.as_str()), Some("a"));
        assert!(p.persona_of(&a(&["z"])).is_none());
    }

    #[test]
    fn an_empty_predicate_names_no_filing() {
        let p = Policy { personas: vec![persona("a", &[], false)], ..Policy::none() };
        assert!(p.persona_of(&a(&["x"])).is_none());
    }

    #[test]
    fn a_partition_label_is_refused_and_an_ordinary_one_is_not() {
        let parts = vec!["builder-lane".to_string()];
        assert!(partition_label("add", "builder-lane", &parts).unwrap_err().text.starts_with("bead: label add: refusing — builder-lane is a partition label"));
        assert!(partition_label("remove", "rune:x", &parts).is_err());
        assert_eq!(partition_label("add", "kit-bump", &parts), Ok(()));
    }

    const NOT_MET: &str = "met environment: ok\nnot met submit-checks: .spira/checks.toml does not exist\n";

    fn checker(code: Option<i32>, stdout: &str, stderr: &str) -> Contract {
        let run = CheckRun { code, stdout: stdout.into(), stderr: stderr.into() };
        Contract::new("spira", move |_| Ok(run.clone()))
    }

    fn deck_policy(contract: Contract) -> Policy {
        Policy { personas: vec![persona("builder", &["builder-lane"], false)], contract, ..Policy::none() }
    }

    fn file(policy: &Policy, labels: &str, env: &[(&str, &str)]) -> Result<String, Refusal> {
        let env: BTreeMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut args = a(&["create", "T", "--labels", labels]);
        create(&mut args, &mut Some("body".into()), policy, &|k| env.get(k).cloned())
    }

    #[test]
    fn a_work_filing_for_a_repo_missing_a_contract_file_is_refused_naming_the_clause() {
        let r = file(&deck_policy(checker(Some(1), NOT_MET, "")), "builder-lane,repo:deck", &[]).expect_err("filing admitted");
        assert_eq!(r.code, 2);
        assert!(r.text.contains("not met submit-checks: .spira/checks.toml does not exist"), "{}", r.text);
        assert!(!r.text.contains("met environment"), "{}", r.text);
        assert!(r.text.contains("--contract-fix") && r.text.contains(CONTRACT_OVERRIDE_ENV), "{}", r.text);
    }

    #[test]
    fn a_contract_fix_filing_for_that_repo_is_admitted() {
        let p = deck_policy(checker(Some(1), NOT_MET, ""));
        assert!(file(&p, "builder-lane,repo:deck,repo-contract-fix", &[]).is_ok());
    }

    #[test]
    fn the_home_repo_is_admitted_without_running_the_check() {
        let p = deck_policy(Contract::new("spira", |_| panic!("the home repo is not checked")));
        assert!(file(&p, "builder-lane,repo:spira", &[]).is_ok());
    }

    #[test]
    fn a_check_that_cannot_tell_refuses_with_what_it_said() {
        for run in [checker(Some(2), "out-text", "err-text"), checker(None, "out-text", "err-text")] {
            let r = file(&deck_policy(run), "builder-lane,repo:deck", &[]).expect_err("filing admitted");
            assert!(r.text.contains("cannot tell") && r.text.contains("out-text") && r.text.contains("err-text") && r.text.contains("--contract-fix"), "{}", r.text);
        }
        let spawn = Contract::new("spira", |_| Err("no such tool".into()));
        let r = file(&deck_policy(spawn), "builder-lane,repo:deck", &[]).expect_err("filing admitted");
        assert!(r.text.contains("cannot tell") && r.text.contains("no such tool"), "{}", r.text);
    }

    #[test]
    fn a_met_contract_admits_and_records_nothing() {
        let p = deck_policy(checker(Some(0), "met environment: ok\n", ""));
        let mut args = a(&["create", "T", "--labels", "builder-lane,repo:deck", "--description", "d"]);
        create(&mut args, &mut None, &p, &|_| None).unwrap();
        assert_eq!(args.last().unwrap(), "d");
    }

    #[test]
    fn a_named_override_admits_a_broken_repo_and_records_why_and_what_was_not_met() {
        let p = deck_policy(checker(Some(1), NOT_MET, ""));
        let mut args = a(&["create", "T", "--labels", "builder-lane,repo:deck", "--description", "d"]);
        create(&mut args, &mut None, &p, &|k| (k == CONTRACT_OVERRIDE_ENV).then(|| "bootstrapping".to_string())).unwrap();
        let text = args.last().unwrap();
        assert!(text.starts_with("Contract considered: bootstrapping") && text.contains("not met submit-checks") && text.ends_with("\n\nd"), "{text}");
        assert!(file(&p, "builder-lane,repo:deck", &[(CONTRACT_OVERRIDE_ENV, "  ")]).is_err());
    }

    #[test]
    fn a_named_override_admits_a_check_that_cannot_tell_and_records_that_it_could_not() {
        let p = deck_policy(checker(Some(2), "out-text", "err-text"));
        let mut args = a(&["create", "T", "--labels", "builder-lane,repo:deck", "--description", "d"]);
        create(&mut args, &mut None, &p, &|k| (k == CONTRACT_OVERRIDE_ENV).then(|| "bootstrapping".to_string())).unwrap();
        let text = args.last().unwrap();
        assert!(text.starts_with("Contract considered: bootstrapping") && text.contains("cannot tell") && text.ends_with("\n\nd"), "{text}");
    }
}
