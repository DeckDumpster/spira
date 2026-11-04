use crate::ports::{Bd, Gh, Git, Http, LcBead, Lifecycle, Mail, Repo, Seen, Stamp};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Mints the credential for one run; the Http port itself carries none (law-beads-is-never-public).
pub type Mint = Box<dyn Fn() -> Result<String, String>>;

pub struct RealHttp {
    mint: Mint,
    bearer: std::cell::OnceCell<Result<String, String>>,
}

impl RealHttp {
    pub fn new(mint: Mint) -> Self {
        RealHttp { mint, bearer: std::cell::OnceCell::new() }
    }

    /// The forge's App installation token, from `broker token`.
    pub fn from_broker() -> Self {
        Self::new(Box::new(|| {
            let bin = spira_config::release_env::tool("broker")?;
            let o = spira_config::bounded::bounded(&bin)
                .arg("token")
                .stdin(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("{bin}: {e}"))?;
            let tok = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && !tok.is_empty() {
                Ok(tok)
            } else {
                Err(format!("{bin} token exited {:?}: {}", o.status.code(), String::from_utf8_lossy(&o.stderr).trim()))
            }
        }))
    }

    fn bearer(&self) -> Result<&str, String> {
        self.bearer.get_or_init(|| (self.mint)()).as_deref().map_err(|e| format!("no App token: {e}"))
    }
}

impl Http for RealHttp {
    fn get(&self, url: &str, timeout_secs: u64) -> Result<(u16, Vec<u8>), String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(timeout_secs))
            .build();
        let bearer = self.bearer()?;
        match agent.get(url).set("Authorization", &format!("Bearer {bearer}")).call() {
            Ok(resp) => {
                let status = resp.status();
                let mut buf = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut buf)
                    .map_err(|e| e.to_string())?;
                Ok((status, buf))
            }
            Err(ureq::Error::Status(code, resp)) => {
                let mut buf = Vec::new();
                let _ = resp.into_reader().read_to_end(&mut buf);
                Ok((code, buf))
            }
            Err(ureq::Error::Transport(t)) => Err(t.to_string()),
        }
    }
}

use std::io::Read;

/// The last-success epoch, kept in one file under the run dir.
pub struct FileStamp(pub std::path::PathBuf);

impl Stamp for FileStamp {
    fn now(&self) -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }
    fn last_ok(&self) -> Option<i64> {
        std::fs::read_to_string(&self.0).ok()?.trim().parse().ok()
    }
    fn mark_ok(&self, at: i64) {
        let tmp = self.0.with_extension("tmp");
        if std::fs::write(&tmp, at.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.0);
        }
    }
}

/// One file per repository under `dir`, one issue number per line.
pub struct FileSeen(pub std::path::PathBuf);

impl FileSeen {
    fn path(&self, slug: &str) -> std::path::PathBuf {
        self.0.join(slug.replace('/', "__"))
    }
}

