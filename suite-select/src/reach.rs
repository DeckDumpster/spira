//! Reverse-dependency reach: a changed file inside a workspace crate reaches every binary of
//! that crate and of every workspace crate that depends on it, so a suite that runs one of
//! those binaries is selected although its `# covers:` never names the file.

use crate::{refuse, Refusal};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Crate directory (relative, `/`-separated) → the binaries its change reaches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reach {
    bins: BTreeMap<String, Vec<String>>,
    /// Crate directory → the packages a change to it can break the compile of: its own and
    /// every workspace package that depends on it, transitively (dev-dependencies included —
    /// a check builds tests too).
    dependents: BTreeMap<String, Vec<String>>,
    /// Binary → the crate directories it is built from: its own crate and every crate that one
    /// depends on (build dependencies included, dev-dependencies not).
    sources: BTreeMap<String, Vec<String>>,
}

#[derive(Default)]
struct Krate {
    dir: String,
    name: String,
    bins: BTreeSet<String>,
    deps: Vec<String>,
    dev_deps: Vec<String>,
}

fn quoted(s: &str) -> Vec<String> {
    s.split('"').skip(1).step_by(2).map(str::to_string).collect()
}

fn members(root: &str) -> Vec<String> {
    let Some(i) = root.find("members") else { return vec![] };
    let rest = &root[i..];
    let Some(open) = rest.find('[') else { return vec![] };
    let Some(close) = rest[open..].find(']') else { return vec![] };
    let body: String = rest[open..open + close]
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    quoted(&body)
}

fn normalize(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn parse_crate(root: &Path, dir: &str) -> Result<Krate, Refusal> {
    let path = root.join(dir).join("Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Refusal(format!("cannot read workspace member {}: {e}", path.display())))?;
    let mut k = Krate {
        dir: dir.to_string(),
        ..Krate::default()
    };
    let mut section = String::new();
    for raw in text.lines() {
        let l = raw.trim();
        if l.starts_with('[') {
            section = l.to_string();
            continue;
        }
        let key = l.split('=').next().unwrap_or("").trim();
        if section == "[package]" && key == "name" {
            k.name = quoted(l).into_iter().next().unwrap_or_default();
        } else if section == "[[bin]]" && key == "name" {
            k.bins.extend(quoted(l).into_iter().next());
        } else if section == "[dependencies]" || section == "[build-dependencies]" || section == "[dev-dependencies]" {
            if let Some(p) = l.find("path") {
                if let Some(rel) = quoted(&l[p..]).into_iter().next() {
                    let into = if section == "[dev-dependencies]" { &mut k.dev_deps } else { &mut k.deps };
                    into.push(normalize(dir, &rel));
                }
            }
        }
    }
    if k.name.is_empty() {
        return refuse(format!("{} has no [package] name", path.display()));
    }
    if root.join(dir).join("src/main.rs").is_file() {
        k.bins.insert(k.name.clone());
    }
    if let Ok(rd) = std::fs::read_dir(root.join(dir).join("src/bin")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = n.strip_suffix(".rs") {
                k.bins.insert(stem.to_string());
            }
        }
    }
    Ok(k)
}

impl Reach {
    /// The workspace at `root`; `None` when there is no workspace manifest. A member that
    /// cannot be read is a refusal, never a smaller graph.
    pub fn load(root: &Path) -> Result<Option<Reach>, Refusal> {
        let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) else {
            return Ok(None);
        };
        let mut crates = Vec::new();
        for m in members(&text) {
            crates.push(parse_crate(root, &m)?);
        }
        let mut bins: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for k in &crates {
            let mut seen: BTreeSet<&str> = BTreeSet::from([k.dir.as_str()]);
            let mut todo = vec![k.dir.as_str()];
            while let Some(d) = todo.pop() {
                for u in crates.iter().filter(|u| u.deps.iter().any(|x| x == d)) {
                    if seen.insert(u.dir.as_str()) {
                        todo.push(u.dir.as_str());
                    }
                }
            }
            let reached: BTreeSet<String> = crates
                .iter()
                .filter(|c| seen.contains(c.dir.as_str()))
                .flat_map(|c| c.bins.iter().cloned())
                .collect();
            bins.insert(k.dir.clone(), reached.into_iter().collect());
        }
        let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for k in &crates {
            let mut seen: BTreeSet<&str> = BTreeSet::from([k.dir.as_str()]);
            let mut todo = vec![k];
            while let Some(c) = todo.pop() {
                for d in &c.deps {
                    if let Some(dep) = crates.iter().find(|x| &x.dir == d) {
                        if seen.insert(dep.dir.as_str()) {
                            todo.push(dep);
                        }
                    }
                }
            }
            for b in &k.bins {
                let e = sources.entry(b.clone()).or_default();
                e.extend(seen.iter().map(|s| s.to_string()));
                e.sort();
                e.dedup();
            }
        }
        let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for k in &crates {
            let mut seen: BTreeSet<&str> = BTreeSet::from([k.dir.as_str()]);
            let mut todo = vec![k.dir.as_str()];
            while let Some(d) = todo.pop() {
                for u in crates.iter().filter(|u| u.deps.iter().chain(&u.dev_deps).any(|x| x == d)) {
                    if seen.insert(u.dir.as_str()) {
                        todo.push(u.dir.as_str());
                    }
                }
            }
            let names: BTreeSet<String> = crates.iter().filter(|c| seen.contains(c.dir.as_str())).map(|c| c.name.clone()).collect();
            dependents.insert(k.dir.clone(), names.into_iter().collect());
        }
        Ok(Some(Reach { bins, dependents, sources }))
    }

    /// The packages `cargo check -p` must cover for a change to `paths` (over-broad-scopes #7,
    /// sp-ipwlic): each changed crate and every workspace package depending on it. `None` when
    /// a path can change how the whole workspace builds — a manifest, the lockfile, the
    /// toolchain pin, the Makefile, `.cargo/`, or a `src/` outside every crate — so the caller
    /// checks the workspace.
    pub fn check_closure<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Option<Vec<String>> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        for p in paths {
            let base = p.rsplit('/').next().unwrap_or(p);
            if matches!(base, "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml" | "Makefile" | "build.rs")
                || p.starts_with(".cargo/")
            {
                return None;
            }
            let owner = self
                .dependents
                .iter()
                .filter(|(d, _)| p.strip_prefix(d.as_str()).is_some_and(|r| r.starts_with('/')))
                .max_by_key(|(d, _)| d.len());
            match owner {
                Some((_, names)) => out.extend(names.iter().cloned()),
                None if p.contains("/src/") || p.starts_with("src/") => return None,
                None => {}
            }
        }
        Some(out.into_iter().collect())
    }

    /// The crate directories binary `bin` is built from; empty for a name no crate builds.
    pub fn sources_of(&self, bin: &str) -> &[String] {
        self.sources.get(bin).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The binaries a change to `path` reaches; empty outside every crate.
    pub fn bins_for(&self, path: &str) -> &[String] {
        self.bins
            .iter()
            .filter(|(d, _)| path.strip_prefix(d.as_str()).is_some_and(|r| r.starts_with('/')))
            .max_by_key(|(d, _)| d.len())
            .map(|(_, b)| b.as_slice())
            .unwrap_or(&[])
    }
}

