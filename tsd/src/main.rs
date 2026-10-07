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
     [--field key=value ...] [--field-str key=value ...]\n   or: tsd-write escape <member> <suite> <class> [batch_id]"
}

/// `_tsd_escape`'s own four classes (wave4-decomposition.md row AC, wave 4.35, sp-kelr2;
/// originally sp-6vd2s) — the whitelist is the guarantee, same reason
/// `_tsd_round_phase`'s now-retired one existed: no other class belongs in this family.
const ESCAPE_CLASSES: &[&str] = &["mapping_gap", "gate_gap", "environment_gap", "flake"];

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = if args.first().map(String::as_str) == Some("escape") { run_escape(&args[1..]) } else { run(&args) };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tsd-write: {e}");
            ExitCode::from(2)
        }
    }
}

/// `tsd-write escape <member> <suite> <class> [batch_id]` — `_tsd_escape`'s own door: one
/// escape-family row, classified per sp-6vd2s (escape-classify.sh, the one live caller).
/// A class outside [`ESCAPE_CLASSES`] is a silent no-op, `Ok(())` rather than a refusal,
/// matching the bash's own `case ... *) return 0 ;; esac` — missing arguments default to
/// an empty string the same way a bash positional does, so a short call just fails the
/// whitelist rather than erroring. `SPIRA_RUN` unset behaves exactly as `"${SPIRA_RUN:-}"`
/// did: an empty root, never a refusal — this call is best-effort end to end, and the
/// `lib.sh` shim above it already swallows any error this returns. `tsd` cannot depend on
/// `spira-config` (`spira-config` itself depends on `tsd`, to write its own admission
/// rows — cfg() is unavailable here on pain of a cycle), so this is a raw env read, not
/// the one door; the caller shell already resolved `$SPIRA_RUN` before exec'ing this.
fn run_escape(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(env::var("SPIRA_RUN").unwrap_or_default());
    escape_row(
        &root,
        &args.first().cloned().unwrap_or_default(),
        &args.get(1).cloned().unwrap_or_default(),
        &args.get(2).cloned().unwrap_or_default(),
        &args.get(3).cloned().unwrap_or_default(),
    )
}

/// The testable half of [`run_escape`] — `root` passed explicitly rather than read from
/// `$SPIRA_RUN`, so a test never has to mutate that process-global env var.
fn escape_row(root: &Path, member: &str, suite: &str, class: &str, batch_id: &str) -> Result<(), String> {
    if !ESCAPE_CLASSES.contains(&class) {
        return Ok(());
    }
    let fields = vec![
        tsd::parse_field_str(&format!("member={member}"))?,
        tsd::parse_field_str(&format!("suite={suite}"))?,
        tsd::parse_field_str(&format!("class={class}"))?,
        tsd::parse_field_str(&format!("batch_id={batch_id}"))?,
    ];
    let line = tsd::build_row(&now_iso(), &default_host(), "escape", &fields)?;
    append_line(&tsd::family_path(root, "escape"), &line)
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
    // `tsd` stays a leaf library/binary (`spira-config` depends on it, so it cannot depend
    // back on `spira-config` without a cycle) — SPIRA_RUN is a registered config key, but
    // this binary cannot reach the one door for it, so a raw env read is this crate's own,
    // explicit exception: callers that have it resolved should prefer passing `--root`.
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
    Command::new("timeout").arg("5").arg("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// A row is one whole line: a JSON object with no newline inside it. Refused before any byte
/// reaches the file, so a malformed row can never be the torn line a reader trips over.
fn check_line(line: &str) -> Result<(), String> {
    if line.contains('\n') || line.contains('\r') {
        return Err("row contains a line break".to_string());
    }
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(serde_json::Value::Object(_)) => Ok(()),
        Ok(_) => Err("row is not a JSON object".to_string()),
        Err(e) => Err(format!("row is not valid JSON: {e}")),
    }
}

/// True when the file's last byte is not a newline — an earlier writer died mid-line.
fn tail_is_torn(f: &mut fs::File) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(false);
    }
    f.seek(SeekFrom::Start(len - 1))?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    Ok(b[0] != b'\n')
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    check_line(line).map_err(|e| format!("{}: refused: {e}", path.display()))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = f.as_raw_fd();
    if unsafe { flock(fd, LOCK_EX) } != 0 {
        return Err(format!("{}: flock failed", path.display()));
    }
    let result = (|| {
        let mut buf = Vec::with_capacity(line.len() + 2);
        if tail_is_torn(&mut f)? {
            buf.push(b'\n');
        }
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        f.write_all(&buf)
    })()
    .map_err(|e| format!("{}: {e}", path.display()));
    let _ = unsafe { flock(fd, LOCK_UN) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_torn_or_multiline_row_is_refused_and_writes_nothing() {
        let dir = testkit::TempDir::new("tsd-torn-refused");
        let path = dir.join("tsd").join("f.jsonl");
        for bad in ["{\"ts\":\"2026", "{\"a\":1}\n{\"b\":2}", "[1]", "", "plain"] {
            assert!(append_line(&path, bad).is_err(), "{bad:?}");
        }
        assert!(!path.exists() || fs::read_to_string(&path).unwrap().is_empty());
        append_line(&path, "{\"a\":1}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
    }

    #[test]
    fn a_row_after_a_planted_torn_tail_is_not_fused_to_it() {
        let dir = testkit::TempDir::new("tsd-torn-tail");
        let path = dir.join("tsd").join("f.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{\"a\":1}\n{\"ts\":\"20").unwrap();
        append_line(&path, "{\"b\":2}").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let last = text.lines().last().unwrap();
        assert_eq!(last, "{\"b\":2}");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn escape_row_with_a_known_class_appends_one_row() {
        let dir = testkit::TempDir::new("tsd-escape");
        escape_row(&dir, "gate", "test-foo", "flake", "b1").unwrap();
        let text = fs::read_to_string(dir.join("tsd").join("escape.jsonl")).unwrap();
        let row: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(row["family"], "escape");
        assert_eq!(row["member"], "gate");
        assert_eq!(row["suite"], "test-foo");
        assert_eq!(row["class"], "flake");
        assert_eq!(row["batch_id"], "b1");
    }

    #[test]
    fn escape_row_with_an_unknown_class_is_a_silent_no_op() {
        let dir = testkit::TempDir::new("tsd-escape-unknown");
        escape_row(&dir, "gate", "test-foo", "not-a-real-class", "").unwrap();
        assert!(!dir.join("tsd").join("escape.jsonl").exists());
    }

    #[test]
    fn escape_row_with_missing_batch_id_still_writes_an_empty_string_field() {
        let dir = testkit::TempDir::new("tsd-escape-nobatch");
        escape_row(&dir, "gate", "test-foo", "mapping_gap", "").unwrap();
        let text = fs::read_to_string(dir.join("tsd").join("escape.jsonl")).unwrap();
        let row: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(row["batch_id"], "");
    }
}
