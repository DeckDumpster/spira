//! `no-download-install` — a script installs software only from a package manager's pinned
//! manifest, never by piping `curl`/`wget` into `tar`/`sh`/`bash`, or by running, unpacking
//! or copying a file it just downloaded.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::{allow_lines, Entry, Finding, LintError, Rule, Tree};

pub struct NoDownloadInstall;

const NAME: &str = "no-download-install";
const ALLOW_FILE: &str = "spira-lint/no-download-install-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn is_script(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if path.starts_with("docs/") || path.starts_with("target/") || name.starts_with("test-") {
        return false;
    }
    name == "Makefile"
        || name.starts_with("Containerfile")
        || name.starts_with("Dockerfile")
        || [".sh", ".bash", ".yml", ".yaml"].iter().any(|x| name.ends_with(x))
}

/// Logical lines (a trailing `\` or `|` joins the next line), each with its first physical
/// line number. Comment-only lines are dropped.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (i, raw) in text.lines().enumerate() {
        let t = raw.trim();
        if open.is_none() && t.starts_with('#') {
            continue;
        }
        let (start, mut acc) = open.take().unwrap_or((i + 1, String::new()));
        let cont = t.ends_with('\\') || (t.ends_with('|') && !t.ends_with(": |"));
        acc.push(' ');
        acc.push_str(t.trim_end_matches('\\'));
        if cont {
            open = Some((start, acc));
        } else {
            out.push((start, acc));
        }
    }
    out.extend(open);
    out
}

