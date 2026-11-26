//! `acceptance::RealHost` at its boundary: a real subprocess.

#[macro_use]
extern crate spira_test_id;

use release::acceptance::{Cmd, Host, RealHost};

#[test_id(tdh4rwf)]
#[test]
fn a_commands_stdin_reaches_the_program_even_past_a_pipe_buffer() {
    let body = "a line of a bead body\n".repeat(20_000);
    let out = RealHost.run(&Cmd::new("cat").stdin(body.clone()));
    assert_eq!((out.rc, out.out == body), (0, true));
}

#[test_id(ttaiwzc)]
#[test]
fn a_command_with_no_stdin_reads_an_empty_one() {
    let out = RealHost.run(&Cmd::new("cat"));
    assert_eq!((out.rc, out.out.as_str()), (0, ""));
}
