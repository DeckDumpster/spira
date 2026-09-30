//! The real world: lib.sh through the seam, `gh` for the one forge question, `spira-lc`
//! by bare name on the launcher PATH.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::ports::{Base, Repo, Sent, World};
use crate::seam::{self, Op, FIELD};

pub struct Real {
    home: PathBuf,
    status: String,
    settings: BTreeMap<String, String>,
    pub submitted_label: String,
    enforce: bool,
}

/// A seam call's exit status and answer; lib.sh's own output has already been passed through.
struct Answer {
    rc: i32,
    text: String,
}

impl Real {
    /// Read the context once. Err: lib.sh could not be loaded, or it names no repository.
    pub fn new(home: PathBuf, status: Option<String>) -> Result<(Real, Vec<Repo>), String> {
        let mut r = Real {
            home,
            status: status.unwrap_or_default(),
            settings: BTreeMap::new(),
            submitted_label: String::new(),
            enforce: spira_config::lifecycle_enforce(None),
        };
        let a = r.seam(Op::Context, &[]);
        if a.rc != 0 {
            return Err(format!("the lib.sh context seam exited {}", a.rc));
        }
        let mut repos = Vec::new();
        for rec in a.text.split('\0').filter(|s| !s.is_empty()) {
            let Some((k, v)) = rec.split_once('=') else { continue };
            if k == "repo" {
                let f: Vec<&str> = v.split(FIELD).collect();
                if f.len() != 4 {
                    return Err(format!("malformed repository record {v:?}"));
                }
                repos.push(Repo { name: f[0].into(), root: (f[1] == "1").then(|| PathBuf::from(f[2])), queued: f[3] == "1" });
            } else {
                r.settings.insert(k.into(), v.into());
            }
        }
        if repos.is_empty() {
            return Err("spira_repos names no repository — nothing could be judged".into());
        }
        r.submitted_label = r.setting("submitted", "spira-submitted");
        Ok((r, repos))
    }

    fn setting(&self, k: &str, default: &str) -> String {
        self.settings.get(k).filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| default.to_string())
    }

    fn seam(&self, op: Op, vals: &[&str]) -> Answer {
        let home = self.home.to_string_lossy().into_owned();
        let mut all = vec![home.as_str(), self.status.as_str()];
        all.extend_from_slice(vals);
        // `-s sending.seam.sh`: bash still reads the script from stdin; the extra word is only
        // so lib.sh's spira_caller (which names the `*.sh` programs up the process chain in
        // every reap-log line) can name this one — a bare `bash` under a binary names nothing.
        let child = Command::new("bash")
            .args(["-s", "sending.seam.sh"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();
        let Ok(mut child) = child else { return Answer { rc: 127, text: String::new() } };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(&seam::stdin_bytes(op, &all));
        }
        let Ok(out) = child.wait_with_output() else { return Answer { rc: 127, text: String::new() } };
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let (logs, answer) = seam::split(&stdout);
        if !logs.is_empty() {
            print!("{logs}");
            if !logs.ends_with('\n') {
                println!();
            }
        }
        Answer { rc: out.status.code().unwrap_or(1), text: answer.to_string() }
    }
}

fn p(x: &Path) -> String {
    x.to_string_lossy().into_owned()
}

impl World for Real {
    fn base(&self, root: &Path) -> Option<Base> {
        let a = self.seam(Op::Base, &[&p(root)]);
        if a.rc != 0 {
            return None;
        }
        let f: Vec<&str> = a.text.split(FIELD).collect();
        let landref = f.first().filter(|s| !s.is_empty())?.to_string();
        let landrefs = f.get(1).map(|s| s.split_whitespace().map(String::from).collect()).unwrap_or_default();
        let remote = f.get(2).filter(|s| !s.is_empty()).map(|s| s.to_string());
        Some(Base { landref, landrefs, remote })
    }
    fn witness(&self, id: &str) -> Option<String> {
        let a = self.seam(Op::Witness, &[id]);
        (a.rc == 0).then_some(a.text)
    }
    fn bead(&self, id: &str) -> Option<Value> {
        let a = self.seam(Op::Bead, &[id]);
        let v: Value = serde_json::from_str(a.text.trim()).ok()?;
        match v {
            Value::Array(mut xs) if !xs.is_empty() => Some(xs.swap_remove(0)),
            Value::Object(_) => Some(v),
            _ => None,
        }
    }
    fn send(&self, id: &str, br: &str, repo: &Path, why: &str, caller: &str) -> Sent {
        let a = self.seam(Op::Send, &[id, br, &p(repo), why, caller]);
        match a.rc {
            0 => Sent::Done,
            10 => Sent::Held(a.text),
            _ if a.text == "certified-queued" => Sent::Queued,
            _ => Sent::Failed(a.text),
        }
    }
    fn close_on_land(&self, id: &str, sha: &str) {
        self.seam(Op::CloseOnLand, &[id, sha]);
    }
    fn destroy_worktree(&self, id: &str, w: &Path, repo: &Path, why: &str) -> bool {
        self.seam(Op::DestroyWorktree, &[id, &p(w), &p(repo), why]).rc == 0
    }
    fn prune(&self, repo: &Path) {
        self.seam(Op::Prune, &[&p(repo)]);
    }
    fn label_add(&self, id: &str, label: &str) {
        self.seam(Op::LabelAdd, &[id, label]);
    }
    fn content_on_base(&self, id: &str, proof: &str) {
        let _ = Command::new("spira-lc")
            .args(["content-on-base", id, proof, "sending"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    fn pr_merged_tip(&self, repo: &Path, br: &str) -> Option<String> {
        let o = Command::new("timeout")
            .arg(self.setting("gh_timeout", "120"))
            .arg(self.setting("gh", "gh"))
            .args(["pr", "view", br, "--json", "state,headRefOid", "-q", r#"select(.state=="MERGED") | .headRefOid"#])
            .current_dir(repo)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
    fn enforce(&self) -> bool {
        self.enforce
    }
    fn emit(&self, line: &str) {
        println!("{line}");
    }
    fn log(&self, msg: &str) {
        println!("{} spira: {msg}", utc_now());
    }
    fn reaplog(&self) -> String {
        self.setting("reaplog", "the reap log")
    }
    fn worktrees(&self) -> PathBuf {
        Path::new(&self.setting("run", "")).join("worktree")
    }
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ`.
fn utc_now() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, (secs % 3600) / 60, secs % 60)
}
