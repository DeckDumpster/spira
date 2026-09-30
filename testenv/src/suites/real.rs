//! The ports against the host (DESIGN-suites.md §4, §5).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::ports::*;

/// host-check.sh's wall (DESIGN-suites.md §6 D5).
pub const HOST_CHECK_WALL: Duration = Duration::from_secs(30);

pub struct Real {
    suite_dir: PathBuf,
    /// incident.sh / mail.sh: the SPIRA_INCIDENT / SPIRA_MAIL_CMD test seams, else found on
    /// PATH (sp-gypjk). None = not on PATH.
    incident: Option<PathBuf>,
    mail: Option<PathBuf>,
    /// The PATH this run was given; host-check.sh is looked up on it at each call.
    path: String,
}

impl Real {
    pub fn new(s: &Settings, env: &dyn Fn(&str) -> Option<String>) -> Real {
        let get = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let path = env("PATH").unwrap_or_default();
        Real {
            incident: get("SPIRA_INCIDENT").or_else(|| crate::util::which_in(&path, "incident.sh")),
            mail: get("SPIRA_MAIL_CMD").or_else(|| crate::util::which_in(&path, "mail.sh")),
            suite_dir: s.suite_dir.clone(),
            path,
        }
    }
}

/// `--reason-file`: a path, or `-` for stdin.
pub fn read_input(p: &str) -> Result<String, String> {
    if p == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| e.to_string())?;
        return Ok(s);
    }
    fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))
}

/// Wait for `child` up to `wall`; kill it past the wall. Some(exit code) when it exited.
fn wait_wall(child: &mut Child, wall: Duration) -> Option<i32> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return st.code(),
            Ok(None) if start.elapsed() < wall => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

impl Clock for Real {
    fn now(&self) -> u64 {
        crate::batch::now_epoch()
    }
}

impl Emit for Real {
    fn out(&self, line: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    }
    fn err(&self, line: &str) {
        eprintln!("{line}");
    }
}

// ------------------------------------------------------------------------ the lib.sh seam

/// The record separator before the seam's answer, so a lib.sh log line is never read as it.
pub const MARK: char = '\u{1e}';

/// S1 `conf`: a fixed script on stdin, then one NUL-terminated value (the directory that
/// holds lib.sh). Nothing in argv or the environment (law-payloads-go-on-stdin). bash reads
/// a non-seekable stdin a byte at a time, so the brace group is parsed and run before the
/// value after it is consumed by the `read` inside.
pub const CONF_SCRIPT: &str = r#"{
set -uo pipefail
IFS= read -r -d '' __home
exec </dev/null
. "$__home/lib.sh" || exit 96
__n="${SPIRA_HOME_REPO:-}"; [ -n "$__n" ] || __n="$(spira_home_repo 2>/dev/null)"
__p="$(repo_root "$__n" 2>/dev/null)"; __pok=$?
__lr=""; [ "$__pok" -eq 0 ] && __lr="$(spira_landref "$__p" 2>/dev/null)"
printf '\036'
printf 'home_repo=%s\0' "$__n"
printf 'scope_set=%s\0' "${SPIRA_SCOPE_LABEL+1}"
printf 'scope=%s\0' "${SPIRA_SCOPE_LABEL:-}"
printf 'db=%s\0' "${SPIRA_DB:-}"
printf 'path_ok=%s\0' "$__pok"
printf 'path=%s\0' "$__p"
printf 'landref=%s\0' "$__lr"
exit 0
}
"#;

/// Parse the seam's stdout: everything before the last MARK is log noise.
pub fn parse_conf(stdout: &str) -> Result<Conf, String> {
    let Some(i) = stdout.rfind(MARK) else {
        return Err("the conf seam printed no answer".into());
    };
    let ans = &stdout[i + MARK.len_utf8()..];
    let kv: std::collections::BTreeMap<&str, &str> = ans
        .split('\0')
        .filter_map(|r| r.split_once('='))
        .collect();
    let g = |k: &str| kv.get(k).copied().unwrap_or("");
    if g("home_repo").is_empty() {
        return Err("the home repository is unset after sourcing lib.sh".into());
    }
    Ok(Conf {
        home_repo: g("home_repo").to_string(),
        scope_label: (g("scope_set") == "1").then(|| g("scope").to_string()),
        db: g("db").to_string(),
        repo_path: (g("path_ok") == "0" && !g("path").is_empty()).then(|| PathBuf::from(g("path"))),
        landref: Some(g("landref").to_string()).filter(|s| !s.is_empty()),
    })
}

