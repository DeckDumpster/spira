//! A consuming site depends on the shared kit by semver. When the kit publishes a release newer
//! than a site's locked version, exactly one bead per site carries it: filed when none is open,
//! amended (its target moved) when one is. Kit commits that publish nothing file nothing.

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
    pub target: String,
}

pub trait World {
    /// The newest non-yanked, non-pre-release version the registry holds for the kit crate.
    fn latest_release(&self) -> Result<String, String>;
    /// The version requirement the site's manifest states for the kit crate.
    fn site_requirement(&self, site: &Site) -> Result<String, String>;
    /// The version the site's lockfile resolves the kit crate to.
    fn site_locked(&self, site: &Site) -> Result<String, String>;
    fn open_bump(&self, site: &Site) -> Result<Option<Open>, String>;
    fn file_bump(&self, site: &Site, title: &str, body: &str, target: &str) -> Result<String, String>;
    fn amend_bump(&self, site: &Site, open: &Open, body: &str, target: &str) -> Result<(), String>;
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

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub filed: Vec<String>,
    pub amended: Vec<String>,
    pub current: Vec<String>,
    pub failed: Vec<String>,
}

pub fn pass(w: &dyn World, sites: &[Site], crate_name: &str) -> Result<Outcome, String> {
    let release = w.latest_release()?;
    let latest = semver::parse_version(&release).ok_or_else(|| format!("registry version {release:?} is not semver"))?;
    let mut o = Outcome::default();
    for site in sites {
        if let Err(e) = site_pass(w, site, crate_name, &release, latest, &mut o) {
            o.failed.push(format!("{}: {e}", site.name));
        }
    }
    Ok(o)
}

fn site_pass(w: &dyn World, site: &Site, crate_name: &str, release: &str, latest: semver::Version, o: &mut Outcome) -> Result<(), String> {
    let locked = w.site_locked(site)?;
    let locked_v = semver::parse_version(&locked).ok_or_else(|| format!("locked version {locked:?} is not semver"))?;
    if locked_v >= latest {
        o.current.push(site.name.clone());
        return Ok(());
    }
    let req = w.site_requirement(site)?;
    let breaking = !semver::admits(&req, release).ok_or_else(|| format!("requirement {req:?} is not readable"))?;
    let text = body(site, crate_name, &req, &locked, release, breaking);
    match w.open_bump(site)? {
        Some(op) if op.target == release => o.current.push(site.name.clone()),
        Some(op) => {
            w.amend_bump(site, &op, &text, release)?;
            o.amended.push(site.name.clone());
        }
        None => {
            w.file_bump(site, &title(site, crate_name, release, breaking), &text, release)?;
            o.filed.push(site.name.clone());
        }
    }
    Ok(())
}
