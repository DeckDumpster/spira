//! `testenv ui <repo-path|name> [spec...]` runs a repository's Playwright specs in the official
//! Playwright image, pinned to the project's `@playwright/test` version, so no browser library is
//! needed on the host. Exits with Playwright's status.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::foreign::{Decl, Ui, DECL};
use crate::runtime::{ContainerRuntime, Overlay, RunRequest};

pub const USAGE: &str = "usage: testenv ui <repo-path|name> [spec...]";
const RC_FAULT: i32 = 2;
const CONFIGS: [&str; 3] = ["playwright.config.ts", "playwright.config.js", "playwright.config.mjs"];
const SEARCH: [&str; 6] = ["tests/visual", "tests/ui", "tests/e2e", "e2e", "frontend", "."];
const IMAGE: &str = "mcr.microsoft.com/playwright";
const RUN_SH: &str = "run.sh";
const SRC: &str = "/src";
const DEFAULT_ARGV: &str = "npm ci --no-audit --no-fund && npx playwright test $UI_SPECS";

pub fn playwright_dir(tree: &Path, declared: Option<&str>) -> Option<String> {
    let has = |d: &str| CONFIGS.iter().any(|c| tree.join(d).join(c).is_file());
    match declared {
        Some(d) => has(d).then(|| d.to_string()),
        None => SEARCH.iter().find(|d| has(d)).map(|d| d.to_string()),
    }
}

fn numeric_version(s: &str) -> Option<String> {
    let v = s.trim().trim_start_matches(['^', '~', '=', 'v', ' ']);
    let ok = !v.is_empty() && v.split('.').count() == 3 && v.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    ok.then(|| v.to_string())
}

/// The installed version from `package-lock.json`, else the exact version `package.json` names.
pub fn playwright_version(dir: &Path) -> Option<String> {
    let json = |f: &str| std::fs::read_to_string(dir.join(f)).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    if let Some(v) = json("package-lock.json")
        .and_then(|l| l["packages"]["node_modules/@playwright/test"]["version"].as_str().and_then(numeric_version))
    {
        return Some(v);
    }
    let pkg = json("package.json")?;
    ["devDependencies", "dependencies"].iter().find_map(|k| pkg[*k]["@playwright/test"].as_str().and_then(numeric_version))
}

pub fn image_for(ui: Option<&Ui>, version: Option<&str>) -> Result<String, String> {
    if let Some(i) = ui.and_then(|u| u.image.clone()) {
        return Ok(i);
    }
    version
        .map(|v| format!("{IMAGE}:v{v}-noble"))
        .ok_or_else(|| "ui: cannot pin the Playwright image: no @playwright/test version in package-lock.json or package.json".into())
}

pub fn argv_for(ui: Option<&Ui>, tree: &Path, dir: &str) -> Vec<String> {
    match ui.filter(|u| !u.argv.is_empty()) {
        Some(u) => u.argv.clone(),
        None if tree.join(dir).join(RUN_SH).is_file() => vec!["bash".into(), "-c".into(), format!("cd {dir} && bash {RUN_SH} $UI_SPECS")],
        None => vec!["bash".into(), "-c".into(), format!("cd {dir} && {DEFAULT_ARGV}")],
    }
}

/// Runs `argv` from the tree's root.
pub fn in_tree(argv: Vec<String>) -> Vec<String> {
    let mut v: Vec<String> = ["bash", "-c", &format!("cd {SRC} && exec \"$@\""), "ui"].iter().map(|s| s.to_string()).collect();
    v.extend(argv);
    v
}

fn plausible_spec(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '[' | ']' | '*'))
}

pub fn main(args: &[String], run: &Path, resolve: &dyn Fn(&str) -> Option<PathBuf>, rt: Option<&dyn ContainerRuntime>) -> i32 {
    let fault = |m: String| {
        eprintln!("{m}");
        RC_FAULT
    };
    let Some(target) = args.first() else { return fault(USAGE.into()) };
    let specs = &args[1..];
    if let Some(s) = specs.iter().find(|s| !plausible_spec(s)) {
        return fault(format!("ui: spec {s:?} is not a plain path or name"));
    }
    let tree = if Path::new(target).is_dir() { PathBuf::from(target) } else { resolve(target).unwrap_or_default() };
    let Ok(tree) = tree.canonicalize() else { return fault(format!("ui: {target} is neither a directory nor a known repository")) };
    let decl = std::fs::read_to_string(tree.join(DECL)).ok().and_then(|t| Decl::parse(&t).ok());
    let ui = decl.as_ref().and_then(|d| d.ui.as_ref());
    let Some(dir) = playwright_dir(&tree, ui.and_then(|u| u.dir.as_deref())) else {
        return fault(format!("ui: no playwright.config.* under {} (declare [ui] dir in {DECL})", tree.display()));
    };
    let image = match image_for(ui, playwright_version(&tree.join(&dir)).as_deref()) {
        Ok(i) => i,
        Err(e) => return fault(e),
    };
    let Some(rt) = rt else { return fault("ui: cannot find the harness (SPIRA_TESTENV_HARNESS)".into()) };
    let log_dir = run.join("ui");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        return fault(format!("ui: cannot create {}: {e}", log_dir.display()));
    }
    let name = format!("testenv-ui-{}", std::process::id());
    let log = log_dir.join(format!("{name}.log"));
    let scratch = log_dir.join(&name);
    if std::fs::create_dir_all(scratch.join("upper")).and_then(|_| std::fs::create_dir_all(scratch.join("work"))).is_err() {
        return fault(format!("ui: cannot create {}", scratch.display()));
    }
    let req = RunRequest {
        name: name.clone(),
        image,
        mount: scratch.clone(),
        workdir: "/work".into(),
        overlays: vec![Overlay { lower: tree.clone(), upper: scratch.join("upper"), work: scratch.join("work"), ctr: SRC.into() }],
        env: vec![("UI_SPECS".into(), specs.join(" ")), ("UI_DIR".into(), dir.clone()), ("CI".into(), "1".into())],
        argv: in_tree(argv_for(ui, &tree, &dir)),
        timeout: Some(Duration::from_secs(ui.and_then(|u| u.timeout).or(decl.as_ref().and_then(|d| d.timeout)).unwrap_or(1800))),
        output: log.clone(),
        ..RunRequest::default()
    };
    eprintln!("ui: {} in {} ({})", if specs.is_empty() { "all specs".into() } else { specs.join(" ") }, req.image, log.display());
    let o = rt.run_once(&req);
    rt.purge(&name);
    let _ = crate::foreign::remove_run_dir(&scratch);
    print!("{}", std::fs::read_to_string(&log).unwrap_or_default());
    o.rc
}

#[cfg(test)]
mod tests;
