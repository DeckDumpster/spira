//! The production [`crate::ports::World`]: git, `bd`-adjacent tools (`release`, `mail`,
//! `overrides.sh`, `units-install`, `exclude.sh`), `gh`, and the `lib.sh` repository-map seam —
//! the same one-shot context call `gate-check`/`queue` use rather than re-deriving the repository map
//! resolution in Rust a second time (DESIGN.md §4: lib.sh stays the one authority).

use crate::ports::{StatusRow, World};
use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Whether `lib.sh` (and whatever it sources — conf.sh, transitively) sources cleanly.
/// Called once, before any subcommand dispatches — see `main.rs`'s doc comment: this is
/// skew.sh's own `trap '[ "$_skew_init_done" = 0 ] && exit 3' EXIT` around its `. lib.sh`,
/// which caught a `bd migrate schema` failure inside conf.sh the same way it would catch
/// lib.sh itself being unreadable. A per-call `seam()` alone can't distinguish "this one
/// lib.sh function returned nothing" from "lib.sh never sourced at all," so this is a
/// dedicated, one-time check, not folded into `seam()`.
pub fn lib_sh_sources(home: &Path) -> bool {
    Command::new("bash")
        .arg("-c")
        .arg(format!(". \"{}\" >/dev/null 2>&1", home.join("lib.sh").display()))
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub struct Real {
    pub home: PathBuf,
    registry: OnceCell<spira_config::repos::Registry>,
}

impl Real {
    pub fn new(home: PathBuf) -> Real {
        Real { home, registry: OnceCell::new() }
    }

    /// The repo registry (`spira_config::repos`, sp-k6lku "wave 4.13"), built once per
    /// process from a single `. lib.sh` snapshot of the four variables [`Registry::new`]
    /// needs (`SPIRA_HOME_REPO`, `SPIRA_REPO`, `SPIRA_REPO_DERIVED`, `SPIRA_REPO_MAP`), in
    /// place of a fresh `bash -c '. lib.sh; <fn>'` subprocess per lookup — `repo_names`,
    /// `repo_root`, `repo_field`, `home_repo`, `repo_land`, `landref`, `ref_remote` and
    /// `ref_branch` below all read this same registry in-process instead.
    fn registry(&self) -> &spira_config::repos::Registry {
        self.registry.get_or_init(|| {
            let script = ". \"$0\" >/dev/null 2>&1 || exit 96\n\
                for __v in SPIRA_HOME_REPO SPIRA_REPO SPIRA_REPO_DERIVED SPIRA_REPO_MAP; do \
                printf '%s=%s\\0' \"$__v\" \"${!__v-}\"; done";
            let out = Command::new("bash")
                .arg("-c")
                .arg(script)
                .arg(self.home.join("lib.sh"))
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output();
            let mut env: BTreeMap<String, String> = BTreeMap::new();
            if let Ok(o) = out {
                for rec in String::from_utf8_lossy(&o.stdout).split('\0') {
                    if let Some((k, v)) = rec.split_once('=') {
                        env.insert(k.to_string(), v.to_string());
                    }
                }
            }
            let map_text = env
                .get("SPIRA_REPO_MAP")
                .filter(|p| !p.is_empty())
                .and_then(|p| std::fs::read_to_string(p).ok());
            spira_config::repos::Registry::new(map_text.as_deref(), &env, &self.home)
        })
    }

    fn git(&self, repo: &Path, args: &[&str]) -> (bool, String) {
        let out = Command::new("git").arg("-C").arg(repo).args(args).stdin(Stdio::null()).output();
        match out {
            Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()),
            Err(_) => (false, String::new()),
        }
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
                    if tag.starts_with("spira-release-") {
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
        let (ok, out) = self.git(repo, &["stash", "push", "-m", tag]);
        if ok { Ok(()) } else { Err(out) }
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
        let out = Command::new("git").arg("-C").arg(repo).arg("show").arg(format!("{rev}:{path}")).stdin(Stdio::null()).output().ok()?;
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
        let mut c = Command::new("release");
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
        Command::new("release")
            .arg("status")
            .stdin(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }
    fn release_build_verify_activate(&self, sha: &str, repo: &Path, base: &str, releases: &Path) -> Result<(), String> {
        let run = |args: &[&str]| -> Result<(), String> {
            let out = Command::new("release").args(args).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
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
        let _ = Command::new("overrides.sh").arg("apply").arg(repo).stdin(Stdio::null()).status();
    }
    fn install_diff(&self, installer: &Path) -> (i32, String) {
        // units-install is a compiled binary now (sp-31dm0): exec it directly, never
        // through bash -- the old install.sh needed `bash <installer>` because it was a
        // script with no guaranteed +x bit; a binary is run like any other.
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
        let path = self.env("PATH")?;
        std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_exec(p)).map(|p| p.to_string_lossy().into_owned())
    }
    fn is_executable(&self, p: &Path) -> bool {
        is_exec(p)
    }
    fn gh_release_list(&self, slug: &str) -> Result<String, String> {
        let gh_timeout = std::env::var("GH_TIMEOUT").unwrap_or_else(|_| "120".to_string());
        let gh = std::env::var("SPIRA_GH").unwrap_or_else(|_| "gh".to_string());
        let out = Command::new("timeout")
            .arg(&gh_timeout)
            .arg(&gh)
            .args(["release", "list", "--repo", slug, "--json", "tagName,isDraft"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(err.lines().next().unwrap_or("").to_string())
        }
    }
    fn mail_send_question(&self, subject: &str, body: &str, default_action: &str) -> Result<(), String> {
        // mail.sh is the `mail` binary now (sp-ooh1k); deps.toml's compat table keeps a
        // spira/mail.sh symlink but skew.sh's own fix called the real name directly
        // rather than lean on that transitional shim -- matched here, sp-yyk47.
        let mut child = Command::new("mail")
            .args(["send", "operator", "--from", "Skew check <skew@spira>", "--subject", subject, "--kind", "question", "--default", default_action])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(body.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "rc={} {}{}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }

    fn now_stamp(&self) -> String {
        let out = Command::new("date").arg("-u").arg("+%Y%m%dT%H%M%SZ").output();
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    }
    fn env(&self, k: &str) -> Option<String> {
        std::env::var(k).ok().filter(|v| !v.is_empty() || k == "SPIRA_ALLOW_FOREIGN_HARNESS")
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

    fn stamp_read(&self, key: &str) -> Option<String> {
        let run = std::env::var("SPIRA_RUN").ok()?;
        std::fs::read_to_string(Path::new(&run).join(key)).ok()
    }
    fn stamp_write(&self, key: &str, val: &str) {
        let Ok(run) = std::env::var("SPIRA_RUN") else { return };
        let dir = PathBuf::from(&run);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(key), val);
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
    let mut child = match Command::new(home.join("exclude.sh")).arg("harness-in").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
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