/// The words of a suite's non-comment lines: what it can invoke.
pub fn words_of(text: &str) -> Vec<String> {
    let set: BTreeSet<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')))
        .filter(|w| !w.is_empty())
        .collect();
    set.into_iter().map(str::to_string).collect()
}

/// Words that run the word after them: a wrapper's own flags and numeric arguments are skipped
/// and the next word is the command.
const WRAPPERS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "time", "command", "exec", "run", "env", "nice",
    "ionice", "sudo", "xargs", "timeout", "flock", "stdbuf", "setsid", "nohup", "builtin", "eval", "watch",
];

fn trim_word(w: &str) -> &str {
    w.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
}

fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(k, _)| {
        !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') && !k.as_bytes()[0].is_ascii_digit()
    })
}

/// What a suite can INVOKE (over-broad-scopes #6, sp-ipwlic): the command word of each
/// pipeline segment, past assignments and wrappers (`timeout 5 bead`, `command -v mail`); the
/// basename of every path-shaped word (`"$ROOT/target/debug/bead"`, `"$BIN/mail-x"`); the word
/// after a bare variable in command position (`"$SPIRA" queue`); the words a `for` loop
/// iterates; and `--bin <name>`. A binary name inside a string, a JSON key or an assertion's
/// prose is not an invocation — matching those as words selected ~131 suites for a `bead/`
/// change. The parse is deliberately loose toward selecting: segments split on every `|`,
/// `&`, `;`, `(`, `)`, `{`, `}` and backtick, quoted or not.
pub fn invocations_of(text: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for line in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
        for w in line.split_whitespace() {
            if w.contains('/') {
                let base = trim_word(w.rsplit('/').next().unwrap_or(""));
                if !base.is_empty() {
                    out.insert(base.to_string());
                }
            }
        }
        let toks: Vec<&str> = line.split_whitespace().collect();
        for (i, t) in toks.iter().enumerate() {
            if *t == "--bin" {
                if let Some(n) = toks.get(i + 1) {
                    out.insert(trim_word(n).to_string());
                }
            }
        }
        for seg in line.split(['|', '&', ';', '(', ')', '{', '}', '`']) {
            let words: Vec<&str> = seg.split_whitespace().collect();
            if words.first() == Some(&"for") {
                if let Some(at) = words.iter().position(|w| *w == "in") {
                    out.extend(words[at + 1..].iter().map(|w| trim_word(w).to_string()).filter(|w| !w.is_empty()));
                }
                continue;
            }
            let mut i = 0;
            let mut after_wrapper = false;
            while i < words.len() {
                let w = words[i];
                if is_assignment(w) || WRAPPERS.contains(&w) {
                    after_wrapper = true;
                    i += 1;
                    continue;
                }
                if after_wrapper && (w.starts_with('-') || w.bytes().all(|b| b.is_ascii_digit() || b == b's' || b == b'.')) {
                    i += 1;
                    continue;
                }
                break;
            }
            let Some(cmd) = words.get(i) else { continue };
            let name = trim_word(cmd.rsplit('/').next().unwrap_or(cmd));
            if !name.is_empty() {
                out.insert(name.to_string());
            }
            if cmd.trim_matches('"').starts_with('$') && !cmd.contains('/') {
                if let Some(next) = words.get(i + 1) {
                    out.insert(trim_word(next).to_string());
                }
            }
        }
    }
    out.remove("");
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> testkit::TempDir {
        let t = testkit::TempDir::new("suite-select-reach");
        let p = t.path();
        std::fs::write(p.join("Cargo.toml"), "[workspace]\nmembers = [\n  \"a\", # lib\n  \"b\",\n  \"c\",\n]\n").unwrap();
        for (d, toml, main) in [
            ("a", "[package]\nname = \"crate-a\"\n", false),
            ("b", "[package]\nname = \"b\"\n[dependencies]\nx = { path = \"../a\" }\n", true),
            ("c", "[package]\nname = \"c\"\n[dev-dependencies]\nx = { path = \"../a\" }\n", true),
        ] {
            std::fs::create_dir_all(p.join(d).join("src")).unwrap();
            std::fs::write(p.join(d).join("Cargo.toml"), toml).unwrap();
            if main {
                std::fs::write(p.join(d).join("src/main.rs"), "").unwrap();
            }
        }
        t
    }

    #[test]
    fn a_change_to_a_library_reaches_the_binaries_that_depend_on_it() {
        let t = ws();
        let r = Reach::load(t.path()).unwrap().unwrap();
        assert_eq!(r.bins_for("a/src/lib.rs"), ["b"]);
        assert_eq!(r.bins_for("b/src/main.rs"), ["b"]);
        assert_eq!(r.bins_for("c/src/main.rs"), ["c"], "a dev-dependency does not reach c's binary");
        assert!(r.bins_for("spira/x.sh").is_empty());
        assert!(r.bins_for("ab/x.rs").is_empty(), "a prefix is not a directory");
    }

    /// Over-broad-scopes #7 (sp-ipwlic): a change to a library checks it and everything that
    /// depends on it — tests included — not the workspace; a manifest, lockfile or toolchain
    /// change still means the whole workspace.
    #[test]
    fn a_change_checks_its_crate_and_its_dependents_and_a_build_input_checks_everything() {
        let t = ws();
        let r = Reach::load(t.path()).unwrap().unwrap();
        assert_eq!(r.check_closure(["a/src/lib.rs"]), Some(vec!["b".into(), "c".into(), "crate-a".into()]));
        assert_eq!(r.check_closure(["b/src/main.rs", "docs/x.md"]), Some(vec!["b".into()]));
        assert_eq!(r.check_closure(["docs/x.md"]), Some(vec![]));
        for whole in ["Cargo.lock", "b/Cargo.toml", "rust-toolchain.toml", "b/build.rs", "stray/src/x.rs"] {
            assert_eq!(r.check_closure(["b/src/main.rs", whole]), None, "{whole}");
        }
    }

    #[test]
    fn no_workspace_is_none_and_an_unreadable_member_is_a_refusal() {
        let t = testkit::TempDir::new("suite-select-reach-none");
        assert_eq!(Reach::load(t.path()), Ok(None));
        std::fs::write(t.path().join("Cargo.toml"), "[workspace]\nmembers = [\"gone\"]\n").unwrap();
        assert!(Reach::load(t.path()).is_err());
    }

    #[test]
    fn words_skip_comments() {
        let w = words_of("# mail is prose\n\"$BIN/mail-x\" send\n");
        assert!(w.contains(&"mail-x".to_string()) && !w.contains(&"prose".to_string()));
    }
}
