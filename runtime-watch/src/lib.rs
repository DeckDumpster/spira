//! A repo pinning a language release upstream no longer supports gets exactly one open bump
//! bead per runtime. An upstream source that cannot be read is "cannot check", never support.

use std::collections::BTreeMap;

pub mod real;
#[cfg(test)]
mod tests;

pub const LABEL: &str = "runtime-bump";
/// Releases a Rust pin may trail current stable before a bump is owed.
pub const RUST_GRACE: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Runtime {
    Rust,
    Node,
    Python,
}

impl Runtime {
    pub const ALL: [Runtime; 3] = [Runtime::Rust, Runtime::Node, Runtime::Python];

    pub fn name(self) -> &'static str {
        match self {
            Runtime::Rust => "rust",
            Runtime::Node => "node",
            Runtime::Python => "python",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Repo {
    pub name: String,
    pub path: String,
    pub git_ref: String,
}

/// A release named by a file: `value` as a version, `needle` the text to search for elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    pub value: String,
    pub needle: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RustSupport {
    pub stable: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EolSupport {
    /// Release line (`20`, `3.11`) to the date its support ends.
    pub eol: BTreeMap<String, String>,
    pub target: String,
}

pub trait World {
    fn repos(&self) -> Vec<Repo>;
    /// `Ok(None)` when the file is not in the repo at its ref.
    fn read(&self, repo: &Repo, file: &str) -> Result<Option<String>, String>;
    /// Every file at the repo's ref containing `needle`.
    fn naming(&self, repo: &Repo, needle: &str) -> Vec<String>;
    fn rust(&self) -> Result<RustSupport, String>;
    fn node(&self) -> Result<EolSupport, String>;
    fn python(&self) -> Result<EolSupport, String>;
    fn open_bump(&self, repo: &Repo, rt: Runtime) -> Result<Option<String>, String>;
    fn file_bump(&self, repo: &Repo, rt: Runtime, title: &str, body: &str) -> Result<String, String>;
    /// `YYYY-MM-DD`.
    fn today(&self) -> String;
}

fn digits(s: &str) -> String {
    s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<String>().trim_end_matches('.').to_string()
}

fn first_version(s: &str) -> Option<String> {
    let i = s.find(|c: char| c.is_ascii_digit())?;
    let v = digits(&s[i..]);
    (!v.is_empty()).then_some(v)
}

fn quoted(line: &str) -> Option<String> {
    let after = line.split_once('=')?.1.trim();
    let q = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &after[1..];
    Some(rest[..rest.find(q)?].to_string())
}

fn minor_line(v: &str) -> Option<(u32, u32)> {
    let mut p = v.split('.');
    Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?))
}

pub fn parse_rust_stable(channel_toml: &str) -> Result<String, String> {
    let mut in_rust = false;
    for l in channel_toml.lines().map(str::trim) {
        if l.starts_with('[') {
            in_rust = l == "[pkg.rust]";
        } else if in_rust && l.starts_with("version") {
            let v = quoted(l).ok_or("unreadable [pkg.rust] version")?;
            let ver = v.split_whitespace().next().unwrap_or("");
            return minor_line(ver).map(|_| ver.to_string()).ok_or_else(|| format!("[pkg.rust] version {v:?} is not a release"));
        }
    }
    Err("no [pkg.rust] version in the channel manifest".into())
}

pub fn parse_node(schedule: &str, index: &str, today: &str) -> Result<EolSupport, String> {
    let sched: serde_json::Value = serde_json::from_str(schedule).map_err(|e| format!("node schedule is not JSON: {e}"))?;
    let obj = sched.as_object().ok_or("node schedule is not an object")?;
    let mut eol = BTreeMap::new();
    for (k, v) in obj {
        if let (Some(major), Some(end)) = (k.strip_prefix('v'), v["end"].as_str()) {
            eol.insert(major.to_string(), end.to_string());
        }
    }
    if eol.is_empty() {
        return Err("node schedule names no release line".into());
    }
    let idx: serde_json::Value = serde_json::from_str(index).map_err(|e| format!("node index is not JSON: {e}"))?;
    let target = idx
        .as_array()
        .ok_or("node index is not a list")?
        .iter()
        .filter(|r| r["lts"].is_string())
        .filter_map(|r| r["version"].as_str())
        .map(|v| v.trim_start_matches('v').to_string())
        .find(|v| eol.get(v.split('.').next().unwrap_or("")).is_some_and(|end| end.as_str() > today))
        .ok_or("no supported LTS release in the node index")?;
    Ok(EolSupport { eol, target })
}

pub fn parse_python(json: &str, today: &str) -> Result<EolSupport, String> {
    let doc: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("python release list is not JSON: {e}"))?;
    let list = doc.as_array().ok_or("python release list is not a list")?;
    let mut eol = BTreeMap::new();
    let mut target = None;
    for r in list {
        let (Some(cycle), Some(latest)) = (r["cycle"].as_str(), r["latest"].as_str()) else { continue };
        let end = match &r["eol"] {
            serde_json::Value::String(d) => d.clone(),
            serde_json::Value::Bool(false) => "9999-12-31".to_string(),
            _ => continue,
        };
        if target.is_none() && end.as_str() > today {
            target = Some(latest.to_string());
        }
        eol.insert(cycle.to_string(), end);
    }
    let target = target.ok_or("no supported release in the python list")?;
    Ok(EolSupport { eol, target })
}

