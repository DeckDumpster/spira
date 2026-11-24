//! The production [`crate::ports::World`]: git, the sibling tools (`release`, `overrides.sh`,
//! `units-install`, `exclude.sh`), `gh`, and in-process the mail library, the clock and the
//! config resolution.

use crate::ports::{StatusRow, World};
use std::cell::OnceCell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Whether an override pass has anything to run: the configured directory declares one. An
/// unresolvable key or unreadable directory is printed and runs nothing.
fn overrides_declared(dir: Result<String, String>) -> bool {
    match dir.and_then(|d| spira_config::overrides::declared(Path::new(&d))) {
        Ok(declared) => declared,
        Err(e) => {
            eprintln!("skew: {e}");
            false
        }
    }
}

/// Whether this process's configuration resolves — the in-process form of skew.sh's own
/// `trap '[ "$_skew_init_done" = 0 ] && exit 3' EXIT` around its `. lib.sh`: a config that
/// cannot resolve is "could not check", never an empty verdict. Called once, before any
/// subcommand dispatches (see `main.rs`).
pub fn config_resolves() -> bool {
    match spira_config::process::config() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("skew: {e}");
            false
        }
    }
}

const GH_RELEASE: &str = "release";

/// `gh release list` through `spawn`, counted as `skew:release-list` when it exits; Err is stderr's first line.
fn release_list_via(
    sink: &spira_otel::metrics::MetricsSink,
    slug: &str,
    spawn: &dyn Fn(&[&str]) -> std::io::Result<std::process::Output>,
) -> Result<String, String> {
    let argv = [GH_RELEASE, "list", "--repo", slug, "--json", "tagName,isDraft"];
    let out = forge::spend::counted_output(sink, "skew:release-list", &argv, || spawn(&argv)).map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().next().unwrap_or("").to_string())
    }
}

/// `YYYYMMDDTHHMMSSZ`, UTC, for `epoch` — what `date -u +%Y%m%dT%H%M%SZ` printed.
pub fn stamp_of(epoch: u64) -> String {
    tsd::iso_utc(epoch).replace(['-', ':'], "")
}

/// One question to the concierge, who operates the loop every skew default acts on, through
/// the mail library in-process, under the same environment and bead store `mail send` loads.
fn send_question(subject: &str, body: &str, default_action: &str) -> Result<(), String> {
    let env = mail::env::Env::load()?;
    // The ask's bead is filed through the store's content door (mail's BdCli).
    let bd = mail::bead::BdCli::configured(env.bd_conn_retries)?;
    send_question_with(&bd, &env, subject, body, default_action)
}

pub(crate) fn send_question_with(bd: &dyn mail::bead::Bd, env: &mail::env::Env, subject: &str, body: &str, default_action: &str) -> Result<(), String> {
    let args = mail::cmds::SendArgs {
        mailbox: "concierge",
        from: Some("Skew check <skew@spira>"),
        subject,
        kind: "question",
        default: default_action,
        class: "",
        bead: "",
        urgent: false,
        digest: false,
        dry_run: false,
        requested_by: "",
    };
    let result = mail::cmds::send(bd, env, &args, body.to_string());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Some(a) = mail::cmds::attempt_of_send(&args, body, &result, now) {
        if let Err(e) = a.write(&env.run_dir, &tsd::default_host()) {
            eprintln!("mail: send attempt not recorded: {e}");
        }
    }
    result.map(|_| ())
}

// Registered config keys (spira/conf.d) this crate's generic `env()` seam also serves —
// $SPIRA_TOML only, never the raw environment, never a fallback (per Ryan 2026-10-05).
const RESOLVED_KEYS: &[&str] = &["SPIRA_RELEASES", "SPIRA_RELEASE_REPO", "SPIRA_GH_INTAKE_REPO", "SPIRA_GH", "SPIRA_RUN", "SPIRA_QUEUE_DIR"];

pub struct Real {
    pub home: PathBuf,
    registry: OnceCell<spira_config::repos::Registry>,
}

impl Real {
    pub fn new(home: PathBuf) -> Real {
        Real { home, registry: OnceCell::new() }
    }

