//! Whether any operator override is declared, read from the directory listing alone.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Whether `dir` holds a non-hidden `*.override` directly: `retired/` does not count, a missing
/// directory declares none, and an unreadable one is an error rather than a silent "none".
pub fn declared(dir: &Path) -> Result<bool, String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("overrides: cannot read {}: {e}", dir.display())),
    };
    for entry in entries {
        let name = entry.map_err(|e| format!("overrides: cannot read {}: {e}", dir.display()))?.file_name();
        if name.to_str().is_some_and(|n| !n.starts_with('.') && n.ends_with(".override")) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tuotrr9)]
    #[test]
    fn a_non_hidden_override_file_directly_in_the_directory_is_declared() {
        let d = testkit::TempDir::new("ov-declared");
        assert_eq!(declared(d.path()), Ok(false), "an empty directory");
        fs::write(d.join("note.txt"), "").unwrap();
        fs::write(d.join(".hidden.override"), "").unwrap();
        assert_eq!(declared(d.path()), Ok(false), "neither a stray file nor a hidden one is a spec");
        fs::write(d.join("hold.override"), "").unwrap();
        assert_eq!(declared(d.path()), Ok(true));
    }

    #[test_id(teiy0jv)]
    #[test]
    fn a_retired_override_is_not_declared() {
        let d = testkit::TempDir::new("ov-retired");
        fs::create_dir_all(d.join("retired")).unwrap();
        fs::write(d.join("retired/old.override"), "").unwrap();
        assert_eq!(declared(d.path()), Ok(false));
    }

    #[test_id(t0cgfq4)]
    #[test]
    fn a_missing_directory_declares_none() {
        let d = testkit::TempDir::new("ov-missing");
        assert_eq!(declared(&d.join("absent")), Ok(false));
    }

    #[test_id(tyacqst)]
    #[test]
    fn an_unreadable_directory_is_an_error_not_none() {
        let d = testkit::TempDir::new("ov-unreadable");
        fs::write(d.join("not-a-dir"), "").unwrap();
        let got = declared(&d.join("not-a-dir"));
        assert!(got.is_err(), "{got:?}");
    }
}
}
