//! Composition by touched set (DESIGN.md "Composition", sp-2ghui): what a branch touches
//! decides what the gate runs. Pure: the engine hands in the changed files, the workspace
//! graph and the mode, and gets back the phases to run.

use spira_config::GateMode;
use spira_config::unit_tier as tier;
use std::collections::{BTreeSet, HashMap};

/// One changed path between the landing ref and the tree under test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    /// The file is executable on either side (a script with no extension is still a script).
    pub exec: bool,
}

/// The kinds of test target a crate, or a selection of crates, has: cargo refuses
/// `--lib` where no selected crate has a library, and `--test '*'` where none has a `tests/` target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Targets {
    pub lib: bool,
    pub bin: bool,
    pub test: bool,
}

impl Targets {
    fn union(self, o: Targets) -> Targets {
        Targets { lib: self.lib || o.lib, bin: self.bin || o.bin, test: self.test || o.test }
    }
}

/// A workspace member, from `cargo metadata --no-deps`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub targets: Targets,
    /// The member's directory relative to the workspace root, `/`-separated, no trailing `/`.
    pub dir: String,
    /// Names of the workspace members it depends on (normal, dev or build).
    pub deps: Vec<String>,
    /// The subset of `deps` that goes into its binaries (normal or build, not dev): the
    /// source closure of a gate tool (toolkey.rs).
    pub build_deps: Vec<String>,
}

/// What one changed path is, for composition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Class {
    /// A file inside a workspace member's directory.
    Crate(String),
    /// A workspace-wide build input (root `Cargo.toml`, `Cargo.lock`, the toolchain pin,
    /// `Makefile`, `clippy.toml`, `.cargo/`): every member is touched.
    Workspace,
    /// `docs/`, `wiki/`, any `*.md` or `*.txt` outside a crate.
    Doc,
    /// A suite or its library (`spira/test-*`, `spira/testlib/`): the round runs these.
    Suite,
    /// A bash (or other script) component: `*.sh`, `*.bash`, `*.py`, or any executable file
    /// outside a crate. Touching one keeps today's full sequence.
    Script,
    /// `systemd/` outside a crate: unit templates that crate tests read from `../systemd`.
    Units,
    /// The config registry and delta: files crate tests read from `CARGO_MANIFEST_DIR/..`.
    Registry,
    /// Anything else (data files, examples): nothing to build.
    Config,
}

/// What the gate runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Composition {
    /// The repository's gate string, whole — today's sequence. `why` names the reason.
    Suites { why: String },
    /// The gate string with suites off and without the build fence ([`gate_string`]), then
    /// `cargo build` and `cargo test` of `crates` on the host. `touched` are the members whose files changed (or `*` for a workspace
    /// input); `crates` adds their dependents, direct and transitive, sorted.
    Unit {
        touched: Vec<String>,
        crates: Vec<String>,
        targets: Targets,
        /// Dependents the selection computed and the cap dropped; sorted. The round tests them.
        dropped: Vec<String>,
    },
    /// The gate string with suites off: nothing buildable changed. Under `gate_mode = fences`
    /// every unforced trial composes this, and the string has lost its suites step
    /// ([`without_suites`]).
    Fences,
}

impl Composition {
    /// The one-word label the meter and the verdict record carry.
    pub fn label(&self) -> String {
        match self {
            Composition::Suites { why } => format!("suites({why})"),
            Composition::Unit { .. } => "unit".into(),
            Composition::Fences => "fences".into(),
        }
    }
    /// Suites off in the gate string's own run.
    pub fn suites_off(&self) -> bool {
        !matches!(self, Composition::Suites { .. })
    }
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty() || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// The member whose directory holds `path` (the deepest, so a nested member wins).
fn owner<'m>(path: &str, members: &'m [Member]) -> Option<&'m Member> {
    members
        .iter()
        .filter(|m| !m.dir.is_empty() && under(path, &m.dir))
        .max_by_key(|m| m.dir.len())
}

pub fn classify(c: &Changed, members: &[Member]) -> Class {
    let p = c.path.as_str();
    if let Some(m) = owner(p, members) {
        return Class::Crate(m.name.clone());
    }
    if matches!(
        p,
        "Cargo.toml"
            | "Cargo.lock"
            | "rust-toolchain"
            | "rust-toolchain.toml"
            | "Makefile"
            | "clippy.toml"
    ) || p.starts_with(".cargo/")
    {
        return Class::Workspace;
    }
    if p.starts_with("docs/") || p.starts_with("wiki/") || p.ends_with(".md") || p.ends_with(".txt")
    {
        return Class::Doc;
    }
    if p.starts_with("spira/test-") || p.starts_with("spira/testlib/") {
        return Class::Suite;
    }
    if under(p, "spira/conf.d") || under(p, "spira/conf.toml.d") || under(p, "spira/config-delta.d") || p == "spira/config-delta.toml" {
        return Class::Registry;
    }
    if p.ends_with(".sh") || p.ends_with(".bash") || p.ends_with(".py") || c.exec {
        return Class::Script;
    }
    if under(p, "systemd") {
        return Class::Units;
    }
    Class::Config
}

pub use suite_select::readers::REGISTRY_READERS;

fn path_readers(path: &str, members: &[Member]) -> Vec<String> {
    suite_select::readers::readers_of(path)
        .into_iter()
        .filter(|r| members.iter().any(|m| m.name == *r))
        .map(str::to_string)
        .collect()
}

/// Readers of `path` land in `touched` (and get their dependents) — except a registry file's
/// registry readers, which are added after the expansion.
fn add_readers(path: &str, class: &Class, members: &[Member], touched: &mut BTreeSet<String>, after: &mut BTreeSet<String>) {
    for r in path_readers(path, members) {
        if matches!(class, Class::Registry) && REGISTRY_READERS.contains(&r.as_str()) {
            after.insert(r);
        } else {
            touched.insert(r);
        }
    }
}

/// `seed` plus every member that depends directly on one of them; sorted.
pub fn with_dependents(seed: &BTreeSet<String>, members: &[Member]) -> Vec<String> {
    let mut rdeps: HashMap<&str, Vec<&str>> = HashMap::new();
    for m in members {
        for d in &m.deps {
            rdeps.entry(d.as_str()).or_default().push(m.name.as_str());
        }
    }
    let mut out: BTreeSet<String> = seed.clone();
    for n in seed {
        for r in rdeps.get(n.as_str()).into_iter().flatten() {
            out.insert(r.to_string());
        }
    }
    out.into_iter().collect()
}

/// `seed` plus every member that depends on one of them, directly or through others; sorted.
pub fn with_transitive_dependents(seed: &BTreeSet<String>, members: &[Member]) -> Vec<String> {
    let mut out: BTreeSet<String> = seed.clone();
    loop {
        let next: BTreeSet<String> = members
            .iter()
            .filter(|m| m.deps.iter().any(|d| out.contains(d)))
            .map(|m| m.name.clone())
            .collect();
        let before = out.len();
        out.extend(next);
        if out.len() == before {
            return out.into_iter().collect();
        }
    }
}

/// The `-p` selection for a diff: every workspace member it touches plus its dependents
/// (`transitive` or direct only) — never `--workspace`. A workspace-wide input (root manifest,
/// lockfile, toolchain pin) names every member.
pub fn select_crates(diff_raw: &[u8], metadata_json: &str, transitive: bool) -> Result<Vec<String>, String> {
    let members = parse_metadata(metadata_json)?;
    let mut touched = BTreeSet::new();
    let mut workspace = false;
    let mut readers = BTreeSet::new();
    for c in parse_diff_raw(diff_raw) {
        let class = classify(&c, &members);
        add_readers(&c.path, &class, &members, &mut touched, &mut readers);
        match class {
            Class::Crate(n) => {
                touched.insert(n);
            }
            Class::Workspace => workspace = true,
            Class::Doc | Class::Suite | Class::Script | Class::Config | Class::Units | Class::Registry => {}
        }
    }
    if workspace {
        return Ok(members.iter().map(|m| m.name.clone()).collect());
    }
    let mut out = if transitive { with_transitive_dependents(&touched, &members) } else { with_dependents(&touched, &members) };
    out.extend(readers);
    out.sort();
    out.dedup();
    Ok(out)
}

/// The inputs that force today's sequence whatever the mode says.
///
/// Ejected suites are not a force (sp-p3srm): a returned bead re-runs exactly what the round
/// named in its own phase ([`Reentry`]), whatever the composition, so a Rust-only branch keeps
/// its unit gate and still has to pass them.
#[derive(Clone, Debug, Default)]
pub struct Forces {
    /// `SPIRA_GATE_ALL=1`: the caller asked for the full corpus.
    pub gate_all: bool,
    /// `SPIRA_GATE_UNIT_MAX_CRATES`: the most crates the unit tier may select before it narrows.
    pub unit_cap: Option<usize>,
}

/// `gate_mode = fences` in force and nothing forcing the whole sequence: the trial runs the
/// fences, the build fence and the unit tests of the crates a branch touches — the gate string
/// without its suites step. The engine reads this before the verdict key, since the
/// string it hashes is the one that runs.
pub fn fences_only(mode: GateMode, forces: &Forces) -> bool {
    mode == GateMode::Fences && !forces.gate_all
}

