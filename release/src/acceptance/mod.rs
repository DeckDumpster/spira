//! `release acceptance` (DESIGN.md "acceptance"): release acceptance on a clean machine.
//! Fresh install from the release tarball, land a bead end to end, uninstall; with a
//! predecessor, also upgrade, rollback, and an aged-install upgrade. Replaces
//! `spira/acceptance-run.sh` and `spira/acceptance-lib.sh` (sp-ak7qm).
//!
//! Every check prints one `ok`/`FAIL` line and one JSON line; a check that cannot look is a
//! FAIL. The external world — `bd`, `git`, `gh`, `systemctl`, the release's own scripts —
//! is reached only through [`Host`], so the whole run is unit-tested against a fake.

pub mod host;
mod phases;
#[cfg(test)]
mod tests;

pub use host::{Cmd, Host, Out, RealHost};

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: release acceptance <tag> --scratch-repo <path> [--prev-tag <tag>] [--record]
                          [--notes-repo <path>] [--file-defects] [--bd-db <path>] [--agent <path>]
                          [--waive-upgrade] [--tarball <path>] [--prev-tarball <path>]";

/// The binaries every release must ship executable in `bin/` (phase A, and its positive control).
pub const RELEASE_BINS: &[&str] = &["loom", "panel", "broker", "spira-supervise", "landing-pass"];

/// Phase D's operator override: a REAL key conf.sh honours (an unknown one is refused, so it
/// would never be in force), whose non-default value changes nothing on an acceptance box.
pub const OVERRIDE_KEY: &str = "SPIRA_CHECK5_MAX_FILE";

/// The note line that records an upgrade waiver on the cut it was given for.
pub const WAIVER_LINE: &str = "upgrade phases waived by operator";

/// What the caller asked for (the command line), before anything is resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    pub tag: String,
    pub scratch_repo: PathBuf,
    pub prev_tag: Option<String>,
    pub record: bool,
    pub file_defects: bool,
    pub waive_upgrade: bool,
    pub bd_db: Option<PathBuf>,
    pub agent: Option<String>,
    pub tarball: Option<PathBuf>,
    pub prev_tarball: Option<PathBuf>,
    pub notes_repo: Option<PathBuf>,
}

/// Parse `release acceptance`'s own arguments (everything after `acceptance`). `Err` is a
/// usage error (exit 2). `--flag value` and `--flag=value` are both accepted.
pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut tag: Option<String> = None;
    let mut scratch: Option<PathBuf> = None;
    let mut i = 0;
    while i < argv.len() {
        let x = argv[i].as_str();
        let (name, inline) = match x.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (x, None),
        };
        let mut val = || -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            i += 1;
            argv.get(i).cloned().ok_or_else(|| format!("{name} needs a value"))
        };
        match name {
            "--scratch-repo" => scratch = Some(val()?.into()),
            "--tarball" => a.tarball = Some(val()?.into()),
            "--prev-tarball" => a.prev_tarball = Some(val()?.into()),
            "--prev-tag" => a.prev_tag = Some(val()?).filter(|s| !s.is_empty()),
            "--bd-db" => a.bd_db = Some(val()?.into()),
            "--agent" => a.agent = Some(val()?).filter(|s| !s.is_empty()),
            "--notes-repo" => a.notes_repo = Some(val()?.into()),
            "--record" => a.record = true,
            "--file-defects" => a.file_defects = true,
            "--waive-upgrade" => a.waive_upgrade = true,
            "-h" | "--help" => return Err(String::new()),
            f if f.starts_with('-') => return Err(format!("unknown option: {f}")),
            p => {
                if tag.is_some() {
                    return Err("too many positional arguments".into());
                }
                tag = Some(p.to_string());
            }
        }
        i += 1;
    }
    a.tag = tag.filter(|t| !t.is_empty()).ok_or("a <tag> is required")?;
    a.scratch_repo = scratch.filter(|p| !p.as_os_str().is_empty()).ok_or("--scratch-repo is required")?;
    // The waiver overrides any --prev-tag: phases B/C/D skip via the same empty-prev path they
    // take when no predecessor exists.
    if a.waive_upgrade {
        a.prev_tag = None;
    }
    Ok(a)
}

