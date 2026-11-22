//! The one door through which spira-lc starts a process. lc-serve starts none: the door refuses
//! there, and spira-lint's `lc-serve-no-spawn` refuses a start anywhere else.

use std::process::Command;

use spira_config::bounded::Bounded;

fn refusal(serving: bool, program: &str) -> Result<(), String> {
    if serving {
        Err(format!("lc-serve starts no process: {program}"))
    } else {
        Ok(())
    }
}

/// A plain command for `program`; refused inside lc-serve.
pub(crate) fn command(program: impl AsRef<std::ffi::OsStr>) -> Result<Command, String> {
    command_when(crate::plan::serving(), program)
}

/// A command under the harness's default bound; refused inside lc-serve.
pub(crate) fn bounded(program: impl AsRef<std::ffi::OsStr>) -> Result<Bounded, String> {
    refusal(crate::plan::serving(), &program.as_ref().to_string_lossy())?;
    Ok(spira_config::bounded::bounded(program))
}

/// A command bounded to `secs` seconds; refused inside lc-serve.
pub(crate) fn bounded_for(program: impl AsRef<std::ffi::OsStr>, secs: &str) -> Result<Bounded, String> {
    refusal(crate::plan::serving(), &program.as_ref().to_string_lossy())?;
    Ok(spira_config::bounded::bounded_for(program, secs))
}

fn command_when(serving: bool, program: impl AsRef<std::ffi::OsStr>) -> Result<Command, String> {
    refusal(serving, &program.as_ref().to_string_lossy())?;
    Ok(Command::new(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serving_refuses_a_process_naming_the_program() {
        let e = command_when(true, "git").unwrap_err();
        assert_eq!(e, "lc-serve starts no process: git");
    }

    #[test]
    fn not_serving_builds_the_command() {
        assert_eq!(command_when(false, "git").unwrap().get_program(), "git");
    }

    #[test]
    fn the_bounded_forms_refuse_only_while_serving() {
        assert!(bounded("git").is_ok() && bounded_for("git", "5").is_ok());
        assert!(!crate::plan::serving());
    }
}
