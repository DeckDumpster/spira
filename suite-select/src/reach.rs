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
}

#[derive(Default)]
struct Krate {
    dir: String,
    name: String,
    bins: BTreeSet<String>,
    deps: Vec<String>,
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
        } else if section == "[dependencies]" || section == "[build-dependencies]" {
            if let Some(p) = l.find("path") {
                if let Some(rel) = quoted(&l[p..]).into_iter().next() {
                    k.deps.push(normalize(dir, &rel));
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
        Ok(Some(Reach { bins }))
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
