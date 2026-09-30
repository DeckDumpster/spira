//! `run --attr-spool <dir>` (DESIGN.md §2.2a, sp-hvtgs): stream the corpus's results to the
//! host while it runs, and serve the batcher's attribution reruns on the same leased VM.
//!
//! The batcher writes `req/<job>.req`; this side answers with `res/<job>/` (results) and
//! `res/<job>.done` (`rc=`), writes `corpus.done` once the corpus's own results, tsd,
//! manifest and binaries are in place, and releases the VM once `close` appears.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread::Scope;
use std::time::{Duration, Instant};

use crate::run::{copy_tree, results_leaf, shell_quote, Host, Remote, REMOTE_BINS};

/// How a job's tree gets its binaries on the VM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    /// Reuse the round's own release binaries: the tree differs from the round's only in
    /// files no binary is built from.
    Artifacts,
    /// A debug (`aeon` profile) build of the job's own tree.
    Aeon,
    /// In `~/round-work` after the corpus: an incremental release build whose binaries land.
    Round,
}

impl Build {
    pub fn word(self) -> &'static str {
        match self {
            Build::Artifacts => "artifacts",
            Build::Aeon => "aeon",
            Build::Round => "round",
        }
    }
    pub fn parse(s: &str) -> Option<Build> {
        match s {
            "artifacts" => Some(Build::Artifacts),
            "aeon" => Some(Build::Aeon),
            "round" => Some(Build::Round),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub job: String,
    /// A branch of the tree-dir's repository.
    pub branch: String,
    pub suites: String,
    pub build: Build,
}

/// `key=value` lines; `branch`, `suites` and a known `build` are required.
pub fn parse_request(job: &str, text: &str) -> Result<Request, String> {
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let branch = get("branch").ok_or(format!("request {job}: no branch="))?;
    let suites = get("suites").ok_or(format!("request {job}: no suites="))?;
    let build = get("build").and_then(|b| Build::parse(&b)).ok_or(format!("request {job}: build= missing or unknown"))?;
    let ok_name = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if !ok_name(job) {
        return Err(format!("request {job:?}: a job id is [A-Za-z0-9._-]+"));
    }
    Ok(Request { job: job.to_string(), branch, suites, build })
}

/// What the VM runs for one job (`REMOTE_ATTR_SCRIPT`'s arguments).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrJob {
    pub host_addr: String,
    pub mirror_port: u16,
    pub job: String,
    /// The mirror ref the job's tree was fetched to (`attr-<job>`).
    pub mirror_ref: String,
    pub suites: String,
    pub build: Build,
    pub toolchain: Option<String>,
}

/// Remote side of one attribution rerun. Results go to `~/attr-results/<job>/<key>/`;
/// `round` also restages `~/round-bins/`.
pub const REMOTE_ATTR_SCRIPT: &str = r#"set -euo pipefail
host_addr="$1" port="$2" job="$3" ref="$4" suites="$5" build="$6" toolchain="$7"
# The launcher the corpus set up (run.rs REMOTE_SCRIPT): the round's staged release on PATH.
. "$HOME/round-launcher.env" || { echo "round-vm: no staged release on this VM (round-launcher.env)" >&2; exit 2; }
if [ -n "$toolchain" ]; then export RUSTUP_TOOLCHAIN="$toolchain"; fi
export SPIRA_VERDICT_TTL=0 SPIRA_BATCH_MAXPAR=1 SPIRA_BATCH_RESULTS="$HOME/attr-results/$job"
rm -rf "$SPIRA_BATCH_RESULTS"; mkdir -p "$SPIRA_BATCH_RESULTS"
# path-ok: the round's own testenv, built on this VM from the round tree by the corpus run
tenv="$HOME/round-work/target/release/testenv"
set +e
case "$build" in
round)
    cd "$HOME/round-work" || exit 2
    git fetch --quiet origin "+refs/heads/$ref:refs/heads/$ref" || exit 2
    git checkout --quiet --force --detach "refs/heads/$ref" || exit 2
    cargo build -q --profile release -p testenv --bin testenv || exit 4
    "$tenv" --mode parallel --profile release --suites "$suites" HEAD
    rc=$?
    rm -rf "$HOME/round-bins"; mkdir -p "$HOME/round-bins"
    find target/release -maxdepth 1 -type f -executable -exec cp {} "$HOME/round-bins/" \; 2>/dev/null
    exit "$rc" ;;
artifacts|aeon)
    d="$HOME/attr/$job"
    rm -rf "$d"; mkdir -p "$HOME/attr"
    git clone --quiet --reference "$HOME/round-work" --branch "$ref" "git://${host_addr}:${port}/mirror.git" "$d" || exit 2
    cd "$d" || exit 2
    if [ "$build" = artifacts ]; then
        "$tenv" --mode parallel --artifacts "$HOME/round-work/target/release" --suites "$suites" HEAD
    else
        "$tenv" --mode parallel --profile aeon --suites "$suites" HEAD
    fi
    rc=$?
    cd "$HOME" && rm -rf "$d"
    exit "$rc" ;;