/// Everything a run needs, resolved: the arguments plus where things live.
#[derive(Debug, Clone)]
pub struct Opts {
    pub a: Args,
    pub bd_db: PathBuf,
    /// Scratch space for this run: releases, run dir, release source, downloads.
    pub tmp: PathBuf,
    /// `${XDG_CONFIG_HOME:-$HOME/.config}` — the instance's config directory is its `spira/`.
    pub xdg_config: PathBuf,
    pub forensics: PathBuf,
    /// This binary: the `release` that runs `install-tarball` (DESIGN.md Decision 1).
    pub release_bin: PathBuf,
    /// The inherited `PATH`, the tail of the release's launcher PATH.
    pub base_path: String,
    pub spira_run: PathBuf,
    pub token_projects: PathBuf,
    pub gh_repo: Option<String>,
    pub notes_repo: Option<PathBuf>,
    pub summon_secs: u64,
    pub asset_wait_secs: u64,
    pub pid: u32,
}

impl Opts {
    pub fn releases(&self) -> PathBuf {
        self.tmp.join("releases")
    }
    pub fn release_src(&self) -> PathBuf {
        self.tmp.join("release-source")
    }
    pub fn conf(&self) -> PathBuf {
        self.xdg_config.join("spira/spira.conf")
    }
    pub fn scratch_name(&self) -> String {
        self.a.scratch_repo.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// The owner/repo `gh` addresses: the first of `SPIRA_FORGE_REPO`, `GH_REPO`,
/// `GITHUB_REPOSITORY`, else the GitHub `origin` of `repo` (the notes repo, else the cwd).
pub fn gh_repo_from(env_vals: [Option<String>; 3], origin_url: Option<String>) -> Option<String> {
    if let Some(r) = env_vals.into_iter().flatten().find(|s| !s.is_empty()) {
        return Some(r);
    }
    let u = origin_url?;
    let u = u.trim();
    let rest = u.strip_prefix("https://github.com/").or_else(|| u.strip_prefix("git@github.com:"))?;
    Some(rest.strip_suffix(".git").unwrap_or(rest).to_string())
}

/// Tally, JSON lines and first-fail snapshots — acceptance-lib.sh's `ok`/`bad`.
pub struct Run<'h> {
    pub h: &'h dyn Host,
    pub o: Opts,
    pub pass: u32,
    pub fail: u32,
    phase: String,
    phase_start: u64,
    snapped: bool,
    snaps: u32,
    run_start: u64,
    /// The phase A probe bead, for snapshots.
    probe: Option<String>,
}

pub fn json_line(phase: &str, check: &str, reason: Option<&str>, ts: &str, elapsed: u64) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let verdict = if reason.is_some() { "fail" } else { "ok" };
    let reason = reason.map(|r| format!(",\"reason\":{}", q(r))).unwrap_or_default();
    format!("{{\"phase\":{},\"check\":{},\"verdict\":\"{verdict}\"{reason},\"ts\":{},\"elapsed\":{elapsed}}}", q(phase), q(check), q(ts))
}