/// THE DECISION (DESIGN.md "Composition").
pub fn compose(
    mode: GateMode,
    forces: &Forces,
    changed: &[Changed],
    members: Result<&[Member], &str>,
) -> Composition {
    let suites = |why: &str| Composition::Suites { why: why.into() };
    if mode == GateMode::Suites {
        return suites("mode");
    }
    if forces.gate_all {
        return suites("gate-all");
    }
    // Fences mode: the fences and the build fence always run; a branch that touches crates also
    // runs those crates' unit tests (and their dependents'), so a broken unit test is red at the
    // gate rather than in a round. Scripts and workspace inputs add nothing here: the round still
    // tests them.
    if fences_only(mode, forces) {
        let Ok(members) = members else { return Composition::Fences };
        let mut touched = BTreeSet::new();
        let mut readers = BTreeSet::new();
        for c in changed {
            let class = classify(c, members);
            add_readers(&c.path, &class, members, &mut touched, &mut readers);
            if let Class::Crate(n) = class {
                touched.insert(n);
            }
        }
        return unit_of(touched, readers, members, forces.unit_cap);
    }
    let members = match members {
        Ok(m) => m,
        Err(_) => return suites("no-metadata"),
    };
    let mut touched = BTreeSet::new();
    let mut workspace = false;
    let mut readers = BTreeSet::new();
    for c in changed {
        let class = classify(c, members);
        add_readers(&c.path, &class, members, &mut touched, &mut readers);
        match class {
            Class::Script => return suites("script"),
            Class::Crate(n) => {
                touched.insert(n);
            }
            Class::Workspace => workspace = true,
            Class::Doc | Class::Suite | Class::Config | Class::Units | Class::Registry => {}
        }
    }
    if workspace {
        let all: Vec<String> = {
            let mut v: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        if all.is_empty() {
            return suites("no-metadata");
        }
        return Composition::Unit {
            touched: vec!["*".into()],
            targets: targets_of(&all, members),
            crates: all,
            dropped: Vec::new(),
        };
    }
    unit_of(touched, readers, members, forces.unit_cap)
}

fn unit_of(mut touched: BTreeSet<String>, readers: BTreeSet<String>, members: &[Member], cap: Option<usize>) -> Composition {
    if touched.is_empty() && readers.is_empty() {
        return Composition::Fences;
    }
    let within = |mut crates: Vec<String>| {
        crates.extend(readers.iter().cloned());
        crates.sort();
        crates.dedup();
        crates
    };
    let full = within(with_transitive_dependents(&touched, members));
    let mut crates = full.clone();
    if cap.is_some_and(|c| crates.len() > c) {
        crates = within(with_dependents(&touched, members));
        if cap.is_some_and(|c| crates.len() > c) {
            crates = within(touched.iter().cloned().collect());
        }
    }
    touched.extend(readers.iter().cloned());
    let dropped = full.into_iter().filter(|c| !crates.contains(c)).collect();
    Composition::Unit { touched: touched.into_iter().collect(), targets: targets_of(&crates, members), crates, dropped }
}

fn targets_of(crates: &[String], members: &[Member]) -> Targets {
    members.iter().filter(|m| crates.contains(&m.name)).fold(Targets::default(), |acc, m| acc.union(m.targets))
}

/// `cargo metadata --format-version 1 --no-deps` → the workspace members.
pub fn parse_metadata(json: &str) -> Result<Vec<Member>, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("cargo metadata: {e}"))?;
    let root = v
        .get("workspace_root")
        .and_then(|r| r.as_str())
        .ok_or("cargo metadata: no workspace_root")?
        .trim_end_matches('/')
        .to_string();
    let members: BTreeSet<&str> = v
        .get("workspace_members")
        .and_then(|m| m.as_array())
        .ok_or("cargo metadata: no workspace_members")?
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    let pkgs = v
        .get("packages")
        .and_then(|p| p.as_array())
        .ok_or("cargo metadata: no packages")?;
    // (name, absolute dir)
    #[allow(clippy::type_complexity)]
    let mut raw: Vec<(String, String, Vec<(String, bool)>, Targets)> = Vec::new();
    for p in pkgs {
        let id = p.get("id").and_then(|x| x.as_str()).unwrap_or("");
        if !members.contains(id) {
            continue;
        }
        let name = p
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let manifest = p
            .get("manifest_path")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let dir = manifest
            .strip_suffix("/Cargo.toml")
            .unwrap_or(manifest)
            .to_string();
        let dep_dirs = p
            .get("dependencies")
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
            .filter_map(|d| {
                let path = d.get("path").and_then(|x| x.as_str())?;
                let dev = d.get("kind").and_then(|k| k.as_str()) == Some("dev");
                Some((path.trim_end_matches('/').to_string(), dev))
            })
            .collect();
        if name.is_empty() {
            return Err("cargo metadata: a member with no name".into());
        }
        let mut targets = Targets::default();
        for kind in p.get("targets").and_then(|t| t.as_array()).into_iter().flatten().flat_map(|t| t.get("kind").and_then(|k| k.as_array()).into_iter().flatten()).filter_map(|k| k.as_str()) {
            match kind {
                "lib" | "rlib" | "dylib" | "proc-macro" => targets.lib = true,
                "bin" => targets.bin = true,
                "test" => targets.test = true,
                _ => {}
            }
        }
        raw.push((name, dir, dep_dirs, targets));
    }
    let by_dir: HashMap<String, String> =
        raw.iter().map(|(n, d, ..)| (d.clone(), n.clone())).collect();
    let rel = |d: &str| -> String {
        d.strip_prefix(&root)
            .map(|r| r.trim_start_matches('/').to_string())
            .unwrap_or_else(|| d.to_string())
    };
    let mut out: Vec<Member> = raw
        .iter()
        .map(|(n, d, deps, targets)| {
            let names = |all: bool| -> Vec<String> {
                let mut v: Vec<String> = deps
                    .iter()
                    .filter(|(_, dev)| all || !dev)
                    .filter_map(|(dd, _)| by_dir.get(dd).cloned())
                    .collect();
                v.sort();
                v.dedup();
                v
            };
            Member {
                name: n.clone(),
                dir: rel(d),
                deps: names(true),
                build_deps: names(false),
                targets: *targets,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// `git diff --raw -z --no-renames` → the changed paths and whether either side is
/// executable (`100755`).
pub fn parse_diff_raw(z: &[u8]) -> Vec<Changed> {
    let s = String::from_utf8_lossy(z);
    let mut it = s.split('\0');
    let mut out = Vec::new();
    while let Some(head) = it.next() {
        let head = head.trim_start_matches('\n');
        if !head.starts_with(':') {
            continue;
        }
        let Some(path) = it.next() else { break };
        let f: Vec<&str> = head[1..].split(' ').collect();
        let exec = f.iter().take(2).any(|m| *m == "100755");
        out.push(Changed {
            path: path.to_string(),
            exec,
        });
    }
    out
}

/// The width of the host phases: `CARGO_BUILD_JOBS` — the host's cores, whatever
/// the pool's size. How many gates run together is `certify_par`'s job, never a narrower
/// gate (law-reduce-the-count-never-throttle-the-job; gate/DESIGN-admission.md D3). It was
/// `cores ÷ certify_par`, which slowed a lone gate for gates that did not exist.
pub fn jobs(host_cores: u64) -> u64 {
    host_cores.max(1)
}

fn pkgs(crates: &[String]) -> String {
    crates.iter().fold(String::new(), |acc, c| acc + " -p " + c)
}

/// The build fence's step in a repository gate string (`spira/build-fence.sh`, sp-9uro3): a
/// `make build` — the release profile, the whole workspace, cold in a fresh gate tree.
pub const BUILD_FENCE_STEP: &str = "bash spira/build-fence.sh";

/// THE GATE STRING A COMPOSITION RUNS (sp-aprxm). A unit composition has its own build
/// phase, which compiles every touched crate and its dependents, every target, in the
/// `dwarf` profile; the build fence's cold release `make build` of the whole workspace in
/// front of it only proved the same thing again, at 85% of a Rust-only gate's wall. So a unit
/// composition runs the gate string with the build fence's step removed, and every other
/// composition (suites, fences) runs it whole.
///
/// The step is removed only where it is a whole element of the `&&` chain: at the start or
/// after `&& `, and followed by ` &&` or the end. The second value says whether it was.
pub fn gate_string(comp: &Composition, gate: &str) -> (String, bool) {
    if !matches!(comp, Composition::Unit { .. }) {
        return (gate.to_string(), false);
    }
    let step = BUILD_FENCE_STEP;
    let mut out = String::with_capacity(gate.len());
    let mut rest = gate;
    let mut dropped = false;
    while let Some(i) = rest.find(step) {
        let before = &rest[..i];
        let after = &rest[i + step.len()..];
        let head_ok =
            (out.is_empty() && before.trim().is_empty()) || before.trim_end().ends_with("&&");
        let tail_ok = after.trim_start().starts_with("&&") || after.trim().is_empty();
        if !(head_ok && tail_ok) {
            out.push_str(&rest[..i + step.len()]);
            rest = after;
            continue;
        }
        dropped = true;
        if let Some(t) = after.trim_start().strip_prefix("&&") {
            // `… && STEP && rest` or `STEP && rest`: keep what came before, drop `STEP && `.
            out.push_str(before);
            rest = t.trim_start();
        } else {
            // `… && STEP` at the end: drop the ` && STEP`.
            let b = before.trim_end();
            out.push_str(b.strip_suffix("&&").unwrap_or(b).trim_end());
            rest = "";
        }
    }
    out.push_str(rest);
    if dropped && out.trim().is_empty() {
        out = "true".into();
    }
    (out, dropped)
}

/// Top-level `&&` elements of a gate string, or `None` when any element is not a `gate.steps`
/// step (a `bash` script, a quoted tool variable, a `{ }` group) or the string chains with
/// anything but `&&`: such a string keeps its own short-circuit semantics.
pub(crate) fn chain_steps(cmd: &str) -> Option<Vec<String>> {
    let (mut steps, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    let c: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '{' => depth += 1,
            '}' => depth -= 1,
            '&' if depth == 0 && c.get(i + 1) == Some(&'&') => {
                steps.push(std::mem::take(&mut cur));
                i += 2;
                continue;
            }
            '|' | ';' | '\n' if depth == 0 => return None,
            _ => {}
        }
        cur.push(c[i]);
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    steps.push(cur);
    let steps: Vec<String> = steps.into_iter().map(|s| s.trim().to_string()).collect();
    let ok = |s: &String| s.starts_with("bash ") || s.starts_with("\"$") || s.starts_with("{ ");
    (steps.len() > 1 && steps.iter().all(ok)).then_some(steps)
}

/// Whether a gate-string element is the suites step (the selector and the suite runner), not
/// a fence.
pub fn is_suites_step(step: &str) -> bool {
    step_label(step) == "suites"
}

/// THE GATE STRING UNDER `gate_mode = fences`: the string with every suites step removed.
/// A string that is not a plain `&&` chain of steps ([`chain_steps`]) is returned whole, with
/// its suites off by the environment as before; one whose every element is a suites step
/// becomes `true`.
pub fn without_suites(cmd: &str) -> String {
    let Some(steps) = chain_steps(cmd) else { return cmd.to_string() };
    let kept: Vec<String> = steps.into_iter().filter(|s| !is_suites_step(s)).collect();
    if kept.is_empty() {
        return "true".into();
    }
    kept.join(" && ")
}

/// The unit a step reports under: the fence it runs, or `suites` for the selector's group.
fn step_label(step: &str) -> String {
    if step.contains("SPIRA_TESTENV_BIN") || step.contains("SPIRA_SELECT_BIN") {
        return "suites".into();
    }
    if step.contains("SPIRA_LINT_BIN") {
        return crate::fence::LINT.into();
    }
    if step.contains("SPIRA_GUARD_BIN") {
        return crate::fence::GUARD.into();
    }
    if step.contains(&format!(" {} ", crate::fixture::FENCE)) {
        return crate::fixture::FENCE.into();
    }
    let stem = |w: &str| w.rsplit('/').next().unwrap_or(w).trim_end_matches(".sh").to_string();
    match crate::parse::bash_paths(step).first() {
        Some(p) => stem(p),
        None => step
            .split_whitespace()
            .next()
            .map(|w| w.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect())
            .filter(|s: &String| !s.is_empty())
            .unwrap_or_else(|| "step".into()),
    }
}

/// THE VERDICT IS THE SET OF RESULTS, NOT THE FIRST FAILURE. A gate string that is a plain
/// `&&` chain of steps runs every step and prints one `gate-step <ok|RED> rc=<n> :: <unit>`
/// line for each; the exit is the first non-zero status, except that a step's 75 (it could
/// not judge) wins, because a red elsewhere must never let an unjudged step read as passed.
/// Any other string is returned unchanged.
pub fn run_all(cmd: &str) -> String {
    let Some(steps) = chain_steps(cmd) else { return cmd.to_string() };
    let mut s = String::from("_g_rc=0; _g_hard=0\n");
    for st in &steps {
        let l = step_label(st);
        s.push_str(&format!(
            "( {st}\n); _g_r=$?; if [ \"$_g_r\" -eq 0 ]; then echo \"gate-step ok rc=0 :: {l}\"; else echo \"gate-step RED rc=$_g_r :: {l}\"; [ \"$_g_rc\" -eq 0 ] && _g_rc=$_g_r; [ \"$_g_r\" -eq 75 ] && _g_hard=75; fi\n"
        ));
    }
    s.push_str("[ \"$_g_hard\" -ne 0 ] && exit \"$_g_hard\"; exit \"$_g_rc\"");
    s
}

/// (crate, package) pairs: the crate's tests run the package's binary prebuilt in the target
/// directory (batcher-cut's fence probe test runs the built gate), so selecting the crate builds it too.
const NEEDS_BUILT: [(&str, &str); 4] = [("batcher-cut", "gate"), ("spira-lc", "testenv"), ("spira-runtime", "work"), ("spira-runtime", "spira-lc")];

/// The words of the unit build line, shared by the gate's build phase and testenv's unit tier so
/// both produce the same artefacts for the same crates: the default profile, no config override, no `-j`.
///
/// The build is [`spira_config::unit_tier::build_words`] with `--timings`: it compiles the test
/// harnesses only, with the flags the test phase uses, so the test phase recompiles nothing. The
/// non-test build of each lib and bin (code behind `cfg(not(test))`) is the round's job, which
/// builds the whole workspace in release before anything lands (operator ruling 2026-11-10:
/// build targets once).
///
/// Only the named crates build: linking every workspace binary on each gate is what pushed the
/// build phase to the gate cap, and a test reaches its own package's binaries through
/// `CARGO_BIN_EXE_*`, which cargo builds with the test. The exception is a binary of another
/// package that a test runs from beside its own (`NEEDS_BUILT`): it is added to the build.
pub fn unit_build_words(crates: &[String]) -> Vec<String> {
    let mut sel = pkgs(crates);
    for (_, bin) in NEEDS_BUILT
        .iter()
        .filter(|(user, bin)| crates.iter().any(|c| c == user) && !crates.iter().any(|c| c == bin))
    {
        sel.push_str(&format!(" -p {bin}"));
    }
    let mut words = vec!["cargo".to_string()];
    words.extend(spira_config::unit_tier::build_words(None, sel.trim()).split_whitespace().map(String::from));
    words.insert(3, "--timings".into());
    words
}

/// Where the unit phase's nextest config lives, relative to the trial tree.
pub const NEXTEST_CONFIG: &str = "target/gate-unit-nextest.toml";

/// Where the trial's deny list is written, relative to the trial tree.
pub const DENY_LIST: &str = "target/gate-deny-list";

/// The unit nextest run's prefix: one sandbox for the whole run, so each test's own runner finds
/// itself held instead of building a bubblewrap of its own.
const AROUND: &str = "tools/test-sandbox/runner --around";

/// The test phase's commands in the order they run, each over `crates`: the unit tier, the
/// integration targets ([`spira_config::unit_tier`]). A tier none of the selected
/// crates has a target for is absent, since cargo refuses a flag no selected crate satisfies.
pub fn unit_test_commands(crates: &[String], targets: Targets, deny: &tier::DenyList) -> Vec<String> {
    labelled_test_commands(crates, targets, deny).into_iter().map(|(_, c)| c).collect()
}

/// [`unit_test_commands`], each with the `gate-substep` name its seconds are reported under.
fn labelled_test_commands(crates: &[String], targets: Targets, deny: &tier::DenyList) -> Vec<(&'static str, String)> {
    let sel = pkgs(crates);
    let sel = sel.trim();
    let mut cmds = Vec::new();
    if targets.lib || targets.bin {
        let filter = if targets.lib { tier::LIB_BINS } else { "--bins" };
        cmds.push(("unit", format!("{AROUND} cargo {}", tier::unit_words(NEXTEST_CONFIG, None, sel, filter))));
    }
    if targets.test {
        cmds.push(("integration", format!("cargo {}{}", tier::integration_words(None, sel), deny.integration_skips())));
    }
    cmds
}

/// The line a bracketed test command prints with its own wall seconds ([`crate::parse::test_split`]).
pub const SUBSTEP_MARK: &str = "gate-substep";

/// The unit phases' commands, in order: build ([`unit_build_words`]), then the tests
/// ([`unit_test_commands`]): each runs whatever the one before it did, the exit is the first
/// non-zero status, and a sandbox that cannot start `cargo nextest` is 75, could not judge, never a red branch.
pub fn unit_commands(crates: &[String], targets: Targets, jobs: u64, deny: &tier::DenyList) -> [(&'static str, String); 2] {
    let mut test = format!(
        "{AROUND} cargo nextest --version >/dev/null 2>&1 || {{ echo 'gate: the test sandbox could not start cargo nextest — refusing to judge unit tests without it'; exit 75; }}; _g_rc=0",
    );
    for (label, c) in labelled_test_commands(crates, targets, deny) {
        test.push_str(&format!(
            "; _g_t=$SECONDS; {c}; _g_r=$?; echo \"{SUBSTEP_MARK} {label} secs=$((SECONDS-_g_t))\"; [ \"$_g_rc\" -ne 0 ] || _g_rc=$_g_r"
        ));
    }
    test.push_str("; exit $_g_rc");
    [
        ("build", format!("CARGO_BUILD_JOBS={jobs} {}", unit_build_words(crates).join(" "))),
        ("test", test),
    ]
}

fn rerun_command(rerun: &str) -> String {
    format!("cargo {}", spira_config::unit_tier::rerun_words(None, rerun))
}

fn sh_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

/// The unit test phase's command as one line a person can paste at a workspace root: the
/// phase's cleared environment, then the test command narrowed to `rerun` (cargo's own
/// `-p <package> <target>` words).
pub fn reproduce_line(env: &[(String, String)], rerun: &str) -> String {
    let vars: String = env.iter().map(|(k, v)| format!("{k}={} ", sh_quote(v))).collect();
    format!("env -i {vars}{}", rerun_command(rerun))
}

/// The `-p` words of a unit composition's crates.
pub fn unit_packages(crates: &[String]) -> String {
    pkgs(crates).trim_start().to_string()
}

/// The environment of the unit test phase: the gate's own, minus the release's directories on
/// PATH and the release locator. A test that shells out to a bare tool name must not find the
/// running release's binary and, through it, the live stores. `SPIRA_RUN` stays: the build
/// wrapper's admission lease needs it. Jobs and test threads are stated, so a host cargo
/// config's cannot decide them.
pub fn test_phase_env(env: &[(String, String)], jobs: u64) -> Vec<(String, String)> {
    let release = env
        .iter()
        .find(|(k, _)| k == spira_config::RELEASE_ENV)
        .map(|(_, v)| v.trim_end_matches('/').to_string())
        .filter(|r| !r.is_empty());
    env.iter()
        .filter(|(k, _)| k != spira_config::RELEASE_ENV && k != TEST_THREADS_ENV && k != JOBS_ENV)
        .map(|(k, v)| {
            let v = match (&release, k.as_str()) {
                (Some(r), "PATH") => v
                    .split(':')
                    .filter(|seg| *seg != r && !seg.starts_with(&format!("{r}/")))
                    .collect::<Vec<_>>()
                    .join(":"),
                _ => v.clone(),
            };
            (k.clone(), v)
        })
        .chain([
            (TEST_THREADS_ENV.to_string(), spira_config::admission::RUN_TEST_THREADS.to_string()),
            (JOBS_ENV.to_string(), jobs.to_string()),
        ])
        .collect()
}

const TEST_THREADS_ENV: &str = "RUST_TEST_THREADS";
const JOBS_ENV: &str = "CARGO_BUILD_JOBS";

// ------------------------------------------------------------------------------ re-entry

/// THE RE-ENTRY CHECK (sp-p3srm; design item 6): a bead the round returned must pass the
/// suites the round named against it, in its next gate, whatever else that gate selects.
///
/// `required` are the named suites that exist on the tree under test, in the order named;
/// `gone` are named suites the tree no longer has (nothing to run); `invalid` are words that
/// are not a suite name at all (`test-*.sh`, no `/`). Only `required` is enforced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reentry {
    pub required: Vec<String>,
    pub gone: Vec<String>,
    pub invalid: Vec<String>,
}

/// A suite name the runner accepts: `test-<name>.sh`, one path component, no shell
/// metacharacters (it is interpolated into the phase's command). The selector's contract
/// (sp-wx2tw): the ejected suites the selector adds and the ones this check requires are
/// read by the same rule.
pub use suite_select::names::is_suite_name;

/// The ejected-suites list (comma or whitespace separated, as the `.ejected` sidecar writes it) against the tree under test. `exists(name)` answers
/// whether `spira/<name>` is on that tree.
pub fn reentry(ejected: &str, exists: impl Fn(&str) -> bool) -> Reentry {
    let mut r = Reentry::default();
    for w in suite_select::names::split_list(ejected) {
        if !is_suite_name(&w) {
            r.invalid.push(w);
        } else if exists(&w) {
            r.required.push(w);
        } else {
            r.gone.push(w);
        }
    }
    r
}

/// The suites the branch itself changed, by name: a composition that defers `spira/test-*.sh`
/// to the round would otherwise let a solo landing change a suite and run none of them.
pub fn touched_suites(changed: &[Changed]) -> Vec<String> {
    let mut v: Vec<String> = changed
        .iter()
        .filter_map(|c| c.path.strip_prefix("spira/"))
        .filter(|n| is_suite_name(n))
        .map(str::to_string)
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The statuses that satisfy the re-entry check: the suite ran and passed, or the corpus
/// itself says it does not block (disabled, quarantined — a round would not eject on it).
/// SKIPPED, SKIP-REQ, UNREACHED, DEFERRED and silence prove nothing.
const SATISFIED: &[&str] = &["ok", "DISABLED", "QUARANTINED-RED"];

/// Split re-entry's `required` into the suites this gate can prove and the ones the tree's own
/// `spira/skip-allowlist.tsv` declares un-runnable under testenv (first column of each
/// non-comment line). A declared skip can never report `ok` here, so requiring it made every
/// branch that touched one NO_VERDICT forever (a suite that
/// needs nested podman). The dropped ones are named by the caller; the full-suite VM round
/// (law-landing-rounds-run-the-full-suite-on-a-vm) is where they run.
pub fn drop_declared_skips(required: &[String], allowlist: &str) -> (Vec<String>, Vec<String>) {
    let declared: std::collections::BTreeSet<&str> = allowlist
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split('\t').next())
        .collect();
    required.iter().cloned().partition(|s| !declared.contains(s.as_str()))
}

/// Split `required` into the suites that fit `budget` seconds, by their recorded wall time
/// (`secs`), and the ones deferred to the round with that time. A suite with no recorded time
/// is kept: nothing says it is slow.
pub fn defer_over_budget(
    required: &[String],
    secs: &HashMap<String, f64>,
    budget: u64,
) -> (Vec<String>, Vec<(String, u64)>) {
    let mut left = budget as f64;
    let (mut kept, mut deferred) = (Vec::new(), Vec::new());
    for s in required {
        match secs.get(s) {
            Some(&t) if t > left => deferred.push((s.clone(), t.ceil() as u64)),
            Some(&t) => {
                left -= t;
                kept.push(s.clone());
            }
            None => kept.push(s.clone()),
        }
    }
    (kept, deferred)
}

/// The required suites that `out` does not show satisfied, in `required`'s order. A suite
/// reported twice counts by its last report (the re-entry phase re-runs what the gate string
/// deferred).
pub fn unproven(out: &str, required: &[String]) -> Vec<String> {
    let mut last: HashMap<&str, bool> = HashMap::new();
    for line in out.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        for i in 0..w.len() {
            if let Some(r) = required.iter().find(|r| r.as_str() == w[i]) {
                if let Some(st) = w.get(i + 1) {
                    last.insert(r.as_str(), SATISFIED.contains(st));
                }
            }
        }
    }
    required
        .iter()
        .filter(|r| !last.get(r.as_str()).copied().unwrap_or(false))
        .cloned()
        .collect()
}

/// The re-entry phase's command: exactly `suites`, by name, through the suite runner the
/// gate string uses, on the revision under test, with **no `--deadline`** — the round named
/// them, so a budget must not cut them (`SPIRA_GATE_TIMEOUT` still bounds the phase). The
/// runner is the tree's own when its definition declares one, as in `engine::base_rerun_cmd`. The
/// runner's harness faults (2, 3) are NO_VERDICT (75), as in the gate string. The runner reads the
/// live config layers with the tree's config delta applied: it refuses a config missing a key
/// the tree adds.
pub fn reentry_command(suites: &[String]) -> String {
    format!(
        "_t=\"$(release config-stage .)\" || {{ echo 'gate: cannot stage the tree config delta' >&2; exit 75; }}; _b=0; SPIRA_TOML=\"$_t\" \"${{SPIRA_TESTENV_BIN:-testenv}}\" --suites {} \"$SPIRA_GATE_BRANCH\" \"$SPIRA_GATE_REPO\" || _b=$?; case \"$_t\" in */release-config-*/layer*) rm -rf \"${{_t%%/layer*}}\";; esac; case \"$_b\" in 2|3|127) exit 75;; *) exit \"$_b\";; esac",
        suites.join(",")
    )
}

#[cfg(test)]
mod tests {
    #[test_id(tn3jr0d)]
    #[test]
    fn run_all_leaves_a_string_it_cannot_split_alone() {
        for c in ["true", "bash a/b.sh || bash a/c.sh", "bash a/b.sh; bash a/c.sh", "echo x && echo y"] {
            assert_eq!(run_all(c), c);
        }
    }

    #[test_id(trr1cci)]
    #[test]
    fn a_changed_suite_is_named_and_a_helper_is_not() {
        let c = |p: &str| Changed { path: p.into(), exec: false };
        let got = touched_suites(&[c("spira/test-b.sh"), c("spira/testlib/x.sh"), c("spira/test-a.sh"), c("spira/test-b.sh"), c("gate/src/a.rs")]);
        assert_eq!(got, vec!["test-a.sh", "test-b.sh"]);
    }

    use super::*;

    fn m(name: &str, dir: &str, deps: &[&str]) -> Member {
        Member {
            name: name.into(),
            dir: dir.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            build_deps: deps.iter().map(|s| s.to_string()).collect(),
            targets: Targets::default(),
        }
    }

    /// spira-config ← queue ← landing-pass; spira-config ← dwarf; gate alone; a nested
    /// member (cockpit/panel) under a directory that also holds bash.
    fn ws() -> Vec<Member> {
        vec![
            m("spira-config", "spira-config", &[]),
            m("queue", "queue", &["spira-config"]),
            m("landing-pass", "landing-pass", &["queue"]),
            m("dwarf", "dwarf", &["spira-config"]),
            m("gate", "gate", &[]),
            m("panel", "cockpit/panel", &[]),
            m("release", "release", &[]),
        ]
    }

    fn ch(paths: &[&str]) -> Vec<Changed> {
        paths
            .iter()
            .map(|p| Changed {
                path: p.to_string(),
                exec: false,
            })
            .collect()
    }

    fn unit(mode: GateMode, paths: &[&str]) -> Composition {
        let w = ws();
        compose(mode, &Forces::default(), &ch(paths), Ok(&w))
    }

    fn crates(c: &Composition) -> Vec<String> {
        match c {
            Composition::Unit { crates, .. } => crates.clone(),
            other => panic!("not unit: {other:?}"),
        }
    }

    #[test_id(t6dr4e9)]
    #[test]
    fn a_dependent_of_a_dependent_is_in_the_test_selection() {
        assert_eq!(crates(&unit(GateMode::Unit, &["spira-config/src/lib.rs"])), v(&["dwarf", "landing-pass", "queue", "spira-config"]));
        assert_eq!(crates(&unit(GateMode::Fences, &["queue/src/lib.rs"])), v(&["landing-pass", "queue"]));
    }

    #[test_id(tfo5rl3)]
    #[test]
    fn a_selection_over_the_cap_narrows_to_direct_dependents_then_to_the_touched_crates() {
        let w = ws();
        let at = |cap: Option<usize>, mode| crates(&compose(mode, &Forces { unit_cap: cap, ..Forces::default() }, &ch(&["spira-config/src/lib.rs"]), Ok(&w)));
        for mode in [GateMode::Unit, GateMode::Fences] {
            assert_eq!(at(None, mode), v(&["dwarf", "landing-pass", "queue", "spira-config"]));
            assert_eq!(at(Some(4), mode), v(&["dwarf", "landing-pass", "queue", "spira-config"]), "at the cap is within it");
            assert_eq!(at(Some(3), mode), v(&["dwarf", "queue", "spira-config"]));
            assert_eq!(at(Some(1), mode), v(&["spira-config"]), "the touched crate is always tested");
        }
    }

    #[test_id(t252a29)]
    #[test]
    fn a_selection_the_cap_narrows_names_the_dependents_it_dropped() {
        let w = ws();
        let at = |cap| compose(GateMode::Fences, &Forces { unit_cap: cap, ..Forces::default() }, &ch(&["spira-config/src/lib.rs"]), Ok(&w));
        let Composition::Unit { crates, dropped, .. } = at(Some(1)) else { panic!("not unit") };
        assert_eq!(crates, v(&["spira-config"]));
        assert_eq!(dropped, v(&["dwarf", "landing-pass", "queue"]));
        let Composition::Unit { dropped, .. } = at(None) else { panic!("not unit") };
        assert!(dropped.is_empty());
    }

    #[test_id(td2t29g)]
    #[test]
    fn a_reader_of_a_touched_file_survives_the_cap() {
        let w = ws();
        let f = Forces { unit_cap: Some(1), ..Forces::default() };
        let got = compose(GateMode::Unit, &f, &ch(&["spira-config/src/lib.rs", "systemd/a.service"]), Ok(&w));
        assert_eq!(crates(&got), v(&["release", "spira-config"]));
    }

    #[test_id(tp2d7ss)]
    #[test]
    fn the_composition_table() {
        use Composition::*;
        let s = |w: &str| Suites { why: w.into() };
        let rows: &[(&[&str], Composition)] = &[
            (
                &["gate/src/engine.rs"],
                Unit {
                    touched: vec!["gate".into()],
                    crates: vec!["gate".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (
                &["gate/src/engine.rs", "docs/x.md", "spira/test-gate.sh"],
                Unit {
                    touched: vec!["gate".into()],
                    crates: vec!["gate".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (
                &["gate/DESIGN.md"],
                Unit {
                    touched: vec!["gate".into()],
                    crates: vec!["gate".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (
                &["cockpit/panel/src/main.rs"],
                Unit {
                    touched: vec!["panel".into()],
                    crates: vec!["panel".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (
                &["cockpit/panel/capture.sh"],
                Unit {
                    touched: vec!["panel".into()],
                    crates: vec!["panel".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (&["gate/src/engine.rs", "spira/lib.sh"], s("script")),
            (&["cockpit/moot-sweep.sh"], s("script")),
            (&["systemd/units.sh"], s("script")),
            (&["rule.sh"], s("script")),
            (&["tools/helper.py"], s("script")),
            (
                &["docs/a.md", "README.md", "INCIDENT-x.md", "notes.txt"],
                Fences,
            ),
            (
                &["spira/test-foo.sh", "spira/testlib/gate-fixture.sh"],
                Fences,
            ),
            (
                &["systemd/spira-gate.service", "spira/deps.toml"],
                Unit {
                    touched: vec!["release".into()],
                    crates: vec!["release".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (
                &["systemd/dolt-tmp-prune.service"],
                Unit {
                    touched: vec!["release".into()],
                    crates: vec!["release".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (
                &["spira/conf.d/SPIRA_TASK_MODES"],
                Unit {
                    touched: vec!["gate".into(), "queue".into(), "release".into(), "spira-config".into()],
                    crates: vec!["gate".into(), "queue".into(), "release".into(), "spira-config".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (
                &["spira/config-delta.d/sp-x.toml", "spira/config-delta.toml"],
                Unit {
                    touched: vec!["gate".into(), "queue".into(), "release".into(), "spira-config".into()],
                    crates: vec!["gate".into(), "queue".into(), "release".into(), "spira-config".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (
                &["spira/conf.d/X", "gate/src/x.rs"],
                Unit {
                    touched: vec!["gate".into(), "queue".into(), "release".into(), "spira-config".into()],
                    crates: vec!["gate".into(), "queue".into(), "release".into(), "spira-config".into()],
                    targets: Targets::default(),
                    dropped: vec![],
                },
            ),
            (&[], Fences),
        ];
        for (paths, want) in rows {
            assert_eq!(&unit(GateMode::Unit, paths), want, "{paths:?}");
        }
    }

    #[test_id(tyridk6)]
    #[test]
    fn a_registry_edit_adds_the_readers_after_the_dependents_expansion_in_both_modes() {
        for mode in [GateMode::Unit, GateMode::Fences] {
            assert_eq!(crates(&unit(mode, &["spira/conf.d/X"])), v(&["gate", "queue", "release", "spira-config"]), "{mode:?}");
            assert_eq!(
                crates(&unit(mode, &["spira/conf.d/X", "dwarf/src/x.rs"])),
                v(&["dwarf", "gate", "queue", "release", "spira-config"]),
                "{mode:?}"
            );
        }
        assert_eq!(
            crates(&unit(GateMode::Unit, &["spira/conf.d/X", "queue/src/x.rs"])),
            v(&["gate", "landing-pass", "queue", "release", "spira-config"])
        );
    }

    #[test_id(tps94xp)]
    #[test]
    fn a_registry_file_with_an_exec_bit_is_still_registry() {
        let w = ws();
        let c = Changed { path: "spira/conf.d/X".into(), exec: true };
        assert_eq!(classify(&c, &w), Class::Registry);
        assert_eq!(classify(&Changed { path: "spira/deps.toml".into(), exec: false }, &w), Class::Config);
    }

    #[test_id(tcdm07o)]
    #[test]
    fn a_registry_edit_selects_the_readers_and_not_spira_configs_dependents() {
        let json = r#"{"workspace_root":"/t","workspace_members":["a","b","c"],"packages":[
            {"id":"a","name":"spira-config","manifest_path":"/t/spira-config/Cargo.toml","dependencies":[]},
            {"id":"b","name":"queue","manifest_path":"/t/queue/Cargo.toml","dependencies":[{"name":"spira-config","path":"/t/spira-config"}]},
            {"id":"c","name":"release","manifest_path":"/t/release/Cargo.toml","dependencies":[]}]}"#;
        let raw = b":100644 100644 aaa bbb M\0spira/conf.d/SPIRA_TASK_MODES\0";
        for transitive in [false, true] {
            assert_eq!(select_crates(raw, json, transitive).unwrap(), v(&["queue", "release", "spira-config"]));
        }
    }

    #[test_id(t87z1ns)]
    #[test]
    fn a_workspace_input_touches_every_member() {
        for p in [
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            "Makefile",
            ".cargo/config.toml",
        ] {
            let c = unit(GateMode::Unit, &[p]);
            assert_eq!(
                crates(&c),
                [
                    "dwarf",
                    "gate",
                    "landing-pass",
                    "panel",
                    "queue",
                    "release",
                    "spira-config"
                ],
                "{p}"
            );
        }
    }

    #[test_id(t915bw1)]
    #[test]
    fn an_executable_without_an_extension_is_a_script() {
        let w = ws();
        let c = compose(
            GateMode::Unit,
            &Forces::default(),
            &[Changed {
                path: "spira/hooks/pre-commit".into(),
                exec: true,
            }],
            Ok(&w),
        );
        assert_eq!(
            c,
            Composition::Suites {
                why: "script".into()
            }
        );
        let c = compose(
            GateMode::Unit,
            &Forces::default(),
            &[Changed {
                path: "spira/hooks/pre-commit".into(),
                exec: false,
            }],
            Ok(&w),
        );
        assert_eq!(c, Composition::Fences);
    }

    #[test_id(tc6sgyw)]
    #[test]
    fn suites_mode_is_todays_gate_whatever_is_touched() {
        for paths in [
            &["gate/src/x.rs"][..],
            &["docs/a.md"],
            &["spira/lib.sh"],
            &[],
        ] {
            assert_eq!(
                unit(GateMode::Suites, paths),
                Composition::Suites { why: "mode".into() }
            );
        }
        let w = ws();
        assert_eq!(
            compose(
                GateMode::Suites,
                &Forces::default(),
                &ch(&["x"]),
                Err("boom")
            ),
            Composition::Suites { why: "mode".into() },
            "suites mode never needs the graph"
        );
        let _ = w;
    }

    #[test_id(t75jlpy)]
    #[test]
    fn fences_mode_composes_unit_tests_for_touched_crates_and_fences_for_the_rest() {
        let w = ws();
        // A touched crate runs its (and its dependents') unit tests; a script or workspace input
        // adds no crate of its own, so alone it stays fences-only (the round tests it).
        assert!(matches!(compose(GateMode::Fences, &Forces::default(), &ch(&["gate/src/x.rs"]), Ok(&w)), Composition::Unit { .. }));
        // A unit file is read by its reader crates: their tests run, as in unit mode.
        assert!(matches!(compose(GateMode::Fences, &Forces::default(), &ch(&["systemd/a.service"]), Ok(&w)), Composition::Unit { .. }));
        for paths in [
            &["Cargo.lock"][..],
            &["spira/lib.sh"],
            &["Cargo.lock", "spira/lib.sh"],
            &["docs/a.md"],
            &[],
        ] {
            assert_eq!(compose(GateMode::Fences, &Forces::default(), &ch(paths), Ok(&w)), Composition::Fences, "{paths:?}");
        }
        let alone = compose(GateMode::Fences, &Forces::default(), &ch(&["gate/src/x.rs"]), Ok(&w));
        for paths in [&["spira/lib.sh", "gate/src/x.rs"][..], &["Cargo.lock", "gate/src/x.rs"]] {
            let got = compose(GateMode::Fences, &Forces::default(), &ch(paths), Ok(&w));
            assert!(matches!(got, Composition::Unit { .. }), "{paths:?}");
            assert_eq!(crates(&got), crates(&alone), "{paths:?}");
        }
        assert_eq!(compose(GateMode::Fences, &Forces::default(), &ch(&["gate/src/x.rs"]), Err("no cargo")), Composition::Fences);
        assert!(fences_only(GateMode::Fences, &Forces::default()));
        let all = Forces { gate_all: true, ..Forces::default() };
        assert!(!fences_only(GateMode::Fences, &all));
        assert_eq!(compose(GateMode::Fences, &all, &ch(&["gate/src/x.rs"]), Ok(&w)).label(), "suites(gate-all)");
        assert!(!fences_only(GateMode::Unit, &Forces::default()) && !fences_only(GateMode::Suites, &Forces::default()));
    }

    #[test_id(t66955k)]
    #[test]
    fn fences_mode_diff_with_lockfile_script_and_crate_files_runs_the_crate_and_its_dependents() {
        let w = ws();
        let got = compose(
            GateMode::Fences,
            &Forces::default(),
            &ch(&["Cargo.lock", "spira/testlib.sh", "queue/Cargo.toml", "queue/src/x.rs", "queue/tests/y.rs"]),
            Ok(&w),
        );
        let Composition::Unit { touched, .. } = &got else { panic!("not unit: {got:?}") };
        assert_eq!(touched, &vec!["queue".to_string()]);
        assert_eq!(crates(&got), vec!["landing-pass".to_string(), "queue".to_string()]);
    }

    #[test_id(tnrvxm3)]
    #[test]
    fn without_suites_drops_the_suites_step_and_keeps_every_fence() {
        let d = crate::def::parse(&std::fs::read_to_string(
            testkit::manifest_dir().join("../gate.steps")).unwrap()).unwrap();
        let got = without_suites(&d.command());
        assert_eq!(got, d.fences_only(&[]).command());
        assert!(got.contains(BUILD_FENCE_STEP), "{got}");
        assert!(!got.contains("SPIRA_SELECT_BIN") && !got.contains("SPIRA_TESTENV_BIN"), "{got}");
        assert_eq!(chain_steps(&got).unwrap().len(), d.steps.len() - 1, "exactly one step dropped");
        // A string it cannot split is left alone; one that is only suites runs `true`.
        assert_eq!(without_suites("bash spira/fence.sh && run-suites"), "bash spira/fence.sh && run-suites");
        assert_eq!(without_suites("{ \"$SPIRA_SELECT_BIN\" gate; } && { \"$SPIRA_TESTENV_BIN\" x; }"), "true");
    }

    #[test_id(ti6n86k)]
    #[test]
    fn gate_all_and_a_missing_graph_fall_back_to_suites() {
        let w = ws();
        let rs = ch(&["gate/src/x.rs"]);
        let f = Forces { gate_all: true, ..Forces::default() };
        assert_eq!(
            compose(GateMode::Unit, &f, &rs, Ok(&w)).label(),
            "suites(gate-all)"
        );
        assert_eq!(
            compose(GateMode::Unit, &Forces::default(), &rs, Err("no cargo")).label(),
            "suites(no-metadata)"
        );
    }

    #[test_id(tr2ljau)]
    #[test]
    fn labels_and_suites_off() {
        assert_eq!(Composition::Fences.label(), "fences");
        assert!(Composition::Fences.suites_off());
        let u = Composition::Unit {
            touched: vec![],
            crates: vec![],
            targets: Targets::default(),
            dropped: vec![],
        };
        assert_eq!(u.label(), "unit");
        assert!(u.suites_off());
        assert!(!Composition::Suites { why: "mode".into() }.suites_off());
    }

    #[test_id(t6utd3p)]
    #[test]
    fn test_phase_env_drops_the_release_from_path_and_the_locator() {
        let e = |k: &str, v: &str| (k.to_string(), v.to_string());
        let env = vec![
            e("PATH", "r/rel/bin:r/rel/spira:usr/bin:bin:h/.cargo/bin:r/release-x/bin"),
            e(spira_config::RELEASE_ENV, "r/rel/"),
            e("SPIRA_RUN", "run"),
            e("HOME", "h"),
        ];
        let (threads, jobs) = (e("RUST_TEST_THREADS", "4"), e("CARGO_BUILD_JOBS", "12"));
        let got = test_phase_env(&env, 12);
        assert_eq!(
            got,
            vec![
                e("PATH", "usr/bin:bin:h/.cargo/bin:r/release-x/bin"),
                e("SPIRA_RUN", "run"),
                e("HOME", "h"),
                threads.clone(),
                jobs.clone(),
            ]
        );
        assert_eq!(test_phase_env(&[e("PATH", "a:b")], 12), vec![e("PATH", "a:b"), threads, jobs]);
    }

    #[test_id(tsg0yc1)]
    #[test]
    fn test_phase_env_states_jobs_and_threads_over_whatever_the_environment_carried() {
        let e = |k: &str, v: &str| (k.to_string(), v.to_string());
        let got = test_phase_env(&[e("RUST_TEST_THREADS", "99"), e("CARGO_BUILD_JOBS", "99"), e("HOME", "h")], 7);
        assert_eq!(got.iter().filter(|(k, _)| k == "RUST_TEST_THREADS").collect::<Vec<_>>(), [&e("RUST_TEST_THREADS", "4")]);
        assert_eq!(got.iter().filter(|(k, _)| k == "CARGO_BUILD_JOBS").collect::<Vec<_>>(), [&e("CARGO_BUILD_JOBS", "7")]);
    }

    #[test_id(tqqjtxd)]
    #[test]
    fn gate_test_run_sees_no_lifecycle_locators() {
        // Canary: only meaningful inside a gate trial (it sets SPIRA_GATE_BRANCH).
        if std::env::var_os("SPIRA_GATE_BRANCH").is_none() {
            return;
        }
        for k in ["SPIRA_LC_PASSWORD_FILE", "SPIRA_LC_SOCKET"] {
            assert!(std::env::var_os(k).is_none(), "{k} is visible inside a gate test run");
        }
    }

    #[test_id(t4k31pg)]
    #[test]
    fn diff_raw_is_parsed_with_the_exec_bit_from_either_side() {
        let z = b":100644 100644 aaa bbb M\0gate/src/x.rs\0:000000 100755 000 ccc A\0spira/hooks/new\0:100755 000000 ddd 000 D\0old.sh\0";
        assert_eq!(
            parse_diff_raw(z),
            vec![
                Changed {
                    path: "gate/src/x.rs".into(),
                    exec: false
                },
                Changed {
                    path: "spira/hooks/new".into(),
                    exec: true
                },
                Changed {
                    path: "old.sh".into(),
                    exec: true
                },
            ]
        );
        assert!(parse_diff_raw(b"").is_empty());
    }

    #[test_id(t2kkaye)]
    #[test]
    fn a_gate_runs_at_the_hosts_full_width_whatever_the_pool() {
        assert_eq!(jobs(32), 32);
        assert_eq!(jobs(3), 3);
        assert_eq!(jobs(0), 1);
    }

    const ALL: Targets = Targets { lib: true, bin: true, test: true };

    #[test_id(tb8oi83)]
    #[test]
    fn the_unit_build_and_test_name_only_the_changed_crates() {
        let [b, t] = unit_commands(&["spira-runtime".into()], ALL, 4, &Default::default());
        assert!(!b.1.contains("--workspace") && !b.1.contains("&&"), "{}", b.1);
        assert!(b.1.ends_with(" --features integration --config profile.dev.incremental=false -p spira-runtime -p work -p spira-lc"), "{}", b.1);
        assert!(!t.1.contains("--workspace") && !t.1.contains("spira-config"), "{}", t.1);
        let [b, _] = unit_commands(&["spira-sim".into()], ALL, 4, &Default::default());
        assert!(b.1.ends_with(" --features integration --config profile.dev.incremental=false -p spira-sim"), "{}", b.1);
    }

    #[test_id(tr9tlh8)]
    #[test]
    fn the_build_compiles_the_test_harnesses_once_and_the_test_phase_reuses_them() {
        let [b, _] = unit_commands(&["gate".into(), "queue".into()], ALL, 4, &Default::default());
        assert!(!b.1.contains("--all-targets"), "{}", b.1);
        let build = b.1.strip_prefix("CARGO_BUILD_JOBS=4 ").unwrap();
        let sel = ["gate".to_string(), "queue".to_string()];
        let [unit, integ] = two(&unit_test_commands(&sel, ALL, &Default::default()));
        for tests in [&unit, &integ] {
            assert!(tests.contains(" --features integration --config profile.dev.incremental=false -p gate -p queue"), "{tests}");
        }
        assert_eq!(build, format!("cargo {}", spira_config::unit_tier::build_words(None, "-p gate -p queue").replacen("--no-run", "--no-run --timings", 1)));
        let sel = ["batcher-cut".to_string()];
        let [b, _] = unit_commands(&sel, ALL, 4, &Default::default());
        assert_eq!(b.1.strip_prefix("CARGO_BUILD_JOBS=4 ").unwrap(), format!("cargo {}", spira_config::unit_tier::build_words(None, "-p batcher-cut -p gate").replacen("--no-run", "--no-run --timings", 1)), "only the extra built package differs");
    }

    fn two(v: &[String]) -> [String; 2] {
        v.to_vec().try_into().unwrap_or_else(|v: Vec<String>| panic!("{v:?}"))
    }

    #[test_id(tfospwf)]
    #[test]
    fn the_gate_runs_the_rounds_tier_commands_differing_only_in_the_package_selection() {
        use spira_config::unit_tier as t;
        let sel = ["gate".to_string(), "queue".to_string()];
        let [unit, integ] = two(&unit_test_commands(&sel, ALL, &Default::default()));
        let w = |c: &str| c.replace("-p gate -p queue", "--workspace");
        assert_eq!(w(&unit), format!("{AROUND} cargo {}", t::unit_words(NEXTEST_CONFIG, None, "--workspace", t::LIB_BINS)));
        assert_eq!(w(&integ), format!("cargo {}", t::integration_words(None, "--workspace")));
    }

    #[test_id(ttxugzt)]
    #[test]
    fn no_unit_command_names_a_thread_count_or_a_job_width_on_the_tests() {
        for (_, c) in unit_commands(&["gate".into(), "queue".into()], ALL, 4, &Default::default()) {
            assert!(!c.contains("--test-threads"), "{c}");
        }
        let [_, t] = unit_commands(&["gate".into()], ALL, 4, &Default::default());
        assert!(!t.1.contains(" -j ") && !t.1.contains("-j4"), "{}", t.1);
    }

    #[test_id(t82z1cw)]
    #[test]
    fn only_the_unit_nextest_run_goes_through_one_sandbox() {
        let [_, t] = unit_commands(&["gate".into()], ALL, 4, &Default::default());
        let cmds: Vec<&str> = t.1.split("; ").collect();
        let run = |needle: &str| *cmds.iter().find(|c| c.contains(needle) && !c.contains(SUBSTEP_MARK)).unwrap();
        assert!(run("nextest run").contains("tools/test-sandbox/runner --around cargo nextest run"), "{}", t.1);
        assert!(!run("--test '*'").contains("test-sandbox/runner"), "{}", t.1);
    }

    #[test_id(t9yar3b)]
    #[test]
    fn a_tier_no_selected_crate_has_a_target_for_is_not_run() {
        let sel = ["spira-claim".to_string()];
        let bins = Targets { lib: false, bin: true, test: false };
        let cmds = unit_test_commands(&sel, bins, &Default::default());
        assert_eq!(cmds.len(), 1, "{cmds:?}");
        assert!(cmds[0].ends_with(" -p spira-claim --bins") && !cmds[0].contains("--lib"), "{cmds:?}");
        assert!(unit_test_commands(&sel, Targets::default(), &Default::default()).is_empty());
        let lib = Targets { lib: true, bin: false, test: false };
        let cmds = unit_test_commands(&sel, lib, &Default::default());
        assert!(cmds.len() == 1 && cmds[0].ends_with(" --lib --bins"), "{cmds:?}");
        let both = Targets { test: true, ..lib };
        let cmds = unit_test_commands(&sel, both, &Default::default());
        assert!(cmds[0].ends_with(" --lib --bins") && cmds[1].ends_with(" --test '*'") && cmds.len() == 2, "{cmds:?}");
    }

    #[test_id(tljh2z4)]
    #[test]
    fn the_target_kinds_of_a_selection_come_from_cargo_metadata() {
        let meta = |name: &str, kinds: &[&str]| {
            let targets: Vec<String> = kinds.iter().map(|k| format!(r#"{{"kind":["{k}"]}}"#)).collect();
            format!(r#"{{"id":"{name} 0.1.0 (path+file:///w/{name})","name":"{name}","manifest_path":"/w/{name}/Cargo.toml","dependencies":[],"targets":[{}]}}"#, targets.join(","))
        };
        let json = format!(
            r#"{{"workspace_root":"/w","workspace_members":["a 0.1.0 (path+file:///w/a)","b 0.1.0 (path+file:///w/b)"],"packages":[{},{}]}}"#,
            meta("a", &["bin"]),
            meta("b", &["lib", "test"])
        );
        let members = parse_metadata(&json).unwrap();
        let a = Targets { lib: false, bin: true, test: false };
        assert_eq!(members[0].targets, a);
        assert_eq!(targets_of(&["a".into(), "b".into()], &members), Targets { lib: true, bin: true, test: true });
        assert_eq!(targets_of(&["a".into()], &members), a);
    }

    #[test_id(tri8yj3)]
    #[test]
    fn the_test_phase_exits_with_the_first_red_whichever_tier_it_is() {
        let tmp = testkit::TempDir::new("gate-test-phase");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        testkit::write_exe(bin.join("cargo"), "#!/bin/bash\n[ \"$1\" = nextest ] && [ \"$2\" = --version ] && exit 0\ncase \"$*\" in *\"$RED\"*) exit \"$CODE\";; esac\nexit 0\n");
        let [_, t] = unit_commands(&["gate".into()], ALL, 4, &Default::default());
        let script = t.1.replace(&format!("{AROUND} "), "");
        let run = |red: &str, code: &str| {
            let out = std::process::Command::new(system_paths::BIN_BASH)
                .args(["-c", &script])
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("RED", red)
                .env("CODE", code)
                .output()
                .unwrap();
            out.status.code()
        };
        assert_eq!(run("no-such-word", "9"), Some(0), "all green");
        assert_eq!(run("--lib", "100"), Some(100), "a red unit tier");
        assert_eq!(run("--test", "101"), Some(101), "a red integration tier");
    }

    #[test_id(t0sawtx)]
    #[test]
    fn a_missing_nextest_is_could_not_judge_not_a_red_branch() {
        let [_, t] = unit_commands(&["gate".into()], ALL, 4, &Default::default());
        assert!(t.1.starts_with(&format!("{AROUND} cargo nextest --version >/dev/null 2>&1 || {{")), "{}", t.1);
        let ws = testkit::manifest_dir().parent().unwrap().to_path_buf();
        let out = std::process::Command::new(system_paths::BIN_BASH)
            .args(["-c", &t.1])
            .current_dir(&ws)
            .env_clear()
            .env("PATH", system_paths::SYSTEM_PATH_MIN)
            .env("HOME", &ws)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(75), "{}", String::from_utf8_lossy(&out.stdout));
        assert!(String::from_utf8_lossy(&out.stdout).contains("the test sandbox could not start cargo nextest"));
    }

    #[test_id(thtbz45)]
    #[test]
    fn selecting_batcher_cut_builds_the_gate_its_fence_probe_test_runs() {
        let [b, t] = unit_commands(&["batcher-cut".into()], ALL, 4, &Default::default());
        assert!(b.1.ends_with(" -p batcher-cut -p gate"), "{}", b.1);
        assert!(!t.1.contains("-p gate"), "{}", t.1);
        let [b, _] = unit_commands(&["batcher-cut".into(), "gate".into()], ALL, 4, &Default::default());
        assert_eq!(b.1.matches("-p gate").count(), 1, "{}", b.1);
    }

    #[test_id(t1sdshj)]
    #[test]
    fn selecting_spira_lc_builds_the_testenv_its_content_writes_tests_run() {
        assert!(unit_build_words(&["spira-lc".into()]).join(" ").contains("-p testenv"));
        let both = unit_build_words(&["spira-lc".into(), "testenv".into()]).join(" ");
        assert_eq!(both.matches("-p testenv").count(), 1, "{both}");
    }

    #[test_id(tgvu7e3)]
    #[test]
    fn selecting_spira_runtime_builds_the_work_client_and_service_its_broker_exec_test_runs() {
        let words = unit_build_words(&["spira-runtime".into()]).join(" ");
        assert!(words.contains("-p work") && words.contains("-p spira-lc"), "{words}");
        let words = unit_build_words(&["spira-runtime".into(), "work".into(), "spira-lc".into()]).join(" ");
        assert_eq!((words.matches("-p work").count(), words.matches("-p spira-lc").count()), (1, 1), "{words}");
    }

    #[test_id(t6c2b5e)]
    #[test]
    fn unit_commands_build_then_test_each_crate() {
        let [b, t] = unit_commands(&["gate".into(), "queue".into()], ALL, 4, &Default::default());
        assert_eq!(b.0, "build");
        assert_eq!(b.1, "CARGO_BUILD_JOBS=4 cargo test --no-run --timings --features integration --config profile.dev.incremental=false -p gate -p queue");
        assert_eq!(t.0, "test");
        assert!(t.1.contains("; _g_t=$SECONDS; tools/test-sandbox/runner --around cargo nextest run --config-file target/gate-unit-nextest.toml --no-fail-fast --features integration --config profile.dev.incremental=false -p gate -p queue --lib --bins; _g_r=$?; echo \"gate-substep unit secs=$((SECONDS-_g_t))\"; "), "{}", t.1);
        assert!(t.1.contains("; _g_t=$SECONDS; cargo test --no-fail-fast --features integration --config profile.dev.incremental=false -p gate -p queue --test '*'; _g_r=$?; echo \"gate-substep integration secs=$((SECONDS-_g_t))\"; [ \"$_g_rc\" -ne 0 ] || _g_rc=$_g_r; exit $_g_rc"), "{}", t.1);
    }

    #[test_id(tsffy7m)]
    #[test]
    fn reproduce_line_quotes_values_and_shares_the_test_commands_flags() {
        let env = vec![("A".to_string(), "x y'z".to_string()), ("PATH".to_string(), "bin".to_string())];
        let line = reproduce_line(&env, "-p gate --lib");
        assert_eq!(
            line,
            "env -i A='x y'\\''z' PATH='bin' cargo test --no-fail-fast --features integration --config profile.dev.incremental=false -p gate --lib"
        );
    }

    #[test_id(tw6sm2e)]
    #[test]
    fn the_dev_profile_keeps_debuginfo_out_of_the_link() {
        let dev = include_str!("../../Cargo.toml").split("[profile.dev]").nth(1).unwrap().split("\n[").next().unwrap();
        assert!(dev.contains("split-debuginfo = \"unpacked\""), "{dev}");
    }

    #[test_id(tr8mnh9)]
    #[test]
    fn unit_commands_name_no_profile_and_every_cargo_call_is_one_shot() {
        for (_, c) in unit_commands(&["gate".into()], ALL, 4, &Default::default()) {
            assert!(!c.contains("--profile"), "{c}");
            assert!(c.contains("--config profile.dev.incremental=false"), "{c}");
        }
    }

    // -------------------------------------------------------------- re-entry (sp-p3srm)

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test_id(tfvge6r)]
    #[test]
    fn suite_names_are_one_test_component_and_nothing_else() {
        assert!(is_suite_name("test-gate-touched.sh"));
        assert!(is_suite_name("test-a_b.2.sh"));
        for bad in [
            "test-.sh",
            "gate.sh",
            "spira/test-a.sh",
            "../test-a.sh",
            "test-a.sh;rm",
            "test-$(x).sh",
            "test-a.bash",
        ] {
            assert!(!is_suite_name(bad), "{bad}");
        }
    }

    #[test_id(t61b91a)]
    #[test]
    fn reentry_splits_the_named_suites_by_what_the_tree_has() {
        let on_tree = |s: &str| s != "test-gone.sh";
        let r = reentry(
            "test-b.sh,test-gone.sh, test-c.sh  test-b.sh,bad;x,",
            on_tree,
        );
        assert_eq!(
            r.required,
            v(&["test-b.sh", "test-c.sh"]),
            "order kept, duplicates once"
        );
        assert_eq!(r.gone, v(&["test-gone.sh"]));
        assert_eq!(r.invalid, v(&["bad;x"]));
        assert_eq!(reentry("", on_tree), Reentry::default());
        assert_eq!(reentry(" \n", on_tree), Reentry::default());
    }

    #[test_id(taaqvuj)]
    #[test]
    fn only_ok_disabled_or_quarantined_satisfies_a_named_suite() {
        let req = v(&[
            "test-a.sh",
            "test-b.sh",
            "test-c.sh",
            "test-d.sh",
            "test-e.sh",
            "test-f.sh",
        ]);
        let out = "  test-a.sh   ok      3s
  test-b.sh   SKIPPED
  test-c.sh   DISABLED
  test-d.sh   QUARANTINED-RED  rc=1 after 2s
  test-e.sh   DEFERRED deadline
cargo: test tests::x ... ok";
        assert_eq!(
            unproven(out, &req),
            v(&["test-b.sh", "test-e.sh", "test-f.sh"])
        );
        for st in [
            "RED     rc=1 after 2s",
            "TIMEOUT after 600s",
            "UNREACHED",
            "SKIP-REQ requires:docker",
        ] {
            assert_eq!(
                unproven(&format!("  test-a.sh   {st}"), &v(&["test-a.sh"])),
                v(&["test-a.sh"]),
                "{st}"
            );
        }
        assert!(unproven("anything", &[]).is_empty());
    }

    #[test_id(txhxtk2)]
    #[test]
    fn the_last_report_of_a_suite_counts() {
        let req = v(&["test-a.sh"]);
        assert!(unproven("  test-a.sh DEFERRED deadline\n  test-a.sh ok 3s", &req).is_empty());
        assert_eq!(
            unproven("  test-a.sh ok 3s\n  test-a.sh RED rc=1", &req),
            req
        );
    }

    #[test_id(t961082)]
    #[test]
    fn a_named_suite_recorded_over_the_remaining_budget_is_deferred_with_its_time() {
        let secs: HashMap<String, f64> =
            [("test-a.sh", 100.0), ("test-b.sh", 120.0), ("test-c.sh", 30.0)]
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect();
        let (kept, deferred) =
            defer_over_budget(&v(&["test-a.sh", "test-b.sh", "test-c.sh", "test-new.sh"]), &secs, 190);
        assert_eq!(kept, v(&["test-a.sh", "test-c.sh", "test-new.sh"]));
        assert_eq!(deferred, vec![("test-b.sh".to_string(), 120)]);
    }

    #[test_id(tfvs4oy)]
    #[test]
    fn the_reentry_command_names_exactly_the_suites_with_no_deadline() {
        let c = reentry_command(&v(&["test-a.sh", "test-b.sh"]));
        assert!(
            c.contains(
                "--suites test-a.sh,test-b.sh \"$SPIRA_GATE_BRANCH\" \"$SPIRA_GATE_REPO\""
            ),
            "{c}"
        );
        assert!(!c.contains("--deadline"));
        assert!(c.contains("2|3|127) exit 75"), "a testenv missing from PATH (127) is NO_VERDICT");
        let bin = "\"${SPIRA_TESTENV_BIN:-testenv}\" --suites";
        assert!(c.contains(bin), "{c}");
        assert!(crate::engine::base_rerun_cmd(&v(&["test-a.sh"])).contains(bin), "the re-entry and the base re-run invoke one binary");
    }

    #[test_id(tti2uiq)]
    #[test]
    fn ejected_suites_no_longer_force_the_suites_composition() {
        // sp-p3srm narrows sp-2ghui: a returned Rust-only bead keeps its unit composition and
        // the re-entry phase adds the named suites.
        let w = ws();
        let c = compose(
            GateMode::Unit,
            &Forces::default(),
            &ch(&["gate/src/x.rs"]),
            Ok(&w),
        );
        assert_eq!(c.label(), "unit");
    }

    // ------------------------------------------------------- the build fence (sp-aprxm)

    /// The production gate string's shape (the spira repository's configured gate, abridged).
    const PROD: &str = r#"bash spira/inventory.sh && "$SPIRA_LINT_BIN" && bash spira/wiki-add-fence.sh && bash spira/build-fence.sh && { _s="$("$SPIRA_SELECT_BIN" gate "$SPIRA_GATE_BASE" x)" || exit 75; [ -n "$_s" ] || exit 0; }"#;

    fn unit_c() -> Composition {
        Composition::Unit {
            touched: vec!["tsd".into()],
            crates: vec!["tsd".into()],
            targets: Targets::default(),
            dropped: vec![],
        }
    }

    #[test_id(t0d1c8l)]
    #[test]
    fn a_unit_composition_builds_once_the_build_fence_is_dropped() {
        let (g, dropped) = gate_string(&unit_c(), PROD);
        assert!(dropped);
        assert_eq!(
            g,
            r#"bash spira/inventory.sh && "$SPIRA_LINT_BIN" && bash spira/wiki-add-fence.sh && { _s="$("$SPIRA_SELECT_BIN" gate "$SPIRA_GATE_BASE" x)" || exit 75; [ -n "$_s" ] || exit 0; }"#
        );
        assert!(!g.contains("build-fence"));
        // …and its build phase compiles the test harnesses the test phase then reuses.
        let [b, _] = unit_commands(&["tsd".into()], ALL, 4, &Default::default());
        assert!(
            b.1.contains(" cargo test --no-run --timings "),
            "{}",
            b.1
        );
    }

    #[test_id(tx4m85l)]
    #[test]
    fn suites_and_fences_compositions_keep_the_build_fence() {
        for c in [
            Composition::Suites { why: "mode".into() },
            Composition::Fences,
        ] {
            assert_eq!(gate_string(&c, PROD), (PROD.to_string(), false), "{c:?}");
        }
    }

    #[test_id(thun5qm)]
    #[test]
    fn the_build_fence_is_dropped_only_as_a_whole_chain_element() {
        let u = unit_c();
        let s = BUILD_FENCE_STEP;
        let rows: &[(String, &str, bool)] = &[
            (format!("{s} && b"), "b", true),
            (format!("a && {s}"), "a", true),
            (format!("a && {s} && b && {s}"), "a && b", true),
            (s.to_string(), "true", true),
            (format!("a &&   {s}   && b"), "a &&   b", true),
            // Not a chain element: kept as written.
            (format!("a; {s} && b"), "", false),
            (format!("a && {s} || x"), "", false),
            (format!("a && {s}2 && b"), "", false),
            (format!("a && x{s} && b"), "", false),
            ("a && b".to_string(), "", false),
            (String::new(), "", false),
        ];
        for (input, want, dropped) in rows {
            let (g, d) = gate_string(&u, input);
            assert_eq!(d, *dropped, "{input:?} -> {g:?}");
            let want = if *dropped {
                want.to_string()
            } else {
                input.clone()
            };
            assert_eq!(g, want, "{input:?}");
        }
    }
}
