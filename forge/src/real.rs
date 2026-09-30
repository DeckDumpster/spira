//! Production `Gh`/`Proc`: exec `timeout $GH_TIMEOUT $SPIRA_GH <args...>`, exactly the two
//! processes bash's `ghq()` launched (`timeout "${GH_TIMEOUT:-120}" "${SPIRA_GH:-gh}" "$@"`).
//! The credential path is whatever `$SPIRA_GH` is — this never reads a token itself.

use crate::ports::{Gh, GhOut, Proc};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub struct RealGh;

fn gh_timeout() -> String {
    std::env::var("GH_TIMEOUT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "120".into())
}

fn spira_gh() -> String {
    std::env::var("SPIRA_GH").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "gh".into())
}

fn run(repo: Option<&Path>, args: &[&str], input: Option<&[u8]>, merge_stderr: bool) -> GhOut {
    let mut c = Command::new("timeout");
    c.arg(gh_timeout()).arg(spira_gh()).args(args);
    if let Some(r) = repo {
        c.current_dir(r);
    }
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    c.stdout(Stdio::piped());
    c.stderr(if merge_stderr { Stdio::piped() } else { Stdio::null() });
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(_) => return GhOut { code: -1, stdout: Vec::new() },
    };
    if let Some(bytes) = input {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(bytes);
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return GhOut { code: -1, stdout: Vec::new() },
    };
    let mut stdout = out.stdout;
    if merge_stderr {
        stdout.extend_from_slice(&out.stderr);
    }
    GhOut { code: out.status.code().unwrap_or(-1), stdout }
}

impl Gh for RealGh {
    fn call(&self, repo: Option<&Path>, args: &[&str]) -> GhOut {
        run(repo, args, None, false)
    }
    fn call_merged(&self, repo: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> GhOut {
        run(repo, args, input, true)
    }
}

pub struct RealProc;

impl Proc for RealProc {
    fn run(&self, program: &str, args: &[&str], input: Option<&[u8]>) -> (i32, Vec<u8>) {
        let mut c = Command::new(program);
        c.args(args);
        c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
        c.stdout(Stdio::piped());
        c.stderr(Stdio::null());
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(_) => return (-1, Vec::new()),
        };
        if let Some(bytes) = input {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(bytes);
            }
        }
        match child.wait_with_output() {
            Ok(o) => (o.status.code().unwrap_or(-1), o.stdout),
            Err(_) => (-1, Vec::new()),
        }
    }
}
