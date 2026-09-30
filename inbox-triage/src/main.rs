//! `inbox-triage` — the concierge's in-session Monitor over `SPIRA_CONCIERGE_INBOX`. See
//! `DESIGN.md`. Replaces `spira/inbox-triage.sh` (sp-48f6g).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Strips the leading UTC timestamp token the same way the bash's `${line#* }` did: up to
/// and including the first space.
fn strip_timestamp(line: &str) -> &str {
    match line.find(' ') {
        Some(i) => &line[i + 1..],
        None => "",
    }
}

/// The three DROP patterns (`_wd`-adjacent bash `case`): echoes of the concierge's own
/// actions and purely informational events.
fn should_drop(body: &str) -> bool {
    body.contains("ROUND RESULT") || body.contains(" OPENED ") || body.contains("pool: NEW CERTIFIED")
}

fn dedup_key_re() -> &'static (regex::Regex, regex::Regex) {
    use std::sync::OnceLock;
    static RE: OnceLock<(regex::Regex, regex::Regex)> = OnceLock::new();
    RE.get_or_init(|| (regex::Regex::new(r"\d{2}:\d{2}:\d{2}Z?").unwrap(), regex::Regex::new(r"oldest \d+s").unwrap()))
}

/// The dedup key: strip any `HH:MM:SS(Z?)` substring and normalise `oldest <n>s` to
/// `oldest Ns`, so the same standing condition with a different elapsed time still dedupes.
fn dedup_key(body: &str) -> String {
    let (time_re, oldest_re) = dedup_key_re();
    let step1 = time_re.replace_all(body, "");
    oldest_re.replace_all(&step1, "oldest Ns").into_owned()
}

/// Where THIS BINARY's own `conf.sh` lives — found by searching upward from the binary's
/// own directory for an ancestor whose `spira/conf.sh` exists. Same approach as `watchd`'s
/// `home_dir`, for the same two reasons: never `$SPIRA_HOME` (a caller's own config input,
/// which a fixture may point elsewhere for reasons that have nothing to do with where this
/// binary's own conf.sh lives — the bash's `inbox-triage.sh` never read it either, only its
/// own `BASH_SOURCE[0]`), and never a fixed parent count (a release's
/// `<release>/bin/inbox-triage` and a testenv/aeon-profile build's
/// `<checkout>/target/aeon/inbox-triage` put `spira/` a different number of levels up).
fn home_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| find_spira_dir(&p, |d| d.join("conf.sh").is_file())).unwrap_or_else(|| PathBuf::from("spira"))
}