*) exit 2 ;;
esac
"#;

pub fn attr_command(job: &AttrJob) -> String {
    let args = [
        job.host_addr.clone(),
        job.mirror_port.to_string(),
        job.job.clone(),
        job.mirror_ref.clone(),
        job.suites.clone(),
        job.build.word().to_string(),
        job.toolchain.clone().unwrap_or_default(),
    ];
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    format!("bash -s -- {}", quoted.join(" "))
}

/// Writes `body` to `path` through a temp file and a rename, so a reader never sees half.
pub fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", path.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Copies the pulled corpus results in `pulled` into `results_dir`: everything but the
/// `.result` files first, then each `.result` not yet there — so a `.result` in
/// `results_dir` still means its `.out` is complete. Returns how many results were new.
pub fn stream_into(pulled: &Path, results_dir: &Path) -> Result<usize, String> {
    let Some(leaf) = results_leaf(pulled) else { return Ok(0) };
    fs::create_dir_all(results_dir).map_err(|e| format!("{}: {e}", results_dir.display()))?;
    let mut files: Vec<PathBuf> = fs::read_dir(&leaf).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort();
    let is_result = |p: &Path| p.extension().map(|x| x == "result").unwrap_or(false);
    let mut new = 0;
    for f in files.iter().filter(|p| !is_result(p)).chain(files.iter().filter(|p| is_result(p))) {
        let to = results_dir.join(f.file_name().unwrap());
        if is_result(f) {
            if to.exists() {
                continue;
            }
            new += 1;
        }
        let body = fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let mut tmp = to.as_os_str().to_owned();
        tmp.push(".streaming");
        fs::write(&tmp, body).map_err(|e| format!("{}: {e}", to.display()))?;
        fs::rename(&tmp, &to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(new)
}

pub struct Spool {
    pub dir: PathBuf,
}

impl Spool {
    pub fn req_dir(&self) -> PathBuf {
        self.dir.join("req")
    }
    pub fn res_dir(&self) -> PathBuf {
        self.dir.join("res")
    }
    pub fn closed(&self) -> bool {
        self.dir.join("close").exists()
    }
    pub fn corpus_done(&self, rc: i32) -> Result<(), String> {
        write_atomic(&self.dir.join("corpus.done"), &format!("rc={rc}\n"))
    }
    /// Requests not yet taken, by job id order.
    pub fn requests(&self, taken: &BTreeSet<String>) -> Vec<(String, Result<Request, String>)> {
        let Ok(rd) = fs::read_dir(self.req_dir()) else { return vec![] };
        let mut v: Vec<(String, Result<Request, String>)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let job = name.strip_suffix(".req")?.to_string();
                if taken.contains(&job) {
                    return None;
                }
                let text = fs::read_to_string(e.path()).unwrap_or_default();
                Some((job.clone(), parse_request(&job, &text)))
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

/// Serves the spool on the VM: each request in its own scoped thread.
pub struct Server<'a> {
    pub spool: Spool,
    pub host: &'a dyn Host,
    pub remote: &'a dyn Remote,
    pub tree_dir: &'a Path,
    pub addr: &'a str,
    pub host_addr: &'a str,
    pub mirror_port: u16,
    pub toolchain: Option<String>,
    pub scratch: PathBuf,
    pub taken: BTreeSet<String>,
    pub in_flight: &'a AtomicUsize,
    pub mirror_lock: &'a Mutex<()>,
}

impl<'a> Server<'a> {
    /// Takes every new request (a `round` build only once the corpus is done) and runs it.
    pub fn serve<'s>(&mut self, sc: &'s Scope<'s, 'a>, corpus_done: bool)
    where
        'a: 's,
    {
        for (job, req) in self.spool.requests(&self.taken) {
            let req = match req {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("round-vm run: {e}");
                    self.taken.insert(job.clone());
                    let _ = write_atomic(&self.spool.res_dir().join(format!("{job}.done")), "rc=2\n");
                    continue;
                }
            };
            if req.build == Build::Round && !corpus_done {
                continue;
            }
            self.taken.insert(job.clone());
            self.in_flight.fetch_add(1, Ordering::SeqCst);
            let (host, remote, tree, addr) = (self.host, self.remote, self.tree_dir, self.addr);
            let (host_addr, port, toolchain) = (self.host_addr.to_string(), self.mirror_port, self.toolchain.clone());
            let res_dir = self.spool.res_dir();
            let scratch = self.scratch.join(format!("job-{job}"));
            let (in_flight, lock) = (self.in_flight, self.mirror_lock);
            sc.spawn(move || {
                let rc = run_job(host, remote, tree, addr, &host_addr, port, toolchain, &req, &res_dir, &scratch, lock);
                let _ = write_atomic(&res_dir.join(format!("{}.done", req.job)), &format!("rc={rc}\n"));
                in_flight.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }

    pub fn idle(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst) == 0
    }
}

#[allow(clippy::too_many_arguments)]
fn run_job(
    host: &dyn Host,
    remote: &dyn Remote,
    tree: &Path,
    addr: &str,
    host_addr: &str,
    port: u16,
    toolchain: Option<String>,
    req: &Request,
    res_dir: &Path,
    scratch: &Path,
    lock: &Mutex<()>,
) -> i32 {
    let mirror_ref = format!("attr-{}", req.job);
    {
        let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = host.mirror_ref(tree, &req.branch, &mirror_ref) {
            eprintln!("round-vm run: job {}: {e}", req.job);
            return 2;
        }
    }
    let job = AttrJob {
        host_addr: host_addr.to_string(),
        mirror_port: port,
        job: req.job.clone(),
        mirror_ref,
        suites: req.suites.clone(),
        build: req.build,
        toolchain,
    };
    let mut rc = match remote.run_attr(addr, &job) {
        Ok(rc) => rc,
        Err(e) => {
            eprintln!("round-vm run: job {}: {e}", req.job);
            return 2;
        }
    };
    let out = res_dir.join(&req.job);
    let _ = fs::remove_dir_all(scratch);
    let pulled = remote.pull(addr, &format!("attr-results/{}/", req.job), scratch).is_ok();
    match results_leaf(scratch) {
        Some(leaf) => {
            if let Err(e) = copy_tree(&leaf, &out) {
                eprintln!("round-vm run: job {}: {e}", req.job);
                rc = 2;
            }
        }
        None if rc == 0 || !pulled => {
            eprintln!("round-vm run: job {}: no results came back", req.job);
            rc = 2;
        }
        None => {}
    }
    if req.build == Build::Round {
        let _ = remote.pull(addr, REMOTE_BINS, &out.join("bins"));
    }
    let _ = fs::remove_dir_all(scratch);
    rc
}

/// After the corpus: keep serving until `close` and nothing in flight, or `linger` passes.
pub fn linger<'s, 'a: 's>(server: &mut Server<'a>, sc: &'s Scope<'s, 'a>, linger: Duration, poll: Duration) {
    let t0 = Instant::now();
    loop {
        server.serve(sc, true);
        if server.spool.closed() && server.idle() {
            return;
        }
        if t0.elapsed() >= linger {
            eprintln!("round-vm run: attribution spool not closed after {}s — releasing the VM", linger.as_secs());
            return;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn a_request_names_its_branch_suites_and_build() {
        let r = parse_request("j7", "branch=spira/batcher-attr/x-j7\nsuites=test-a.sh\nbuild=aeon\n").unwrap();
        assert_eq!((r.branch.as_str(), r.suites.as_str(), r.build), ("spira/batcher-attr/x-j7", "test-a.sh", Build::Aeon));
        assert!(parse_request("j7", "branch=b\nsuites=s\nbuild=warp\n").is_err());
        assert!(parse_request("j7", "suites=s\nbuild=aeon\n").is_err());
        assert!(parse_request("../x", "branch=b\nsuites=s\nbuild=aeon\n").is_err());
    }

    #[test]
    fn the_attr_command_keeps_empty_arguments_in_place() {
        let j = AttrJob {
            host_addr: "h".into(),
            mirror_port: 9430,
            job: "j1".into(),
            mirror_ref: "attr-j1".into(),
            suites: "test-a.sh".into(),
            build: Build::Artifacts,
            toolchain: None,
        };
        assert_eq!(attr_command(&j), "bash -s -- 'h' '9430' 'j1' 'attr-j1' 'test-a.sh' 'artifacts' ''");
    }

    #[test]
    fn streaming_copies_outputs_before_results_and_each_result_once() {
        let d = TempDir::new();
        let pulled = d.path().join("pulled");
        let out = d.path().join("results");
        fs::create_dir_all(pulled.join("KEY")).unwrap();
        fs::write(pulled.join("KEY/test-a.sh.out"), "FAIL x\n").unwrap();
        fs::write(pulled.join("KEY/test-a.sh.result"), "red 1 3 fp p e 1\n").unwrap();
        assert_eq!(stream_into(&pulled, &out).unwrap(), 1);
        assert_eq!(fs::read_to_string(out.join("test-a.sh.out")).unwrap(), "FAIL x\n");
        fs::write(pulled.join("KEY/test-b.sh.result"), "ok 1 3 - p e 0\n").unwrap();
        assert_eq!(stream_into(&pulled, &out).unwrap(), 1, "only the new result counts");
        assert_eq!(stream_into(&d.path().join("nothing"), &out).unwrap(), 0);
    }
}
