//! `repo-contract scaffold`: the contract files a repo lacks, derived from what it already pins.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const TOOLS: &str = "repo-contract/tools.toml";
const RUST_PIN: &str = "rust-toolchain.toml";
const PYTHON_PIN: &str = ".python-version";
const UV_LOCK: &str = "uv.lock";
const DEPLOY_WORKFLOW: &str = ".github/workflows/deploy.yml";
const CHECKS_HEADER: &str = "# Rows `work checks` runs before a branch is submitted. Add a [[check]] with name, run, budget_ms.\n";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pins {
    pub rust: bool,
    pub python: Option<String>,
    pub uv_lock: bool,
    pub programs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmapped(pub String);

impl std::fmt::Display for Unmapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not mapped: {} — add it to {TOOLS}", self.0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolTable {
    pub tools: BTreeMap<String, String>,
    pub provided_by_rust: Vec<String>,
    pub provided_by_python: Vec<String>,
    pub rust_overlay_rev: String,
}

impl ToolTable {
    pub fn parse(text: &str) -> Result<ToolTable, String> {
        let t: toml::Table = text.parse().map_err(|e| format!("{TOOLS}: {e}"))?;
        let strs = |t: Option<&toml::Value>| -> Vec<String> {
            t.and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
        };
        let tools = t
            .get("tools")
            .and_then(|v| v.as_table())
            .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
            .unwrap_or_default();
        let provided = t.get("provided").and_then(|v| v.as_table());
        let rev = t.get("rust-overlay").and_then(|v| v.get("rev")).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        Ok(ToolTable { tools, provided_by_rust: strs(provided.and_then(|p| p.get("rust"))), provided_by_python: strs(provided.and_then(|p| p.get("python"))), rust_overlay_rev: rev })
    }
}

/// The `<major><minor>` of a `.python-version`, as nixpkgs names it (`3.12.4` -> `312`).
fn python_attr(version: &str) -> Result<String, String> {
    let mut parts = version.trim().split('.');
    match (parts.next(), parts.next()) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit()) => Ok(format!("{a}{b}")),
        _ => Err(format!("{PYTHON_PIN} holds {:?}; scaffold reads a `<major>.<minor>` version", version.trim())),
    }
}

pub fn pins(tree: &Path) -> Result<Pins, String> {
    let read = |rel: &str| -> Result<Option<String>, String> {
        match std::fs::read_to_string(tree.join(rel)) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", tree.join(rel).display())),
        }
    };
    let python = read(PYTHON_PIN)?.map(|v| python_attr(&v).map(|_| v.trim().to_string())).transpose()?;
    let mut programs = Vec::new();
    if let Some(text) = read(crate::clause::CHECKS)? {
        for row in habits::config::parse_checks(&text).map_err(|e| format!("{}: {e}", crate::clause::CHECKS))?.iter().filter(|r| r.build.is_none()) {
            if let Some(p) = habits::config::argv(&row.run, &[]).into_iter().next().filter(|p| !p.contains('/')) {
                if !programs.contains(&p) {
                    programs.push(p);
                }
            }
        }
    }
    Ok(Pins { rust: tree.join(RUST_PIN).is_file(), python, uv_lock: tree.join(UV_LOCK).is_file(), programs })
}