/// The file a download line writes, when it names one.
fn downloaded_file(line: &str) -> Option<String> {
    static OUT: OnceLock<Regex> = OnceLock::new();
    static REMOTE: OnceLock<Regex> = OnceLock::new();
    static FETCH: OnceLock<Regex> = OnceLock::new();
    if !re(&FETCH, r"\b(curl|wget)\b").is_match(line) {
        return None;
    }
    if let Some(c) = re(&OUT, r#"(?:\s-o|\s-O|\s--output|\s--output-document|\s>)\s*["']?([^\s"'|;&]+)"#).captures(line) {
        let f = c[1].to_string();
        if f != "-" && f != "/dev/null" && !f.starts_with('-') {
            return Some(f);
        }
    }
    if line.contains(" -O ") || line.ends_with(" -O") || line.contains(" -O\t") || re(&FETCH, r"\bwget\b").is_match(line) || line.contains(" -LO ") {
        return re(&REMOTE, r#"https?://[^\s"']*/([^/\s"'?]+)["']?(?:\s|$)"#).captures(line).map(|c| c[1].to_string());
    }
    None
}

impl Rule for NoDownloadInstall {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_script(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static PIPE: OnceLock<Regex> = OnceLock::new();
        static SUBST: OnceLock<Regex> = OnceLock::new();
        let pipe = re(&PIPE, r"\b(?:curl|wget)\b[^|]*\|\s*(?:sudo\s+(?:-\S+\s+)*)?(?:env\s+\S+=\S+\s+)?(?:tar|sh|bash|zsh|dash|unzip)\b");
        let subst = re(&SUBST, r#"\b(?:sh|bash|zsh|source)\s+(?:-\w+\s+)*(?:<\(|-c\s+["']?\$\()\s*(?:curl|wget)\b"#);

        let files = crate::scope(tree, self)?;
        let allow = allow_lines(&tree.read_text(ALLOW_FILE));
        let mut out = Vec::new();
        let mut offenders = BTreeSet::new();
        for e in files {
            let Some(src) = tree.content(e) else { continue };
            let lines = logical_lines(&String::from_utf8_lossy(src));
            let mut hits: Vec<(usize, &'static str)> = Vec::new();
            for (n, (at, l)) in lines.iter().enumerate() {
                if pipe.is_match(l) || subst.is_match(l) {
                    hits.push((*at, "downloads and pipes straight into an interpreter or unpacker"));
                    continue;
                }
                let Some(f) = downloaded_file(l) else { continue };
                let q = regex::escape(&f);
                let used = Regex::new(&format!(
                    r"\b(?:tar|unzip|dpkg\s+-i|install|cp|mv|sh|bash|chmod\s+\S*\+x)\b[^\n]*{q}(?:[\s\x22\x27;&|]|$)"
                ))
                .expect("escaped pattern");
                if lines[n + 1..].iter().any(|(_, later)| used.is_match(later)) {
                    hits.push((*at, "downloads a file and then unpacks, installs or runs it"));
                }
            }
            if hits.is_empty() {
                continue;
            }
            offenders.insert(e.path.clone());
            if allow.iter().any(|p| p == &e.path) {
                continue;
            }
            for (line, what) in hits {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: format!("{what} — declare the tool in a package manager's pinned manifest and install it from there") });
            }
        }
        for (i, p) in tree.read_text(ALLOW_FILE).lines().enumerate() {
            let t = p.trim();
            if t.is_empty() || t.starts_with('#') || offenders.contains(t) {
                continue;
            }
            out.push(Finding {
                rule: NAME,
                path: ALLOW_FILE.to_string(),
                line: Some(i + 1),
                message: format!("lists {t}, which no longer needs an exception — remove the line (the list only shrinks)"),
            });
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Every tool and library a build, test or CI step needs is declared with a pinned version \
in a package manager's manifest (Cargo.toml or a cargo-binstall entry, package.json plus \
lockfile, an apt package list read by runner-deps) and installed only from it. A download piped \
into tar or a shell has no pin and no checksum, and a wrong URL fails silently into whatever \
fallback follows. spira-lint/no-download-install-allow lists files that predate the rule; it \
only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        NoDownloadInstall.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    const PIPE_TAR: &str = "#!/bin/sh\necho start\ncurl -fsSL https://get.example.invalid/linux | tar -xz -C /tmp\n";

    #[test]
    fn a_planted_download_install_is_caught_and_named_by_line() {
        let t = TempDir::new("nodl-fail");
        t.write(ALLOW_FILE, "");
        t.write("ci.sh", PIPE_TAR);
        t.write("setup.sh", "#!/bin/sh\ncurl -fsSL https://x.invalid/i.sh \\\n  | sudo bash -s -- --yes\n");
        t.write("sub.sh", "bash <(curl -s https://x.invalid/i.sh)\nsh -c \"$(wget -qO- https://x.invalid/i.sh)\"\n");
        t.write("file.sh", "curl -fsSL -o /tmp/tool.tgz https://x.invalid/tool.tgz\ntar -xzf /tmp/tool.tgz -C /usr/local\n");
        t.write("bin.sh", "wget https://x.invalid/rel/tool\nchmod +x tool\nsudo cp tool /usr/local/bin/\n");
        t.write("wf.yml", "steps:\n  - run: |\n      curl -fsSL https://x.invalid/d.tar.gz \\\n        | tar -xz -C /tmp\n");
        let got = run(&t, &["ci.sh", "setup.sh", "sub.sh", "file.sh", "bin.sh", "wf.yml"]).unwrap();
        let hits: Vec<String> = got.iter().map(|g| g.split(" — ").next().unwrap().to_string()).collect();
        assert_eq!(
            hits,
            vec![
                "no-download-install: bin.sh:1: downloads a file and then unpacks, installs or runs it",
                "no-download-install: ci.sh:3: downloads and pipes straight into an interpreter or unpacker",
                "no-download-install: file.sh:1: downloads a file and then unpacks, installs or runs it",
                "no-download-install: setup.sh:2: downloads and pipes straight into an interpreter or unpacker",
                "no-download-install: sub.sh:1: downloads and pipes straight into an interpreter or unpacker",
                "no-download-install: sub.sh:2: downloads and pipes straight into an interpreter or unpacker",
                "no-download-install: wf.yml:3: downloads and pipes straight into an interpreter or unpacker",
            ]
        );
    }

    #[test]
    fn manifest_declared_installs_and_innocent_curl_pass() {
        let t = TempDir::new("nodl-pass");
        t.write(ALLOW_FILE, "");
        t.write(
            "ci.sh",
            "#!/bin/sh\ncargo binstall --locked --no-confirm --manifest-path tools/Cargo.toml cargo-nextest\n\
cargo nextest run\napt-get install -y $(cat spira/runner-deps.apt)\n\
# curl -fsSL https://x.invalid | sh\n\
curl -fsS https://x.invalid/health | jq .ok\n\
curl -fsSL https://x.invalid/key.gpg | sudo gpg --dearmor -o /usr/share/keyrings/k.gpg\n\
curl -fsS -o /tmp/out.json https://x.invalid/data\njq . /tmp/out.json\n",
        );
        t.write("test-fixture.sh", PIPE_TAR);
        t.write("docs/runbook.md", PIPE_TAR);
        assert_eq!(run(&t, &["ci.sh", "test-fixture.sh", "docs/runbook.md"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("nodl-allow");
        t.write("old.sh", PIPE_TAR);
        t.write("fixed.sh", "#!/bin/sh\ncargo binstall cargo-nextest\n");
        t.write(ALLOW_FILE, "# header\nold.sh\nfixed.sh\n");
        assert_eq!(
            run(&t, &["old.sh", "fixed.sh"]).unwrap(),
            vec!["no-download-install: spira-lint/no-download-install-allow:3: lists fixed.sh, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }

    #[test]
    fn no_scripts_is_a_refusal() {
        let t = TempDir::new("nodl-empty");
        t.write("a.rs", "x\n");
        assert_eq!(run(&t, &["a.rs"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(NoDownloadInstall)]
}