impl Seen for FileSeen {
    fn load(&self, slug: &str) -> Option<Vec<u64>> {
        let text = std::fs::read_to_string(self.path(slug)).ok()?;
        Some(text.lines().filter_map(|l| l.trim().parse().ok()).collect())
    }
    fn save(&self, slug: &str, numbers: &[u64]) {
        let path = self.path(slug);
        let tmp = path.with_extension("tmp");
        let body: String = numbers.iter().map(|n| format!("{n}\n")).collect();
        if std::fs::create_dir_all(&self.0).is_ok() && std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

pub struct RealLifecycle {
    lc: spira_config::lc_call::LcClient,
}

impl RealLifecycle {
    pub fn new() -> Result<Self, String> {
        Ok(RealLifecycle { lc: spira_config::lc_call::LcClient::configured()? })
    }

    /// Every read through `lc`.
    pub fn over(lc: spira_config::lc_call::LcClient) -> Self {
        RealLifecycle { lc }
    }
}

impl Lifecycle for RealLifecycle {
    fn bead(&self, id: &str) -> Result<Option<LcBead>, String> {
        let r = self.lc.call(&["show", id]).map_err(String::from)?;
        parse_show(Some(r.code), r.out.as_bytes())
    }

    fn all(&self) -> Result<std::collections::BTreeMap<String, LcBead>, String> {
        let read = |extra: &[&str]| -> Result<serde_json::Value, String> {
            let argv: Vec<&str> = std::iter::once("list").chain(extra.iter().copied()).collect();
            let r = self.lc.call(&argv).map_err(String::from)?;
            if r.code != 0 {
                return Err(format!("spira-lc list exited {:?}", Some(r.code)));
            }
            serde_json::from_str(&r.out).map_err(|e| format!("spira-lc list: unparsed reply: {e}"))
        };
        parse_list(&read(&[])?, &read(&["--delivery"])?)
    }
}

/// `spira-lc list` rows joined with `spira-lc list --delivery` rows: a landed bead's sha is its
/// delivery's merge sha, else its tip, exactly as [`parse_show`] reads it.
pub fn parse_list(beads: &serde_json::Value, deliveries: &serde_json::Value) -> Result<std::collections::BTreeMap<String, LcBead>, String> {
    let text = |r: &serde_json::Value, k: &str| r.get(k).and_then(|t| t.as_str()).unwrap_or("").to_string();
    let rows = |v: &serde_json::Value, what: &str| v.as_array().cloned().ok_or(format!("spira-lc list: {what} is not an array"));
    let merges: std::collections::BTreeMap<String, String> =
        rows(deliveries, "delivery reply")?.iter().map(|r| (text(r, "bead_id"), text(r, "merge_sha"))).filter(|(_, m)| !m.is_empty()).collect();
    let mut out = std::collections::BTreeMap::new();
    for r in rows(beads, "reply")? {
        let id = text(&r, "bead_id");
        let sha = merges.get(&id).cloned().unwrap_or_else(|| text(&r, "tip"));
        out.insert(id, LcBead { state: text(&r, "state"), sha, reason: text(&r, "reason") });
    }
    Ok(out)
}

/// `spira-lc show`: exit 0 with the row, exit 1 with `{}` when the machine has no such bead,
/// anything else is no answer.
pub fn parse_show(code: Option<i32>, stdout: &[u8]) -> Result<Option<LcBead>, String> {
    match code {
        Some(1) => return Ok(None),
        Some(0) => {}
        other => return Err(format!("spira-lc show exited {other:?}")),
    }
    let v: serde_json::Value = serde_json::from_slice(stdout).map_err(|e| format!("spira-lc show: unparsed reply: {e}"))?;
    let Some(bead) = v.get("bead") else { return Ok(None) };
    let text = |x: Option<&serde_json::Value>| x.and_then(|t| t.as_str()).unwrap_or("").to_string();
    let merge = text(v.get("delivery").and_then(|d| d.get("merge_sha")));
    let sha = if merge.is_empty() { text(bead.get("tip")) } else { merge };
    Ok(Some(LcBead { state: text(bead.get("state")), sha, reason: text(bead.get("reason")) }))
}

pub struct RealBd {
    pub bdq: String,
}

impl RealBd {
    fn cmd(&self) -> Command {
        // FORK-EDGE: content store write (create/note/dep); its reads (list, show) are unserved verb content
        Command::new(&self.bdq)
    }
}

impl Bd for RealBd {
    fn list_all_json(&self) -> Option<serde_json::Value> {
        let out = self
            .cmd()
            .args(["list", "--all", "--limit", "0", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    fn create(&self, title: &str, external_ref: &str, labels: &str, priority: &str, body: &[u8]) -> bool {
        let mut child = match self
            .cmd()
            .arg("create")
            .arg(title)
            .arg("--external-ref")
            .arg(external_ref)
            .arg("--labels")
            .arg(labels)
            .arg("-t")
            .arg("bug")
            .arg("-p")
            .arg(priority)
            .arg("--body-file")
            .arg("-")
            .arg("--silent")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body);
        }
        let Ok(out) = child.wait_with_output() else { return false };
        out.status.success()
    }

    fn note(&self, id: &str, text: &str) -> bool {
        let status = self
            .cmd()
            .arg("note")
            .arg(id)
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success())
    }

    // Through the lifecycle machine (sp-3fue0j), never a raw bd close.
    fn close(&self, id: &str, reason: &str) -> bool {
        spira_config::lifecycle_row::close(id, reason, "gh-intake", None).is_ok()
    }

    fn show_json(&self, id: &str) -> Option<serde_json::Value> {
        let out = self.cmd().arg("show").arg(id).arg("--json").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    fn list_asks(&self, which: spira_config::nonwork::Which, label: &str) -> Option<serde_json::Value> {
        use spira_config::nonwork::{status_args, Kind};
        let out = self
            .cmd()
            .arg("list")
            .args(status_args(Kind::Ask, which))
            .args(["--label", label, "--limit", "0", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    fn dep_remove(&self, id: &str, other: &str) -> bool {
        let status = self.cmd().args(["dep", "remove", id, other]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        matches!(status, Ok(s) if s.success())
    }

    fn dep_relate(&self, from: &str, to: &str) -> bool {
        let status = self.cmd().args(["dep", "relate", from, to]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        matches!(status, Ok(s) if s.success())
    }
}

/// `repo_root` (family U), in-process via `spira_config::repos` (sp-k6lku, "wave 4.13") —
/// no longer a `bash -c '. lib.sh; ...'` seam at all.
pub struct RealRepo {
    pub spira_home: String,
}

impl Repo for RealRepo {
    /// `repo_root <name>` (family U) in-process now (sp-k6lku, "wave 4.13") through
    /// `spira_config::repos::Registry::from_env` — the one production door onto a
    /// registry (following the structural fix for sp-z3eyk): building one from a bare
    /// `std::env::vars()` directly, with no resolution, found no map at all in production,
    /// since conf.sh exports none of `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO`/
    /// `SPIRA_REPO_DERIVED`; `from_env` resolves them in-process instead. No
    /// `bash -c '. lib.sh; repo_root ...'` subprocess at all (gh-intake is a one-shot
    /// binary, so there is no loop to amortise a cached Registry over).
    fn root_with_git(&self, name: &str) -> Option<String> {
        let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&self.spira_home));
        let root = reg.root(name)?;
        if std::path::Path::new(&root).join(".git").exists() {
            Some(root)
        } else {
            None
        }
    }

    fn forge_slug(&self, name: &str) -> Option<String> {
        let root = self.root_with_git(name)?;
        let remotes = Command::new("timeout").args(["5", "git", "-C", &root, "remote"]).stdin(Stdio::null()).output().ok()?;
        let remotes = String::from_utf8_lossy(&remotes.stdout).to_string();
        let remote = if remotes.lines().any(|r| r == "origin") { "origin" } else { remotes.lines().next()? };
        let url = Command::new("timeout").args(["5", "git", "-C", &root, "remote", "get-url", remote]).stdin(Stdio::null()).output().ok()?;
        crate::model::github_slug(&String::from_utf8_lossy(&url.stdout))
    }

    /// `spira_repos` (sp-j3fim) — in-process, same registry construction as every other
    /// call here (one-shot binary, no loop to amortise it over).
    fn all_names(&self) -> Vec<String> {
        let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&self.spira_home));
        reg.all()
    }

    /// `spira_landref <repo-path>` (family W, sp-j3fim) — in-process.
    fn landref(&self, repo_path: &str) -> Option<String> {
        let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&self.spira_home));
        spira_config::repos::landref(&reg, repo_path)
    }

    /// `spira_landrefs <repo-path>` (family W, sp-j3fim) — in-process; flattens the
    /// (base, local-counterpart) pair `spira_config::repos::landrefs` returns into the
    /// list of refs `Git::landing_commit` greps.
    fn landrefs(&self, repo_path: &str) -> Vec<String> {
        let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), std::path::Path::new(&self.spira_home));
        match spira_config::repos::landrefs(&reg, repo_path) {
            Some((base, Some(local))) => vec![base, local],
            Some((base, None)) => vec![base],
            None => Vec::new(),
        }
    }
}