/// `nixpkgs_rev` is the revision the release's `flake.lock` pins.
pub fn render_flake(pins: &Pins, table: &ToolTable, nixpkgs_rev: &str) -> Result<String, Unmapped> {
    let mut packages: Vec<String> = Vec::new();
    let mut add = |p: String| {
        if !packages.contains(&p) {
            packages.push(p);
        }
    };
    if pins.python.is_some() {
        let version = pins.python.as_deref().unwrap_or_default();
        add(python_attr(version).map(|v| format!("python{v}")).map_err(|_| Unmapped(format!("{PYTHON_PIN} {version:?}")))?);
        if pins.uv_lock {
            add("uv".into());
        }
    }
    for prog in &pins.programs {
        let provided = (pins.rust && table.provided_by_rust.contains(prog)) || (pins.python.is_some() && table.provided_by_python.contains(prog));
        if provided {
            continue;
        }
        match table.tools.get(prog) {
            Some(attr) => add(attr.clone()),
            None => return Err(Unmapped(prog.clone())),
        }
    }
    let mut inputs = format!("  inputs.nixpkgs.url = \"github:NixOS/nixpkgs/{nixpkgs_rev}\";\n");
    let (mut args, mut overlays, mut rust_pkg) = ("nixpkgs".to_string(), String::new(), String::new());
    if pins.rust {
        inputs += &format!("  inputs.rust-overlay.url = \"github:oxalica/rust-overlay/{}\";\n  inputs.rust-overlay.inputs.nixpkgs.follows = \"nixpkgs\";\n", table.rust_overlay_rev);
        args += ", rust-overlay";
        overlays = " overlays = [ rust-overlay.overlays.default ];".into();
        rust_pkg = "          (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)\n".into();
    }
    let attrs: String = packages.iter().map(|p| format!("          pkgs.{p}\n")).collect();
    let env = if pins.python.is_some() { "        UV_PYTHON_DOWNLOADS = \"never\";\n" } else { "" };
    Ok(format!(
        "{{\n  description = \"dev shell for the tools this repo's pins and check rows name\";\n{inputs}\n  outputs = {{ self, {args} }}:\n    let\n      system = \"x86_64-linux\";\n      pkgs = import nixpkgs {{ inherit system;{overlays} }};\n    in {{\n      devShells.${{system}}.default = pkgs.mkShell {{\n        packages = [\n{rust_pkg}{attrs}        ];\n{env}      }};\n    }};\n}}\n"
    ))
}

/// The nixpkgs revision a `flake.lock` pins.
pub fn nixpkgs_rev(flake_lock: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(flake_lock).map_err(|e| format!("the release's flake.lock is not JSON: {e}"))?;
    v.pointer("/nodes/nixpkgs/locked/rev").and_then(|r| r.as_str()).map(String::from).ok_or_else(|| "the release's flake.lock pins no nixpkgs rev".into())
}