    /// The repo registry (`spira_config::repos::Registry::from_env`, sp-k6lku "wave
    /// 4.13"), built once per process, in-process — no `bash -c '. lib.sh; ...'`
    /// subprocess at all: conf.sh's own resolution (`SPIRA_HOME_REPO`/`SPIRA_REPO`/
    /// `SPIRA_REPO_DERIVED`/`SPIRA_REPO_MAP`, none of which it exports) is the same
    /// `spira_config::resolve()` engine `from_env` calls when this process's bare
    /// environment lacks them (sp-z3eyk), which this process's own, always does: `skew`
    /// is launched by a unit or a bare shell, never by something that sourced conf.sh
    /// first. `repo_names`, `repo_root`, `repo_field`, `home_repo`, `repo_land`, `landref`,
    /// `ref_remote` and `ref_branch` below all read this same registry.
    fn registry(&self) -> &spira_config::repos::Registry {
        self.registry.get_or_init(|| spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), &self.home))
    }

    fn git(&self, repo: &Path, args: &[&str]) -> (bool, String) {
        let out = spira_config::bounded::bounded("git").arg("-C").arg(repo).args(args).stdin(Stdio::null()).output();
        match out {
            Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()),
            Err(_) => (false, String::new()),
        }
    }

    fn git_checked(&self, repo: &Path, args: &[&str]) -> Result<(), String> {
        let o = spira_config::bounded::bounded("git").arg("-C").arg(repo).args(args).stdin(Stdio::null()).output().map_err(|e| format!("git did not run: {e}"))?;
        if o.status.success() {
            return Ok(());
        }
        let said = format!("{}{}", String::from_utf8_lossy(&o.stderr), String::from_utf8_lossy(&o.stdout));
        let said = said.trim();
        Err(if said.is_empty() { format!("git {} exited {} with no output", args.join(" "), o.status) } else { said.to_string() })
    }

    fn git_lines(&self, repo: &Path, args: &[&str]) -> Vec<String> {
        let (_, out) = self.git(repo, args);
        out.lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
    }
}

