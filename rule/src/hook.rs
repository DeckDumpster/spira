//! The wiki regeneration hook a statute write runs before it is in force.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// `hook` itself when it is an absolute path to an executable file; relative paths would
/// resolve against the caller's cwd.
pub fn check(hook: &str) -> Result<String, String> {
    if !Path::new(hook).is_absolute() {
        return Err(format!("hook '{hook}' is not an absolute path"));
    }
    let executable = std::fs::metadata(hook).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    if !executable {
        return Err(format!("hook '{hook}' is not executable"));
    }
    Ok(hook.to_string())
}

/// True only when the hook ran and exited 0.
pub fn synth(hook: &str) -> bool {
    // batch-job: this runs whatever its caller names, as long as that takes
    Command::new(hook).status().map(|s| s.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(dir: &Path, name: &str, body: &str, mode: u32) -> String {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p.to_string_lossy().into_owned()
    }

    // covers: UC-operator-channel-43
    #[test_id(tjnssi1)]
    #[test]
    fn a_statute_is_in_force_only_when_the_hook_ran_and_exited_zero() {
        let t = testkit::TempDir::new("rule-hook");
        let ok = script(t.path(), "ok", "#!/bin/sh\nexit 0\n", 0o755);
        let failing = script(t.path(), "failing", "#!/bin/sh\nexit 3\n", 0o755);
        assert!(synth(&ok));
        assert!(!synth(&failing));
        assert!(!synth(&t.path().join("missing").to_string_lossy()));
    }

    // covers: UC-operator-channel-43
    #[test_id(t960zcw)]
    #[test]
    fn a_missing_relative_or_non_executable_hook_is_refused_before_any_write() {
        let t = testkit::TempDir::new("rule-hook");
        let plain = script(t.path(), "plain", "#!/bin/sh\nexit 0\n", 0o644);
        let ok = script(t.path(), "ok", "#!/bin/sh\nexit 0\n", 0o755);
        assert_eq!(check(&ok), Ok(ok.clone()));
        assert!(check(&plain).unwrap_err().contains("not executable"));
        assert!(check(&t.path().join("missing").to_string_lossy()).unwrap_err().contains("not executable"));
        assert!(check("law-synth.sh").unwrap_err().contains("not an absolute path"));
        assert!(check(&t.path().to_string_lossy()).unwrap_err().contains("not executable"));
    }
}
