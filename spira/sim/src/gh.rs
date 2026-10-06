use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const STATE_ENV: &str = "SIM_GH_DIR";
const STATE_FILE: &str = "state.json";
const CALL_DEADLINE: Duration = Duration::from_secs(5);
const EPOCH: &str = "2000-01-01T00:00:00Z";
const OWNER_REPO: &str = "sim/sim";

pub struct Out {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

type R<T> = Result<T, String>;

fn call(program: &str, args: &[&str], cwd: Option<&Path>, input: &[u8]) -> R<Vec<u8>> {
    let mut c = Command::new(program);
    c.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let mut child = c.spawn().map_err(|e| format!("{program}: {e}"))?;
    let mut si = child.stdin.take().ok_or("no stdin")?;
    let input = input.to_vec();
    let w = std::thread::spawn(move || {
        let _ = si.write_all(&input);
    });
    let mut so = child.stdout.take().ok_or("no stdout")?;
    let mut se = child.stderr.take().ok_or("no stderr")?;
    let r_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let r_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if start.elapsed() > CALL_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{program}: no result within {}s", CALL_DEADLINE.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let _ = w.join();
    let out = r_out.join().unwrap_or_default();
    let err = r_err.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        Err(format!("{program}: {status}: {}", String::from_utf8_lossy(&err).trim()))
    }
}

pub fn jq(json: &str, expr: &str) -> R<String> {
    let out = call("jq", &["-r", expr], None, json.as_bytes())?;
    String::from_utf8(out).map_err(|e| e.to_string())
}

pub struct State {
    dir: PathBuf,
    pub v: Value,
}

