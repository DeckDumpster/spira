//! The gate definition the tree under test carries (sp-quu2w; DESIGN.md "The tree owns its
//! gate"). A repository whose landing ref has a [`PATH`] file is gated by the definition in
//! the tree being judged, never by a string in production config: a branch that ports or
//! deletes a fence edits the definition in the same commit, and its own gate runs what it
//! will land with.
//!
//! The format is one directive per line, so a fence port is a one-line diff and two ports
//! touching different fences merge without conflict:
//!
//! ```text
//! # a comment; blank lines are ignored
//! bin SPIRA_LINT_BIN spira-lint      # build this cargo package from the tree, export $VAR
//! step bash spira/inventory.sh       # one element of the `&&` chain, in order
//! step "$SPIRA_LINT_BIN"
//! own gate/                          # a change to this path must be certified by its own gate
//! ```
//!
//! `own <path>` names the gate's own files (a trailing `/` is a directory prefix, anything
//! else an exact file). It changes no trial: the batcher reads it from the landing ref and
//! admits a change to such a path into a cut only once its own gate certified it.
//!
//! [`Def::command`] joins the steps with ` && `: the exact string production config held,
//! so everything downstream of it (the fences' expectations, the build-fence drop of a unit
//! composition, the verdict key) reads the same kind of string it always has.

use spira_config::gate_source::{self, GateSource};

/// Where a gated tree keeps its definition, relative to the repository root.
pub const PATH: &str = "gate.steps";

/// The gate tree's directory of tools keyed by tree id (sp-g9f3t; DESIGN.md "Tools keyed by
/// the tree they were built from"). Under `target/`, which `git clean -e target` keeps.
pub const TOOLS_DIR: &str = "target/tree-tools";

/// The stamp in a keyed tools directory: the tree id its binaries were built from.
pub const TOOLS_STAMP: &str = "TREE";

/// A binary the steps call that is built from the tree under test, not the installed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bin {
    /// The environment variable the steps read it through (`SPIRA_LINT_BIN`).
    pub var: String,
    /// The cargo package, whose binary of the same name `cargo build -p` produces.
    pub package: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Def {
    pub bins: Vec<Bin>,
    pub steps: Vec<String>,
    pub own: Vec<String>,
}