pub fn run_conf_seam(lib_dir: &Path) -> Result<Conf, String> {
    let mut child = Command::new("bash")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("bash: {e}"))?;
    let mut input = CONF_SCRIPT.as_bytes().to_vec();
    input.extend_from_slice(lib_dir.as_os_str().as_encoded_bytes());
    input.push(0);
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(&input);
    }
    let mut out = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_end(&mut out);
    }
    let rc = child.wait().ok().and_then(|s| s.code()).unwrap_or(127);
    let out = String::from_utf8_lossy(&out);
    if let Some(i) = out.rfind(MARK) {
        let logs = &out[..i];
        if !logs.is_empty() {
            eprint!("{logs}");
        }
    }
    if rc != 0 {
        return Err(format!("lib.sh conf seam exited {rc}"));
    }
    parse_conf(&out)
}

impl LibSeam for Real {
    fn conf(&self) -> Result<Conf, String> {
        run_conf_seam(&self.suite_dir)
    }
}

// ----------------------------------------------------------------------------- scripts

impl Intake for Real {
    fn file(&self, f: &FlakeFiling) -> Result<String, String> {
        let Some(incident) = self.incident.as_ref().filter(|i| fs::File::open(i).is_ok()) else {
            return Err(format!(
                "no intake (incident.sh is not on PATH) — {} flake finding reaches nobody",
                f.suite
            ));
        };
        let mut child = Command::new("bash")
            .arg(incident)
            .arg("file")
            .arg(&f.title)
            .arg("-")
            .env("SPIRA_INCIDENT_TYPE", "bug")
            .env("SPIRA_INCIDENT_PRIORITY", f.priority.to_string())
            .env("SPIRA_INCIDENT_ACTOR", "suites")
            .env("SPIRA_INCIDENT_LABELS", &f.labels)
            .env("SPIRA_INCIDENT_REPO", &f.repo)
            .env("SPIRA_INCIDENT_REF", &f.reference)
            .env("SPIRA_SIN_EXEMPT", "1")
            .env("SPIRA_INCIDENT_PATH", &f.path)
            .env("SPIRA_INCIDENT_CAUSE", "suite-flaky")
            .env("SPIRA_DB", &f.db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("incident.sh: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(f.payload.as_bytes());
        }
        let mut out = String::new();
        if let Some(mut so) = child.stdout.take() {
            let _ = so.read_to_string(&mut out);
        }
        match child.wait().ok().and_then(|s| s.code()) {
            Some(0) => Ok(out),
            _ => Err(format!(
                "the intake could not file flake finding for {} — it stays spooled and drain will retry",
                f.suite
            )),
        }
    }
}

impl Mail for Real {
    fn send_operator(&self, from: &str, subject: &str, bead: Option<&str>, body: &str) -> bool {
        let Some(mail) = self.mail.as_ref() else {
            return false;
        };
        let mut c = Command::new("bash");
        c.arg(mail)
            .args(["send", "operator", "--from", from, "--subject", subject]);
        if let Some(b) = bead {
            c.args(["--bead", b]);
        }
        let Ok(mut child) = c
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(body.as_bytes());
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    }
}

impl HostCheck for Real {
    fn count(&self, flag: &str) -> Option<String> {
        // host-check.sh on the launcher's PATH (sp-gypjk); absent or not executable is None.
        let script = crate::util::which_in(&self.path, "host-check.sh")?;
        let mut child = Command::new("bash")
            .arg(&script)
            .arg(flag)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut so = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = so.read_to_string(&mut s);
            s
        });
        let rc = wait_wall(&mut child, HOST_CHECK_WALL);
        let out = reader.join().unwrap_or_default();
        (rc == Some(0))
            .then(|| out.trim_end_matches('\n').to_string())
            .filter(|s| !s.is_empty())
    }
}

