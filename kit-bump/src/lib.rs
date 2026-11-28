//! A consuming site depends on the shared kit by semver. When the kit publishes a release newer
//! than a site's locked version, exactly one bead per site carries it: filed when none is open,
//! amended (its target moved) when one is. Kit commits that publish nothing file nothing.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod real;
pub mod semver;
#[cfg(test)]
mod tests;

pub const LABEL: &str = "kit-bump";

#[derive(Debug, Clone, PartialEq)]
pub struct Site {
    pub name: String,
    pub path: String,
    pub git_ref: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Open {
    pub id: String,
}

pub trait World {
    /// The version requirement the site's manifest states for the kit crate.
    fn site_requirement(&self, site: &Site) -> Result<String, String>;
    /// The version the site's lockfile resolves the kit crate to.
    fn site_locked(&self, site: &Site) -> Result<String, String>;
    fn file_bump(&self, site: &Site, title: &str, body: &str) -> Result<String, String>;
    fn amend_bump(&self, open: &Open, body: &str, target: &str) -> Result<(), String>;
}

fn dependency_line<'a>(manifest: &'a str, crate_name: &str) -> Option<&'a str> {
    manifest.lines().map(str::trim).find(|l| {
        l.strip_prefix(crate_name).is_some_and(|r| r.trim_start().starts_with('='))
    })
}

fn first_quoted(s: &str) -> Option<&str> {
    let after = &s[s.find('"')? + 1..];
    Some(&after[..after.find('"')?])
}

/// The requirement a manifest states for `crate_name`: the string itself, or a table's `version`.
pub fn parse_requirement(manifest: &str, crate_name: &str) -> Option<String> {
    let line = dependency_line(manifest, crate_name)?;
    let value = line.split_once('=')?.1.trim();
    let req = if value.starts_with('{') {
        let after = &value[value.find("version")? + 7..];
        first_quoted(after.trim_start().strip_prefix('=')?)?
    } else {
        first_quoted(value)?
    };
    (!req.is_empty()).then(|| req.to_string())
}

/// The version a lockfile resolves `crate_name` to.
pub fn parse_locked(lock: &str, crate_name: &str) -> Option<String> {
    let mut lines = lock.lines().map(str::trim);
    while let Some(l) = lines.next() {
        if first_quoted(l).is_some_and(|n| l.starts_with("name") && n == crate_name) {
            let v = lines.next()?;
            return v.starts_with("version").then(|| first_quoted(v).map(str::to_string)).flatten();
        }
    }
    None
}

/// The newest stable version in a sparse-index file (one JSON object per line).
pub fn newest_release(index: &str) -> Option<String> {
    index
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|d| d["yanked"] != true)
        .filter_map(|d| d["vers"].as_str().map(str::to_string))
        .filter_map(|v| semver::parse_version(&v).map(|p| (p, v)))
        .max()
        .map(|(_, v)| v)
}

/// `SPIRA_KIT_SITES`: space-separated `name:manifest-file` pairs; a bare `name` reads its
/// workspace manifest, `Cargo.toml`.
pub fn site_specs(spec: &str) -> Vec<(String, String)> {
    spec.split_whitespace()
        .filter_map(|pair| match pair.split_once(':') {
            Some((n, f)) if !n.is_empty() && !f.is_empty() => Some((n.to_string(), f.to_string())),
            Some(_) => None,
            None => Some((pair.to_string(), "Cargo.toml".to_string())),
        })
        .collect()
}

pub fn title(site: &Site, crate_name: &str, release: &str, breaking: bool) -> String {
    if breaking {
        format!("upgrade {crate_name} to {release} (breaking) in {}", site.name)
    } else {
        format!("update {crate_name} lockfile to {release} in {}", site.name)
    }
}

pub fn body(site: &Site, crate_name: &str, req: &str, locked: &str, release: &str, breaking: bool) -> String {
    let mut b = format!("`{crate_name}` {release} is published; `{}` of {} locks {locked}.\n\n", site.file, site.name);
    if breaking {
        b.push_str(&format!(
            "The release is outside the requirement `{req}`: this is a breaking upgrade. Raise the requirement in `{}` to admit {release}, adapt the site to the new API, and refresh the lockfile.\n",
            site.file
        ));
    } else {
        b.push_str(&format!(
            "The requirement `{req}` admits it: run `cargo update -p {crate_name} --precise {release}` and land the lockfile change.\n"
        ));
    }
    b.push_str("\nThis bead is kept current by kit-bump: a later release moves its target instead of filing another. Work that needs the bump in force depends on this bead.\n");
    b
}