fn is_var(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_package(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Parse a definition. Every refusal names the line: a definition the gate cannot read is
/// never read as a shorter one.
pub fn parse(text: &str) -> Result<Def, String> {
    let mut d = Def::default();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match word {
            "step" => {
                if rest.is_empty() {
                    return Err(format!("{PATH}:{n}: `step` with no command"));
                }
                d.steps.push(rest.to_string());
            }
            "bin" => {
                let f: Vec<&str> = rest.split_whitespace().collect();
                let [var, package] = f.as_slice() else {
                    return Err(format!("{PATH}:{n}: `bin` takes two words, <VAR> <cargo-package>: {line}"));
                };
                if !is_var(var) || !is_package(package) {
                    return Err(format!("{PATH}:{n}: `bin {var} {package}`: <VAR> must be an upper-case variable name and <cargo-package> a package name"));
                }
                if d.bins.iter().any(|b| b.var == *var) {
                    return Err(format!("{PATH}:{n}: `bin {var}` declared twice"));
                }
                d.bins.push(Bin { var: var.to_string(), package: package.to_string() });
            }
            "own" => {
                if rest.is_empty() {
                    return Err(format!("{PATH}:{n}: `own` with no path"));
                }
                if rest.contains(char::is_whitespace) {
                    return Err(format!("{PATH}:{n}: `own` takes one path, with no whitespace in it: {line}"));
                }
                if rest.starts_with('/') || rest.split('/').any(|c| c == "..") {
                    return Err(format!("{PATH}:{n}: `own {rest}`: the path must be repo-relative, with no `..`"));
                }
                if d.own.iter().any(|o| o == rest) {
                    return Err(format!("{PATH}:{n}: `own {rest}` declared twice"));
                }
                d.own.push(rest.to_string());
            }
            _ => return Err(format!("{PATH}:{n}: unknown directive `{word}` (a line is `step <command>`, `bin <VAR> <package>`, `own <path>`, a `#` comment or blank)")),
        }
    }
    if d.steps.is_empty() {
        return Err(format!("{PATH} names no step — a gate that runs nothing judges nothing"));
    }
    Ok(d)
}

impl Def {
    /// Whether `path` is one of the gate's own files: an exact `own` entry, or under an
    /// `own` directory entry (one ending in `/`).
    pub fn owns(&self, path: &str) -> bool {
        self.own.iter().any(|o| if o.ends_with('/') { path.starts_with(o.as_str()) } else { path == o })
    }

    /// The gate string: the steps, in order, joined as one `&&` chain.
    pub fn command(&self) -> String {
        self.steps.join(" && ")
    }

    /// What the verdict key hashes as "the command": the string, plus the binaries built
    /// from the tree (a different tool set is a different trial).
    pub fn key_text(&self) -> String {
        let mut s = self.command();
        for b in &self.bins {
            s.push_str(&format!("\n# bin {} {}", b.var, b.package));
        }
        s
    }

    /// Where `bin`'s binary lands in `tree`: the `dev` profile, the tree's own target dir
    /// (the gate command runs under `env -i`, so no CARGO_TARGET_DIR moves it).
    pub fn bin_path(tree: &std::path::Path, b: &Bin) -> std::path::PathBuf {
        tree.join("target").join("debug").join(&b.package)
    }

    /// Where a trial's tools live (sp-g9f3t): a directory of the gate tree keyed by the git
    /// tree id they were built from, stamped with that id in `TREE`. The steps read this,
    /// never [`Def::bin_path`], which names no tree.
    pub fn tools_dir(tree: &std::path::Path, tree_id: &str) -> std::path::PathBuf {
        tree.join(TOOLS_DIR).join(tree_id)
    }

    /// The definition `gate_mode = fences` runs: the steps without the suites step
    /// ([`crate::compose::is_suites_step`]), and only the `bin`s a remaining step or one of
    /// `also` (the commands the trial runs beside the string: the re-entry phase) reads
    /// (`$VAR` or `${VAR`), so the tools phase builds what runs — not the suite selector, and
    /// the suite runner only when re-entry suites may run. A definition left with no step runs
    /// `true`.
    pub fn fences_only(&self, also: &[String]) -> Def {
        let mut steps: Vec<String> =
            self.steps.iter().filter(|s| !crate::compose::is_suites_step(s)).cloned().collect();
        if steps.is_empty() {
            steps.push("true".into());
        }
        let reads = |v: &str| steps.iter().chain(also).any(|s| s.contains(&format!("${v}")) || s.contains(&format!("${{{v}")));
        let bins = self.bins.iter().filter(|b| reads(&b.var)).cloned().collect();
        Def { bins, steps, own: self.own.clone() }
    }

    /// The step that selects and runs the suites covering a diff.
    pub fn suites_step(&self) -> Option<String> {
        self.steps.iter().find(|s| crate::compose::is_suites_step(s)).cloned()
    }

    /// Every package this definition builds.
    pub fn packages(&self) -> Vec<String> {
        self.bins.iter().map(|b| b.package.clone()).collect()
    }

    /// The tools phase: build every `bin` package from the tree, in the profile the unit
    /// phases use (so a unit composition's build reuses it), and prove each binary exists.
    /// None when the definition builds nothing.
    pub fn tools_command(&self, jobs: u64) -> Option<String> {
        if self.bins.is_empty() {
            return None;
        }
        let pkgs = self.bins.iter().fold(String::new(), |acc, b| acc + " -p " + &b.package);
        let mut c = format!("cargo build -j {jobs} {}{pkgs}", spira_config::build::one_shot_words("dev"));
        for b in &self.bins {
            c.push_str(&format!(
                // path-ok: the gate builds this binary into the gate tree and names where it lands.
                " && {{ [ -x target/debug/{p} ] || {{ echo 'gate: {PATH}: bin {v} {p} built no target/debug/{p}'; exit 1; }}; }}",
                p = b.package,
                v = b.var
            ));
        }
        Some(c)
    }
}

/// Which definition a trial runs, and why (DESIGN.md "The tree owns its gate").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// The tree's own definition.
    Tree(Def),
    /// The repository has not adopted a tree definition (its landing ref has none): the
    /// repository's configured gate column, as before sp-quu2w. Empty means syntax only.
    Column(String),
    /// The repository declares a full CI in `.spira/testenv.toml`: the gate runs its default
    /// tiers through testenv, `build` first when the declaration has a build tier.
    Testenv { tiers: Vec<String>, build_tier: bool },
    /// The definition cannot be read. `branch` says whose fault: true when the base's own
    /// definition reads (the branch broke it), false when the base's is broken too.
    Refused { branch: bool, why: String },
}

