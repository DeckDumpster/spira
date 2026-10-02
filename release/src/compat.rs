//! Compat names (sp-6onps-compat, a P0): `spira/deps.toml`'s `[[compat]]` table is the one
//! declared list of (real binary, old bare-name alias) pairs a release's `bin/` and
//! `spira/` must also carry a symlink under — read here and by
//! `spira/build-tarball.sh` (python3/tomllib, the same technique `conf.sh` already uses
//! for the table's `[[dep]]` sibling), never a second hand-written name list either could
//! drift from the other or from the table itself. The bug this closes: only
//! build-tarball.sh knew the list; `release build` — what `queue land-local` and every
//! real deploy actually takes — never read it, so a release built that way shipped with
//! none of the compat names in `bin/` or `spira/`.

use std::path::Path;

/// Reads `<stage>/spira/deps.toml`'s `[[compat]]` table. Empty (never an error) when the
/// file is absent or does not parse — a release missing compat aliases is a degraded
/// release, not a build that must refuse; `build`'s own required-binaries check already
/// covers the real failure modes.
pub fn read(stage: &Path) -> Vec<(String, String)> {
    let path = stage.join("spira").join("deps.toml");
    let doc = spira_config::deps::load_manifest(&path);
    doc.compat.into_iter().map(|c| (c.name, c.alias)).collect()
}

/// Symlinks `bin/<alias> -> <name>` and `spira/<alias> -> ../bin/<name>` for every compat
/// entry whose target binary exists in `bin_dir` — silently skipped otherwise (e.g.
/// mail.sh's entry, ahead of sp-ooh1k landing the `mail` binary). Returns the aliases
/// actually created, for the caller to fold into its own manifest/log.
pub fn link(stage: &Path, bin_dir: &Path) -> Result<Vec<String>, String> {
    let spira_dir = stage.join("spira");
    let mut made = Vec::new();
    for (name, alias) in read(stage) {
        if !bin_dir.join(&name).is_file() {
            continue;
        }
        let bin_alias = bin_dir.join(&alias);
        if !bin_alias.exists() {
            std::os::unix::fs::symlink(&name, &bin_alias)
                .map_err(|e| format!("cannot symlink bin/{alias} -> {name}: {e}"))?;
        }
        let spira_alias = spira_dir.join(&alias);
        if spira_dir.is_dir() && !spira_alias.exists() {
            std::os::unix::fs::symlink(format!("../bin/{name}"), &spira_alias)
                .map_err(|e| format!("cannot symlink spira/{alias} -> ../bin/{name}: {e}"))?;
        }
        made.push(alias);
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn stage_with_deps_toml(text: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new("compat");
        fs::create_dir_all(d.join("spira")).unwrap();
        fs::write(d.join("spira").join("deps.toml"), text).unwrap();
        d
    }

    #[test]
    fn read_parses_every_compat_entry() {
        let d = stage_with_deps_toml(
            "[[compat]]\nname = \"world\"\nalias = \"world.sh\"\n\n[[compat]]\nname = \"ctrl\"\nalias = \"ctrl.sh\"\n",
        );
        let v = read(&d);
        assert_eq!(v, vec![("world".to_string(), "world.sh".to_string()), ("ctrl".to_string(), "ctrl.sh".to_string())]);
    }

    #[test]
    fn read_is_empty_when_deps_toml_is_missing() {
        let d = testkit::TempDir::new("compat-missing");
        assert!(read(&d).is_empty());
    }

    #[test]
    fn read_is_empty_when_the_table_is_absent() {
        let d = stage_with_deps_toml("[[dep]]\nname = \"bd\"\n");
        assert!(read(&d).is_empty());
    }

    #[test]
    fn link_creates_both_symlinks_only_when_the_binary_exists() {
        let d = stage_with_deps_toml(
            "[[compat]]\nname = \"world\"\nalias = \"world.sh\"\n\n[[compat]]\nname = \"mail\"\nalias = \"mail.sh\"\n",
        );
        let bin = d.join("bin");
        fs::create_dir_all(&bin).unwrap();
        testkit::write_exe(bin.join("world"), "#!/bin/sh\n");
        // "mail" is NOT built yet (ahead of sp-ooh1k) — its entry must be skipped, not fail.
        let made = link(&d, &bin).unwrap();
        assert_eq!(made, vec!["world.sh".to_string()]);
        assert!(bin.join("world.sh").is_symlink());
        assert_eq!(fs::read_link(bin.join("world.sh")).unwrap().to_str().unwrap(), "world");
        assert!(d.join("spira").join("world.sh").is_symlink());
        assert_eq!(fs::read_link(d.join("spira").join("world.sh")).unwrap().to_str().unwrap(), "../bin/world");
        assert!(!bin.join("mail.sh").exists());
        assert!(!d.join("spira").join("mail.sh").exists());
    }

    #[test]
    fn link_is_idempotent() {
        let d = stage_with_deps_toml("[[compat]]\nname = \"world\"\nalias = \"world.sh\"\n");
        let bin = d.join("bin");
        fs::create_dir_all(&bin).unwrap();
        testkit::write_exe(bin.join("world"), "#!/bin/sh\n");
        link(&d, &bin).unwrap();
        // A second run must not error on "already exists".
        let made = link(&d, &bin).unwrap();
        assert_eq!(made, vec!["world.sh".to_string()]);
    }
}