impl World for Real {
    fn repo_names(&self) -> Vec<String> {
        self.registry().names()
    }
    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        self.registry().root(name).map(PathBuf::from)
    }
    fn repo_field(&self, name: &str, field: &str) -> Option<String> {
        let col = match field {
            "path" => spira_config::repos::Column::Path,
            "land" => spira_config::repos::Column::Land,
            "base" => spira_config::repos::Column::Base,
            "format" => spira_config::repos::Column::Format,
            "gate" => spira_config::repos::Column::Gate,
            "lanes" => spira_config::repos::Column::Lanes,
            _ => return None,
        };
        self.registry().field(name, col)
    }
    fn same_repo(&self, a: &Path, b: &Path) -> bool {
        spira_config::repos::same_repo(&a.to_string_lossy(), &b.to_string_lossy())
    }
    fn home_repo(&self) -> String {
        self.registry().home_repo().to_string()
    }
    fn landref(&self, repo: &Path) -> Option<String> {
        spira_config::repos::landref(self.registry(), &repo.to_string_lossy())
    }
    fn repo_land(&self, name: &str) -> String {
        self.registry().land(name)
    }
    fn ref_remote(&self, base: &str, repo: &Path) -> Option<String> {
        spira_config::repos::ref_remote(base, Some(&repo.to_string_lossy()))
    }
    fn ref_branch(&self, base: &str) -> String {
        spira_config::repos::ref_branch(base)
    }

    fn is_git_repo(&self, p: &Path) -> bool {
        p.join(".git").exists()
    }

    fn harness_in(&self, repo: &Path) -> Vec<String> {
        let (_, files) = self.git(repo, &["ls-files"]);
        pipe_through_exclude(&self.home, &files)
    }
    fn harness_in_ref(&self, repo: &Path, ref_: &str) -> Vec<String> {
        let (_, files) = self.git(repo, &["ls-tree", "-r", "--name-only", ref_]);
        pipe_through_exclude(&self.home, &files)
    }
    // (harness_in/harness_in_ref above pass the raw multi-line `git` output straight through
    // to `exclude.sh harness-in` on stdin, exactly as bash's pipeline did.)

    fn changed_files(&self, repo: &Path, base: &str, ref_: &str) -> Vec<String> {
        self.git_lines(repo, &["diff", "--name-only", &format!("{base}...{ref_}")])
    }
    fn tags_matching(&self, repo: &Path, pattern: &str) -> Vec<String> {
        let mut v = self.git_lines(repo, &["tag", "-l", pattern]);
        v.sort();
        v
    }
    fn local_tag_sidecars(&self, dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return out,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("tag") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                for line in text.lines() {
                    let tag: String = line.chars().filter(|c| *c != ' ' && *c != '\t').collect();
                    if spira_config::release_tag::is_release_tag(&tag) {
                        out.push(tag);
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
    fn rev_parse(&self, repo: &Path, rev: &str, _peel: bool) -> Option<String> {
        let (ok, out) = self.git(repo, &["rev-parse", "-q", "--verify", rev]);
        if ok { Some(out.trim().to_string()) } else { None }
    }
    fn rev_parse_short(&self, repo: &Path, rev: &str) -> Option<String> {
        let (ok, out) = self.git(repo, &["rev-parse", "--short", rev]);
        if ok { Some(out.trim().to_string()) } else { None }
    }
    fn is_ancestor(&self, repo: &Path, ancestor: &str, descendant: &str) -> bool {
        self.git(repo, &["merge-base", "--is-ancestor", ancestor, descendant]).0
    }
    fn rev_list_count(&self, repo: &Path, range: &str) -> Option<u64> {
        let (ok, out) = self.git(repo, &["rev-list", "--count", range]);
        if ok { out.trim().parse().ok() } else { None }
    }
    fn current_branch(&self, repo: &Path) -> String {
        self.git(repo, &["branch", "--show-current"]).1.trim().to_string()
    }
    fn dirty_tracked(&self, repo: &Path) -> String {
        self.git(repo, &["status", "--porcelain", "--untracked-files=no"]).1
    }
    fn stash_push(&self, repo: &Path, tag: &str) -> Result<(), String> {
        self.git_checked(repo, &["stash", "push", "-m", tag])
    }
    fn restore_deleted(&self, repo: &Path) -> Result<(), String> {
        self.git_checked(repo, &["checkout", "--", "."])
    }
    fn fetch(&self, repo: &Path, remote: &str) -> bool {
        self.git(repo, &["fetch", "-q", "--no-write-fetch-head", remote]).0
    }
    fn diff_status(&self, repo: &Path, range: &str) -> Vec<StatusRow> {
        let (_, out) = self.git(repo, &["diff", "--diff-filter=MAD", "--name-status", range]);
        out.lines()
            .filter_map(|l| {
                let mut parts = l.splitn(2, char::is_whitespace);
                let status = parts.next()?.chars().next()?;
                let path = parts.next()?.trim();
                if path.is_empty() { None } else { Some((status, path.to_string())) }
            })
            .collect()
    }
    fn show_file(&self, repo: &Path, rev: &str, path: &str) -> Option<Vec<u8>> {
        let out = spira_config::bounded::bounded("git").arg("-C").arg(repo).arg("show").arg(format!("{rev}:{path}")).stdin(Stdio::null()).output().ok()?;
        if out.status.success() { Some(out.stdout) } else { None }
    }
    fn file_mode(&self, repo: &Path, rev: &str, path: &str) -> Option<String> {
        let (_, out) = self.git(repo, &["ls-tree", rev, path]);
        out.split_whitespace().next().map(str::to_string)
    }
    fn reset_mixed(&self, repo: &Path, rev: &str) -> bool {
        self.git(repo, &["reset", "--mixed", "-q", rev]).0
    }
    fn merge_ff_only(&self, repo: &Path, rev: &str) -> bool {
        self.git(repo, &["merge", "--ff-only", "-q", rev]).0
    }

    fn release_verify_no_pre_activate(&self, name: &str, releases: Option<&Path>) -> (bool, String) {
        let release = match spira_config::release_env::tool("release") {
            Ok(r) => r,
            Err(e) => return (false, e),
        };
        // batch-job: release runs for as long as its work does
        // FORK-EDGE: release is a sibling binary (build/verify/activate), not a library
        let mut c = Command::new(release);
        c.arg("verify").arg(name).arg("--no-pre-activate");
        if let Some(r) = releases {
            c.arg("--releases").arg(r);
        }
        let out = c.stdin(Stdio::null()).output();
        match out {
            Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => (false, e.to_string()),
        }
    }
    fn release_status(&self) -> String {
        let Ok(release) = spira_config::release_env::tool("release") else { return String::new() };
        // batch-job: release runs for as long as its work does
        Command::new(release)
            .arg("status")
            .stdin(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }
    fn release_build_verify_activate(&self, sha: &str, repo: &Path, base: &str, releases: &Path) -> Result<(), String> {
        let run = |args: &[&str]| -> Result<(), String> {
            // batch-job: release runs for as long as its work does
            let out = Command::new(spira_config::release_env::tool("release")?).args(args).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
            }
        };
        let repo_s = repo.to_string_lossy().into_owned();
        let releases_s = releases.to_string_lossy().into_owned();
        run(&["build", sha, "--repo", &repo_s, "--releases", &releases_s])?;
        run(&["verify", sha, "--releases", &releases_s])?;
        run(&["activate", sha, "--repo", &repo_s, "--landed-ref", base, "--releases", &releases_s])?;
        Ok(())
    }
    fn overrides_apply(&self, repo: &Path) {
        // bash: `overrides.sh apply "$repo"` -- unredirected, so whatever it prints (an
        // override's own "<label>: applied"/"retired" line) reaches skew's own stdout
        // directly. Redirecting to Stdio::null() here discarded that line entirely; fixed
        // by inheriting stdout/stderr (Command's own default) instead of silencing them.
        // Caught live by testenv's test-overrides.sh (sp-yyk47).
        // FORK-EDGE: overrides.sh (223 lines of shell, over the port-in-place bound)
        if !overrides_declared(spira_config::process::cfg("SPIRA_OVERRIDES")) {
            return;
        }
        let _ = spira_config::bounded::bounded("overrides.sh").arg("apply").arg(repo).stdin(Stdio::null()).status();
    }
    fn install_diff(&self, installer: &Path) -> (i32, String) {
        // units-install is a compiled binary now (sp-31dm0): exec it directly, never
        // through bash -- the old install.sh needed `bash <installer>` because it was a
        // script with no guaranteed +x bit; a binary is run like any other.
        // batch-job: this runs whatever its caller names, as long as that takes
        let out = Command::new(installer).arg("--diff").stdin(Stdio::null()).output();
        match out {
            Ok(o) => (
                o.status.code().unwrap_or(-1),
                format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
            ),
            Err(e) => (3, e.to_string()),
        }
    }
    fn which(&self, name: &str) -> Option<String> {
        // PATH is not a registered config key, so this can never actually fail — but
        // `env()` is now fallible in general, and `which()` has no Result of its own to
        // propagate into, so a resolution failure (impossible here) refuses loudly and
        // answers "not found" rather than silently reading it as PATH being unset.
        let path = match self.env("PATH") {
            Ok(v) => v?,
            Err(e) => {
                eprintln!("skew: {e}");
                return None;
            }
        };
        std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_exec(p)).map(|p| p.to_string_lossy().into_owned())
    }
    fn is_executable(&self, p: &Path) -> bool {
        is_exec(p)
    }
    fn gh_release_list(&self, slug: &str) -> Result<String, String> {
        let gh_timeout = std::env::var("GH_TIMEOUT").unwrap_or_else(|_| "120".to_string());
        // SPIRA_GH's own declared meaning (spira/conf.d/SPIRA_GH): "empty means the system
        // gh" — not a Rust-invented default, the registered key's own empty-is-a-value
        // contract, read once through the `env()` seam (which routes it through
        // `spira_config::process::cfg`, never the raw environment).
        let gh = self.env("SPIRA_GH")?.filter(|v| !v.is_empty()).unwrap_or_else(|| "gh".to_string());
        let sink = forge::telemetry::sink();
        release_list_via(&sink, slug, &|argv| Command::new("timeout").arg(&gh_timeout).arg(&gh).args(argv).stdin(Stdio::null()).output())
    }
    fn mail_send_question(&self, subject: &str, body: &str, default_action: &str) -> Result<(), String> {
        send_question(subject, body, default_action)
    }

    fn now_stamp(&self) -> String {
        stamp_of(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()))
    }
    fn env(&self, k: &str) -> Result<Option<String>, String> {
        if RESOLVED_KEYS.contains(&k) {
            // The one source of config: $SPIRA_TOML, resolved once per process — never the
            // raw environment, never a fallback (per Ryan 2026-10-05). A resolution failure
            // (missing $SPIRA_TOML, a bad/missing config file, ...) is a real error and must
            // surface as one — never collapsed into the same `None` a legitimately-unset,
            // unregistered env var would produce.
            return spira_config::process::cfg(k).map(Some);
        }
        Ok(std::env::var(k).ok().filter(|v| !v.is_empty() || k == "SPIRA_ALLOW_FOREIGN_HARNESS"))
    }
    fn is_symlink(&self, p: &Path) -> bool {
        std::fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
    }
    fn readlink(&self, p: &Path) -> Option<String> {
        std::fs::read_link(p).ok().map(|p| p.to_string_lossy().into_owned())
    }
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
    fn read_to_string(&self, p: &Path) -> Option<String> {
        std::fs::read_to_string(p).ok()
    }
    fn write_staged(&self, p: &Path, content: &[u8], executable: bool) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let tmp = p.with_extension(format!("spira-new.{}", std::process::id()));
        std::fs::write(&tmp, content).map_err(|e| e.to_string())?;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())
    }
    fn remove_file(&self, p: &Path) {
        let _ = std::fs::remove_file(p);
    }
    fn mkdir_p(&self, p: &Path) {
        let _ = std::fs::create_dir_all(p);
    }

    fn queue_lock_held(&self, repo_name: &str) -> bool {
        use std::os::unix::io::AsRawFd;
        let dir = match self.env("SPIRA_QUEUE_DIR") {
            Ok(Some(d)) if !d.is_empty() => PathBuf::from(d),
            Ok(_) => return false,
            Err(e) => {
                eprintln!("skew: {e}");
                return false;
            }
        };
        let Ok(f) = std::fs::OpenOptions::new().read(true).open(dir.join(repo_name).join("lock")) else { return false };
        // SAFETY: flock on a descriptor we own; released when `f` drops.
        let got = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        !got
    }
    fn stamp_read(&self, key: &str) -> Option<String> {
        // SPIRA_RUN is a registered key; a resolution failure here is real and must refuse
        // loudly rather than be read as "no stamp dir configured."
        let run = match self.env("SPIRA_RUN") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skew: {e}");
                return None;
            }
        }
        .filter(|v| !v.is_empty())?;
        std::fs::read_to_string(Path::new(&run).join(key)).ok()
    }
    fn stamp_write(&self, key: &str, val: &str) {
        let run = match self.env("SPIRA_RUN") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skew: {e}");
                return;
            }
        };
        let Some(run) = run.filter(|v| !v.is_empty()) else { return };
        let dir = PathBuf::from(&run);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(key), val);
    }
    fn run_log_append(&self, name: &str, line: &str) {
        let run = match self.env("SPIRA_RUN") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skew: {e}");
                return;
            }
        };
        let Some(run) = run.filter(|v| !v.is_empty()) else { return };
        let dir = PathBuf::from(&run);
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{name}.log"))) {
            let _ = writeln!(f, "{line}");
        }
    }
    fn index_lock_age_secs(&self, repo: &Path) -> Option<u64> {
        let (ok, out) = self.git(repo, &["rev-parse", "--git-path", "index.lock"]);
        let rel = out.trim();
        if !ok || rel.is_empty() {
            return None;
        }
        let meta = std::fs::metadata(repo.join(rel)).ok()?;
        Some(meta.modified().ok()?.elapsed().map(|d| d.as_secs()).unwrap_or(0))
    }
    fn remove_index_lock(&self, repo: &Path) -> Result<(), String> {
        let (ok, out) = self.git(repo, &["rev-parse", "--git-path", "index.lock"]);
        let rel = out.trim();
        if !ok || rel.is_empty() {
            return Err("cannot resolve index.lock".into());
        }
        std::fs::remove_file(repo.join(rel)).map_err(|e| e.to_string())
    }
    fn git_running_in(&self, repo: &Path) -> bool {
        let Ok(want) = std::fs::canonicalize(repo) else { return false };
        let Ok(procs) = std::fs::read_dir(system_paths::PROC) else { return false };
        procs.flatten().any(|e| {
            let p = e.path();
            let is_git = std::fs::read_to_string(p.join("comm")).map(|c| c.trim() == "git").unwrap_or(false);
            is_git && std::fs::read_link(p.join("cwd")).map(|c| c == want).unwrap_or(false)
        })
    }

    fn out(&self, s: &str) {
        println!("{s}");
    }
    fn err(&self, s: &str) {
        eprintln!("{s}");
    }
}