impl<'h> Run<'h> {
    pub fn new(h: &'h dyn Host, o: Opts) -> Run<'h> {
        let now = h.now();
        Run { h, o, pass: 0, fail: 0, phase: "prerequisites".into(), phase_start: now, snapped: false, snaps: 0, run_start: now, probe: None }
    }

    fn jsonl(&self, check: &str, reason: Option<&str>) {
        let now = self.h.now();
        let line = json_line(&self.phase, check, reason, &crate::fsutil::rfc3339(now), now.saturating_sub(self.phase_start));
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(self.o.forensics.join("checks.jsonl")) {
            let _ = writeln!(f, "{line}");
        }
    }

    pub fn phase(&mut self, name: &str) {
        self.phase = name.into();
        self.snapped = false;
        self.phase_start = self.h.now();
    }

    pub fn ok(&mut self, name: &str) {
        self.pass += 1;
        println!("  ok    {name}");
        self.jsonl(name, None);
    }

    pub fn bad(&mut self, name: &str, reason: &str) {
        self.fail += 1;
        println!("  FAIL  {name}: {reason}");
        self.jsonl(name, Some(reason));
        if !self.snapped {
            self.snapped = true;
            let label = format!("first-fail-{}", self.phase);
            self.snapshot(&label);
        }
    }

    pub fn check(&mut self, name: &str, good: bool, reason: impl FnOnce() -> String) {
        if good {
            self.ok(name)
        } else {
            let r = reason();
            self.bad(name, &r)
        }
    }

    pub fn is0(&mut self, name: &str, rc: i32) {
        self.check(name, rc == 0, || format!("exit {rc}"))
    }

    pub fn want(&mut self, name: &str, want: &str, got: &str) {
        self.check(name, got.contains(want), || format!("wanted [{want}] in [{got}]"))
    }

    pub fn is_same(&mut self, name: &str, want: &str, got: &str) {
        self.check(name, want == got, || format!("wanted [{want}] got [{got}]"))
    }

    /// Dump systemd/bd/run-dir/config/scratch-repo state under `<forensics>/NN-<label>`. Best-effort
    /// throughout: a forensics failure never changes the verdict.
    pub fn snapshot(&mut self, label: &str) {
        self.snaps += 1;
        let dir = self.o.forensics.join(format!("{:02}-{label}", self.snaps));
        if fs::create_dir_all(dir.join("journal")).is_err() {
            return;
        }
        let h = self.h;
        let put = |name: &str, c: Cmd| {
            let _ = fs::write(dir.join(name), h.run(&c).text);
        };
        let sc = |args: &[&str]| Cmd::new("systemctl").arg("--user").args(args.iter().copied());
        let since = crate::fsutil::rfc3339(self.run_start).replace('T', " ").replace('Z', " UTC");
        put("timers.txt", sc(&["list-timers", "--all"]));
        put("units.txt", sc(&["list-units", "spira-*", "--all"]));
        let units: Vec<String> = first_fields(&h.run(&sc(&["list-units", "spira-*", "--all", "--no-legend", "--plain"])).out);
        let mut status = String::new();
        for u in &units {
            status.push_str(&format!("\n=== {u} ===\n"));
            status.push_str(&h.run(&sc(&["status", u, "--no-pager", "-l"])).text);
            let j = Cmd::new("journalctl").args(["--user", "-u", u, &format!("--since={since}"), "--no-pager", "-l"]);
            let _ = fs::write(dir.join("journal").join(format!("{u}.txt")), h.run(&j).text);
        }
        let _ = fs::write(dir.join("unit-status.txt"), status);
        put("journal-full.txt", Cmd::new("journalctl").args(["--user", &format!("--since={since}"), "--no-pager", "-l"]));
        let db = self.o.bd_db.display().to_string();
        put("bd-list.json", Cmd::new("bd").args(["-C", &db, "list", "--all", "--json"]));
        put("bd-ready.json", Cmd::new("bd").args(["-C", &db, "ready", "--json"]));
        if let Some(id) = &self.probe {
            put("bd-probe.json", Cmd::new("bd").args(["-C", &db, "show", id, "--json"]));
        }
        let run = &self.o.spira_run;
        if run.is_dir() {
            put("run-listing.txt", Cmd::new("ls").arg("-laR").arg(run.display().to_string()));
            for f in files_named(run, |n| n.ends_with(".log") && !is_secret(n)) {
                if let Some(n) = f.file_name() {
                    let _ = fs::copy(&f, dir.join(n));
                }
            }
            for n in ["queue"] {
                let _ = fs::copy(run.join(n), dir.join(n));
            }
        }
        // The instance's whole config directory (its conf and repository map), file by file.
        if let Ok(rd) = fs::read_dir(self.o.xdg_config.join("spira")) {
            for e in rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && !is_secret(&e.file_name().to_string_lossy())) {
                let _ = fs::copy(e.path(), dir.join(e.file_name()));
            }
        }
        let repo = self.o.a.scratch_repo.display().to_string();
        put("scratch-log.txt", Cmd::new("git").args(["-C", &repo, "log", "--all", "--oneline"]));
        put("scratch-refs.txt", Cmd::new("git").args(["-C", &repo, "show-ref"]));
        put("ps.txt", Cmd::new("ps").args(["-ef", "--forest"]));
        put("free.txt", Cmd::new("free").arg("-m"));
        put("df.txt", Cmd::new("df").arg("-h"));
        println!("snapshot: {}", dir.display());
    }
}