/// One operator mail, sent in-process through the `mail` library.
pub type SendFn = Box<dyn Fn(&mail::cmds::SendArgs, String) -> Result<(), String>>;

pub struct RealMail {
    stand_in: Option<String>,
    send: SendFn,
}

impl RealMail {
    /// Production: `stand_in` (a fixture's `SPIRA_MAIL_BIN`) runs that program; with none, the
    /// `mail` library sends in-process exactly as `mail send` would.
    pub fn new(stand_in: Option<String>) -> Self {
        Self::with(stand_in, Box::new(send_in_process))
    }

    pub fn with(stand_in: Option<String>, send: SendFn) -> Self {
        RealMail { stand_in, send }
    }
}

/// `mail send`'s own path: the config, the bead store's door, then the recorded send.
fn send_in_process(args: &mail::cmds::SendArgs, body: String) -> Result<(), String> {
    let env = mail::env::Env::load().map_err(|e| format!("mail: {e}"))?;
    mail::maildir::mailbox_valid(args.mailbox).map_err(|e| format!("mail: send: {e}"))?;
    let bdq = spira_config::release_env::tool("bdq").map_err(|e| format!("mail: {e}"))?;
    let bd = mail::bead::BdCli { bdq, conn_retries: env.bd_conn_retries };
    mail::cmds::send_recorded(&bd, &env, args, body).map(|_| ()).map_err(|e| format!("mail: send: {e}"))
}