impl State {
    pub fn open(dir: &Path) -> R<State> {
        if !dir.is_dir() {
            return Err(format!("{} is not a directory: no sim gh state here", dir.display()));
        }
        let f = dir.join(STATE_FILE);
        let v = if f.is_file() {
            serde_json::from_slice(&std::fs::read(&f).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", f.display()))?
        } else {
            json!({"next": {}, "prs": [], "runs": [], "checks": [], "tags": [], "releases": [], "protection": {}})
        };
        Ok(State { dir: dir.to_path_buf(), v })
    }

    pub fn save(&self) -> R<()> {
        let tmp = self.dir.join(format!("{STATE_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.v).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.dir.join(STATE_FILE)).map_err(|e| e.to_string())
    }

    pub fn now(&self) -> String {
        std::fs::read_to_string(self.dir.join("clock")).map(|s| s.trim().to_string()).unwrap_or_else(|_| EPOCH.to_string())
    }

    pub fn set_clock(&self, t: &str) -> R<()> {
        std::fs::write(self.dir.join("clock"), t).map_err(|e| e.to_string())
    }

    fn next(&mut self, kind: &str) -> u64 {
        let n = self.v["next"][kind].as_u64().unwrap_or(1);
        self.v["next"][kind] = json!(n + 1);
        n
    }

    fn list(&self, k: &str) -> &Vec<Value> {
        self.v[k].as_array().expect("state list")
    }

    fn list_mut(&mut self, k: &str) -> &mut Vec<Value> {
        self.v[k].as_array_mut().expect("state list")
    }

    fn find(&mut self, kind: &str, key: &str, want: &Value) -> Option<&mut Value> {
        self.list_mut(kind).iter_mut().find(|x| &x[key] == want)
    }

    pub fn tag_add(&mut self, name: &str, sha: &str) -> R<()> {
        if self.list("tags").iter().any(|t| t["name"] == name) {
            return Err(format!("tag {name} already exists"));
        }
        self.list_mut("tags").push(json!({"name": name, "sha": sha}));
        Ok(())
    }

    pub fn release_add(&mut self, tag: &str, draft: bool, assets: &[String]) -> R<()> {
        if self.list("releases").iter().any(|r| r["tagName"] == tag) {
            return Err(format!("release {tag} already exists"));
        }
        let now = self.now();
        let a: Vec<Value> = assets
            .iter()
            .map(|n| json!({"name": n, "size": 0, "url": format!("https://github.com/{OWNER_REPO}/releases/download/{tag}/{n}")}))
            .collect();
        self.list_mut("releases").push(json!({
            "tagName": tag, "name": tag, "isDraft": draft, "isPrerelease": false, "publishedAt": now, "assets": a,
            "url": format!("https://github.com/{OWNER_REPO}/releases/tag/{tag}"),
        }));
        Ok(())
    }

    pub fn check_run(&mut self, sha: &str, name: &str, status: &str, conclusion: &str) {
        let id = self.next("check");
        self.list_mut("checks").push(json!({"id": id, "name": name, "head_sha": sha, "status": status, "conclusion": if conclusion.is_empty() { Value::Null } else { json!(conclusion) }}));
    }

    pub fn run_add(&mut self, s: &RunSpec) -> u64 {
        let id = self.next("run");
        let now = self.now();
        let jobs: Vec<Value> = s
            .jobs
            .iter()
            .map(|(n, st, c)| {
                let jid = self.next("job");
                json!({"id": jid, "name": n, "status": st, "conclusion": if c.is_empty() { Value::Null } else { json!(c) }, "created_at": now, "started_at": now, "completed_at": now})
            })
            .collect();
        let arts: Vec<Value> = s
            .artifacts
            .iter()
            .map(|(n, files)| {
                let aid = self.next("artifact");
                let d = self.dir.join("artifacts").join(aid.to_string());
                let _ = std::fs::create_dir_all(&d);
                for f in files {
                    let _ = std::fs::write(d.join(f), "PASS\n");
                }
                json!({"id": aid, "name": n, "files": files})
            })
            .collect();
        let wf = workflow_name(&s.workflow);
        self.list_mut("runs").push(json!({
            "databaseId": id, "workflowName": wf, "name": wf, "displayTitle": wf, "headBranch": s.branch, "headSha": s.sha,
            "event": s.event, "status": s.status, "conclusion": s.conclusion, "createdAt": now, "updatedAt": now,
            "url": format!("https://github.com/{OWNER_REPO}/actions/runs/{id}"), "jobs": jobs, "artifacts": arts, "inputs": s.inputs,
        }));
        id
    }

    pub fn run_complete(&mut self, id: u64, conclusion: &str) -> R<()> {
        let now = self.now();
        let r = self.find("runs", "databaseId", &json!(id)).ok_or(format!("no run {id}"))?;
        r["status"] = json!("completed");
        r["conclusion"] = json!(conclusion);
        r["updatedAt"] = json!(now);
        Ok(())
    }

    pub fn pr_merge(&mut self, n: u64) -> R<()> {
        let now = self.now();
        let p = self.find("prs", "number", &json!(n)).ok_or(format!("no pull request {n}"))?;
        if p["state"] != "OPEN" {
            return Err(format!("pull request {n} is not open"));
        }
        p["state"] = json!("MERGED");
        p["mergedAt"] = json!(now);
        p["mergeSha"] = p["headRefOid"].clone();
        Ok(())
    }

    pub fn pr_set_mergeable(&mut self, n: u64, mergeable: &str, status: &str) -> R<()> {
        let p = self.find("prs", "number", &json!(n)).ok_or(format!("no pull request {n}"))?;
        p["mergeable"] = json!(mergeable);
        p["mergeStateStatus"] = json!(status);
        Ok(())
    }
}

#[derive(Default)]
pub struct RunSpec {
    pub workflow: String,
    pub branch: String,
    pub sha: String,
    pub event: String,
    pub status: String,
    pub conclusion: String,
    pub jobs: Vec<(String, String, String)>,
    pub artifacts: Vec<(String, Vec<String>)>,
    pub inputs: Map<String, Value>,
}

fn workflow_name(w: &str) -> String {
    match w.strip_suffix(".yml").or_else(|| w.strip_suffix(".yaml")) {
        Some(stem) => {
            let mut c = stem.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
        None => w.to_string(),
    }
}

struct Flags {
    valued: Vec<(String, String)>,
    bools: Vec<String>,
    pos: Vec<String>,
}

impl Flags {
    fn get(&self, k: &str) -> Option<&str> {
        self.valued.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    fn all(&self, k: &str) -> Vec<&str> {
        self.valued.iter().filter(|(n, _)| n == k).map(|(_, v)| v.as_str()).collect()
    }
    fn has(&self, k: &str) -> bool {
        self.bools.iter().any(|b| b == k)
    }
}

fn parse(args: &[String], valued: &[(&str, &str)], bools: &[&str]) -> R<Flags> {
    let mut f = Flags { valued: vec![], bools: vec![], pos: vec![] };
    let canon = |a: &str| -> Option<String> {
        valued.iter().find(|(n, s)| *n == a || (!s.is_empty() && *s == a)).map(|(n, _)| n.to_string())
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if a == "-" || !a.starts_with('-') {
            f.pos.push(a.to_string());
            continue;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (a, None),
        };
        if let Some(c) = canon(name) {
            let v = match inline {
                Some(v) => v,
                None => {
                    let v = args.get(i).ok_or(format!("flag needs an argument: {name}"))?.clone();
                    i += 1;
                    v
                }
            };
            f.valued.push((c, v));
        } else if bools.contains(&name) {
            f.bools.push(name.to_string());
        } else {
            return Err(format!("unknown flag: {name}"));
        }
    }
    Ok(f)
}

fn read_stdin() -> R<Vec<u8>> {
    let mut b = Vec::new();
    std::io::stdin().read_to_end(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

fn project(v: &Value, fields: &str) -> R<Value> {
    let mut m = Map::new();
    for f in fields.split(',').filter(|f| !f.is_empty()) {
        let x = v.get(f).ok_or(format!("Unknown JSON field: \"{f}\""))?;
        m.insert(f.to_string(), x.clone());
    }
    Ok(Value::Object(m))
}

fn emit(json: Value, q: Option<&str>) -> R<Out> {
    let text = serde_json::to_string(&json).map_err(|e| e.to_string())?;
    let stdout = match q {
        Some(expr) => jq(&text, expr)?,
        None => text + "\n",
    };
    Ok(Out { code: 0, stdout: stdout.into_bytes(), stderr: String::new() })
}

fn text(s: String) -> R<Out> {
    Ok(Out { code: 0, stdout: s.into_bytes(), stderr: String::new() })
}

fn resolve_sha(cwd: &Path, rev: &str) -> R<String> {
    for cand in [format!("refs/remotes/origin/{rev}^{{commit}}"), format!("{rev}^{{commit}}")] {
        if let Ok(o) = call("git", &["-C", &cwd.to_string_lossy(), "rev-parse", "--verify", "-q", &cand], None, b"") {
            return Ok(String::from_utf8_lossy(&o).trim().to_string());
        }
    }
    Err(format!("could not resolve {rev} in {}", cwd.display()))
}

pub fn run(state_dir: &Path, cwd: &Path, args: &[String]) -> R<Out> {
    let mut st = State::open(state_dir)?;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--repo" | "-R" => {
                it.next().ok_or("flag needs an argument: --repo")?;
            }
            _ if a.starts_with("--repo=") => {}
            _ => rest.push(a.clone()),
        }
    }
    let verb: Vec<&str> = rest.iter().take(2).map(String::as_str).collect();
    let tail = &rest[rest.len().min(2)..];
    let out = match verb.as_slice() {
        ["api", ..] => api(&mut st, &rest[1..]),
        ["pr", "create"] => pr_create(&mut st, cwd, tail),
        ["pr", "view"] => pr_view(&st, tail),
        ["pr", "list"] => pr_list(&st, tail),
        ["pr", "close"] => pr_close(&mut st, tail),
        ["pr", "comment"] => pr_comment(&mut st, tail),
        ["pr", "merge"] => pr_automerge(&mut st, tail),
        ["run", "list"] => run_list(&st, tail),
        ["run", "view"] => run_view(&st, tail),
        ["run", "cancel"] => run_cancel(&mut st, tail),
        ["run", "rerun"] => run_rerun(&mut st, tail),
        ["workflow", "run"] => workflow_run(&mut st, cwd, tail),
        ["release", "list"] => release_list(&st, tail),
        ["release", "view"] => release_view(&st, tail),
        _ => Err(format!("sim gh: no such verb: {}", rest.join(" "))),
    }?;
    if out.code == 0 {
        st.save()?;
    }
    Ok(out)
}

fn select_pr<'a>(st: &'a State, sel: &str) -> R<&'a Value> {
    let by_num = sel.rsplit('/').next().and_then(|n| n.parse::<u64>().ok());
    let prs = st.list("prs");
    let found = match by_num {
        Some(n) => prs.iter().find(|p| p["number"] == n),
        None => prs.iter().rev().find(|p| p["headRefName"] == sel && p["state"] == "OPEN").or_else(|| prs.iter().rev().find(|p| p["headRefName"] == sel)),
    };
    found.ok_or(format!("no pull requests found for {sel}"))
}

fn pr_create(st: &mut State, cwd: &Path, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--head", "-H"), ("--base", "-B"), ("--title", "-t"), ("--body", "-b"), ("--body-file", "-F")], &[])?;
    let head = f.get("--head").ok_or("--head is required")?.to_string();
    let base = f.get("--base").ok_or("--base is required")?.to_string();
    let title = f.get("--title").ok_or("--title is required")?.to_string();
    let body = match (f.get("--body-file"), f.get("--body")) {
        (Some("-"), _) => String::from_utf8_lossy(&read_stdin()?).into_owned(),
        (Some(p), _) => std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?,
        (None, Some(b)) => b.to_string(),
        (None, None) => String::new(),
    };
    if let Some(p) = st.list("prs").iter().find(|p| p["headRefName"] == head.as_str() && p["baseRefName"] == base.as_str() && p["state"] == "OPEN") {
        return Ok(Out {
            code: 1,
            stdout: Vec::new(),
            stderr: format!("a pull request for branch \"{head}\" into branch \"{base}\" already exists:\n{}\n", p["url"].as_str().unwrap_or("")),
        });
    }
    let sha = resolve_sha(cwd, &head)?;
    let n = st.next("pr");
    let url = format!("https://github.com/{OWNER_REPO}/pull/{n}");
    let now = st.now();
    st.list_mut("prs").push(json!({
        "number": n, "title": title, "body": body, "state": "OPEN", "headRefName": head, "baseRefName": base,
        "headRefOid": sha, "url": url, "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "isDraft": false,
        "autoMergeRequest": Value::Null, "comments": [], "createdAt": now, "mergedAt": Value::Null, "mergeSha": Value::Null,
    }));
    text(format!("{url}\n"))
}

fn json_or_text(items: Vec<&Value>, f: &Flags, render: impl Fn(&Value) -> String) -> R<Out> {
    match f.get("--json") {
        Some(fields) => {
            let arr: R<Vec<Value>> = items.iter().map(|p| project(p, fields)).collect();
            emit(Value::Array(arr?), f.get("--jq"))
        }
        None => text(items.iter().map(|p| render(p) + "\n").collect()),
    }
}

fn pr_view(st: &State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--json", ""), ("--jq", "-q")], &[])?;
    let sel = f.pos.first().ok_or("accepts 1 arg(s), received 0")?;
    let p = select_pr(st, sel)?;
    match f.get("--json") {
        Some(fields) => emit(project(p, fields)?, f.get("--jq")),
        None => text(format!("title:\t{}\nstate:\t{}\nnumber:\t{}\nurl:\t{}\n--\n{}\n", p["title"].as_str().unwrap_or(""), p["state"].as_str().unwrap_or(""), p["number"], p["url"].as_str().unwrap_or(""), p["body"].as_str().unwrap_or(""))),
    }
}

fn pr_list(st: &State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--json", ""), ("--jq", "-q"), ("--state", "-s"), ("--limit", "-L"), ("--head", "-H"), ("--base", "-B")], &[])?;
    let state = f.get("--state").unwrap_or("open").to_uppercase();
    let limit: usize = f.get("--limit").map(|l| l.parse().map_err(|_| format!("invalid --limit {l}"))).transpose()?.unwrap_or(30);
    let items: Vec<&Value> = st
        .list("prs")
        .iter()
        .rev()
        .filter(|p| state == "ALL" || p["state"] == state.as_str())
        .filter(|p| f.get("--head").is_none_or(|h| p["headRefName"] == h))
        .filter(|p| f.get("--base").is_none_or(|b| p["baseRefName"] == b))
        .take(limit)
        .collect();
    json_or_text(items, &f, |p| format!("{}\t{}\t{}\t{}", p["number"], p["title"].as_str().unwrap_or(""), p["headRefName"].as_str().unwrap_or(""), p["state"].as_str().unwrap_or("")))
}

fn pr_target(st: &mut State, f: &Flags) -> R<u64> {
    let sel = f.pos.first().ok_or("accepts 1 arg(s), received 0")?;
    select_pr(st, sel)?["number"].as_u64().ok_or_else(|| "pull request without a number".to_string())
}

fn pr_close(st: &mut State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--comment", "-c")], &["--delete-branch", "-d"])?;
    let n = pr_target(st, &f)?;
    let p = st.find("prs", "number", &json!(n)).ok_or("no such pull request")?;
    if p["state"] != "OPEN" {
        return Ok(Out { code: 1, stdout: Vec::new(), stderr: format!("Pull request #{n} is already {}\n", p["state"].as_str().unwrap_or("").to_lowercase()) });
    }
    p["state"] = json!("CLOSED");
    Ok(Out { code: 0, stdout: Vec::new(), stderr: format!("✓ Closed pull request #{n}\n") })
}

fn pr_comment(st: &mut State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--body", "-b"), ("--body-file", "-F")], &[])?;
    let n = pr_target(st, &f)?;
    let body = match (f.get("--body-file"), f.get("--body")) {
        (Some("-"), _) => String::from_utf8_lossy(&read_stdin()?).into_owned(),
        (Some(p), _) => std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?,
        (None, Some(b)) => b.to_string(),
        (None, None) => return Err("flag required: --body or --body-file".into()),
    };
    let now = st.now();
    let p = st.find("prs", "number", &json!(n)).ok_or("no such pull request")?;
    p["comments"].as_array_mut().expect("comments").push(json!({"body": body, "createdAt": now}));
    text(format!("https://github.com/{OWNER_REPO}/pull/{n}#issuecomment\n"))
}

fn pr_automerge(st: &mut State, args: &[String]) -> R<Out> {
    let f = parse(args, &[], &["--auto", "--squash", "--merge", "--rebase", "--delete-branch", "-d"])?;
    if !f.has("--auto") {
        return Err("sim gh: pr merge answers only --auto: a merge is the simulator's decision (sim ghctl pr-merge)".into());
    }
    let n = pr_target(st, &f)?;
    let method = if f.has("--merge") { "MERGE" } else if f.has("--rebase") { "REBASE" } else { "SQUASH" };
    let p = st.find("prs", "number", &json!(n)).ok_or("no such pull request")?;
    if p["state"] != "OPEN" {
        return Ok(Out { code: 1, stdout: Vec::new(), stderr: format!("Pull request #{n} is not open\n") });
    }
    p["autoMergeRequest"] = json!({"mergeMethod": method});
    Ok(Out { code: 0, stdout: Vec::new(), stderr: format!("✓ Pull request #{n} will be automatically merged when all requirements are met\n") })
}

fn matches_workflow(r: &Value, w: &str) -> bool {
    r["workflowName"] == w || r["workflowName"] == workflow_name(w).as_str()
}

fn run_list(st: &State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--json", ""), ("--jq", "-q"), ("--branch", "-b"), ("--workflow", "-w"), ("--limit", "-L"), ("--status", "-s"), ("--event", "-e")], &[])?;
    let limit: usize = f.get("--limit").map(|l| l.parse().map_err(|_| format!("invalid --limit {l}"))).transpose()?.unwrap_or(20);
    let items: Vec<&Value> = st
        .list("runs")
        .iter()
        .rev()
        .filter(|r| f.get("--branch").is_none_or(|b| r["headBranch"] == b))
        .filter(|r| f.get("--workflow").is_none_or(|w| matches_workflow(r, w)))
        .filter(|r| f.get("--status").is_none_or(|s| r["status"] == s || r["conclusion"] == s))
        .filter(|r| f.get("--event").is_none_or(|e| r["event"] == e))
        .take(limit)
        .collect();
    json_or_text(items, &f, |r| format!("{}\t{}\t{}\t{}", r["databaseId"], r["status"].as_str().unwrap_or(""), r["conclusion"].as_str().unwrap_or(""), r["headBranch"].as_str().unwrap_or("")))
}

fn select_run<'a>(st: &'a State, f: &Flags) -> R<&'a Value> {
    let id = f.pos.first().ok_or("run or job ID required when not running interactively")?;
    let n: u64 = id.parse().map_err(|_| format!("invalid run id {id}"))?;
    st.list("runs").iter().find(|r| r["databaseId"] == n).ok_or(format!("could not find any workflow run with ID {id}"))
}