/// A forensics snapshot is uploaded as an artifact, so a credential-named file is never collected.
pub fn is_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("credential") || n.contains("password") || n.contains("secret") || n.contains("token") || n.ends_with(".key") || n.ends_with(".pem")
}

/// The first whitespace-separated field of every non-empty line.
pub fn first_fields(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()
}

/// Every regular file under `root` (recursively) whose name satisfies `f`.
fn files_named(root: &Path, f: impl Fn(&str) -> bool + Copy) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => out.extend(files_named(&p, f)),
                Ok(t) if t.is_file() && f(&e.file_name().to_string_lossy()) => out.push(p),
                _ => {}
            }
        }
    }
    out
}

/// The comma-separated `bin/<name>` entries of [`RELEASE_BINS`] missing or not executable
/// under `release_dir`; empty when nothing is.
pub fn missing_release_bins(release_dir: &Path) -> String {
    RELEASE_BINS
        .iter()
        .filter(|b| !crate::fsutil::is_executable(&release_dir.join("bin").join(b)))
        .map(|b| format!("bin/{b}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The bead id from `bd create`'s "Created issue: <id>" line, past any warning printed ahead
/// of it (GH#2950). `None` when there is none.
pub fn extract_bead_id(out: &str) -> Option<String> {
    for line in out.lines() {
        if let Some(pos) = line.find("Created issue: ") {
            let rest = &line[pos + "Created issue: ".len()..];
            let id: String = rest.chars().take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-').collect();
            // <prefix>-<suffix>, both non-empty (the script's [a-z0-9]*-[a-z0-9]*).
            let id = id.split_once('-').filter(|(p, s)| !p.is_empty() && !s.is_empty()).map(|(p, s)| {
                let s: String = s.chars().take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect();
                format!("{p}-{s}")
            });
            if id.is_some() {
                return id;
            }
        }
    }
    None
}

/// `bd … --json` output as a list of objects: warning lines before the first line starting
/// with `[` or `{` are dropped; a single object is a one-element list. `None` when unparsable.
pub fn bd_json(out: &str) -> Option<Vec<serde_json::Value>> {
    let start = out.lines().position(|l| l.starts_with('[') || l.starts_with('{'))?;
    let body: Vec<&str> = out.lines().skip(start).collect();
    match serde_json::from_str::<serde_json::Value>(&body.join("\n")).ok()? {
        serde_json::Value::Array(v) => Some(v),
        v @ serde_json::Value::Object(_) => Some(vec![v]),
        _ => None,
    }
}

/// Whether `bd show <id> --json` says the aeon's close is recorded: status closed, or open
/// carrying `spira-submitted` (sp-qsona: only the landing pass closes a work bead).
/// Unreadable is not finished.
pub fn bead_finished(show_json: &str) -> bool {
    let Some(v) = bd_json(show_json) else { return false };
    let Some(r) = v.first() else { return false };
    let closed = r.get("status").and_then(|s| s.as_str()) == Some("closed");
    let submitted = r.get("labels").and_then(|l| l.as_array()).is_some_and(|l| l.iter().any(|x| x.as_str() == Some("spira-submitted")));
    closed || submitted
}

/// Whether a lifecycle history (`lifecycle_states`) shows the model's work handed off: it
/// reached SUBMITTED, or anything only a submitted bead can reach.
pub fn lifecycle_submitted(states: &[String]) -> bool {
    states.iter().any(|s| matches!(s.as_str(), "SUBMITTED" | "CERTIFIED" | "IN_DELIVERY" | "QUEUED" | "BATCHED" | "LANDED" | "DONE"))
}

/// Whether `id` is among the beads of a `bd ready --json` reply. Unreadable is not claimable.
pub fn ready_has(ready_json: &str, id: &str) -> bool {
    bd_json(ready_json).is_some_and(|v| v.iter().any(|b| b.get("id").and_then(|x| x.as_str()) == Some(id)))
}

/// The states a bead passed through per `spira-lc history <id>`: the first applied event's
/// `from_state`, then every applied event's `to_state`, consecutive repeats collapsed.
/// Unreadable is empty. A bead's row is created READY without an event, so READY is only
/// ever a `from_state`; and the store returns `applied` as the string "1" — reading only
/// `to_state` and only a numeric 1 made this empty for every real history (sp-6ka75).
pub fn lifecycle_states(history_json: &str) -> Vec<String> {
    fn push(out: &mut Vec<String>, s: &str) {
        if out.last().map(String::as_str) != Some(s) {
            out.push(s.to_string());
        }
    }
    let mut out: Vec<String> = Vec::new();
    for e in bd_json(history_json).unwrap_or_default() {
        let applied = e.get("applied").is_some_and(|a| {
            a.as_i64() == Some(1) || a.as_bool() == Some(true) || a.as_str().is_some_and(|x| x == "1" || x == "true")
        });
        if !applied {
            continue;
        }
        if let (true, Some(f)) = (out.is_empty(), e.get("from_state").and_then(|s| s.as_str())) {
            push(&mut out, f);
        }
        if let Some(s) = e.get("to_state").and_then(|s| s.as_str()) {
            push(&mut out, s);
        }
    }
    out
}

/// The states the probe bead's history must pass through, in order, for its repository's
/// land mode: the queue modes add the delivery machine's QUEUED and BATCHED.
pub fn expected_lifecycle(mode: &str) -> Vec<&'static str> {
    let mut v = vec!["READY", "WORKING", "SUBMITTED", "CERTIFIED"];
    if is_queue_mode(mode) {
        v.extend(["IN_DELIVERY", "QUEUED", "BATCHED"]);
    }
    v.push("LANDED");
    v
}

/// The first of `want` that is not found, in order, in `got`; `None` when it all is.
pub fn missing_in_order<'a>(want: &[&'a str], got: &[String]) -> Option<&'a str> {
    let mut it = got.iter();
    want.iter().copied().find(|w| !it.any(|g| g == w))
}