/// `exclude.sh harness-in`, fed `text` (raw, possibly multi-line `git` output) on stdin.
fn pipe_through_exclude(home: &Path, text: &str) -> Vec<String> {
    // FORK-EDGE: exclude.sh (345 lines of shell, over the port-in-place bound)
    let mut child = match spira_config::bounded::bounded(home.join("exclude.sh")).arg("harness-in").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child
        .wait_with_output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(test)]
mod gh_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test_id(tu66npu)]
    #[test]
    fn a_release_list_through_a_fake_gh_is_counted_once_under_skew_release_list() {
        let fake = |_: &[&str]| Ok(std::process::Output { status: std::process::ExitStatus::from_raw(0), stdout: b"[]".to_vec(), stderr: Vec::new() });
        let (got_list, got) = spira_otel::metrics::fixture::capture(|sink| release_list_via(sink, "o/r", &fake));
        assert_eq!(got_list.as_deref(), Ok("[]"));
        let series: Vec<_> = got.iter().filter(|s| s.name == forge::spend::CALLS).collect();
        assert_eq!(series.len(), 1, "{got:?}");
        assert_eq!((series[0].label("caller"), series[0].label("outcome"), series[0].value as u64), (Some("skew:release-list"), Some("fresh"), 1));
    }
}

#[cfg(test)]
mod send_tests {
    use super::*;
    use mail::bead::{Bd, BdOut};