impl RealMail {
    /// `mail send operator <flags>` with `body` on stdin, through the fixture's stand-in
    /// program: stdout discarded, stderr the error text.
    fn stand_in_send(bin: &str, flags: &[&str], body: &[u8]) -> Result<(), String> {
        // FORK-EDGE: a fixture's SPIRA_MAIL_BIN stand-in only; production sends in-process
        let mut child = spira_config::bounded::bounded(bin)
            .arg("send")
            .arg("operator")
            .args(flags)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body);
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn deliver(&self, args: &mail::cmds::SendArgs, flags: &[&str], body: &[u8]) -> Result<(), String> {
        match &self.stand_in {
            Some(bin) => Self::stand_in_send(bin, flags, body),
            None => (self.send)(args, String::from_utf8_lossy(body).into_owned()),
        }
    }
}

fn operator_args<'a>(from: &'a str, subject: &'a str, kind: &'a str, default: &'a str, bead: &'a str) -> mail::cmds::SendArgs<'a> {
    mail::cmds::SendArgs {
        mailbox: "operator",
        from: Some(from),
        subject,
        kind,
        default,
        class: "",
        bead,
        urgent: false,
        digest: false,
        dry_run: false,
        requested_by: "",
    }
}

impl Mail for RealMail {
    fn send_operator_note(&self, subject: &str, body: &[u8]) -> bool {
        let from = "gh-intake <intake@spira>";
        let flags = ["--from", from, "--subject", subject, "--kind", "note"];
        self.deliver(&operator_args(from, subject, "note", "", ""), &flags, body).is_ok()
    }

    /// lib.sh `gh_issue_ask_unlanded`'s mail call (sp-j3fim): the error text is what
    /// `mail send` says on stderr when it refuses.
    fn send_question(&self, from: &str, subject: &str, default: &str, bead_id: &str, body: &[u8]) -> Result<(), String> {
        let flags = ["--from", from, "--subject", subject, "--kind", "question", "--default", default, "--bead", bead_id];
        self.deliver(&operator_args(from, subject, "question", default, bead_id), &flags, body)
    }
}

/// `ghq` is NOT its own binary — lib.sh's shim is `ghq() { command bdq __ghq "$@"; }`,
/// and that shim was never ported to a standalone executable (bead/src/bin/bdq.rs's
/// `cmd_ghq` is reached only through `bdq __ghq ...`). This execs `bdq` with that
/// internal subcommand prepended, by bare name on the launcher PATH, the same door every
/// bash family already shells through for `gh` access (sp-j3fim).
pub struct RealGh {
    pub bdq_bin: String,
}

impl RealGh {
    fn cmd(&self) -> Command {
        // FORK-EDGE: needs bdq's __ghq (gh with the App token) as a library call; gh itself stays the child
        let mut c = Command::new(&self.bdq_bin);
        c.arg("__ghq");
        c
    }
}

impl Gh for RealGh {
    fn issue_state(&self, repo: &str, issue_n: &str) -> String {
        let out = self
            .cmd()
            .args(["issue", "view", issue_n, "--repo", repo, "--json", "state", "-q", ".state"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => String::new(),
        }
    }
    fn issue_comment(&self, repo: &str, issue_n: &str, body: &str) -> bool {
        let status = self
            .cmd()
            .args(["issue", "comment", issue_n, "--repo", repo, "--body", body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success())
    }
    fn issue_close(&self, repo: &str, issue_n: &str) -> bool {
        let status = self
            .cmd()
            .args(["issue", "close", issue_n, "--repo", repo])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success())
    }
}

/// Plain git (sp-j3fim) — every call reads a checkout already on disk; no credential.
pub struct RealGit;

impl RealGit {
    fn cmd(repo: &Path) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(repo);
        c
    }
}

