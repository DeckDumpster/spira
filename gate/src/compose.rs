//! Composition by touched set (DESIGN.md "Composition", sp-2ghui): what a branch touches
//! decides what the gate runs. Pure: the engine hands in the changed files, the workspace
//! graph and the mode, and gets back the phases to run.

use spira_config::GateMode;
use std::collections::{BTreeSet, HashMap};

/// One changed path between the landing ref and the tree under test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    /// The file is executable on either side (a script with no extension is still a script).
    pub exec: bool,
}

/// A workspace member, from `cargo metadata --no-deps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    /// The member's directory relative to the workspace root, `/`-separated, no trailing `/`.
    pub dir: String,
    /// Names of the workspace members it depends on (normal, dev or build).
    pub deps: Vec<String>,
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
    /// Anything else (units, data files, examples): nothing to build.
    Config,
}

/// What the gate runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Composition {
    /// The repository's gate string, whole — today's sequence. `why` names the reason.
    Suites { why: String },
    /// The gate string with suites off and without the build fence ([`gate_string`]), then
    /// `cargo build` and `cargo test` of `crates` on the host. `touched` are the members whose files changed (or `*` for a workspace
    /// input); `crates` adds their reverse dependents, sorted.
    Unit {
        touched: Vec<String>,
        crates: Vec<String>,
    },
    /// The gate string with suites off: nothing buildable changed.
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
    if p.ends_with(".sh") || p.ends_with(".bash") || p.ends_with(".py") || c.exec {
        return Class::Script;
    }
    Class::Config
}