pub fn is_queue_mode(mode: &str) -> bool {
    matches!(mode, "queue" | "queue.forge" | "queue.local")
}

/// The three land modes every release proves, with queue satisfied by either spelling.
pub const LAND_MODES: [&str; 3] = ["queue", "pr", "push"];

/// The land modes of `LAND_MODES` that no repository in `modes` carries.
pub fn land_modes_missing(modes: &[String]) -> Vec<&'static str> {
    LAND_MODES.iter().copied().filter(|m| !modes.iter().any(|x| if *m == "queue" { is_queue_mode(x) } else { x == m })).collect()
}

/// The installed `spira-*` unit files, "name state" per line, sorted; transient units
/// excluded (a landing pass alive at snapshot time is not part of an install).
pub fn unit_set(list_unit_files: &str) -> Vec<String> {
    let mut v: Vec<String> = list_unit_files
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (n, s) = (f.next()?, f.next().unwrap_or(""));
            (n.starts_with("spira-") && s != "transient").then(|| format!("{n} {s}"))
        })
        .collect();
    v.sort();
    v
}

/// The value of the first `KEY = value` line for `key` in a spira.conf text.
pub fn conf_line_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim_start().to_string())
    })
}

/// The acceptance note recorded on `refs/tags/<tag>` (DESIGN.md "acceptance", Schema).
pub struct Note<'a> {
    pub verdict: &'a str,
    pub date: &'a str,
    pub tag: &'a str,
    pub pass: u32,
    pub fail: u32,
    pub tarball: Option<(&'a str, &'a str)>,
    pub prev_tag: Option<&'a str>,
    pub waived: bool,
}