/// What one revision carries of a repository's gate: its `gate.steps`, its testenv declaration,
/// and whether it has the harness's own Containerfile (which makes the declaration the harness's).
#[derive(Clone, Copy, Debug, Default)]
pub struct Side<'a> {
    pub steps: Option<&'a [u8]>,
    pub declaration: Option<&'a str>,
    pub containerfile: bool,
}

impl Side<'_> {
    pub fn source(&self, land: &str, column: &str) -> Result<GateSource, String> {
        gate_source::resolve(self.steps.is_some(), self.declaration, self.containerfile, land, column)
    }
}

/// Resolve the definition for the tree under test from the landing ref's side (`base`) and the
/// tree's own. The column is used only when the base has never had a definition; once the base
/// has one, a tree without a readable one is refused, never quietly gated by config.
pub fn resolve(base: Side, tree: Side, land: &str, column: &str) -> Resolved {
    let read = |b: &[u8]| -> Result<Def, String> {
        std::str::from_utf8(b)
            .map_err(|_| format!("{PATH} is not UTF-8"))
            .and_then(parse)
    };
    let steps_refused = |t: Option<&[u8]>| match (base.steps, t) {
        (Some(_), None) => Some(Resolved::Refused {
            branch: true,
            why: format!("the landing ref has {PATH} and the tree under test does not — deleting the gate definition is not a way through it"),
        }),
        _ => None,
    };
    if let Some(r) = steps_refused(tree.steps) {
        return r;
    }
    if let Some(t) = tree.steps {
        return match read(t) {
            Ok(d) => Resolved::Tree(d),
            Err(e) => {
                let base_ok = base.steps.map(|b| read(b).is_ok()).unwrap_or(true);
                Resolved::Refused { branch: base_ok, why: e }
            }
        };
    }
    let base_source = base.source(land, column);
    match tree.source(land, column) {
        Err(why) => Resolved::Refused { branch: base_source.is_ok(), why },
        Ok(GateSource::Testenv { tiers, build_tier }) => Resolved::Testenv { tiers, build_tier },
        Ok(_) if matches!(base_source, Ok(GateSource::Testenv { .. })) => Resolved::Refused {
            branch: true,
            why: format!("the landing ref declares its CI in {} and the tree under test does not — deleting the declaration or its ci_entry is not a way through it", crate::own::DECL),
        },
        Ok(_) => Resolved::Column(column.to_string()),
    }
}

