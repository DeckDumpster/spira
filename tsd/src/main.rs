// tsd-write — the IO seam: appends one row to a run/tsd/ family file under an exclusive
// flock, so concurrent producers (suites racing in a batch, aeons on different hosts) never
// interleave partial lines. The row format itself lives in lib.rs and is tested there without
// touching a filesystem.
//
// usage: tsd-write --family <name> [--root <dir>] [--host <id>] [--ts <iso8601>]
//                   [--field key=value ...] [--field-str key=value ...]
//
//   --root defaults to $SPIRA_RUN.
//   --host defaults to this machine's hostname (law-producers-stamp-their-own-clock: the
//          producer's own id, not the query layer's).
//   --ts   defaults to now, UTC, second precision — the producer's own clock stamp.
//   --field     JSON-sniffs its value (numbers, bools stay typed).
//   --field-str keeps its value a string regardless of shape (a SHA, an id).

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
const LOCK_EX: i32 = 2;
const LOCK_UN: i32 = 8;

fn usage() -> &'static str {
    "usage: tsd-write --family <name> [--root <dir>] [--host <id>] [--ts <iso8601>] \
     [--field key=value ...] [--field-str key=value ...]"
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tsd-write: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut family: Option<String> = None;
    let mut root: Option<PathBuf> = None;
    let mut host: Option<String> = None;
    let mut ts: Option<String> = None;
    let mut fields: Vec<(String, serde_json::Value)> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let next = |i: &mut usize| -> Result<&String, String> {
            *i += 1;
            args.get(*i).ok_or_else(|| usage().to_string())
        };
        match args[i].as_str() {
            "--family" => family = Some(next(&mut i)?.clone()),
            "--root" => root = Some(PathBuf::from(next(&mut i)?)),
            "--host" => host = Some(next(&mut i)?.clone()),
            "--ts" => ts = Some(next(&mut i)?.clone()),
            "--field" => fields.push(tsd::parse_field(next(&mut i)?)?),
            "--field-str" => fields.push(tsd::parse_field_str(next(&mut i)?)?),
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
        i += 1;
    }

    let family = family.ok_or_else(|| usage().to_string())?;
    if !tsd::valid_family(&family) {
        return Err(format!(
            "family {family:?} must start with a letter and contain only lowercase letters, digits and hyphens"
        ));
    }
    let root = match root.or_else(|| env::var("SPIRA_RUN").ok().map(PathBuf::from)) {
        Some(r) => r,
        None => return Err("no --root and SPIRA_RUN is unset".to_string()),
    };
    let host = host.unwrap_or_else(default_host);
    let ts = ts.unwrap_or_else(now_iso);

    let line = tsd::build_row(&ts, &host, &family, &fields)?;
    let path = tsd::family_path(&root, &family);
    append_line(&path, &line)
}

fn default_host() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown-host".to_string())
}

fn now_iso() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = f.as_raw_fd();
    if unsafe { flock(fd, LOCK_EX) } != 0 {
        return Err(format!("{}: flock failed", path.display()));
    }
    let result = writeln!(f, "{line}").map_err(|e| format!("{}: {e}", path.display()));
    let _ = unsafe { flock(fd, LOCK_UN) };
    result
}