pub fn civil_date(epoch_secs: i64) -> String {
    let z = epoch_secs.div_euclid(86400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn rust_pins(w: &dyn World, repo: &Repo) -> Result<Vec<Pin>, String> {
    let mut out = Vec::new();
    for file in ["rust-toolchain.toml", "rust-toolchain"] {
        let Some(text) = w.read(repo, file)? else { continue };
        let value = text
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("channel"))
            .and_then(quoted)
            .or_else(|| (!text.contains('=')).then(|| text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or("").to_string()));
        if let Some(v) = value.filter(|v| v.starts_with(|c: char| c.is_ascii_digit())) {
            out.push(Pin { needle: v.clone(), value: v, file: file.into() });
        }
    }
    Ok(out)
}

pub fn node_pins(w: &dyn World, repo: &Repo) -> Result<Vec<Pin>, String> {
    let mut out = Vec::new();
    if let Some(t) = w.read(repo, ".nvmrc")? {
        let v = t.trim().trim_start_matches('v');
        if v.starts_with(|c: char| c.is_ascii_digit()) {
            out.push(Pin { value: digits(v), needle: v.into(), file: ".nvmrc".into() });
        }
    }
    if let Some(t) = w.read(repo, "package.json")? {
        let doc: serde_json::Value = serde_json::from_str(&t).map_err(|e| format!("package.json is not JSON: {e}"))?;
        if let Some(spec) = doc["engines"]["node"].as_str() {
            if let Some(v) = first_version(spec) {
                out.push(Pin { value: v, needle: spec.into(), file: "package.json".into() });
            }
        }
    }
    if let Some(t) = w.read(repo, "flake.nix")? {
        for (i, _) in t.match_indices("nodejs_") {
            let d = digits(&t[i + 7..]);
            if !d.is_empty() && !d.contains('.') {
                out.push(Pin { needle: format!("nodejs_{d}"), value: d, file: "flake.nix".into() });
            }
        }
    }
    Ok(out)
}

pub fn python_pins(w: &dyn World, repo: &Repo) -> Result<Vec<Pin>, String> {
    let mut out = Vec::new();
    if let Some(t) = w.read(repo, "pyproject.toml")? {
        if let Some(l) = t.lines().map(str::trim).find(|l| l.starts_with("requires-python")) {
            if let Some(spec) = quoted(l) {
                if let Some(v) = first_version(&spec) {
                    out.push(Pin { value: v, needle: spec, file: "pyproject.toml".into() });
                }
            }
        }
    }
    if let Some(t) = w.read(repo, ".python-version")? {
        let v = t.trim();
        if v.starts_with(|c: char| c.is_ascii_digit()) {
            out.push(Pin { value: digits(v), needle: v.into(), file: ".python-version".into() });
        }
    }
    if let Some(t) = w.read(repo, "Dockerfile")? {
        for l in t.lines().map(str::trim).filter(|l| l.to_ascii_uppercase().starts_with("FROM ")) {
            if let Some(i) = l.find("python:") {
                if let Some(v) = first_version(&l[i + 7..]) {
                    out.push(Pin { needle: format!("python:{v}"), value: v, file: "Dockerfile".into() });
                }
            }
        }
    }
    Ok(out)
}

fn line_of(v: &str) -> String {
    v.split('.').take(2).collect::<Vec<_>>().join(".")
}

fn eol_stale(sup: &EolSupport, pins: Vec<Pin>, today: &str, major_only: bool) -> Result<Vec<Pin>, String> {
    let mut stale = Vec::new();
    for p in pins {
        let key = if major_only { p.value.split('.').next().unwrap_or("").to_string() } else { line_of(&p.value) };
        let end = sup.eol.get(&key).ok_or_else(|| format!("upstream does not list release line {key} (pin {} in {})", p.value, p.file))?;
        if end.as_str() <= today {
            stale.push(p);
        }
    }
    Ok(stale)
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub filed: Vec<String>,
    pub open: Vec<String>,
    pub current: Vec<String>,
    pub failed: Vec<String>,
    pub cannot_check: Vec<String>,
}

fn body(repo: &Repo, rt: Runtime, stale: &[Pin], target: &str, files: &[String]) -> String {
    let pins: Vec<String> = stale.iter().map(|p| format!("`{}` in `{}`", p.value, p.file)).collect();
    let mut b = format!(
        "{} pins {} {}, which upstream no longer supports. Move it to {target}.\n\nFiles naming the old pin:\n",
        repo.name,
        rt.name(),
        pins.join(", "),
    );
    for f in files {
        b.push_str(&format!("- {f}\n"));
    }
    b.push_str(&format!("\n## Build\n\nBump every pin above to {target} (exact version) in {}, rebuild, and land the change.\n", repo.name));
    b
}

pub fn pass(w: &dyn World, max_files: usize) -> Outcome {
    let today = w.today();
    let mut o = Outcome::default();
    let (mut rust, mut node, mut py) = (None, None, None);
    for repo in w.repos() {
        for rt in Runtime::ALL {
            let tag = format!("{} {}", repo.name, rt.name());
            let pins = match rt {
                Runtime::Rust => rust_pins(w, &repo),
                Runtime::Node => node_pins(w, &repo),
                Runtime::Python => python_pins(w, &repo),
            };
            let pins = match pins {
                Ok(p) if p.is_empty() => continue,
                Ok(p) => p,
                Err(e) => {
                    o.cannot_check.push(format!("{tag}: {e}"));
                    continue;
                }
            };
            let verdict: Result<(Vec<Pin>, String), String> = match rt {
                Runtime::Rust => rust.get_or_insert_with(|| w.rust()).clone().and_then(|s| rust_stale(&s, pins)),
                Runtime::Node => node.get_or_insert_with(|| w.node()).clone().and_then(|s| eol_stale(&s, pins, &today, true).map(|v| (v, s.target))),
                Runtime::Python => py.get_or_insert_with(|| w.python()).clone().and_then(|s| eol_stale(&s, pins, &today, false).map(|v| (v, s.target))),
            };
            let (stale, target) = match verdict {
                Ok(v) => v,
                Err(e) => {
                    o.cannot_check.push(format!("{tag}: {e}"));
                    continue;
                }
            };
            if stale.is_empty() {
                o.current.push(tag);
                continue;
            }
            match w.open_bump(&repo, rt) {
                Ok(Some(_)) => {
                    o.open.push(tag);
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    o.failed.push(format!("{tag}: {e}"));
                    continue;
                }
            }
            if o.filed.len() >= max_files {
                continue;
            }
            let mut files: Vec<String> = Vec::new();
            for p in &stale {
                for f in std::iter::once(p.file.clone()).chain(w.naming(&repo, &p.needle)) {
                    if !files.contains(&f) {
                        files.push(f);
                    }
                }
            }
            let title = format!("bump {} {} to {target}", rt.name(), stale[0].value);
            match w.file_bump(&repo, rt, &title, &body(&repo, rt, &stale, &target, &files)) {
                Ok(_) => o.filed.push(tag),
                Err(e) => o.failed.push(format!("{tag}: {e}")),
            }
        }
    }
    o
}

fn rust_stale(s: &RustSupport, pins: Vec<Pin>) -> Result<(Vec<Pin>, String), String> {
    let (smaj, smin) = minor_line(&s.stable).ok_or_else(|| format!("stable {:?} is not a release", s.stable))?;
    let mut stale = Vec::new();
    for p in pins {
        let (maj, min) = minor_line(&p.value).ok_or_else(|| format!("pin {:?} in {} is not a release", p.value, p.file))?;
        if maj < smaj || (maj == smaj && smin.saturating_sub(min) > RUST_GRACE) {
            stale.push(p);
        }
    }
    Ok((stale, s.stable.clone()))
}