/// The base trial's definition: the landing ref's own, or the column when it has none.
/// Err when the base's definition does not read.
pub fn resolve_base(base: Side, land: &str, column: &str) -> Result<Resolved, String> {
    if let Some(b) = base.steps {
        return std::str::from_utf8(b)
            .map_err(|_| format!("{PATH} is not UTF-8"))
            .and_then(parse)
            .map(Resolved::Tree);
    }
    Ok(match base.source(land, column)? {
        GateSource::Testenv { tiers, build_tier } => Resolved::Testenv { tiers, build_tier },
        _ => Resolved::Column(column.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        spira_config::resolve::checkout_root_for_process().unwrap()
    }

    fn checked_in() -> Def {
        let text = std::fs::read_to_string(root().join(PATH)).expect("the tree carries gate.steps");
        parse(&text).expect("the tree's gate.steps parses")
    }

    #[test_id(tu6v55h)]
    #[test]
    fn the_checked_in_gate_expects_spira_lint_not_the_wrapper_that_runs_it() {
        let d = checked_in();
        let direct = crate::fence::expected(&d.command());
        assert!(direct.iter().any(|f| f == "spira-lint") && !direct.iter().any(|f| f == "lint-staged"), "{direct:?}");
        let comp = crate::compose::gate_string(&crate::compose::Composition::Unit { touched: vec![], crates: vec![], targets: Default::default(), dropped: vec![] }, &d.command()).0;
        let composed = crate::fence::expected(&comp);
        assert!(composed.iter().any(|f| f == "spira-lint") && !composed.iter().any(|f| f == "lint-staged"), "{composed:?}");
    }

    #[test_id(t895t7e)]
    #[test]
    fn every_fence_a_bash_step_is_expected_to_print_is_printed_by_its_script() {
        let d = checked_in();
        let unit = crate::compose::gate_string(&crate::compose::Composition::Unit { touched: vec![], crates: vec![], targets: Default::default(), dropped: vec![] }, &d.command()).0;
        for s in [d.command(), unit] {
            let paths = crate::parse::bash_paths(&s);
            for name in crate::fence::expected(&s) {
                for path in paths.iter().filter(|p| std::path::Path::new(p.as_str()).file_stem().is_some_and(|st| st == name.as_str())) {
                    let text = std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
                    let want = format!("fence: {name} checked");
                    assert!(
                        text.lines().any(|l| !l.trim().starts_with('#') && l.contains(&want)),
                        "the gate expects `{want}` from the step `bash {path}`, but {path} prints no such line"
                    );
                }
            }
        }
    }

    #[test_id(tt45brl)]
    #[test]
    fn parse_reads_steps_bins_comments_and_blanks() {
        let d = parse("# c\n\nbin SPIRA_LINT_BIN spira-lint\nstep  bash a.sh  \nstep \"$SPIRA_LINT_BIN\" --only x\n").unwrap();
        assert_eq!(d.bins, [Bin { var: "SPIRA_LINT_BIN".into(), package: "spira-lint".into() }]);
        assert_eq!(d.command(), r#"bash a.sh && "$SPIRA_LINT_BIN" --only x"#);
        assert_eq!(d.key_text(), "bash a.sh && \"$SPIRA_LINT_BIN\" --only x\n# bin SPIRA_LINT_BIN spira-lint");
    }

    #[test_id(tbnlznv)]
    #[test]
    fn parse_reads_own_paths() {
        let d = parse("own gate.steps\nown gate/\nstep a\n").unwrap();
        assert_eq!(d.own, ["gate.steps", "gate/"]);
        assert_eq!(d.key_text(), parse("step a\n").unwrap().key_text());
        assert!(d.owns("gate.steps") && d.owns("gate/src/def.rs"));
        assert!(!d.owns("gate.steps.bak") && !d.owns("gatex/a.rs") && !d.owns("gate") && !d.owns("spira/gate.steps"));
    }

    #[test_id(t88dax7)]
    #[test]
    fn fences_only_keeps_the_fences_and_only_the_bins_they_read() {
        let d = parse("bin SPIRA_LINT_BIN spira-lint\nbin SPIRA_GUARD_BIN lifecycle-guard\nbin SPIRA_SELECT_BIN suite-select\nbin SPIRA_TESTENV_BIN testenv\nstep \"$SPIRA_LINT_BIN\"\nstep \"${SPIRA_GUARD_BIN}\" --gate .\nstep bash spira/build-fence.sh\nstep { \"$SPIRA_SELECT_BIN\" gate x && \"$SPIRA_TESTENV_BIN\" --suites y; }\n").unwrap();
        let f = d.fences_only(&[]);
        assert_eq!(f.packages(), ["spira-lint", "lifecycle-guard"]);
        // The re-entry phase reads the suite runner: kept when re-entry suites may run.
        let r = d.fences_only(&[crate::compose::reentry_command(&[])]);
        assert_eq!(r.packages(), ["spira-lint", "lifecycle-guard", "testenv"]);
        assert_eq!(r.command(), f.command());
        assert_eq!(f.command(), "\"$SPIRA_LINT_BIN\" && \"${SPIRA_GUARD_BIN}\" --gate . && bash spira/build-fence.sh");
        let only = parse("bin SPIRA_SELECT_BIN suite-select\nstep { \"$SPIRA_SELECT_BIN\" gate; }\n").unwrap().fences_only(&[]);
        assert_eq!((only.command(), only.bins.len()), ("true".to_string(), 0));
    }

    #[test_id(trqcloy)]
    #[test]
    fn parse_refuses_what_it_cannot_read() {
        for bad in [
            "",
            "# only a comment\n",
            "bin X y\n",
            "step\n",
            "stpe bash a.sh\n",
            "bin lower pkg\nstep a\n",
            "bin X Pkg;rm\nstep a\n",
            "bin X a b\nstep a\n",
            "bin X a\nbin X b\nstep a\n",
            "own\nstep a\n",
            "own /abs\nstep a\n",
            "own ../x\nstep a\n",
            "own a\nown a\nstep a\n",
            "own a b\nstep a\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test_id(thcysks)]
    #[test]
    fn tools_command_builds_each_bin_and_proves_it() {
        let d = parse("bin SPIRA_LINT_BIN spira-lint\nstep a\n").unwrap();
        let c = d.tools_command(4).unwrap();
        assert!(c.starts_with("cargo build -j 4 --config profile.dev.incremental=false -p spira-lint && "), "{c}");
        // path-ok: asserting the tools phase's own proof of the binary it built.
        assert!(c.contains("[ -x target/debug/spira-lint ]"), "{c}");
        assert_eq!(parse("step a\n").unwrap().tools_command(4), None);
        assert_eq!(
            Def::bin_path(std::path::Path::new("t"), &d.bins[0]),
            // path-ok: the gate tree's build output, asserted.
            std::path::PathBuf::from("t/target/debug/spira-lint")
        );
    }

    #[test_id(tt2xoz8)]
    #[test]
    fn the_check_scripts_opt_out_of_incremental_on_their_cargo_line() {
        fn opts_out(src: &str) -> bool {
            let words = spira_config::build::one_shot_words("dev");
            src.lines()
                .find(|l| ["\"$CARGO_BIN\" check ", "cargo build "].iter().any(|w| l.trim_start().starts_with(w)))
                .is_some_and(|l| l.contains(&words))
        }
        let words = spira_config::build::one_shot_words("dev");
        for (name, src) in [
            ("build-fence.sh", include_str!("../../spira/build-fence.sh")),
            ("lint-staged.sh", include_str!("../../spira/lint-staged.sh")),
        ] {
            assert!(opts_out(src), "{name}: its cargo line lacks {words}");
            assert!(!opts_out(&src.replace(&words, "")), "{name}: the matcher passed a line without the override");
        }
    }

    #[test_id(trz6ip5)]
    #[test]
    fn resolution_falls_to_the_column_only_for_a_repository_that_never_adopted() {
        let good = b"step bash a.sh\n".as_slice();
        let bad = b"nonsense\n".as_slice();
        let steps = |b: &'static [u8]| Side { steps: Some(b), ..Side::default() };
        let none = Side::default();
        let r = |base: Side, tree: Side| resolve(base, tree, "queue.local", "col");
        assert_eq!(r(none, none), Resolved::Column("col".into()));
        assert!(matches!(r(none, steps(good)), Resolved::Tree(_)));
        assert!(matches!(r(steps(good), steps(good)), Resolved::Tree(_)));
        // The base has one and the tree dropped it: the branch's refusal, never the column.
        assert!(matches!(r(steps(good), none), Resolved::Refused { branch: true, .. }));
        // An unreadable tree definition: the branch's when the base's reads, not otherwise.
        assert!(matches!(r(steps(good), steps(bad)), Resolved::Refused { branch: true, .. }));
        assert!(matches!(r(steps(bad), steps(bad)), Resolved::Refused { branch: false, .. }));
        assert!(matches!(r(none, steps(bad)), Resolved::Refused { branch: true, .. }));
        assert_eq!(resolve_base(none, "queue.local", "col"), Ok(Resolved::Column("col".into())));
        assert!(resolve_base(steps(bad), "queue.local", "col").is_err());
    }

    const DECL: &str = "ci_entry = \"bash gate.sh\"\ndefault = [\"gate\"]\n[tier.gate]\ncmd = \"x\"\n";

    fn declared(text: &str) -> Side<'_> {
        Side { declaration: Some(text), ..Side::default() }
    }

    #[test_id(t1bzu6p)]
    #[test]
    fn a_declaration_with_a_ci_entry_gates_a_queue_local_repository_whatever_its_column() {
        let want = Resolved::Testenv { tiers: vec!["gate".into()], build_tier: false };
        assert_eq!(resolve(Side::default(), declared(DECL), "queue.local", ""), want);
        assert_eq!(resolve(declared(DECL), declared(DECL), "queue.local", "bash gate.sh"), want);
        assert_eq!(resolve_base(declared(DECL), "queue.local", ""), Ok(want));
        let forge = resolve(declared(DECL), declared(DECL), "queue", "col");
        assert_eq!(forge, Resolved::Column("col".into()));
    }

    #[test_id(tu66ogy)]
    #[test]
    fn deleting_the_declaration_or_its_ci_entry_is_the_branchs_refusal() {
        let gone = resolve(declared(DECL), Side::default(), "queue.local", "");
        assert!(matches!(gone, Resolved::Refused { branch: true, .. }), "{gone:?}");
        let no_entry = "default = [\"gate\"]\n[tier.gate]\ncmd = \"x\"\n";
        let dropped = resolve(declared(DECL), declared(no_entry), "queue.local", "");
        assert!(matches!(dropped, Resolved::Refused { branch: true, .. }), "{dropped:?}");
    }

    #[test_id(tsr96u0)]
    #[test]
    fn an_unreadable_declaration_is_refused_and_blames_the_branch_only_when_the_base_reads() {
        let bad = "ci_entry = [";
        assert!(matches!(resolve(declared(DECL), declared(bad), "queue.local", ""), Resolved::Refused { branch: true, .. }));
        assert!(matches!(resolve(declared(bad), declared(bad), "queue.local", ""), Resolved::Refused { branch: false, .. }));
        assert!(resolve_base(declared(bad), "queue.local", "").is_err());
    }

    /// Every `bash <path>` the tree's definition names is a file the tree carries, and every
    /// `bin` is a workspace package: a branch that deletes a fence script removes its step in
    /// the same commit, or this (and the gate) refuses it.
    #[test_id(th4rr5f)]
    #[test]
    fn the_checked_in_definition_names_only_files_it_carries() {
        let d = checked_in();
        let missing: Vec<String> = crate::parse::bash_paths(&d.command())
            .into_iter()
            .filter(|p| !root().join(p).is_file())
            .collect();
        assert!(missing.is_empty(), "gate.steps names files the tree lacks: {missing:?}");
        for b in &d.bins {
            assert!(root().join(&b.package).join("Cargo.toml").is_file(), "bin {} names no package dir", b.package);
        }
    }

    /// The compile check stays reachable (doctor's sp-1hmrm check, now on the tree): a
    /// suites composition runs the build fence; a unit composition drops it for its own
    /// build phase (sp-aprxm).
    #[test_id(ttasfs3)]
    #[test]
    fn the_checked_in_definition_keeps_a_compile_check_and_proves_its_fences() {
        let d = checked_in();
        assert!(
            d.steps.iter().any(|s| s == crate::compose::BUILD_FENCE_STEP),
            "gate.steps has no `step {}`",
            crate::compose::BUILD_FENCE_STEP
        );
        let fences = crate::fence::expected(&d.command());
        assert!(fences.iter().any(|f| f == "build-fence"), "{fences:?}");
        assert!(!fences.iter().any(|f| f == crate::fence::SELECTOR), "{fences:?}");
    }

    /// The lifecycle analyser guards every landing (sp-ts2qr; design bead-lifecycle-state-
    /// machine §3.6(3), §3.8(2)): the tree builds its own lifecycle-guard, runs it over the
    /// whole gate tree in gate mode, and the gate expects its fence line. The step carries no
    /// `--rules` file and no path but the tree's root: there is no allow-list to hand it.
    #[test_id(tk3oya7)]
    #[test]
    fn the_checked_in_definition_runs_the_lifecycle_guard_over_the_whole_tree() {
        let d = checked_in();
        assert!(
            d.bins.iter().any(|b| b.var == "SPIRA_GUARD_BIN" && b.package == "lifecycle-guard"),
            "gate.steps does not build the tree's lifecycle-guard: {:?}",
            d.bins
        );
        let steps: Vec<&String> = d.steps.iter().filter(|s| s.contains("SPIRA_GUARD_BIN")).collect();
        assert_eq!(
            steps,
            [&r#""$SPIRA_GUARD_BIN" --gate ."#.to_string()],
            "the guard runs once, in gate mode, over the tree root, with no rules or allow-list"
        );
        let fences = crate::fence::expected(&d.command());
        assert!(fences.iter().any(|f| f == crate::fence::GUARD), "{fences:?}");
        // It is a fence of every composition: the unit composition drops only the build fence.
        let unit = crate::compose::gate_string(&crate::compose::Composition::Unit { touched: Vec::new(), crates: Vec::new(), targets: Default::default(), dropped: Vec::new() }, &d.command()).0;
        assert!(crate::fence::expected(&unit).iter().any(|f| f == crate::fence::GUARD), "{unit}");
    }
}