/// Refuses a directory that is the registered checkout or has the landing branch checked out.
pub fn guard_target(into: &Path, root: &Path, head_branch: Option<&str>, land_branch: &str) -> Result<(), String> {
    let resolve = |p: &Path| p.canonicalize().map_err(|e| format!("refusing: cannot resolve {} ({e}); pass --into <a worktree> that exists", p.display()));
    if resolve(into)?.starts_with(resolve(root)?) {
        return Err(format!("refusing: {} is the registered checkout or inside it; run scaffold in a bead's worktree, or pass --into <a worktree>", into.display()));
    }
    if head_branch == Some(land_branch) {
        return Err(format!("refusing: {} has the landing branch {land_branch} checked out; run scaffold on a branch for a bead, or pass --into <a worktree>", into.display()));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// (path relative to the tree, content): files that do not exist and will be created.
    pub files: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// What to write into `tree`: only files absent from it.
pub fn plan(tree: &Path, table: &ToolTable, nixpkgs_rev: &str, land: &str) -> Result<Plan, String> {
    let pins = pins(tree)?;
    let mut p = Plan { files: Vec::new(), notes: Vec::new() };
    if tree.join("flake.nix").exists() {
        p.notes.push("flake.nix exists; left as it is".into());
    } else {
        p.files.push(("flake.nix".into(), render_flake(&pins, table, nixpkgs_rev).map_err(|u| u.to_string())?));
    }
    if tree.join(crate::clause::CHECKS).exists() {
        p.notes.push(format!("{} exists; left as it is", crate::clause::CHECKS));
    } else {
        p.files.push((crate::clause::CHECKS.into(), CHECKS_HEADER.into()));
    }
    if tree.join(crate::clause::DEPLOY).exists() {
        p.notes.push(format!("{} exists; left as it is", crate::clause::DEPLOY));
    } else if tree.join(DEPLOY_WORKFLOW).is_file() {
        let after = format!("the {} run for the landed commit on {land} succeeds", DEPLOY_WORKFLOW.rsplit('/').next().unwrap_or_default());
        p.files.push((crate::clause::DEPLOY.into(), format!("after_landing = {:?}\n", after)));
    } else {
        p.notes.push(format!("{} not written: the tree has no {DEPLOY_WORKFLOW} to derive a deploy declaration from; write it by hand", crate::clause::DEPLOY));
    }
    Ok(p)
}

/// Writes each file with `create_new`, so a file that appeared since `plan` is never overwritten.
pub fn write(tree: &Path, plan: &Plan) -> Result<Vec<PathBuf>, String> {
    use std::io::Write;
    let mut done = Vec::new();
    for (rel, content) in &plan.files {
        let path = tree.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        f.write_all(content.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
        done.push(path);
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS_TOML: &str = include_str!("../tools.toml");
    const REV: &str = "7fd4004d0511991fff3a0524856a9be09e28c5f0";

    const PY_PYTHON: &str = include_str!("../tests/data/python-uv/.python-version");
    const PY_CHECKS: &str = include_str!("../tests/data/python-uv/.spira/checks.toml");
    const PY_UV_LOCK: &str = include_str!("../tests/data/python-uv/uv.lock");
    const RUST_A_TOOLCHAIN: &str = include_str!("../tests/data/rust-a/rust-toolchain.toml");
    const RUST_B_TOOLCHAIN: &str = include_str!("../tests/data/rust-b/rust-toolchain.toml");

    fn table() -> ToolTable {
        ToolTable::parse(TOOLS_TOML).unwrap()
    }

    fn tree_with(files: &[(&str, &str)]) -> testkit::TempDir {
        let d = testkit::TempDir::new("repo-contract-scaffold");
        for (rel, content) in files {
            std::fs::create_dir_all(d.join(rel).parent().unwrap()).unwrap();
            std::fs::write(d.join(rel), content).unwrap();
        }
        d
    }

    fn python_uv() -> testkit::TempDir {
        tree_with(&[(".python-version", PY_PYTHON), (".spira/checks.toml", PY_CHECKS), ("uv.lock", PY_UV_LOCK)])
    }

    fn rust_a() -> testkit::TempDir {
        tree_with(&[("rust-toolchain.toml", RUST_A_TOOLCHAIN), (".github/workflows/deploy.yml", "")])
    }

    fn rust_b() -> testkit::TempDir {
        tree_with(&[("rust-toolchain.toml", RUST_B_TOOLCHAIN)])
    }

    #[test]
    fn an_unresolvable_into_is_refused_rather_than_passed() {
        let root = tree_with(&[]);
        let r = guard_target(&root.join("no-such-dir"), &root, None, "main");
        assert!(r.unwrap_err().contains("pass --into"));
    }

    #[test]
    fn a_bad_python_version_is_refused_by_render_flake() {
        let p = Pins { python: Some("system".into()), uv_lock: true, ..Pins::default() };
        assert!(render_flake(&p, &table(), REV).is_err());
    }

    #[test]
    fn python_uv_gets_python312_and_uv_and_its_row_program_from_the_table() {
        let p = pins(&python_uv()).unwrap();
        assert_eq!(p, Pins { rust: false, python: Some("3.12".into()), uv_lock: true, programs: vec!["uv".into()] });
        let flake = render_flake(&p, &table(), REV).unwrap();
        assert!(flake.contains("pkgs.python312") && flake.contains("pkgs.uv") && flake.contains("UV_PYTHON_DOWNLOADS = \"never\""));
        assert!(flake.contains(&format!("github:NixOS/nixpkgs/{REV}")) && !flake.contains("rust-overlay"));
    }

    #[test]
    fn rust_repos_get_the_toolchain_file_through_a_pinned_overlay() {
        for tree in [rust_a(), rust_b()] {
            let p = pins(&tree).unwrap();
            assert!(p.rust && p.python.is_none());
            let flake = render_flake(&p, &table(), REV).unwrap();
            assert!(flake.contains("rust-bin.fromRustupToolchainFile ./rust-toolchain.toml"));
            assert!(flake.contains(&format!("github:oxalica/rust-overlay/{}", table().rust_overlay_rev)) && !table().rust_overlay_rev.is_empty());
        }
    }

    #[test]
    fn a_program_the_table_does_not_map_is_refused_by_name() {
        let p = Pins { programs: vec!["frobnicate".into()], ..Pins::default() };
        let e = render_flake(&p, &table(), REV).unwrap_err();
        assert_eq!(e.to_string(), "not mapped: frobnicate — add it to repo-contract/tools.toml");
    }

    #[test]
    fn the_nixpkgs_revision_is_read_from_a_lock_never_written_in_code() {
        let lock = r#"{"nodes":{"nixpkgs":{"locked":{"rev":"abc123"}}}}"#;
        assert_eq!(nixpkgs_rev(lock).unwrap(), "abc123");
        assert!(nixpkgs_rev("{}").is_err());
        let flake = render_flake(&Pins::default(), &table(), "abc123").unwrap();
        assert!(flake.contains("github:NixOS/nixpkgs/abc123"));
    }

    #[test]
    fn an_existing_flake_is_never_overwritten() {
        let d = rust_a();
        std::fs::write(d.join("flake.nix"), "mine").unwrap();
        let p = plan(&d, &table(), REV, "local/main").unwrap();
        assert!(p.files.iter().all(|(r, _)| r != "flake.nix"));
        write(&d, &p).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("flake.nix")).unwrap(), "mine");
        let stale = Plan { files: vec![("flake.nix".into(), "x".into())], notes: vec![] };
        assert!(write(&d, &stale).is_err());
        assert_eq!(std::fs::read_to_string(d.join("flake.nix")).unwrap(), "mine");
    }

    #[test]
    fn a_missing_checks_file_is_written_without_rows_and_deploy_follows_the_workflow() {
        let d = rust_a();
        let p = plan(&d, &table(), REV, "local/main").unwrap();
        let get = |n: &str| p.files.iter().find(|(r, _)| r == n).map(|(_, c)| c.clone());
        assert!(!get(".spira/checks.toml").unwrap().contains("[[check]]\n"));
        assert!(get(".spira/deploy.toml").unwrap().contains("the deploy.yml run for the landed commit on local/main succeeds"));
        let d = rust_b();
        let p = plan(&d, &table(), REV, "local/main").unwrap();
        assert!(p.files.iter().all(|(r, _)| r != ".spira/deploy.toml"));
        assert!(p.notes.iter().any(|n| n.contains(".spira/deploy.toml not written")));
    }

    #[test]
    fn into_the_registered_checkout_or_the_landing_branch_is_refused_naming_the_exit() {
        let a = testkit::TempDir::new("repo-contract-root");
        let b = testkit::TempDir::new("repo-contract-wt");
        assert!(guard_target(&a, &a, None, "main").unwrap_err().contains("--into <a worktree>"));
        std::fs::create_dir(a.join("sub")).unwrap();
        assert!(guard_target(&a.join("sub"), &a, None, "main").is_err());
        assert!(guard_target(&b, &a, Some("main"), "main").unwrap_err().contains("branch for a bead"));
        assert!(guard_target(&b, &a, Some("spira/x"), "main").is_ok());
    }

    #[test]
    fn a_bad_python_pin_is_refused() {
        assert!(python_attr("system").is_err());
        assert_eq!(python_attr("3.12.4\n").unwrap(), "312");
    }
}