fn find_spira_dir(exe: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut dir = exe.parent()?;
    loop {
        let candidate = dir.join("spira");
        if exists(&candidate) {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

const SCRIPT: &str = r#"set -uo pipefail
HERE="$1"; shift
. "$HERE/conf.sh" >/dev/null 2>&1 || exit 97
for v in "$@"; do printf '%s=%s\0' "$v" "${!v-}"; done
exit 0
"#;

struct Config {
    inbox: String,
    dedup: u64,
}

fn load_config(home: &Path) -> Result<Config, String> {
    let out = Command::new("bash")
        .arg("-c")
        .arg(SCRIPT)
        .arg("inbox-triage-context")
        .arg(home)
        .args(["SPIRA_CONCIERGE_INBOX", "SPIRA_CONCIERGE_INBOX_DEDUP"])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run bash: {e}"))?;
    if !out.status.success() {
        return Err(format!("inbox-triage: conf.sh could not be sourced from {} (exit {})", home.display(), out.status.code().unwrap_or(-1)));
    }
    let mut kv: HashMap<String, String> = HashMap::new();
    for rec in String::from_utf8_lossy(&out.stdout).split('\0') {
        if let Some((k, v)) = rec.split_once('=') {
            kv.insert(k.to_string(), v.to_string());
        }
    }
    Ok(Config {
        inbox: kv.remove("SPIRA_CONCIERGE_INBOX").unwrap_or_default(),
        dedup: kv.remove("SPIRA_CONCIERGE_INBOX_DEDUP").and_then(|s| s.parse().ok()).unwrap_or(600),
    })
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn main() {
    let cfg = match load_config(&home_dir()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let log = Path::new(&cfg.inbox);
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::OpenOptions::new().create(true).append(true).open(log).is_err() {
        eprintln!("inbox-triage: cannot create {}", log.display());
        std::process::exit(1);
    }

    let mut file = File::open(log).unwrap_or_else(|e| {
        eprintln!("inbox-triage: cannot open {}: {e}", log.display());
        std::process::exit(1);
    });
    // `tail -n 0 -F`: start from the current end, never replaying history — this is a live
    // Monitor, not a replay tool.
    let mut offset = file.metadata().map(|m| m.len()).unwrap_or(0);

    let mut last: HashMap<String, u64> = HashMap::new();
    let stdout = std::io::stdout();
    loop {
        if let Ok((new_offset, lines)) = read_new_lines(&mut file, offset) {
            offset = new_offset;
            let now = now_secs();
            let mut out = stdout.lock();
            for line in lines {
                let body = strip_timestamp(&line);
                if should_drop(body) {
                    continue;
                }
                let key = dedup_key(body);
                if let Some(&t) = last.get(&key) {
                    if now.saturating_sub(t) < cfg.dedup {
                        continue;
                    }
                }
                last.insert(key, now);
                let _ = writeln!(out, "{body}");
            }
            let _ = out.flush();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn read_new_lines(file: &mut File, offset: u64) -> std::io::Result<(u64, Vec<String>)> {
    use std::io::{Seek, SeekFrom};
    let len = file.metadata()?.len();
    if len < offset {
        file.seek(SeekFrom::Start(0))?;
        return Ok((0, Vec::new()));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let mut lines = Vec::new();
    let mut consumed = 0u64;
    let mut rest = &buf[..];
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
        lines.push(String::from_utf8_lossy(&rest[..nl]).into_owned());
        consumed += nl as u64 + 1;
        rest = &rest[nl + 1..];
    }
    Ok((offset + consumed, lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn has(dirs: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = dirs.iter().map(PathBuf::from).collect();
        move |p: &Path| set.contains(p)
    }

    #[test]
    fn a_release_layout_finds_spira_one_level_above_bin() {
        let exe = Path::new("/opt/spira/spira-releases/abc123/bin/inbox-triage");
        let exists = has(&["/opt/spira/spira-releases/abc123/spira"]);
        assert_eq!(find_spira_dir(exe, exists), Some(PathBuf::from("/opt/spira/spira-releases/abc123/spira")));
    }

    #[test]
    fn a_testenv_aeon_profile_build_finds_spira_two_levels_above_target_aeon() {
        let exe = Path::new("/workspace/target/aeon/inbox-triage");
        let exists = has(&["/workspace/spira"]);
        assert_eq!(find_spira_dir(exe, exists), Some(PathBuf::from("/workspace/spira")));
    }

    #[test]
    fn no_ancestor_with_a_spira_dir_is_none() {
        let exe = Path::new("/a/b/c/inbox-triage");
        assert_eq!(find_spira_dir(exe, |_| false), None);
    }

    #[test]
    fn strips_the_leading_timestamp_token() {
        assert_eq!(strip_timestamp("2026-09-30T00:00:00Z the rest of it"), "the rest of it");
        assert_eq!(strip_timestamp("no-space-at-all"), "");
    }

    #[test]
    fn drops_the_three_known_noise_patterns() {
        assert!(should_drop("pool: ROUND RESULT green"));
        assert!(should_drop("[watch:pr-notify] sp-abc12 OPENED against main"));
        assert!(should_drop("pool: NEW CERTIFIED sp-abc12"));
        assert!(!should_drop("[watch:round-duty] asks: NEW ASK sp-f63uj: something"));
    }

    #[test]
    fn dedup_key_strips_times_and_normalises_oldest() {
        let a = dedup_key("[watch:dolt] oldest 42s waiting on the store, seen at 11:34:05Z");
        let b = dedup_key("[watch:dolt] oldest 900s waiting on the store, seen at 11:40:12Z");
        assert_eq!(a, b, "the same standing condition dedupes despite different elapsed times");
    }

    #[test]
    fn dedup_key_leaves_unrelated_text_alone() {
        assert_eq!(dedup_key("plain text with no times in it"), "plain text with no times in it");
    }

    #[test]
    fn read_new_lines_only_returns_what_is_past_the_offset_and_complete() {
        let d = testkit::TempDir::new("inbox-triage");
        let p = d.join("log");
        std::fs::write(&p, "one\ntwo\n").unwrap();
        let mut f = File::open(&p).unwrap();
        let (off, lines) = read_new_lines(&mut f, 0).unwrap();
        assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"three\npart").unwrap();
        let (off2, lines2) = read_new_lines(&mut f, off).unwrap();
        assert_eq!(lines2, vec!["three".to_string()]);
        assert!(off2 > off);
    }
}
