//! A consuming site pins the shared kit by git commit. When the kit's landing tip moves past a
//! site's pin, exactly one bump bead per site carries it: filed when none is open, amended
//! (its target moved) when one is.

use std::collections::BTreeMap;

pub mod real;
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
pub struct Commit {
    pub sha: String,
    pub subject: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Open {
    pub id: String,
    pub target: String,
}

pub trait World {
    fn kit_tip(&self) -> Result<String, String>;
    fn site_pin(&self, site: &Site) -> Result<String, String>;
    /// True when `ancestor` is reachable from `descendant` (or equal to it).
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool;
    /// Kit commits in `pin..tip`, oldest first; empty when the pin is not in the kit's history.
    fn commits(&self, pin: &str, tip: &str) -> Vec<Commit>;
    fn open_bump(&self, site: &Site) -> Result<Option<Open>, String>;
    fn file_bump(&self, site: &Site, title: &str, body: &str, target: &str) -> Result<String, String>;
    fn amend_bump(&self, site: &Site, open: &Open, body: &str, target: &str) -> Result<(), String>;
    fn now(&self) -> i64;
}

/// The rev a manifest pins `crate_name` at: the `rev = "…"` on the line naming the crate.
pub fn parse_pin(manifest: &str, crate_name: &str) -> Option<String> {
    manifest.lines().filter(|l| l.contains(crate_name)).find_map(|l| {
        let after = &l[l.find("rev")? + 3..];
        let after = after.trim_start().strip_prefix('=')?.trim_start().strip_prefix('"')?;
        let rev = &after[..after.find('"')?];
        (!rev.is_empty() && rev.chars().all(|c| c.is_ascii_hexdigit())).then(|| rev.to_string())
    })
}

/// Bead ids (`sp-` and a base-36 tail) named in commit subjects, first-seen order, no repeats.
pub fn carried_beads(commits: &[Commit]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in commits {
        let b = c.subject.as_bytes();
        let mut i = 0;
        while let Some(p) = c.subject[i..].find("sp-") {
            let start = i + p;
            let boundary = start == 0 || !b[start - 1].is_ascii_alphanumeric();
            let end = c.subject[start + 3..]
                .find(|ch: char| !ch.is_ascii_alphanumeric())
                .map_or(c.subject.len(), |e| start + 3 + e);
            let id = &c.subject[start..end];
            if boundary && id.len() > 3 && !out.iter().any(|x| x == id) {
                out.push(id.to_string());
            }
            i = start + 3;
        }
    }
    out
}

pub fn title(site: &Site, crate_name: &str) -> String {
    format!("bump {crate_name} pin in {}", site.name)
}

pub fn body(site: &Site, crate_name: &str, pin: &str, tip: &str, commits: &[Commit], beads: &[String]) -> String {
    let short = |s: &str| s.chars().take(12).collect::<String>();
    let mut b = format!(
        "Move the `{crate_name}` pin in `{}` of {} from {} to {}.\n\nKit beads carried: {}\n\nKit commits carried:\n",
        site.file,
        site.name,
        short(pin),
        short(tip),
        if beads.is_empty() { "none named".to_string() } else { beads.join(", ") }
    );
    for c in commits {
        b.push_str(&format!("- {} {}\n", short(&c.sha), c.subject));
    }
    b.push_str("\nThis bead is kept current by kit-bump: a later kit landing moves its target instead of filing another. Work that needs the bump in force depends on this bead.\n");
    b
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub filed: Vec<String>,
    pub amended: Vec<String>,
    pub current: Vec<String>,
    pub failed: Vec<String>,
    /// Hours since the oldest unpinned kit commit landed, per behind site.
    pub lag_hours: BTreeMap<String, f64>,
}

pub fn pass(w: &dyn World, sites: &[Site], crate_name: &str) -> Result<Outcome, String> {
    let tip = w.kit_tip()?;
    let mut o = Outcome::default();
    for site in sites {
        let pin = match w.site_pin(site) {
            Ok(p) => p,
            Err(e) => {
                o.failed.push(format!("{}: {e}", site.name));
                continue;
            }
        };
        let tip_in_pin = w.is_ancestor(&tip, &pin);
        if tip_in_pin {
            o.current.push(site.name.clone());
            continue;
        }
        let commits = w.commits(&pin, &tip);
        if let Some(first) = commits.first() {
            o.lag_hours.insert(site.name.clone(), (w.now() - first.at).max(0) as f64 / 3600.0);
        }
        let open = match w.open_bump(site) {
            Ok(v) => v,
            Err(e) => {
                o.failed.push(format!("{}: {e}", site.name));
                continue;
            }
        };
        let text = body(site, crate_name, &pin, &tip, &commits, &carried_beads(&commits));
        let res = match &open {
            Some(op) if op.target == tip => {
                o.current.push(site.name.clone());
                continue;
            }
            Some(op) => w.amend_bump(site, op, &text, &tip).map(|()| o.amended.push(site.name.clone())),
            None => w
                .file_bump(site, &title(site, crate_name), &text, &tip)
                .map(|_| o.filed.push(site.name.clone())),
        };
        if let Err(e) = res {
            o.failed.push(format!("{}: {e}", site.name));
        }
    }
    Ok(o)
}