fn run_view(st: &State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--json", ""), ("--jq", "-q")], &[])?;
    let r = select_run(st, &f)?;
    match f.get("--json") {
        Some(fields) => emit(project(r, fields)?, f.get("--jq")),
        None => text(format!("{} {}\n{}\n", r["workflowName"].as_str().unwrap_or(""), r["status"].as_str().unwrap_or(""), r["url"].as_str().unwrap_or(""))),
    }
}

fn run_cancel(st: &mut State, args: &[String]) -> R<Out> {
    let f = parse(args, &[], &[])?;
    let id = select_run(st, &f)?["databaseId"].clone();
    let now = st.now();
    let r = st.find("runs", "databaseId", &id).ok_or("no such run")?;
    if r["status"] == "completed" {
        return Ok(Out { code: 1, stdout: Vec::new(), stderr: "Cannot cancel a workflow run that is completed\n".into() });
    }
    r["status"] = json!("completed");
    r["conclusion"] = json!("cancelled");
    r["updatedAt"] = json!(now);
    Ok(Out { code: 0, stdout: Vec::new(), stderr: format!("✓ Request to cancel workflow {id} submitted.\n") })
}

fn run_rerun(st: &mut State, args: &[String]) -> R<Out> {
    let f = parse(args, &[], &["--failed"])?;
    let id = select_run(st, &f)?["databaseId"].clone();
    let now = st.now();
    let r = st.find("runs", "databaseId", &id).ok_or("no such run")?;
    if r["status"] != "completed" {
        return Ok(Out { code: 1, stdout: Vec::new(), stderr: "run cannot be rerun; this workflow run has not completed\n".into() });
    }
    r["status"] = json!("queued");
    r["conclusion"] = json!("");
    r["updatedAt"] = json!(now);
    Ok(Out { code: 0, stdout: Vec::new(), stderr: format!("✓ Requested rerun of run {id}\n") })
}