impl Note<'_> {
    pub fn text(&self) -> String {
        let mut s = format!("{}\n{} {}  {} passed, {} failed", self.verdict, self.date, self.tag, self.pass, self.fail);
        if let Some((sha, name)) = self.tarball {
            s.push_str(&format!("\nsha256:{sha} tarball:{name}"));
        }
        if let Some(p) = self.prev_tag {
            s.push_str(&format!("\naged-install from={p}: {}", self.verdict));
        }
        if self.waived {
            s.push_str(&format!("\n{WAIVER_LINE}"));
        }
        s
    }
}

/// `release acceptance <argv>`: resolve, run, return the exit code (0 PASS, 1 FAIL, 2 usage
/// or prerequisite).
pub fn main(argv: &[String]) -> u8 {
    let a = match parse_args(argv) {
        Ok(a) => a,
        Err(m) => {
            if !m.is_empty() {
                eprintln!("release acceptance: {m}");
            }
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
    let home = PathBuf::from(env("HOME").unwrap_or_else(|| "/".into()));
    let notes_repo = a.notes_repo.clone().or_else(|| env("SPIRA_NOTES_REPO").map(PathBuf::from));
    if a.record && notes_repo.is_none() {
        eprintln!("release acceptance: --record needs --notes-repo <path> (or SPIRA_NOTES_REPO): the repository whose refs/tags/<tag> carries the note");
        return 2;
    }
    let release_bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("release acceptance: cannot resolve this binary's own path: {e}");
            return 2;
        }
    };
    let mk = |prefix: &str| -> Result<PathBuf, String> {
        let base = std::env::temp_dir();
        for n in 0..1000u32 {
            let p = base.join(format!("{prefix}{}-{}-{n}", std::process::id(), crate::fsutil::now_secs()));
            if fs::create_dir(&p).is_ok() {
                return Ok(p);
            }
        }
        Err(format!("cannot make a scratch directory under {}", base.display()))
    };
    let tmp = match mk("release-acceptance-") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("release acceptance: {e}");
            return 2;
        }
    };
    let forensics = match env("SPIRA_ACCEPTANCE_FORENSICS") {
        Some(p) => PathBuf::from(p),
        None => match mk("release-acceptance-forensics-") {
            Ok(p) => p,
            Err(e) => {
                eprintln!("release acceptance: {e}");
                return 2;
            }
        },
    };
    let origin_of = notes_repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let origin = RealHost.run(&Cmd::new("git").args(["-C", &origin_of.display().to_string(), "remote", "get-url", "origin"]));
    let gh_repo = gh_repo_from([env("SPIRA_FORGE_REPO"), env("GH_REPO"), env("GITHUB_REPOSITORY")], (origin.rc == 0).then_some(origin.out));
    let secs = |k: &str, d: u64| env(k).and_then(|v| v.parse().ok()).unwrap_or(d);
    let o = Opts {
        bd_db: a.bd_db.clone().unwrap_or_else(|| home.join("spira-acceptance-test-db")),
        tmp: tmp.clone(),
        xdg_config: env("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")),
        forensics,
        release_bin,
        base_path: std::env::var("PATH").unwrap_or_default(),
        spira_run: env("SPIRA_RUN").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share/spira/run")),
        token_projects: env("SPIRA_TOKEN_PROJECTS").map(PathBuf::from).unwrap_or_else(|| home.join(".claude/projects")),
        gh_repo,
        notes_repo,
        summon_secs: secs("SPIRA_ACCEPT_SUMMON_SECS", 180),
        asset_wait_secs: secs("SPIRA_ACCEPT_ASSET_WAIT_SECS", 600),
        pid: std::process::id(),
        a,
    };
    let rc = phases::run(&RealHost, o);
    crate::fsutil::make_writable(&tmp);
    let _ = fs::remove_dir_all(&tmp);
    rc
}