    struct NoStore;

    impl Bd for NoStore {
        fn run(&self, _args: &[String], _stdin: Option<&str>) -> BdOut {
            BdOut::ok("")
        }
        fn close(&self, _id: &str, _reason: &str) -> Result<(), spira_store::lc_call::LcError> {
            Ok(())
        }
        fn ensure_ask(&self, _id: &str, _work_bead: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn env(root: &Path) -> mail::env::Env {
        mail::env::Env {
            mail_root: root.join("mail"),
            kinds_dir: testkit::manifest_dir().join("../spira/mail/kinds"),
            index_file: root.join("index"),
            mute: false,
            loom_budget_ms: 1000,
            repeat_window_s: 0,
            tidy_fresh_s: 0,
            id_prefix: "sp".into(),
            ask_label: String::new(),
            db: String::new(),
            bd_conn_retries: 1,
            operator_actor: "operator".into(),
            run_dir: root.join("run"),
            home: PathBuf::from(root),
            mail_from: None,
            lint_considered: None,
            repeat_considered: None,
            operator_considered: None,
            statute_guards: String::new(),
            statute_considered: None,
            allow_blocking: false,
            bead_id: None,
            lock_timeout_ms: 1000,
            condition: None,
        }
    }

    // covers: UC-operator-channel-05
    #[test_id(tvlbvev)]
    #[test]
    fn an_escalation_is_delivered_to_the_concierge_and_never_the_operator() {
        let f = crate::tests::Fake::default();
        *f.mail_ok.borrow_mut() = true;
        crate::escalate(&f, "v2:LOCAL-BEHIND=1 LOCAL-TAMPERED=0", "LOCAL-BEHIND x\n");
        let (subject, body, default) = f.mail_calls.borrow()[0].clone();

        let tmp = testkit::TempDir::new("skew-send");
        std::fs::create_dir_all(tmp.path().join("run")).unwrap();
        send_question_with(&NoStore, &env(tmp.path()), &subject, &body, &default).unwrap();
        assert_eq!(std::fs::read_dir(tmp.path().join("mail/concierge/new")).unwrap().count(), 1);
        assert!(!tmp.path().join("mail/operator/new").exists());
    }
}

#[cfg(test)]
mod swap_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    // covers: UC-instance-lifecycle-37
    #[test_id(tml9443)]
    #[test]
    fn write_staged_swaps_in_a_new_inode_so_a_held_handle_keeps_the_old_content() {
        let tmp = testkit::TempDir::new("skew-swap");
        let dir = tmp.path().to_path_buf();
        let p = dir.join("conf.sh");
        std::fs::write(&p, "old").unwrap();
        let before = std::fs::metadata(&p).unwrap().ino();
        let held = std::fs::File::open(&p).unwrap();
        let w = Real::new(dir.clone());
        w.write_staged(&p, b"new", false).unwrap();
        assert_ne!(std::fs::metadata(&p).unwrap().ino(), before);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        let mut old = String::new();
        std::io::Read::read_to_string(&mut &held, &mut old).unwrap();
        assert_eq!(old, "old");
    }
}

#[cfg(test)]
mod overrides_tests {
    use super::overrides_declared;

    #[test_id(tyj71nl)]
    #[test]
    fn an_override_pass_runs_only_when_one_is_declared() {
        let d = testkit::TempDir::new("skew-overrides");
        let dir = || Ok(d.path().display().to_string());
        assert!(!overrides_declared(dir()), "none declared");
        std::fs::write(d.join("hold.override"), "").unwrap();
        assert!(overrides_declared(dir()), "one declared");
        std::fs::write(d.join("plain"), "").unwrap();
        assert!(!overrides_declared(Ok(d.join("plain").display().to_string())), "an unreadable directory runs nothing");
        assert!(!overrides_declared(Err("no key".into())));
    }
}
