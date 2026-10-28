//! `no-download-install` — a script installs software only from a package manager's pinned
//! manifest, never by piping `curl`/`wget` into `tar`/`sh`/`bash`, or by running, unpacking
//! or copying a file it just downloaded.

use std::sync::OnceLock;

use regex::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct NoDownloadInstall;

const NAME: &str = "no-download-install";

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

/// The one bootstrap: installing the package manager itself. Nix's own versioned installer,
/// downloaded to a file and verified by `sha256sum -c` before it runs.
fn is_checksummed_nix_bootstrap(line: &str, later: &[(usize, String)]) -> bool {
    static NIX: OnceLock<Regex> = OnceLock::new();
    re(&NIX, r"https://releases\.nixos\.org/nix/nix-\d+\.\d+\.\d+/install\b").is_match(line) && later.iter().any(|(_, l)| l.contains("sha256sum -c"))
}

impl Rule for NoDownloadInstall {
    fn name(&self) -> &'static str {
        NAME
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
        let mut out = Vec::new();
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
                    r"\b(?:tar|unzip|gunzip|gzip|zcat|xz|unxz|dpkg\s+-i|install|cp|mv|sh|bash|chmod\s+\S*\+x)\b[^\n]*{q}(?:[\s\x22\x27;&|]|$)"
                ))
                .expect("escaped pattern");
                if lines[n + 1..].iter().any(|(_, later)| used.is_match(later)) && !is_checksummed_nix_bootstrap(l, &lines[n + 1..]) {
                    hits.push((*at, "downloads a file and then unpacks, installs or runs it"));
                }
            }
            if hits.is_empty() {
                continue;
            }
            for (line, what) in hits {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: format!("{what} — declare the tool in a package manager's pinned manifest and install it from there") });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Every tool and library a build, test or CI step needs is declared with a pinned version \
in a package manager's manifest (Cargo.toml or a cargo-binstall entry, package.json plus \
lockfile, an apt package list read by runner-deps) and installed only from it. A download piped \
into tar or a shell has no pin and no checksum, and a wrong URL fails silently into whatever \
fallback follows. The only bootstrap is Nix itself, from its versioned installer URL, \
downloaded to a file and verified with sha256sum -c before it runs."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(NoDownloadInstall)]
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
    fn only_the_checksummed_nix_bootstrap_is_waived() {
        let t = TempDir::new("nodl-nix");
        let url = "https://releases.nixos.org/nix/nix-2.28.3/install";
        t.write("ok.sh", &format!("curl -fsSL -o \"$i\" {url}\necho \"abc  $i\" | sha256sum -c -\nsh \"$i\" --daemon\n"));
        t.write("unverified.sh", &format!("curl -fsSL -o \"$i\" {url}\nsh \"$i\" --daemon\n"));
        t.write("piped.sh", &format!("curl -fsSL {url} | sh\nsha256sum -c x\n"));
        t.write("other.sh", "curl -fsSL -o \"$i\" https://x.invalid/install\necho a | sha256sum -c -\nsh \"$i\"\n");
        t.write("gz.sh", "curl -fsSL -o /tmp/t.gz https://x.invalid/t.gz\ngunzip -c /tmp/t.gz > /usr/local/bin/t\n");
        let got = run(&t, &["ok.sh", "unverified.sh", "piped.sh", "other.sh", "gz.sh"]).unwrap();
        let hits: Vec<String> = got.iter().map(|g| g.split(" — ").next().unwrap().split(": ").nth(1).unwrap().to_string()).collect();
        assert_eq!(hits, vec!["gz.sh:1", "other.sh:1", "piped.sh:1", "unverified.sh:1"]);
    }

    #[test]
    fn no_scripts_is_a_refusal() {
        let t = TempDir::new("nodl-empty");
        t.write("a.rs", "x\n");
        assert_eq!(run(&t, &["a.rs"]), Err(LintError::EmptyScope));
    }
}