/// `seed` plus every member that depends on one of them, transitively; sorted.
pub fn with_dependents(seed: &BTreeSet<String>, members: &[Member]) -> Vec<String> {
    let mut rdeps: HashMap<&str, Vec<&str>> = HashMap::new();
    for m in members {
        for d in &m.deps {
            rdeps.entry(d.as_str()).or_default().push(m.name.as_str());
        }
    }
    let mut out: BTreeSet<String> = seed.clone();
    let mut todo: Vec<String> = seed.iter().cloned().collect();
    while let Some(n) = todo.pop() {
        for r in rdeps.get(n.as_str()).into_iter().flatten() {
            if out.insert(r.to_string()) {
                todo.push(r.to_string());
            }
        }
    }
    out.into_iter().collect()
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
    let members = match members {
        Ok(m) => m,
        Err(_) => return suites("no-metadata"),
    };
    let mut touched = BTreeSet::new();
    let mut workspace = false;
    for c in changed {
        match classify(c, members) {
            Class::Script => return suites("script"),
            Class::Crate(n) => {
                touched.insert(n);
            }
            Class::Workspace => workspace = true,
            Class::Doc | Class::Suite | Class::Config => {}
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
            crates: all,
        };
    }
    if touched.is_empty() {
        return Composition::Fences;
    }
    let crates = with_dependents(&touched, members);
    Composition::Unit {
        touched: touched.into_iter().collect(),
        crates,
    }
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
    let mut raw: Vec<(String, String, Vec<String>)> = Vec::new();
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
            .filter_map(|d| d.get("path").and_then(|x| x.as_str()))
            .map(|s| s.trim_end_matches('/').to_string())
            .collect();
        if name.is_empty() {
            return Err("cargo metadata: a member with no name".into());
        }
        raw.push((name, dir, dep_dirs));
    }
    let by_dir: HashMap<String, String> =
        raw.iter().map(|(n, d, _)| (d.clone(), n.clone())).collect();
    let rel = |d: &str| -> String {
        d.strip_prefix(&root)
            .map(|r| r.trim_start_matches('/').to_string())
            .unwrap_or_else(|| d.to_string())
    };
    let mut out: Vec<Member> = raw
        .iter()
        .map(|(n, d, deps)| {
            let mut deps: Vec<String> = deps
                .iter()
                .filter_map(|dd| by_dir.get(dd).cloned())
                .collect();
            deps.sort();
            deps.dedup();
            Member {
                name: n.clone(),
                dir: rel(d),
                deps,
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

/// The CPU budget of the host phases: `cargo -j` and `--test-threads`. The host's cores
/// divided among the admission pool, so the pool's gates together use about the host.
pub fn jobs(host_cores: u64, par: u64) -> u64 {
    (host_cores / par.max(1)).max(1)
}

fn pkgs(crates: &[String]) -> String {
    crates.iter().fold(String::new(), |acc, c| acc + " -p " + c)
}

/// The build fence's step in a repository gate string (`spira/build-fence.sh`, sp-9uro3): a
/// `make build` — the release profile, the whole workspace, cold in a fresh gate tree.
pub const BUILD_FENCE_STEP: &str = "bash spira/build-fence.sh";

/// THE GATE STRING A COMPOSITION RUNS (sp-aprxm). A unit composition has its own build
/// phase, which compiles every touched crate and its reverse dependents, every target, in the
/// `aeon` profile; the build fence's cold release `make build` of the whole workspace in
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

/// The unit phases' commands, in order: build (the `aeon` profile), then the tests.
///
/// The build is `cargo build --all-targets` (sp-aprxm), not `cargo test --no-run`: the latter
/// compiles a binary crate only under `cfg(test)` unless it has integration tests, so code
/// behind `#[cfg(not(test))]` (spira-claim's `main`) would go uncompiled once the build fence
/// no longer runs in front of it. `--all-targets` builds every lib and bin both ways and the
/// test harnesses; the test phase reuses those harnesses (same profile, same mode).
pub fn unit_commands(crates: &[String], jobs: u64) -> [(&'static str, String); 2] {
    let p = pkgs(crates);
    [
        (
            "build",
            format!("cargo build --profile aeon -j {jobs} --all-targets{p}"),
        ),
        (
            "test",
            format!("cargo test --profile aeon -j {jobs}{p} -- --test-threads={jobs}"),
        ),
    ]
}

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
/// metacharacters (it is interpolated into the phase's command).
pub fn is_suite_name(s: &str) -> bool {
    s.len() > "test-.sh".len()
        && s.starts_with("test-")
        && s.ends_with(".sh")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// The ejected-suites list (comma or whitespace separated, as the `.ejected` sidecar and the
/// EJECTED landstate row write it) against the tree under test. `exists(name)` answers
/// whether `spira/<name>` is on that tree.
pub fn reentry(ejected: &str, exists: impl Fn(&str) -> bool) -> Reentry {
    let mut r = Reentry::default();
    for w in ejected.split(|c: char| c == ',' || c.is_whitespace()) {
        if w.is_empty()
            || r.required
                .iter()
                .chain(&r.gone)
                .chain(&r.invalid)
                .any(|x| x == w)
        {
            continue;
        }
        if !is_suite_name(w) {
            r.invalid.push(w.to_string());
        } else if exists(w) {
            r.required.push(w.to_string());
        } else {
            r.gone.push(w.to_string());
        }
    }
    r
}

/// The statuses that satisfy the re-entry check: the suite ran and passed, or the corpus
/// itself says it does not block (disabled, quarantined — a round would not eject on it).
/// SKIPPED, SKIP-REQ, UNREACHED, DEFERRED and silence prove nothing.
const SATISFIED: &[&str] = &["ok", "DISABLED", "QUARANTINED-RED"];

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
/// runner's harness faults (2, 3) are NO_VERDICT (75), as in the gate string.
pub fn reentry_command(suites: &[String]) -> String {
    format!(
        "[ -n \"${{SPIRA_TESTENV_BIN:-}}\" ] || {{ echo 'gate: re-entry: SPIRA_TESTENV_BIN is unset — cannot run the suites the round named' >&2; exit 75; }}; \
_b=0; \"$SPIRA_TESTENV_BIN\" --suites {} \"$SPIRA_GATE_BRANCH\" || _b=$?; case \"$_b\" in 2|3) exit 75;; *) exit \"$_b\";; esac",
        suites.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(name: &str, dir: &str, deps: &[&str]) -> Member {
        Member {
            name: name.into(),
            dir: dir.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// spira-config ← queue ← landing-pass; spira-config ← aeon; gate alone; a nested
    /// member (cockpit/panel) under a directory that also holds bash.
    fn ws() -> Vec<Member> {
        vec![
            m("spira-config", "spira-config", &[]),
            m("queue", "queue", &["spira-config"]),
            m("landing-pass", "landing-pass", &["queue"]),
            m("aeon", "aeon", &["spira-config"]),
            m("gate", "gate", &[]),
            m("panel", "cockpit/panel", &[]),
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
                },
            ),
            (
                &["gate/src/engine.rs", "docs/x.md", "spira/test-gate.sh"],
                Unit {
                    touched: vec!["gate".into()],
                    crates: vec!["gate".into()],
                },
            ),
            (
                &["gate/DESIGN.md"],
                Unit {
                    touched: vec!["gate".into()],
                    crates: vec!["gate".into()],
                },
            ),
            (
                &["cockpit/panel/src/main.rs"],
                Unit {
                    touched: vec!["panel".into()],
                    crates: vec!["panel".into()],
                },
            ),
            (
                &["cockpit/panel/capture.sh"],
                Unit {
                    touched: vec!["panel".into()],
                    crates: vec!["panel".into()],
                },
            ),
            (&["gate/src/engine.rs", "spira/lib.sh"], s("script")),
            (&["cockpit/layout.sh"], s("script")),
            (&["systemd/units.sh"], s("script")),
            (&["rule.sh"], s("script")),
            (&["spira/ready-bucket.py"], s("script")),
            (
                &["docs/a.md", "README.md", "INCIDENT-x.md", "notes.txt"],
                Fences,
            ),
            (
                &["spira/test-foo.sh", "spira/testlib/gate-fixture.sh"],
                Fences,
            ),
            (&["systemd/spira-gate.service", "spira/deps.toml"], Fences),
            (&[], Fences),
        ];
        for (paths, want) in rows {
            assert_eq!(&unit(GateMode::Unit, paths), want, "{paths:?}");
        }
    }

    #[test]
    fn a_touched_crate_brings_its_reverse_dependents() {
        let c = unit(GateMode::Unit, &["spira-config/src/lib.rs"]);
        assert_eq!(
            crates(&c),
            ["aeon", "landing-pass", "queue", "spira-config"]
        );
        let Composition::Unit { touched, .. } = &c else {
            unreachable!()
        };
        assert_eq!(touched, &["spira-config"]);
        assert_eq!(
            crates(&unit(GateMode::Unit, &["queue/src/x.rs"])),
            ["landing-pass", "queue"]
        );
        assert_eq!(
            crates(&unit(GateMode::Unit, &["landing-pass/src/x.rs"])),
            ["landing-pass"]
        );
    }

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
                    "aeon",
                    "gate",
                    "landing-pass",
                    "panel",
                    "queue",
                    "spira-config"
                ],
                "{p}"
            );
        }
    }

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

    #[test]
    fn gate_all_and_a_missing_graph_fall_back_to_suites() {
        let w = ws();
        let rs = ch(&["gate/src/x.rs"]);
        let f = Forces { gate_all: true };
        assert_eq!(
            compose(GateMode::Unit, &f, &rs, Ok(&w)).label(),
            "suites(gate-all)"
        );
        assert_eq!(
            compose(GateMode::Unit, &Forces::default(), &rs, Err("no cargo")).label(),
            "suites(no-metadata)"
        );
    }

    #[test]
    fn labels_and_suites_off() {
        assert_eq!(Composition::Fences.label(), "fences");
        assert!(Composition::Fences.suites_off());
        let u = Composition::Unit {
            touched: vec![],
            crates: vec![],
        };
        assert_eq!(u.label(), "unit");
        assert!(u.suites_off());
        assert!(!Composition::Suites { why: "mode".into() }.suites_off());
    }

    #[test]
    fn metadata_is_parsed_to_members_with_workspace_deps_only() {
        let json = r#"{
          "workspace_root": "/t/gate-tree",
          "workspace_members": ["a 0.1.0 (path+file:///t/gate-tree/a)", "b 0.1.0 (path+file:///t/gate-tree/nested/b)"],
          "packages": [
            {"id": "a 0.1.0 (path+file:///t/gate-tree/a)", "name": "a",
             "manifest_path": "/t/gate-tree/a/Cargo.toml",
             "dependencies": [{"name": "serde"}, {"name": "b", "path": "/t/gate-tree/nested/b"}]},
            {"id": "b 0.1.0 (path+file:///t/gate-tree/nested/b)", "name": "b",
             "manifest_path": "/t/gate-tree/nested/b/Cargo.toml", "dependencies": []}
          ]}"#;
        let got = parse_metadata(json).unwrap();
        assert_eq!(got, vec![m("a", "a", &["b"]), m("b", "nested/b", &[])]);
        assert!(parse_metadata("not json").is_err());
        assert!(parse_metadata("{}").is_err());
    }

    #[test]
    fn the_real_workspace_metadata_parses() {
        // A positive control against this workspace's own shape: gate is a member and
        // queue depends on spira-config.
        let out = std::process::Command::new("cargo")
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--offline",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output();
        let Ok(out) = out else { return };
        if !out.status.success() {
            return;
        }
        let ms = parse_metadata(&String::from_utf8_lossy(&out.stdout)).unwrap();
        let q = ms
            .iter()
            .find(|m| m.name == "queue")
            .expect("queue is a member");
        assert!(q.deps.contains(&"spira-config".to_string()), "{q:?}");
        assert!(ms.iter().any(|m| m.name == "gate" && m.dir == "gate"));
        assert!(ms.iter().any(|m| m.dir == "cockpit/panel"));
    }

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

    #[test]
    fn the_cpu_budget_divides_the_host_among_the_pool() {
        assert_eq!(jobs(32, 4), 8);
        assert_eq!(jobs(3, 4), 1);
        assert_eq!(jobs(8, 0), 8);
    }

    #[test]
    fn unit_commands_build_then_test_each_crate() {
        let [b, t] = unit_commands(&["gate".into(), "queue".into()], 4);
        assert_eq!(b.0, "build");
        assert_eq!(
            b.1,
            "cargo build --profile aeon -j 4 --all-targets -p gate -p queue"
        );
        assert_eq!(t.0, "test");
        assert_eq!(
            t.1,
            "cargo test --profile aeon -j 4 -p gate -p queue -- --test-threads=4"
        );
    }

    // -------------------------------------------------------------- re-entry (sp-p3srm)

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

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

    #[test]
    fn the_last_report_of_a_suite_counts() {
        let req = v(&["test-a.sh"]);
        assert!(unproven("  test-a.sh DEFERRED deadline\n  test-a.sh ok 3s", &req).is_empty());
        assert_eq!(
            unproven("  test-a.sh ok 3s\n  test-a.sh RED rc=1", &req),
            req
        );
    }

    #[test]
    fn the_reentry_command_names_exactly_the_suites_with_no_deadline() {
        let c = reentry_command(&v(&["test-a.sh", "test-b.sh"]));
        assert!(
            c.contains(
                "\"$SPIRA_TESTENV_BIN\" --suites test-a.sh,test-b.sh \"$SPIRA_GATE_BRANCH\""
            ),
            "{c}"
        );
        assert!(!c.contains("--deadline"));
        assert!(c.contains("2|3) exit 75"));
        assert!(c.contains("SPIRA_TESTENV_BIN is unset"));
    }

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
    const PROD: &str = r#"bash spira/inventory.sh && "$SPIRA_LINT_BIN" && bash spira/wiki-add-fence.sh && bash spira/build-fence.sh && { _s="$(bash spira/gate-touched.sh "$SPIRA_GATE_BASE" x)"; [ -n "$_s" ] || exit 0; }"#;

    fn unit_c() -> Composition {
        Composition::Unit {
            touched: vec!["tsd".into()],
            crates: vec!["tsd".into()],
        }
    }

    #[test]
    fn a_unit_composition_builds_once_the_build_fence_is_dropped() {
        let (g, dropped) = gate_string(&unit_c(), PROD);
        assert!(dropped);
        assert_eq!(
            g,
            r#"bash spira/inventory.sh && "$SPIRA_LINT_BIN" && bash spira/wiki-add-fence.sh && { _s="$(bash spira/gate-touched.sh "$SPIRA_GATE_BASE" x)"; [ -n "$_s" ] || exit 0; }"#
        );
        assert!(!g.contains("build-fence"));
        // …and its build phase is the compile check that replaces it: every target.
        let [b, _] = unit_commands(&["tsd".into()], 4);
        assert!(
            b.1.starts_with("cargo build ") && b.1.contains("--all-targets"),
            "{}",
            b.1
        );
    }

    #[test]
    fn suites_and_fences_compositions_keep_the_build_fence() {
        for c in [
            Composition::Suites { why: "mode".into() },
            Composition::Fences,
        ] {
            assert_eq!(gate_string(&c, PROD), (PROD.to_string(), false), "{c:?}");
        }
    }

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
