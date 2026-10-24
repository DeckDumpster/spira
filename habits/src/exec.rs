use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub enum Ran {
    Out { stdout: String, stderr: String },
    Missing,
    Timeout,
}

fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = r.read_to_end(&mut b);
        String::from_utf8_lossy(&b).into_owned()
    })
}

/// Runs `argv` in its own process group, killing the group when `budget` lapses.
pub fn run(argv: &[String], cwd: &Path, budget: Duration) -> Ran {
    let Some((prog, args)) = argv.split_first() else { return Ran::Missing };
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let Ok(mut child) = cmd.spawn() else { return Ran::Missing };
    let (out, err) = (drain(child.stdout.take().unwrap()), drain(child.stderr.take().unwrap()));
    let deadline = Instant::now() + budget;
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if Instant::now() >= deadline => {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                break true;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break true,
        }
    };
    let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if timed_out {
        Ran::Timeout
    } else {
        Ran::Out { stdout, stderr }
    }
}
