//! `testenv owed <tip> <repo>`: the declared tiers a tip's diff against the landing base obliges
//! it to have passed, one per line. A repository declares them as `paths` on a tier in
//! `.spira/testenv.toml`; one that declares none owes nothing.

use std::path::PathBuf;
use std::process::Command;

use crate::foreign::{Decl, DECL};

pub const USAGE: &str = "usage: testenv owed <40-hex tip> <repo-name>";
const RC_FAULT: i32 = 2;

/// The tiers `decl_text` owes for `changed` (a `git diff --name-only` listing).
pub fn owed_for(decl_text: Option<&str>, changed: &str) -> Result<Vec<String>, String> {
    let Some(text) = decl_text else { return Ok(Vec::new()) };
    let decl = Decl::parse(text)?;
    let paths: Vec<&str> = changed.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    Ok(decl.owed(&paths))
}

fn git(repo: &std::path::Path, args: &[&str]) -> Option<String> {
    let o = Command::new("timeout").arg("5").arg("git").arg("-C").arg(repo).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn main(args: &[String], resolve: &dyn Fn(&str) -> Option<(PathBuf, String)>) -> i32 {
    let fault = |m: String| {
        eprintln!("{m}");
        RC_FAULT
    };
    let (Some(tip), Some(name)) = (args.first(), args.get(1)) else { return fault(USAGE.into()) };
    if tip.len() != 40 || !tip.bytes().all(|b| b.is_ascii_hexdigit()) {
        return fault(format!("owed: {tip}: not a full 40-hex commit name"));
    }
    let Some((repo, base)) = resolve(name) else { return fault(format!("owed: unknown repository {name}")) };
    let decl = git(&repo, &["show", &format!("{tip}:{DECL}")]);
    if decl.is_none() {
        return 0;
    }
    let Some(changed) = crate::shots::changed_paths(&repo, tip, &[&base]) else {
        return fault(format!("owed: cannot diff {tip} against {base:?} in {}", repo.display()));
    };
    match owed_for(decl.as_deref(), &changed) {
        Ok(tiers) => {
            for t in tiers {
                println!("{t}");
            }
            0
        }
        Err(e) => fault(format!("owed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECL_TEXT: &str = r#"
image = "i"
[tier.rust]
argv = ["x"]
paths = ["crates/", "Cargo.toml", "*.rs"]
[tier.frontend]
argv = ["x"]
paths = ["frontend/"]
[tier.lint]
argv = ["x"]
"#;

    #[test]
    fn a_frontend_diff_owes_the_frontend_tier_and_not_the_rust_one() {
        assert_eq!(owed_for(Some(DECL_TEXT), "frontend/src/a.svelte\n").unwrap(), ["frontend"]);
    }

    #[test]
    fn a_backend_only_diff_does_not_owe_the_frontend_tier() {
        assert_eq!(owed_for(Some(DECL_TEXT), "crates/db/src/lib.rs\n").unwrap(), ["rust"]);
        assert!(owed_for(Some(DECL_TEXT), "README.md\n").unwrap().is_empty(), "a tier with no paths is never owed");
    }

    #[test]
    fn no_declaration_owes_nothing_and_a_bad_path_is_refused() {
        assert!(owed_for(None, "frontend/a\n").unwrap().is_empty());
        assert!(owed_for(Some("image=\"i\"\n[tier.a]\nargv=[\"x\"]\npaths=[\"../x\"]\n"), "x").is_err());
    }
}