impl Git for RealGit {
    fn rev_parse_short(&self, repo: &Path, sha: &str) -> Option<String> {
        let o = Self::cmd(repo).args(["rev-parse", "--short", sha]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        if !o.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn subject_of(&self, repo: &Path, sha: &str) -> Option<String> {
        let o = Self::cmd(repo).args(["log", "--format=%s", "-1", sha]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    fn landing_commit(&self, repo: &Path, id: &str, refs: &[String]) -> Option<String> {
        if refs.is_empty() {
            return None;
        }
        let grep = format!("--grep={id}");
        let mut args: Vec<&str> = vec!["log", "--format=%H%x09%s", grep.as_str(), "-F"];
        args.extend(refs.iter().map(String::as_str));
        let o = Self::cmd(repo).args(&args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        if !o.status.success() {
            return None;
        }
        let out = String::from_utf8_lossy(&o.stdout);
        let land = format!("spira: land {id}");
        let own = format!("{id}:");
        for line in out.lines() {
            let Some((sha, subj)) = line.split_once('\t') else { continue };
            if subj == land || subj.starts_with(&format!("{land} ")) || subj.starts_with(&own) {
                return Some(sha.to_string());
            }
        }
        None
    }

    fn grep_ancestor(&self, repo: &Path, id: &str, land_ref: &str) -> Option<String> {
        let grep = format!("--grep={id}");
        let o = Self::cmd(repo)
            .args(["log", "--format=%H", grep.as_str(), land_ref])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        String::from_utf8_lossy(&o.stdout).lines().next().map(str::to_string).filter(|s| !s.is_empty())
    }

    fn sha_is_ancestor(&self, repo: &Path, sha: &str, land_ref: &str) -> bool {
        let exists = Self::cmd(repo).args(["cat-file", "-e", sha]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if !exists {
            return false;
        }
        Self::cmd(repo)
            .args(["merge-base", "--is-ancestor", sha, land_ref])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn show_reads_state_and_the_deliverys_merge_sha_else_the_tip() {
        let landed = br#"{"bead":{"state":"LANDED","tip":"tt"},"delivery":{"merge_sha":"mm"}}"#;
        let b = parse_show(Some(0), landed).unwrap().unwrap();
        assert_eq!((b.state.as_str(), b.sha.as_str()), ("LANDED", "mm"));
        let certified = br#"{"bead":{"state":"CERTIFIED","tip":"tt"},"delivery":null}"#;
        assert_eq!(parse_show(Some(0), certified).unwrap().unwrap().sha, "tt");
    }

    #[test]
    fn list_rows_read_as_show_reads_them() {
        let beads = serde_json::json!([{"bead_id":"a","state":"LANDED","tip":"tt","reason":""},{"bead_id":"b","state":"SUPERSEDED","tip":"","reason":"a"}]);
        let dels = serde_json::json!([{"bead_id":"a","merge_sha":"mm"}]);
        let m = parse_list(&beads, &dels).unwrap();
        assert_eq!((m["a"].state.as_str(), m["a"].sha.as_str()), ("LANDED", "mm"));
        assert_eq!(m["b"].reason, "a");
        assert!(parse_list(&serde_json::json!({}), &dels).is_err());
    }

    #[test]
    fn no_row_is_none_and_no_answer_is_an_error() {
        assert_eq!(parse_show(Some(1), b"{}"), Ok(None));
        assert!(parse_show(Some(2), b"").is_err());
        assert!(parse_show(None, b"").is_err());
        assert!(parse_show(Some(0), b"not json").is_err());
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use spira_config::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};

    struct Scripted(Mutex<Vec<Vec<String>>>);

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            let out = match (argv[0].as_str(), argv.get(1).map(String::as_str)) {
                ("show", Some("sp-1")) => r#"{"bead":{"state":"LANDED","tip":"tt"},"delivery":{"merge_sha":"mm"}}"#,
                ("show", _) => return Ok(Reply { code: 1, out: "{}".into(), err: String::new() }),
                ("list", Some("--delivery")) => r#"[{"bead_id":"a","merge_sha":"mm"}]"#,
                ("list", _) => r#"[{"bead_id":"a","state":"LANDED","tip":"tt","reason":""}]"#,
                _ => return Err("unscripted".into()),
            };
            Ok(Reply { code: 0, out: out.into(), err: String::new() })
        }
    }

    #[test]
    fn the_lifecycle_reads_go_through_the_store_client_not_a_program() {
        let t = Arc::new(Scripted(Mutex::default()));
        let lc = RealLifecycle::over(LcClient::over(t.clone(), true));
        assert_eq!(lc.bead("sp-1").unwrap().unwrap().sha, "mm");
        assert_eq!(lc.bead("sp-2").unwrap(), None);
        assert_eq!(lc.all().unwrap()["a"].sha, "mm");
        assert_eq!(t.0.lock().unwrap().len(), 4);
    }

    type Sent = Arc<Mutex<Vec<(String, String, String, String, String, String)>>>;

    fn recording(fail: bool) -> (RealMail, Sent) {
        let sent: Sent = Arc::default();
        let s = sent.clone();
        let send: SendFn = Box::new(move |a: &mail::cmds::SendArgs, body: String| {
            s.lock().unwrap().push((a.mailbox.into(), a.from.unwrap_or("").into(), a.kind.into(), a.default.into(), a.bead.into(), body));
            if fail { Err("mail: send: refused".into()) } else { Ok(()) }
        });
        (RealMail::with(None, send), sent)
    }

    #[test]
    fn operator_mail_is_sent_in_process_with_the_cli_flags() {
        let (m, sent) = recording(false);
        assert!(m.send_operator_note("digest", b"new untrusted"));
        assert_eq!(m.send_question("gh <g@s>", "unlanded?", "wait", "sp-9", b"why"), Ok(()));
        let got = sent.lock().unwrap().clone();
        assert_eq!(got[0], ("operator".into(), "gh-intake <intake@spira>".into(), "note".into(), "".into(), "".into(), "new untrusted".into()));
        assert_eq!(got[1], ("operator".into(), "gh <g@s>".into(), "question".into(), "wait".into(), "sp-9".into(), "why".into()));
        let (m, _) = recording(true);
        assert!(!m.send_operator_note("digest", b"x"));
        assert_eq!(m.send_question("f", "s", "d", "b", b"x"), Err("mail: send: refused".into()));
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    fn serve(n: usize) -> (String, std::thread::JoinHandle<Vec<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            (0..n)
                .map(|_| {
                    let (mut c, _) = l.accept().unwrap();
                    let mut r = BufReader::new(c.try_clone().unwrap());
                    let mut head = Vec::new();
                    loop {
                        let mut line = String::new();
                        r.read_line(&mut line).unwrap();
                        if line.trim().is_empty() {
                            break;
                        }
                        head.push(line.trim().to_string());
                    }
                    c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    head
                })
                .collect()
        });
        (url, h)
    }

    #[test]
    fn every_get_carries_the_minted_token_as_a_bearer() {
        let (url, h) = serve(3);
        let minted = std::rc::Rc::new(std::cell::Cell::new(0));
        let m = minted.clone();
        let http = RealHttp::new(Box::new(move || {
            m.set(m.get() + 1);
            Ok("tok-123".into())
        }));
        for _ in 0..3 {
            assert_eq!(http.get(&url, 5).unwrap().0, 200);
        }
        for head in h.join().unwrap() {
            assert!(head.iter().any(|l| l.eq_ignore_ascii_case("authorization: Bearer tok-123")), "{head:?}");
        }
        assert_eq!(minted.get(), 1, "one mint per run");
    }

    #[test]
    fn a_failed_mint_refuses_the_poll_rather_than_going_unauthenticated() {
        let http = RealHttp::new(Box::new(|| Err("no creds".into())));
        let err = http.get("http://127.0.0.1:1/x", 5).unwrap_err();
        assert!(err.contains("no App token") && err.contains("no creds"), "{err}");
    }
}
