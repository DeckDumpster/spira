//! Function-level reach inside the harness's shared shell files (over-broad-scopes #6,
//! sp-ipwlic). `spira/lib.sh` is 2,400 lines and 148 functions, and 83 suites name it as a
//! whole file, so one changed line selected all 83. A change confined to some of its functions
//! reaches only the suites that call one of them — directly, through another function (in any
//! script of the directory) that calls one, or by running a script that does.
//!
//! The analysis only ever narrows a whole-file claim, so where it cannot see it keeps: a
//! change to top-level code, or a diff it cannot place, selects every suite naming the file
//! (see [`crate::select`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The files whose whole-file `# covers:` claims narrow to the functions a change touches.
pub const FUNCTION_SCOPED: &[&str] = &["spira/lib.sh", "spira/conf.sh"];

/// The shell scripts of a suite directory: each function's body tokens, and each script's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellGraph {
    /// Function name → the tokens of its body (every definition of the name, merged).
    fns: BTreeMap<String, BTreeSet<String>>,
    /// Script basename (`queue.sh`) → the tokens of its top-level code: what running or
    /// sourcing it executes outside its function bodies (a body is reached through `fns`).
    scripts: BTreeMap<String, BTreeSet<String>>,
    /// Script basename → the functions it defines.
    defines: BTreeMap<String, BTreeSet<String>>,
}

/// The tokens of `text`'s non-comment lines, `.` kept so `queue.sh` is one token.
pub fn tokens_of(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// `^[A-Za-z_][A-Za-z0-9_]*\(\)` → the name (the same definitions `changed_functions` reads).
pub fn fn_def(line: &str) -> Option<String> {
    let b = line.as_bytes();
    if b.is_empty() || !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
        return None;
    }
    let n = b.iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'_').count();
    line[n..].starts_with("()").then(|| line[..n].to_string())
}

impl ShellGraph {
    /// Every non-suite `*.sh` directly in `dir`. An unreadable script is left out: the graph
    /// then misses a path, so [`ShellGraph::complete`] says no and nothing narrows.
    pub fn load(dir: &Path) -> ShellGraph {
        let mut g = ShellGraph::default();
        let Ok(rd) = std::fs::read_dir(dir) else { return g };
        let mut complete = true;
        for ent in rd.flatten() {
            let n = ent.file_name().to_string_lossy().to_string();
            if !n.ends_with(".sh") || crate::corpus::is_suite_file(&n) {
                continue;
            }
            match std::fs::read(ent.path()) {
                Ok(b) => g.add(&n, &String::from_utf8_lossy(&b)),
                Err(_) => complete = false,
            }
        }
        if !complete {
            return ShellGraph::default();
        }
        g
    }

    /// Adds one script's text under its basename.
    pub fn add(&mut self, name: &str, text: &str) {
        let mut cur: Option<String> = None;
        let mut body = String::new();
        let mut top = String::new();
        let flush = |cur: &mut Option<String>, body: &mut String, g: &mut ShellGraph| {
            if let Some(f) = cur.take() {
                g.fns.entry(f.clone()).or_default().extend(tokens_of(body));
                g.defines.entry(name.to_string()).or_default().insert(f);
            }
            body.clear();
        };
        for line in text.lines() {
            if line.starts_with('}') {
                flush(&mut cur, &mut body, self);
                continue;
            }
            if let Some(f) = fn_def(line) {
                flush(&mut cur, &mut body, self);
                body.push_str(&line[f.len() + 2..]);
                body.push('\n');
                cur = Some(f);
                continue;
            }
            if cur.is_some() {
                body.push_str(line);
                body.push('\n');
            } else {
                top.push_str(line);
                top.push('\n');
            }
        }
        flush(&mut cur, &mut body, self);
        self.scripts.insert(name.to_string(), tokens_of(&top));
    }

    /// Whether `file` (`spira/lib.sh`) is a script this graph read and defines functions in.
    pub fn knows(&self, file: &str) -> bool {
        let base = file.rsplit('/').next().unwrap_or(file);
        self.defines.get(base).is_some_and(|d| !d.is_empty())
    }

    /// Every name a change to `changed` reaches: the functions themselves, every function that
    /// calls one (transitively, across scripts), and every script that calls one.
    pub fn affected(&self, changed: &[String]) -> BTreeSet<String> {
        let mut a: BTreeSet<String> = changed.iter().cloned().collect();
        loop {
            let before = a.len();
            for (f, toks) in &self.fns {
                if !a.contains(f) && toks.iter().any(|t| a.contains(t)) {
                    a.insert(f.clone());
                }
            }
            for (s, toks) in &self.scripts {
                if !a.contains(s) && toks.iter().any(|t| a.contains(t)) {
                    a.insert(s.clone());
                }
            }
            if a.len() == before {
                return a;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_reaches_its_callers_across_scripts() {
        let mut g = ShellGraph::default();
        g.add("lib.sh", "foo() {\n  :\n}\nbar() {\n  foo\n}\nqux() {\n  :\n}\n");
        g.add("run.sh", ". lib.sh\nbar\n");
        g.add("top.sh", "\"$HERE/run.sh\"\n");
        let a = g.affected(&["foo".into()]);
        for n in ["foo", "bar", "run.sh", "top.sh"] {
            assert!(a.contains(n), "{n}: {a:?}");
        }
        assert!(!a.contains("qux") && !a.contains("lib.sh"), "{a:?}");
        assert!(g.knows("spira/lib.sh") && !g.knows("spira/run.sh"));
    }
}
