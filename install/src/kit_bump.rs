//! Renders `kit-bump.conf` — the flag file `spira-kit-bump.service` reads — from the
//! `SPIRA_KIT_*` keys and the repo map.

use spira_config::repos::{Column, Registry};
use std::path::Path;

pub const FILE: &str = "kit-bump.conf";

pub struct Spec<'a> {
    pub kit_repo: &'a str,
    pub krate: &'a str,
    pub sites: &'a str,
}

fn base_of(reg: &Registry, repo: &str) -> Result<(String, String), String> {
    let path = reg.field(repo, Column::Path).unwrap_or_default();
    let base = reg.field(repo, Column::Base).unwrap_or_default();
    if path.is_empty() || base.is_empty() {
        return Err(format!("repo {repo:?} needs a path and a base ref in the repo map"));
    }
    Ok((path, base))
}

pub fn render(spec: &Spec, reg: &Registry) -> Result<String, String> {
    if spec.krate.is_empty() {
        return Err("SPIRA_KIT_CRATE is empty".into());
    }
    let (kit, kit_ref) = base_of(reg, spec.kit_repo)?;
    let mut out = format!("kit {kit}\nkit-ref {kit_ref}\ncrate {}\n", spec.krate);
    let mut n = 0;
    for pair in spec.sites.split_whitespace() {
        let (name, file) = pair.split_once(':').filter(|(a, b)| !a.is_empty() && !b.is_empty()).ok_or_else(|| format!("SPIRA_KIT_SITES entry {pair:?} is not NAME:FILE"))?;
        let (path, git_ref) = base_of(reg, name)?;
        out.push_str(&format!("site {name}:{path}:{git_ref}:{file}\n"));
        n += 1;
    }
    if n == 0 {
        return Err("SPIRA_KIT_SITES names no site".into());
    }
    Ok(out)
}

pub fn write(run: &Path, spec: &Spec, reg: &Registry) -> Result<std::path::PathBuf, String> {
    let text = render(spec, reg)?;
    let p = run.join(FILE);
    spira_config::write_atomic(&p, &text).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    Ok(p)
}

/// `Ok(None)` when no kit is configured; a configured kit that cannot be rendered is an error.
pub fn install(run: &Path, home: &Path, cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<Option<std::path::PathBuf>, String> {
    let (kit_repo, krate, sites) = (cfg("SPIRA_KIT_REPO")?, cfg("SPIRA_KIT_CRATE")?, cfg("SPIRA_KIT_SITES")?);
    if kit_repo.is_empty() {
        return Ok(None);
    }
    let reg = Registry::from_env(std::env::vars().collect(), home);
    write(run, &Spec { kit_repo: &kit_repo, krate: &krate, sites: &sites }, &reg).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn parse(t: &str) -> Registry {
        Registry::new(Some(t), &BTreeMap::new(), Path::new("/h"))
    }

    const MAP: &str = "kit | /k | queue.forge | origin/main | | | plan\napp | /a | queue.forge | origin/master | | | plan\nweb | /w | queue.forge | origin/trunk | | | plan\n";

    fn spec<'a>(sites: &'a str) -> Spec<'a> {
        Spec { kit_repo: "kit", krate: "kit-core", sites }
    }

    #[test]
    fn renders_the_kit_the_crate_and_each_site_with_its_own_path_and_ref() {
        let t = render(&spec("app:Cargo.toml web:sub/Cargo.toml"), &parse(MAP)).unwrap();
        assert_eq!(t, "kit /k\nkit-ref origin/main\ncrate kit-core\nsite app:/a:origin/master:Cargo.toml\nsite web:/w:origin/trunk:sub/Cargo.toml\n");
    }

    #[test]
    fn the_written_file_is_one_name_value_flag_per_line_with_every_required_flag() {
        let d = testkit::TempDir::new("kit-bump-conf");
        let p = write(d.path(), &spec("app:Cargo.toml web:Cargo.toml"), &parse(MAP)).unwrap();
        let text = std::fs::read_to_string(p).unwrap();
        let flags: Vec<&str> = text.lines().map(|l| l.split_once(' ').expect("name value").0).collect();
        for need in ["kit", "kit-ref", "crate", "site"] {
            assert!(flags.contains(&need), "{need} missing from {text}");
        }
        assert_eq!(flags.iter().filter(|f| **f == "site").count(), 2);
    }

    #[test]
    fn install_writes_nothing_without_a_kit() {
        let d = testkit::TempDir::new("kit-bump-install");
        let none = |_: &str| Ok(String::new());
        assert_eq!(install(d.path(), d.path(), &none).unwrap(), None);
        assert!(!d.path().join(FILE).exists());
    }

    #[test]
    fn a_site_or_kit_missing_from_the_map_is_refused() {
        assert!(render(&spec("ghost:Cargo.toml"), &parse(MAP)).is_err());
        assert!(render(&Spec { kit_repo: "ghost", krate: "c", sites: "app:Cargo.toml" }, &parse(MAP)).is_err());
        assert!(render(&spec(""), &parse(MAP)).is_err());
        assert!(render(&spec("app"), &parse(MAP)).is_err());
    }
}