fn workflow_run(st: &mut State, cwd: &Path, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--ref", "-r"), ("--field", "-F"), ("--raw-field", "-f")], &[])?;
    let wf = f.pos.first().ok_or("workflow ID or name required")?.clone();
    let r#ref = f.get("--ref").ok_or("--ref is required: a simulated dispatch has no default branch")?.to_string();
    let sha = resolve_sha(cwd, &r#ref)?;
    let mut inputs = Map::new();
    for kv in f.all("--raw-field").into_iter().chain(f.all("--field")) {
        let (k, v) = kv.split_once('=').ok_or(format!("field {kv} requires a value separated by an '=' sign"))?;
        inputs.insert(k.to_string(), json!(v));
    }
    st.run_add(&RunSpec { workflow: wf.clone(), branch: r#ref.clone(), sha, event: "workflow_dispatch".into(), status: "queued".into(), inputs, ..Default::default() });
    text(format!("✓ Created workflow_dispatch event for {wf} at {}\n", r#ref))
}

fn release_list(st: &State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--json", ""), ("--jq", "-q"), ("--limit", "-L")], &["--exclude-drafts", "--exclude-pre-releases"])?;
    let limit: usize = f.get("--limit").map(|l| l.parse().map_err(|_| format!("invalid --limit {l}"))).transpose()?.unwrap_or(30);
    let items: Vec<&Value> = st.list("releases").iter().rev().filter(|r| !(f.has("--exclude-drafts") && r["isDraft"] == true)).take(limit).collect();
    json_or_text(items, &f, |r| format!("{}\t{}", r["tagName"].as_str().unwrap_or(""), if r["isDraft"] == true { "Draft" } else { "Latest" }))
}

fn release_view(st: &State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--json", ""), ("--jq", "-q")], &[])?;
    let tag = f.pos.first().ok_or("accepts 1 arg(s), received 0")?;
    let r = st.list("releases").iter().find(|r| r["tagName"] == tag.as_str()).ok_or("release not found")?;
    match f.get("--json") {
        Some(fields) => emit(project(r, fields)?, f.get("--jq")),
        None => text(format!("title:\t{}\ntag:\t{}\n", r["name"].as_str().unwrap_or(""), tag)),
    }
}

fn http_error(code: u16) -> R<Out> {
    let msg = match code {
        404 => "Not Found",
        _ => "Error",
    };
    Ok(Out { code: 1, stdout: Vec::new(), stderr: format!("gh: {msg} (HTTP {code})\n") })
}

fn gh_run_json(r: &Value) -> Value {
    json!({
        "id": r["databaseId"], "name": r["workflowName"], "head_branch": r["headBranch"], "head_sha": r["headSha"],
        "event": r["event"], "status": r["status"], "conclusion": if r["conclusion"] == "" { Value::Null } else { r["conclusion"].clone() },
        "created_at": r["createdAt"], "updated_at": r["updatedAt"], "html_url": r["url"],
    })
}

fn gh_pr_json(p: &Value) -> Value {
    let state = if p["state"] == "OPEN" { "open" } else { "closed" };
    json!({
        "number": p["number"], "state": state, "title": p["title"], "html_url": p["url"], "merged_at": p["mergedAt"],
        "merge_commit_sha": p["mergeSha"], "head": {"ref": p["headRefName"], "sha": p["headRefOid"]}, "base": {"ref": p["baseRefName"]},
    })
}

fn query(q: &str) -> Vec<(String, String)> {
    q.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).unwrap_or((p.to_string(), String::new()))).collect()
}

fn api(st: &mut State, args: &[String]) -> R<Out> {
    let f = parse(args, &[("--method", "-X"), ("--jq", "-q"), ("--input", ""), ("--raw-field", "-f"), ("--field", "-F"), ("--header", "-H")], &["--paginate", "--silent", "--include"])?;
    let endpoint = f.pos.first().ok_or("accepts 1 arg(s), received 0")?;
    let method = f.get("--method").unwrap_or(if f.get("--input").is_some() || !f.all("--raw-field").is_empty() { "POST" } else { "GET" }).to_uppercase();
    let path = endpoint.trim_start_matches('/').replace("{owner}", "sim").replace("{repo}", "sim");
    let (path, q) = path.split_once('?').map(|(p, q)| (p.to_string(), query(q))).unwrap_or((path, vec![]));
    let segs: Vec<&str> = path.split('/').collect();
    let (rest, repo_ok) = match segs.as_slice() {
        ["repos", _, _, rest @ ..] => (rest.to_vec(), true),
        _ => (vec![], false),
    };
    if !repo_ok {
        return http_error(404);
    }
    let qv = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let jq_expr = f.get("--jq");
    let body: Value = match (method.as_str(), rest.as_slice()) {
        ("GET", ["commits", sha, "pulls"]) => {
            let prs: Vec<Value> = st.list("prs").iter().filter(|p| p["headRefOid"] == *sha || p["mergeSha"] == *sha).map(gh_pr_json).collect();
            Value::Array(prs)
        }
        ("GET", ["commits", sha, "check-runs"]) => {
            let c: Vec<&Value> = st.list("checks").iter().filter(|c| c["head_sha"] == *sha).collect();
            json!({"total_count": c.len(), "check_runs": c})
        }
        ("GET", ["actions", "runs"]) => {
            let runs: Vec<Value> = st
                .list("runs")
                .iter()
                .rev()
                .filter(|r| qv("head_sha").is_none_or(|s| r["headSha"] == s))
                .filter(|r| qv("event").is_none_or(|e| r["event"] == e))
                .filter(|r| qv("status").is_none_or(|s| r["status"] == s || r["conclusion"] == s))
                .filter(|r| qv("branch").is_none_or(|b| r["headBranch"] == b))
                .take(qv("per_page").and_then(|n| n.parse().ok()).unwrap_or(30))
                .map(gh_run_json)
                .collect();
            json!({"total_count": runs.len(), "workflow_runs": runs})
        }
        ("GET", ["actions", "runs", id]) => match find_run(st, id) {
            Some(r) => gh_run_json(r),
            None => return http_error(404),
        },
        ("GET", ["actions", "runs", id, "jobs"]) => match find_run(st, id) {
            Some(r) => json!({"total_count": r["jobs"].as_array().map_or(0, Vec::len), "jobs": r["jobs"]}),
            None => return http_error(404),
        },
        ("GET", ["actions", "runs", id, "artifacts"]) => match find_run(st, id) {
            Some(r) => {
                let a: Vec<Value> = r["artifacts"].as_array().cloned().unwrap_or_default().iter().map(|a| json!({"id": a["id"], "name": a["name"]})).collect();
                json!({"total_count": a.len(), "artifacts": a})
            }
            None => return http_error(404),
        },
        ("POST", ["actions", "runs", id, "cancel" | "force-cancel"]) => {
            let rid: u64 = id.parse().map_err(|_| format!("invalid run id {id}"))?;
            let now = st.now();
            match st.find("runs", "databaseId", &json!(rid)) {
                Some(r) => {
                    r["status"] = json!("completed");
                    r["conclusion"] = json!("cancelled");
                    r["updatedAt"] = json!(now);
                    json!({})
                }
                None => return http_error(404),
            }
        }
        ("GET", ["actions", "artifacts", id, "zip"]) => {
            let aid: u64 = id.parse().map_err(|_| format!("invalid artifact id {id}"))?;
            let known = st.list("runs").iter().flat_map(|r| r["artifacts"].as_array().cloned().unwrap_or_default()).find(|a| a["id"] == aid);
            let Some(a) = known else { return http_error(404) };
            let files: Vec<(String, Vec<u8>)> = a["files"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|n| n.as_str())
                .map(|n| (n.to_string(), std::fs::read(st.dir.join("artifacts").join(aid.to_string()).join(n)).unwrap_or_default()))
                .collect();
            return Ok(Out { code: 0, stdout: zip(&files), stderr: String::new() });
        }
        ("GET", ["actions", "jobs", id, "logs"]) => {
            let jid: u64 = id.parse().map_err(|_| format!("invalid job id {id}"))?;
            let known = st.list("runs").iter().flat_map(|r| r["jobs"].as_array().cloned().unwrap_or_default()).any(|j| j["id"] == jid);
            if !known {
                return http_error(404);
            }
            return text(String::new());
        }
        ("GET", ["actions", "runners"]) => json!({"total_count": 0, "runners": []}),
        ("GET", ["check-runs", id, "annotations"]) if id.parse::<u64>().is_ok() => json!([]),
        ("GET", ["branches", b]) => json!({"name": b, "protected": st.v["protection"].get(*b).is_some()}),
        ("GET", ["branches", b, "protection"]) => match st.v["protection"].get(*b) {
            Some(p) => p.clone(),
            None => return http_error(404),
        },
        ("PUT", ["branches", b, "protection"]) => {
            let input = if f.get("--input") == Some("-") { read_stdin()? } else { b"{}".to_vec() };
            let v: Value = serde_json::from_slice(&input).map_err(|e| format!("--input: {e}"))?;
            st.v["protection"][*b] = v.clone();
            v
        }
        ("GET", ["releases"]) => Value::Array(st.list("releases").iter().rev().map(|r| json!({"tag_name": r["tagName"], "draft": r["isDraft"]})).collect()),
        ("GET", ["releases", "tags", tag]) => match st.list("releases").iter().find(|r| r["tagName"] == *tag) {
            Some(r) => json!({"tag_name": r["tagName"], "draft": r["isDraft"]}),
            None => return http_error(404),
        },
        ("GET", ["git", "matching-refs", "tags"]) | ("GET", ["tags"]) => Value::Array(st.list("tags").iter().map(|t| json!({"ref": format!("refs/tags/{}", t["name"].as_str().unwrap_or("")), "name": t["name"], "object": {"sha": t["sha"]}})).collect()),
        ("DELETE", ["git", "refs", "tags", tag]) => {
            let tags = st.list_mut("tags");
            let before = tags.len();
            tags.retain(|t| t["name"] != *tag);
            if tags.len() == before {
                return http_error(404);
            }
            return text(String::new());
        }
        _ => return http_error(404),
    };
    let text_body = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    if f.has("--silent") {
        return text(String::new());
    }
    match jq_expr {
        Some(e) => text(jq(&text_body, e)?),
        None => text(text_body + "\n"),
    }
}

fn find_run<'a>(st: &'a State, id: &str) -> Option<&'a Value> {
    let n: u64 = id.parse().ok()?;
    st.list("runs").iter().find(|r| r["databaseId"] == n)
}

fn crc32(b: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &x in b {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

pub fn zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let off = out.len() as u32;
        let crc = crc32(data);
        let hdr = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend(sig.to_le_bytes());
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend(20u16.to_le_bytes());
            h.extend(0u16.to_le_bytes());
            h.extend(0u16.to_le_bytes());
            h.extend(0u16.to_le_bytes());
            h.extend(0x21u16.to_le_bytes());
            h.extend(crc.to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            h
        };
        out.extend(hdr(0x0403_4b50, false));
        out.extend(name.as_bytes());
        out.extend(data);
        let mut c = hdr(0x0201_4b50, true);
        c.extend(0u16.to_le_bytes());
        c.extend(0u16.to_le_bytes());
        c.extend(0u16.to_le_bytes());
        c.extend(0u32.to_le_bytes());
        c.extend(off.to_le_bytes());
        c.extend(name.as_bytes());
        central.extend(c);
    }
    let start = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// `sim ghctl <dir> <verb> ...`: the simulator's hand on the fake forge's world — everything
/// a real GitHub does on its own (checks finishing, merges, tags) is an explicit act here.
pub fn ctl(dir: &Path, args: &[String]) -> R<String> {
    let mut st = State::open(dir)?;
    let verb = args.first().map(String::as_str).ok_or("usage: sim ghctl <dir> <verb> ...")?;
    let a = &args[1..];
    let out = match verb {
        "check-run" => {
            let f = parse(a, &[], &[])?;
            let [sha, name, status] = &f.pos[..3.min(f.pos.len())] else { return Err("usage: check-run <sha> <name> <status> [conclusion]".into()) };
            st.check_run(sha, name, status, f.pos.get(3).map(String::as_str).unwrap_or(""));
            String::new()
        }
        "run-add" => {
            let f = parse(a, &[("--workflow", ""), ("--branch", ""), ("--sha", ""), ("--event", ""), ("--status", ""), ("--conclusion", ""), ("--job", ""), ("--artifact", "")], &[])?;
            let need = |k: &str| f.get(k).map(str::to_string).ok_or(format!("run-add: {k} is required"));
            let jobs = f
                .all("--job")
                .iter()
                .map(|j| {
                    let p: Vec<&str> = j.splitn(3, ':').collect();
                    match p.as_slice() {
                        [n, s, c] => Ok((n.to_string(), s.to_string(), c.to_string())),
                        [n, s] => Ok((n.to_string(), s.to_string(), String::new())),
                        _ => Err(format!("--job {j}: want name:status[:conclusion]")),
                    }
                })
                .collect::<R<Vec<_>>>()?;
            let artifacts = f
                .all("--artifact")
                .iter()
                .map(|x| {
                    let (n, files) = x.split_once(':').ok_or(format!("--artifact {x}: want name:file[,file...]"))?;
                    Ok((n.to_string(), files.split(',').map(str::to_string).collect()))
                })
                .collect::<R<Vec<_>>>()?;
            let id = st.run_add(&RunSpec {
                workflow: f.get("--workflow").unwrap_or("Gate").to_string(),
                branch: need("--branch")?,
                sha: need("--sha")?,
                event: f.get("--event").unwrap_or("pull_request").to_string(),
                status: f.get("--status").unwrap_or("completed").to_string(),
                conclusion: f.get("--conclusion").unwrap_or("").to_string(),
                jobs,
                artifacts,
                inputs: Map::new(),
            });
            format!("{id}\n")
        }
        "run-complete" => {
            let f = parse(a, &[], &[])?;
            let [id, c] = &f.pos[..] else { return Err("usage: run-complete <id> <conclusion>".into()) };
            st.run_complete(id.parse().map_err(|_| format!("invalid run id {id}"))?, c)?;
            String::new()
        }
        "pr-merge" => {
            let f = parse(a, &[], &[])?;
            let [n] = &f.pos[..] else { return Err("usage: pr-merge <number>".into()) };
            st.pr_merge(n.parse().map_err(|_| format!("invalid pr {n}"))?)?;
            String::new()
        }
        "pr-mergeable" => {
            let f = parse(a, &[], &[])?;
            let [n, m, s] = &f.pos[..] else { return Err("usage: pr-mergeable <number> <MERGEABLE|CONFLICTING|UNKNOWN> <CLEAN|DIRTY|...>".into()) };
            st.pr_set_mergeable(n.parse().map_err(|_| format!("invalid pr {n}"))?, m, s)?;
            String::new()
        }
        "tag" => {
            let f = parse(a, &[], &[])?;
            let [name, sha] = &f.pos[..] else { return Err("usage: tag <name> <sha>".into()) };
            st.tag_add(name, sha)?;
            String::new()
        }
        "release" => {
            let f = parse(a, &[("--asset", "")], &["--draft"])?;
            let [tag] = &f.pos[..] else { return Err("usage: release <tag> [--draft] [--asset <name>]...".into()) };
            let assets: Vec<String> = f.all("--asset").iter().map(|s| s.to_string()).collect();
            st.release_add(tag, f.has("--draft"), &assets)?;
            String::new()
        }
        "clock" => {
            let f = parse(a, &[], &[])?;
            let [t] = &f.pos[..] else { return Err("usage: clock <rfc3339>".into()) };
            st.set_clock(t)?;
            String::new()
        }
        _ => return Err(format!("sim ghctl: no such verb: {verb}")),
    };
    st.save()?;
    Ok(out)
}
