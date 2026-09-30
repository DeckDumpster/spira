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
//! ```
//!
//! [`Def::command`] joins the steps with ` && `: the exact string production config held,
//! so everything downstream of it (the fences' expectations, the build-fence drop of a unit
//! composition, the verdict key) reads the same kind of string it always has.

/// Where a gated tree keeps its definition, relative to the repository root.
pub const PATH: &str = "gate.steps";

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
            _ => return Err(format!("{PATH}:{n}: unknown directive `{word}` (a line is `step <command>`, `bin <VAR> <package>`, a `#` comment or blank)")),
        }
    }
    if d.steps.is_empty() {
        return Err(format!("{PATH} names no step — a gate that runs nothing judges nothing"));
    }
    Ok(d)
}

impl Def {
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

    /// Where `bin`'s binary lands in `tree`: the aeon profile, the tree's own target dir
    /// (the gate command runs under `env -i`, so no CARGO_TARGET_DIR moves it).
    pub fn bin_path(tree: &std::path::Path, b: &Bin) -> std::path::PathBuf {
        tree.join("target").join("aeon").join(&b.package)
    }

    /// The tools phase: build every `bin` package from the tree, in the profile the unit
    /// phases use (so a unit composition's build reuses it), and prove each binary exists.
    /// None when the definition builds nothing.
    pub fn tools_command(&self, jobs: u64) -> Option<String> {
        if self.bins.is_empty() {
            return None;
        }
        let pkgs = self.bins.iter().fold(String::new(), |acc, b| acc + " -p " + &b.package);
        let mut c = format!("cargo build --profile aeon -j {jobs}{pkgs}");
        for b in &self.bins {
            c.push_str(&format!(
                " && {{ [ -x target/aeon/{p} ] || {{ echo 'gate: {PATH}: bin {v} {p} built no target/aeon/{p}'; exit 1; }}; }}",
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
    /// repo-map's gate column, as before sp-quu2w. Empty means syntax only.
    Column(String),
    /// The definition cannot be read. `branch` says whose fault: true when the base's own
    /// definition reads (the branch broke it), false when the base's is broken too.
    Refused { branch: bool, why: String },
}

/// Resolve the definition for the tree under test from the two blobs of [`PATH`]: the
/// landing ref's (`base`) and the tree's own (`tree`). The column is used only when the
/// base has never had a definition; once the base has one, a tree without a readable one
/// is refused, never quietly gated by config.
pub fn resolve(base: Option<&[u8]>, tree: Option<&[u8]>, column: &str) -> Resolved {
    let read = |b: &[u8]| -> Result<Def, String> {
        std::str::from_utf8(b)
            .map_err(|_| format!("{PATH} is not UTF-8"))
            .and_then(parse)
    };
    match (base, tree) {
        (None, None) => Resolved::Column(column.to_string()),
        (Some(_), None) => Resolved::Refused {
            branch: true,
            why: format!("the landing ref has {PATH} and the tree under test does not — deleting the gate definition is not a way through it"),
        },
        (b, Some(t)) => match read(t) {
            Ok(d) => Resolved::Tree(d),
            Err(e) => {
                let base_ok = b.map(|b| read(b).is_ok()).unwrap_or(true);
                Resolved::Refused { branch: base_ok, why: e }
            }
        },
    }
}

/// The base trial's definition: the landing ref's own, or the column when it has none.
/// Err when the base's definition does not read.
pub fn resolve_base(base: Option<&[u8]>, column: &str) -> Result<Resolved, String> {
    match base {
        None => Ok(Resolved::Column(column.to_string())),
        Some(b) => std::str::from_utf8(b)
            .map_err(|_| format!("{PATH} is not UTF-8"))
            .and_then(parse)
            .map(Resolved::Tree),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    fn checked_in() -> Def {
        let text = std::fs::read_to_string(root().join(PATH)).expect("the tree carries gate.steps");
        parse(&text).expect("the tree's gate.steps parses")
    }

    #[test]
    fn parse_reads_steps_bins_comments_and_blanks() {
        let d = parse("# c\n\nbin SPIRA_LINT_BIN spira-lint\nstep  bash a.sh  \nstep \"$SPIRA_LINT_BIN\" --only x\n").unwrap();
        assert_eq!(d.bins, [Bin { var: "SPIRA_LINT_BIN".into(), package: "spira-lint".into() }]);
        assert_eq!(d.command(), r#"bash a.sh && "$SPIRA_LINT_BIN" --only x"#);
        assert_eq!(d.key_text(), "bash a.sh && \"$SPIRA_LINT_BIN\" --only x\n# bin SPIRA_LINT_BIN spira-lint");
    }

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
        ] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn tools_command_builds_each_bin_and_proves_it() {
        let d = parse("bin SPIRA_LINT_BIN spira-lint\nstep a\n").unwrap();
        let c = d.tools_command(4).unwrap();
        assert!(c.starts_with("cargo build --profile aeon -j 4 -p spira-lint && "), "{c}");
        assert!(c.contains("[ -x target/aeon/spira-lint ]"), "{c}");
        assert_eq!(parse("step a\n").unwrap().tools_command(4), None);
        assert_eq!(
            Def::bin_path(std::path::Path::new("/t"), &d.bins[0]),
            std::path::PathBuf::from("/t/target/aeon/spira-lint")
        );
    }

    #[test]
    fn resolution_falls_to_the_column_only_for_a_repository_that_never_adopted() {
        let good = b"step bash a.sh\n".as_slice();
        let bad = b"nonsense\n".as_slice();
        assert_eq!(resolve(None, None, "col"), Resolved::Column("col".into()));
        assert!(matches!(resolve(None, Some(good), "col"), Resolved::Tree(_)));
        assert!(matches!(resolve(Some(good), Some(good), "col"), Resolved::Tree(_)));
        // The base has one and the tree dropped it: the branch's refusal, never the column.
        assert!(matches!(resolve(Some(good), None, "col"), Resolved::Refused { branch: true, .. }));
        // An unreadable tree definition: the branch's when the base's reads, not otherwise.
        assert!(matches!(resolve(Some(good), Some(bad), "col"), Resolved::Refused { branch: true, .. }));
        assert!(matches!(resolve(Some(bad), Some(bad), "col"), Resolved::Refused { branch: false, .. }));
        assert!(matches!(resolve(None, Some(bad), "col"), Resolved::Refused { branch: true, .. }));
        assert_eq!(resolve_base(None, "col"), Ok(Resolved::Column("col".into())));
        assert!(resolve_base(Some(bad), "col").is_err());
    }

    /// Every `bash <path>` the tree's definition names is a file the tree carries, and every
    /// `bin` is a workspace package: a branch that deletes a fence script removes its step in
    /// the same commit, or this (and the gate) refuses it.
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
}