impl Queue for Real {
    fn submit(&self, branch: &str) -> bool {
        // The release's `queue`, by name on the launcher's PATH (sp-gypjk).
        let mut c = Command::new("queue");
        let err = std::io::stderr();
        c.arg("submit")
            .arg(branch)
            .stdin(Stdio::null())
            .stdout(Stdio::from(err))
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

// --------------------------------------------------------------------------------- git

fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    c
}

fn git_out(mut c: Command, stdin: Option<&[u8]>) -> Result<String, String> {
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = c.spawn().map_err(|e| format!("git: {e}"))?;
    if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
        let _ = si.write_all(data);
    }
    let o = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

impl Git for Real {
    fn commit_of(&self, repo: &Path, rev: &str) -> Option<String> {
        let mut c = git(repo);
        c.args(["rev-parse", "--verify", "-q"]).arg(format!("{rev}^{{commit}}"));
        git_out(c, None).ok().filter(|s| !s.is_empty())
    }
    fn tree_has(&self, repo: &Path, commit: &str, path: &str) -> bool {
        let mut c = git(repo);
        c.args(["cat-file", "-e"]).arg(format!("{commit}:{path}"));
        git_out(c, None).is_ok()
    }
    fn show(&self, repo: &Path, commit: &str, path: &str) -> Option<String> {
        if !self.tree_has(repo, commit, path) {
            return None;
        }
        let mut c = git(repo);
        c.arg("cat-file").arg("blob").arg(format!("{commit}:{path}"));
        c.stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null());
        let o = c.output().ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    }
    fn commit_file(
        &self,
        repo: &Path,
        parent: &str,
        path: &str,
        content: &str,
        message: &str,
        who: (&str, &str),
    ) -> Result<String, String> {
        let mut c = git(repo);
        c.args(["hash-object", "-w", "--stdin"]);
        let blob = git_out(c, Some(content.as_bytes()))?;
        let index = std::env::temp_dir().join(format!(
            "testenv-suites-index-{}-{}",
            std::process::id(),
            crate::batch::now_epoch()
        ));
        let with_index = |args: &[&str]| {
            let mut c = git(repo);
            c.env("GIT_INDEX_FILE", &index).args(args);
            git_out(c, None)
        };
        let result = (|| {
            with_index(&["read-tree", parent])?;
            with_index(&["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{path}")])?;
            let tree = with_index(&["write-tree"])?;
            let mut c = git(repo);
            c.env("GIT_AUTHOR_NAME", who.0)
                .env("GIT_AUTHOR_EMAIL", who.1)
                .env("GIT_COMMITTER_NAME", who.0)
                .env("GIT_COMMITTER_EMAIL", who.1)
                .args(["commit-tree", &tree, "-p", parent, "-F", "-"]);
            git_out(c, Some(message.as_bytes()))
        })();
        let _ = fs::remove_file(&index);
        result
    }
    fn create_branch(&self, repo: &Path, branch: &str, commit: &str) -> bool {
        let mut c = git(repo);
        c.args(["update-ref", &format!("refs/heads/{branch}"), commit, ""]);
        git_out(c, None).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("suites-real-{tag}"))
    }

    fn sh(dir: &Path, args: &[&str]) {
        let st = Command::new("git").arg("-C").arg(dir).args(args)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn conf_seam_round_trips_through_bash_and_ignores_log_lines() {
        let d = tmp("conf");
        fs::write(
            d.join("lib.sh"),
            "echo 'spira: noise while sourcing'\nSPIRA_HOME_REPO=spira; SPIRA_SCOPE_LABEL=; SPIRA_DB=/db\n\
             repo_root() { [ \"$1\" = spira ] && printf /repo || return 1; }\n\
             spira_landref() { printf local/main; }\n",
        )
        .unwrap();
        let c = run_conf_seam(&d).unwrap();
        assert_eq!(c.home_repo, "spira");
        assert_eq!(c.scope_label, Some(String::new()), "set-but-empty is not unset");
        assert_eq!(c.db, "/db");
        assert_eq!(c.repo_path, Some(PathBuf::from("/repo")));
        assert_eq!(c.landref.as_deref(), Some("local/main"));
        fs::write(d.join("lib.sh"), "spira_home_repo() { printf other; }\nrepo_root() { return 1; }\n").unwrap();
        let c = run_conf_seam(&d).unwrap();
        assert_eq!((c.home_repo.as_str(), c.scope_label.clone(), c.repo_path.clone(), c.landref.clone()), ("other", None, None, None));
        fs::write(d.join("lib.sh"), "exit 3\n").unwrap();
        assert!(run_conf_seam(&d).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn parse_conf_needs_the_mark_and_a_home_repo() {
        assert!(parse_conf("no mark").is_err());
        assert!(parse_conf("\u{1e}home_repo=\0").is_err());
        let c = parse_conf("log\n\u{1e}home_repo=x\0path_ok=1\0path=/p\0").unwrap();
        assert_eq!(c.repo_path, None);
    }

    #[test]
    fn commit_file_builds_a_branch_without_touching_the_checkout() {
        let r = tmp("git");
        sh(&r, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(r.join("spira")).unwrap();
        fs::write(r.join("spira/suite-state"), "# header\n").unwrap();
        fs::write(r.join("spira/test-a.sh"), "").unwrap();
        sh(&r, &["add", "-A"]);
        sh(&r, &["commit", "-q", "-m", "base"]);
        fs::write(r.join("dirty.txt"), "untracked").unwrap();
        let s = Settings::load(&crate::settings::Source { env: &|_: &str| None, config: None }, &r);
        let real = Real::new(&s, &|_: &str| None);
        let base = real.commit_of(&r, "main").unwrap();
        assert!(real.tree_has(&r, &base, "spira/test-a.sh"));
        assert!(!real.tree_has(&r, &base, "spira/test-z.sh"));
        assert_eq!(real.show(&r, &base, "spira/suite-state").as_deref(), Some("# header\n"));
        assert_eq!(real.show(&r, &base, "spira/absent"), None);
        let c = real
            .commit_file(&r, &base, "spira/suite-state", "# header\ntest-a.sh | disabled | t | | x\n", "suite-state: test-a.sh -> disabled  sp-emvlk\n", ("spira", "spira@spira.invalid"))
            .unwrap();
        assert!(real.create_branch(&r, "spira-suite-state/test-a-1", &c));
        assert!(!real.create_branch(&r, "spira-suite-state/test-a-1", &c), "create-only");
        let tip = real.commit_of(&r, "spira-suite-state/test-a-1").unwrap();
        assert_eq!(tip, c);
        assert_eq!(real.show(&r, &tip, "spira/suite-state").unwrap(), "# header\ntest-a.sh | disabled | t | | x\n");
        assert!(real.tree_has(&r, &tip, "spira/test-a.sh"), "the rest of the tree is the parent's");
        // the checkout is untouched: HEAD, the index and the working file
        let head = Command::new("git").arg("-C").arg(&r).args(["symbolic-ref", "--short", "HEAD"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "main");
        assert_eq!(real.commit_of(&r, "HEAD").unwrap(), base);
        assert_eq!(fs::read_to_string(r.join("spira/suite-state")).unwrap(), "# header\n");
        let st = Command::new("git").arg("-C").arg(&r).args(["status", "--porcelain"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&st.stdout).trim(), "?? dirty.txt");
        let log = Command::new("git").arg("-C").arg(&r).args(["log", "-1", "--format=%an <%ae>|%s|%P", &tip]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), format!("spira <spira@spira.invalid>|suite-state: test-a.sh -> disabled  sp-emvlk|{base}"));
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn host_check_reads_one_number_and_renders_absence_as_none() {
        let d = tmp("hc");
        fs::create_dir_all(d.join("spira")).unwrap();
        let s = Settings::load(&crate::settings::Source { env: &|_: &str| None, config: None }, &d);
        let on_path = d.join("spira").display().to_string();
        let real = Real::new(&s, &|k: &str| (k == "PATH").then(|| on_path.clone()));
        assert_eq!(real.count("--count-undeclared"), None, "absent script");
        let hc = d.join("spira/host-check.sh");
        // Written by testkit (no ETXTBSY race, testkit/DESIGN.md), then made NOT executable for
        // the first check and executable again for the rest — chmod opens nothing.
        testkit::write_exe(&hc, "case \"$1\" in --count-undeclared) echo 7;; --count-copying) exit 1;; *) :;; esac\n");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hc, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(real.count("--count-undeclared"), None, "not executable");
        fs::set_permissions(&hc, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(real.count("--count-undeclared").as_deref(), Some("7"));
        assert_eq!(real.count("--count-copying"), None, "failed");
        assert_eq!(real.count("--other"), None, "silent");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_child_past_its_wall_is_killed_and_reads_as_absent() {
        let mut c = Command::new("sleep").arg("5").spawn().unwrap();
        let t = Instant::now();
        assert_eq!(wait_wall(&mut c, Duration::from_millis(100)), None);
        assert!(t.elapsed() < Duration::from_secs(3));
    }
}
