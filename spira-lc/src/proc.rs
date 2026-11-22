//! The one door through which spira-lc starts a process. lc-serve starts none: the door refuses
//! there, and spira-lint's `lc-serve-no-spawn` refuses a start anywhere else.

pub(crate) use std::process::Command;

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
    bounded_when(crate::plan::serving(), program)
}

/// A command bounded to `secs` seconds; refused inside lc-serve.
pub(crate) fn bounded_for(program: impl AsRef<std::ffi::OsStr>, secs: &str) -> Result<Bounded, String> {
    bounded_for_when(crate::plan::serving(), program, secs)
}

fn bounded_when(serving: bool, program: impl AsRef<std::ffi::OsStr>) -> Result<Bounded, String> {
    refusal(serving, &program.as_ref().to_string_lossy())?;
    Ok(spira_config::bounded::bounded(program))
}

fn bounded_for_when(serving: bool, program: impl AsRef<std::ffi::OsStr>, secs: &str) -> Result<Bounded, String> {
    refusal(serving, &program.as_ref().to_string_lossy())?;
    Ok(spira_config::bounded::bounded_for(program, secs))
}

fn command_when(serving: bool, program: impl AsRef<std::ffi::OsStr>) -> Result<Command, String> {
    refusal(serving, &program.as_ref().to_string_lossy())?;
    Ok(Command::new(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(ttgjohr)]
    #[test]
    fn serving_refuses_a_process_naming_the_program() {
        let e = command_when(true, "git").unwrap_err();
        assert_eq!(e, "lc-serve starts no process: git");
    }

    #[test_id(toevcvi)]
    #[test]
    fn not_serving_builds_the_command() {
        assert_eq!(command_when(false, "git").unwrap().get_program(), "git");
    }

    #[test_id(t6wrxu8)]
    #[test]
    fn the_bounded_forms_refuse_while_serving_and_build_otherwise() {
        assert_eq!(bounded_when(true, "git").err().unwrap(), "lc-serve starts no process: git");
        assert_eq!(bounded_for_when(true, "git", "5").err().unwrap(), "lc-serve starts no process: git");
        assert!(bounded_when(false, "git").is_ok() && bounded_for_when(false, "git", "5").is_ok());
        assert!(!crate::plan::serving(), "the public doors read the process's own serving flag");
        assert!(command("git").is_ok() && bounded("git").is_ok() && bounded_for("git", "5").is_ok());
    }
}
