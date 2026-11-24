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
    /// A debug (`dev` profile) build of the job's own tree.
    Dev,
    /// In `~/round-work` after the corpus: an incremental release build whose binaries land.
    Round,
    /// Only whether the job's tree builds as a workspace; no suite runs and no binaries land.
    Workspace,
    /// Only whether the job's tree passes the workspace unit tests; exit 5 when it does not.
    Unit,
    /// Like `Unit`, for the `tests/` targets.
    Integration,
    /// Only whether `spira-lint` is clean on the job's tree against the landing ref; exit 5 when it is not.
    Lint,
}

impl Build {
    pub fn word(self) -> &'static str {
        match self {
            Build::Artifacts => "artifacts",
            Build::Dev => "dev",
            Build::Round => "round",
            Build::Workspace => "workspace",
            Build::Unit => "unit",
            Build::Integration => "integration",
            Build::Lint => "lint",
        }
    }
    pub fn parse(s: &str) -> Option<Build> {
        match s {
            "artifacts" => Some(Build::Artifacts),
            "dev" => Some(Build::Dev),
            "round" => Some(Build::Round),
            "workspace" => Some(Build::Workspace),
            "unit" => Some(Build::Unit),
            "integration" => Some(Build::Integration),
            "lint" => Some(Build::Lint),
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
pub const REMOTE_ATTR_SCRIPT: &str = concat!(r#"set -euo pipefail
host_addr="$1" port="$2" job="$3" ref="$4" suites="$5" build="$6" toolchain="$7"
# The launcher the corpus set up (run.rs REMOTE_SCRIPT): the round's staged release on PATH and its SPIRA_TOML.
. "$HOME/round-launcher.env" || { echo "round-vm: no staged release on this VM (round-launcher.env)" >&2; exit 2; }
if [ -n "$toolchain" ]; then export RUSTUP_TOOLCHAIN="$toolchain"; fi
reason="attribution re-run $job of the round's red"
export SPIRA_BATCH_MAXPAR=1 SPIRA_BATCH_RESULTS="$HOME/attr-results/$job"
rm -rf "$SPIRA_BATCH_RESULTS"; mkdir -p "$SPIRA_BATCH_RESULTS"
# path-ok: the round's own testenv, built on this VM from the round tree by the corpus run
tenv="$HOME/round-work/target/release/testenv"
set +e
"#, crate::run::unit_env_fn!(), r#"# suites= names packages, or package=test for a failing test: the run is then those tests alone,
# unless a package is named bare, which keeps every test of the packages named.
scope_of() {
    scope="--workspace"; filters=""
    [ "$suites" != "$1" ] || return 0
    scope=""; local bare="" t p
    for t in ${suites//,/ }; do
        p="${t%%=*}"
        case " $scope " in *" -p $p "*) ;; *) scope="$scope -p $p" ;; esac
        if [ "$p" = "$t" ]; then bare=1; else filters="$filters ${t#*=}"; fi
    done
    if [ -n "$bare" ] || [ -z "$filters" ]; then filters=""; else filters="-- --exact$filters"; fi
}
case "$build" in
round)
    cd "$HOME/round-work" || exit 2
    git fetch --quiet origin "+refs/heads/$ref:refs/heads/$ref" || exit 2
    git checkout --quiet --force --detach "refs/heads/$ref" || exit 2
    cargo build -j "$(nproc)" -q --profile release -p testenv --bin testenv || exit 4
    "$tenv" --mode parallel --profile release --suites "$suites" --repeat-reason "$reason" HEAD
    rc=$?
    rm -rf "$HOME/round-bins"; mkdir -p "$HOME/round-bins"
    find target/release -maxdepth 1 -type f -executable -exec cp {} "$HOME/round-bins/" \; 2>/dev/null
    exit "$rc" ;;
workspace)
    d="$HOME/attr/$job"
    rm -rf "$d"; mkdir -p "$HOME/attr"
    git clone --quiet --reference "$HOME/round-work" --branch "$ref" "git://${host_addr}:${port}/mirror.git" "$d" || exit 2
    cd "$d" || exit 2
    CARGO_TARGET_DIR="$HOME/attr-target" cargo build -j "$(nproc)" -q --profile release --workspace --config profile.release.incremental=false >&2 || { cd "$HOME" && rm -rf "$d"; exit 4; }
    echo "ok 0 0 - - - 0" > "$SPIRA_BATCH_RESULTS/workspace-build.result"
    cd "$HOME" && rm -rf "$d"
    exit 0 ;;
unit)
    d="$HOME/attr/$job"
    rm -rf "$d"; mkdir -p "$HOME/attr"
    git clone --quiet --reference "$HOME/round-work" --branch "$ref" "git://${host_addr}:${port}/mirror.git" "$d" || exit 2
    cd "$d" || exit 2
    scope_of unit
    [ -f "$HOME/round-nextest.toml" ] || { echo "round-vm: no round-nextest.toml on this VM: the round's unit tier has not run here" >&2; exit 2; }
    CARGO_TARGET_DIR="$HOME/attr-target" unit_env "#, spira_config::unit_tier_unit!("\"$HOME/round-nextest.toml\"", "unittest", "$scope"), r#" $filters 2>&1 | tee "$SPIRA_BATCH_RESULTS/unit-tests.out" >&2
    rc=${PIPESTATUS[0]}
    cd "$HOME" && rm -rf "$d"
    if [ "$rc" -eq 100 ] || [ "$rc" -eq 101 ]; then echo "red 0 0 - - - $rc" > "$SPIRA_BATCH_RESULTS/unit-tests.result"; exit 5; fi
    [ "$rc" -eq 0 ] || exit "$rc"
    echo "ok 0 0 - - - 0" > "$SPIRA_BATCH_RESULTS/unit-tests.result"
    exit 0 ;;
integration)
    d="$HOME/attr/$job"
    rm -rf "$d"; mkdir -p "$HOME/attr"
    git clone --quiet --reference "$HOME/round-work" --branch "$ref" "git://${host_addr}:${port}/mirror.git" "$d" || exit 2
    cd "$d" || exit 2
    scope_of integration
    if [ "$scope" != "--workspace" ]; then
        bins=$("$tenv" test-bins ${scope//-p /}) || { cd "$HOME" && rm -rf "$d"; exit 4; }
        if [ -n "$bins" ]; then
            bin_scope=""; for b in $bins; do bin_scope="$bin_scope -p $b"; done
            CARGO_TARGET_DIR="$HOME/attr-target" unit_env build --profile unittest --config profile.unittest.incremental=false $bin_scope >&2 || { cd "$HOME" && rm -rf "$d"; exit 4; }
        fi
    fi
    CARGO_TARGET_DIR="$HOME/attr-target" unit_env "#, spira_config::unit_tier_integration!("unittest", "$scope"), r#" $filters 2>&1 | tee "$SPIRA_BATCH_RESULTS/integration-tests.out" >&2
    rc=${PIPESTATUS[0]}
    cd "$HOME" && rm -rf "$d"
    if [ "$rc" -eq 101 ]; then echo "red 0 0 - - - $rc" > "$SPIRA_BATCH_RESULTS/integration-tests.result"; exit 5; fi
    [ "$rc" -eq 0 ] || exit "$rc"
    echo "ok 0 0 - - - 0" > "$SPIRA_BATCH_RESULTS/integration-tests.result"
    exit 0 ;;
lint)
    d="$HOME/attr/$job"
    rm -rf "$d"; mkdir -p "$HOME/attr"
    git clone --quiet --reference "$HOME/round-work" --branch "$ref" "git://${host_addr}:${port}/mirror.git" "$d" || exit 2
    cd "$d" || exit 2
    CARGO_TARGET_DIR="$HOME/attr-target" cargo build -j "$(nproc)" -q --profile release --locked -p spira-lint --config profile.release.incremental=false >&2 || { cd "$HOME" && rm -rf "$d"; exit 4; }
    "$HOME/attr-target/release/spira-lint" --root "$d" --base origin/base >&2
    rc=$?
    cd "$HOME" && rm -rf "$d"
    [ "$rc" -eq 0 ] || exit 5
    echo "ok 0 0 - - - 0" > "$SPIRA_BATCH_RESULTS/spira-lint.result"
    exit 0 ;;
artifacts|dev)
    d="$HOME/attr/$job"
    rm -rf "$d"; mkdir -p "$HOME/attr"
    git clone --quiet --reference "$HOME/round-work" --branch "$ref" "git://${host_addr}:${port}/mirror.git" "$d" || exit 2
    cd "$d" || exit 2
    if [ "$build" = artifacts ]; then
        "$tenv" --mode parallel --artifacts "$HOME/round-work/target/release" --suites "$suites" --repeat-reason "$reason" HEAD "$d"
    else
        "$tenv" --mode parallel --profile dev --suites "$suites" --repeat-reason "$reason" HEAD "$d"
    fi
    rc=$?
    cd "$HOME" && rm -rf "$d"
    exit "$rc" ;;
*) exit 2 ;;
esac
"#);

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
    let mut failed: BTreeSet<std::ffi::OsString> = BTreeSet::new();
    for f in files.iter().filter(|p| !is_result(p)).chain(files.iter().filter(|p| is_result(p))) {
        let to = results_dir.join(f.file_name().unwrap());
        if is_result(f) && (to.exists() || f.file_stem().is_some_and(|st| failed.contains(st))) {
            continue;
        }
        let copied = fs::read(f).map_err(|e| format!("{}: {e}", f.display())).and_then(|body| {
            let mut tmp = to.as_os_str().to_owned();
            tmp.push(".streaming");
            let _ = fs::create_dir_all(results_dir);
            fs::write(&tmp, body).map_err(|e| format!("{}: {e}", to.display()))?;
            fs::rename(&tmp, &to).map_err(|e| format!("{}: {e}", to.display()))
        });
        match copied {
            Ok(()) if is_result(f) => new += 1,
            Ok(()) => {}
            Err(e) => {
                eprintln!("round-vm run: streaming results: {e}");
                failed.extend(f.file_stem().map(|st| st.to_owned()));
            }
        }
    }
    Ok(new)
}

/// Wall time the attribution legs ran on the VM, summed over legs; concurrent legs add up.
#[derive(Default)]
pub struct AttrTime(std::sync::atomic::AtomicU64);

impl AttrTime {
    pub fn add(&self, d: Duration) {
        self.0.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
    }
    pub fn secs(&self) -> u64 {
        (self.0.load(Ordering::SeqCst) + 500) / 1000
    }
}

/// One lock per (tree, suites, build): testenv keys a run by exactly those, and refuses a second
/// live run of a key. Jobs for one key wait their turn here instead of being refused there.
#[derive(Default)]
pub struct TreeQueue(Mutex<std::collections::HashMap<String, std::sync::Arc<Mutex<()>>>>);

impl TreeQueue {
    pub fn lock_for(&self, tree: &str, suites: &str, build: Build) -> std::sync::Arc<Mutex<()>> {
        let key = format!("{tree}\t{suites}\t{}", build.word());
        self.0.lock().unwrap_or_else(|p| p.into_inner()).entry(key).or_default().clone()
    }
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
    pub tree_queue: &'a TreeQueue,
    pub attr_time: &'a AttrTime,
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
            let (in_flight, lock, queue, attr_time) = (self.in_flight, self.mirror_lock, self.tree_queue, self.attr_time);
            sc.spawn(move || {
                let began = Instant::now();
                let rc = run_job(host, remote, tree, addr, &host_addr, port, toolchain, &req, &res_dir, &scratch, lock, queue);
                attr_time.add(began.elapsed());
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
    queue: &TreeQueue,
) -> i32 {
    let mirror_ref = format!("attr-{}", req.job);
    let tree_sha = {
        let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
        match host.mirror_ref(tree, &req.branch, &mirror_ref) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("round-vm run: job {}: {e}", req.job);
                return 2;
            }
        }
    };
    let job = AttrJob {
        host_addr: host_addr.to_string(),
        mirror_port: port,
        job: req.job.clone(),
        mirror_ref,
        suites: req.suites.clone(),
        build: req.build,
        toolchain,
    };
    let turn = queue.lock_for(&tree_sha, &req.suites, req.build);
    let ran = {
        let _turn = turn.lock().unwrap_or_else(|p| p.into_inner());
        remote.run_attr(addr, &job)
    };
    let mut rc = match ran {
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

    /// sp-anhnd6: the corpus writes `round-launcher.env` and every rerun sources it; a launcher
    /// without SPIRA_TOML refused all 640 reruns of a round with "batch: SPIRA_TOML is not set".
    /// The corpus's own line writes the file, and the rerun's own line reads it in a bare env.
    #[test_id(tf6zqur)]
    #[test]
    fn a_rerun_sourcing_the_launcher_env_finds_the_rounds_spira_toml() {
        let line_of = |script: &str, needle: &str| script.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no line with {needle:?}")).to_string();
        let write = line_of(crate::run::REMOTE_SCRIPT, "> ~/round-launcher.env");
        let source = line_of(REMOTE_ATTR_SCRIPT, ". \"$HOME/round-launcher.env\"");
        let d = TempDir::new();
        let toml = "vm/round-work/spira/testlib/complete.toml:vm/round config.toml";
        let wrote = std::process::Command::new("bash")
            .args(["-c", &format!("set -euo pipefail\n{write}")])
            .env("HOME", d.path())
            .env("SPIRA_RELEASE", "r/rel")
            .env("SPIRA_HOME", "r/rel/spira")
            .env("SPIRA_REPO", "r/work")
            .env("SPIRA_TOML", toml)
            .env("CARGO_HOME", "vm/cache home")
            .env("RUSTC_WRAPPER", "vm/cache home/bin/sccache")
            .status()
            .unwrap();
        assert!(wrote.success(), "the corpus's launcher line runs");
        let env = std::fs::read_to_string(d.path().join(crate::run::LAUNCHER_ENV)).unwrap();
        assert!(env.contains("export SPIRA_TOML="), "the launcher env carries SPIRA_TOML:\n{env}");
        assert!(env.contains("export SPIRA_HOME=r/rel/spira\n"), "the launcher env carries the staged release's registry home:\n{env}");
        let out = std::process::Command::new("bash")
            .args(["-c", &format!("set -euo pipefail\n{source}\nprintf '%s|%s|%s' \"${{SPIRA_TOML:?batch: SPIRA_TOML is not set}}\" \"$CARGO_HOME\" \"$RUSTC_WRAPPER\"")])
            .env_clear()
            .env("HOME", d.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "the rerun sees SPIRA_TOML: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{toml}|vm/cache home|vm/cache home/bin/sccache"), "the rerun sees the corpus's values, intact");
    }

    #[test_id(tr02x5o)]
    #[test]
    fn attribution_legs_add_their_wall_time_and_round_to_whole_seconds() {
        let t = AttrTime::default();
        assert_eq!(t.secs(), 0);
        t.add(Duration::from_millis(1400));
        t.add(Duration::from_millis(1400));
        assert_eq!(t.secs(), 3, "two concurrent legs of 1.4s are 2.8s of leg time");
    }

    #[test_id(t1shnth)]
    #[test]
    fn a_request_names_its_branch_suites_and_build() {
        let r = parse_request("j7", "branch=spira/batcher-attr/x-j7\nsuites=test-a.sh\nbuild=dev\n").unwrap();
        assert_eq!((r.branch.as_str(), r.suites.as_str(), r.build), ("spira/batcher-attr/x-j7", "test-a.sh", Build::Dev));
        assert!(parse_request("j7", "branch=b\nsuites=s\nbuild=warp\n").is_err());
        assert!(parse_request("j7", "suites=s\nbuild=dev\n").is_err());
        assert!(parse_request("../x", "branch=b\nsuites=s\nbuild=dev\n").is_err());
    }

    #[test_id(tbp4hyh)]
    #[test]
    fn a_workspace_build_request_is_parsed_and_its_script_exits_4_on_a_failed_build_and_0_with_a_result() {
        let r = parse_request("b1", "branch=spira/batcher-attr/x-b1\nsuites=workspace\nbuild=workspace\n").unwrap();
        assert_eq!(r.build, Build::Workspace);
        assert_eq!(Build::Workspace.word(), "workspace");
        let arm = REMOTE_ATTR_SCRIPT.split("workspace)").nth(1).and_then(|a| a.split("exit 0 ;;").next()).unwrap();
        assert!(arm.contains("cargo build") && arm.contains("--workspace") && arm.contains("exit 4"), "{arm}");
        assert!(arm.contains("workspace-build.result"), "a success leaves a result so the job is not read as lost: {arm}");
    }

    fn attr_job_rc(build: &str, cargo_rc: i32, result: &str) -> (i32, bool) {
        let (rc, made, _) = attr_job(build, build, cargo_rc, result);
        (rc, made)
    }

    fn attr_job(build: &str, suites: &str, cargo_rc: i32, result: &str) -> (i32, bool, String) {
        let d = TempDir::new();
        let (rc, made, ran, _) = attr_job_env(&d, build, suites, cargo_rc, result, None);
        (rc, made, ran)
    }

    fn attr_job_in(d: &TempDir, build: &str, suites: &str, cargo_rc: i32, result: &str) -> (i32, bool, String) {
        let (rc, made, ran, _) = attr_job_env(d, build, suites, cargo_rc, result, None);
        (rc, made, ran)
    }

    fn attr_job_env(d: &TempDir, build: &str, suites: &str, cargo_rc: i32, result: &str, launcher: Option<&str>) -> (i32, bool, String, String) {
        let (bin, home) = (d.path().join("bin"), d.path().join("home"));
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&home).unwrap();
        let launcher = launcher.unwrap_or("export CARGO_HOME={d}\n").replace("{d}", &d.path().display().to_string());
        fs::write(home.join("round-launcher.env"), launcher).unwrap();
        fs::write(home.join("round-nextest.toml"), "").unwrap();
        let stub = |name: &str, body: &str| testkit::write_exe(bin.join(name), &format!("#!/bin/bash\n{body}\n"));
        stub("git", "for a; do last=$a; done; mkdir -p \"$last\"");
        let said = if cargo_rc == 0 { "" } else { "echo 'test x ... FAILED'; " };
        if !home.join("round-work/target/release/testenv").exists() {
            fs::create_dir_all(home.join("round-work/target/release")).unwrap();
            testkit::write_exe(home.join("round-work/target/release/testenv"), "#!/bin/bash\necho \"$*\" >> \"$HOME/testenv.calls\"; [ ! -e \"$HOME/listing-fails\" ] || exit 1; for a in \"$@\"; do sed -n \"s/^$a //p\" \"$HOME/test-bins\" 2>/dev/null; done; exit 0\n");
        }
        stub("cargo", &format!("echo \"$*\" >> \"$HOME/cargo.calls\"; case \"$*\" in \"build --profile unittest\"*) env > \"$HOME/build.env\"; [ ! -e \"$HOME/build-fails\" ]; exit $?;; esac; echo \"$*\" > \"$HOME/cargo.args\"; env > \"$HOME/cargo.env\"; {said}exit {cargo_rc}"));
        let status = std::process::Command::new(system_paths::BIN_BASH)
            .args(["-c", REMOTE_ATTR_SCRIPT, "bash", "h", "9430", "j1", "attr-j1", suites, build, ""])
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", &home)
            .status()
            .unwrap();
        let ran = fs::read_to_string(home.join("cargo.args")).unwrap_or_default();
        let env = fs::read_to_string(home.join("cargo.env")).unwrap_or_default();
        if cargo_rc == 0 && build == "unit" {
            assert!(ran.contains("--lib --bins"), "the unit phase's targets: {ran}");
            assert!(!env.contains("SPIRA_"), "a scrubbed environment: {env}");
        }
        (status.code().unwrap(), home.join("attr-results/j1").join(result).exists(), ran, env)
    }

    #[test_id(twf96dt)]
    #[test]
    fn the_unit_and_integration_probes_run_cargo_in_the_environment_the_round_launcher_recorded() {
        let launcher = "export CARGO_HOME={d}\nexport RUSTC_WRAPPER={d}/bin/sccache\n";
        for (build, result) in [("unit", "unit-tests.result"), ("integration", "integration-tests.result")] {
            let (_, _, _, env) = attr_job_env(&TempDir::new(), build, build, 0, result, Some(launcher));
            let home_line = env.lines().find(|l| l.starts_with("CARGO_HOME=")).unwrap_or_else(|| panic!("{build}: {env}"));
            let dir = home_line.trim_start_matches("CARGO_HOME=");
            assert!(env.contains(&format!("RUSTC_WRAPPER={dir}/bin/sccache\n")), "{build}: {env}");
            assert!(env.lines().any(|l| l.starts_with("PATH=") && l.contains(&format!("{dir}/bin"))), "{build}: {env}");
            assert!(!env.contains("SPIRA_"), "{build}: {env}");
            assert!(env.lines().any(|l| l.starts_with("CARGO_TARGET_DIR=") && l.ends_with("home/attr-target")), "{build}: the target dir arrives unquoted: {env}");
        }
    }

    #[test_id(tl4x7ms)]
    #[test]
    fn the_dev_and_artifacts_probes_ask_testenv_to_test_the_probes_clone_not_the_launchers_spira_repo() {
        for build in ["dev", "artifacts"] {
            let d = TempDir::new();
            let tenv = d.path().join("home/round-work/target/release");
            fs::create_dir_all(&tenv).unwrap();
            testkit::write_exe(tenv.join("testenv"), "#!/bin/bash\necho \"$*\" > \"$HOME/testenv.args\"; echo \"$SPIRA_REPO\" > \"$HOME/testenv.repo\"\n");
            let launcher = format!("export SPIRA_REPO={}/elsewhere\n", d.path().display());
            attr_job_env(&d, build, "s.sh", 0, "none", Some(&launcher));
            let args = fs::read_to_string(d.path().join("home/testenv.args")).unwrap();
            let clone = d.path().join("home/attr/j1");
            assert!(args.trim_end().ends_with(&format!("HEAD {}", clone.display())), "{build}: {args}");
            assert!(fs::read_to_string(d.path().join("home/testenv.repo")).unwrap().contains("elsewhere"), "{build}: the launcher's repo is what the bare call would have tested");
        }
    }

    #[test_id(t50qwpl)]
    #[test]
    fn the_round_and_the_attribution_probe_splice_the_same_unit_env_fragment() {
        assert!(crate::run::REMOTE_SCRIPT.contains(crate::run::UNIT_ENV_FN));
        assert!(REMOTE_ATTR_SCRIPT.contains(crate::run::UNIT_ENV_FN));
        for script in [crate::run::REMOTE_SCRIPT, REMOTE_ATTR_SCRIPT] {
            assert_eq!(script.matches("unit_env() {").count(), 1, "one definition per script");
        }
    }

    #[test_id(txlw94u)]
    #[test]
    fn a_red_unit_or_integration_probe_leaves_its_cargo_output_and_a_red_result() {
        for (build, rc) in [("unit", 100), ("integration", 101)] {
            let d = TempDir::new();
            let (code, made, _) = attr_job_in(&d, build, build, rc, &format!("{build}-tests.result"));
            assert_eq!((code, made), (5, true), "{build}");
            let res = d.path().join("home/attr-results/j1");
            let out = fs::read_to_string(res.join(format!("{build}-tests.out"))).unwrap();
            assert!(out.contains("test x ... FAILED"), "{build}: {out}");
            let result = fs::read_to_string(res.join(format!("{build}-tests.result"))).unwrap();
            assert_eq!(result.split_whitespace().next(), Some("red"), "{build}: {result}");
        }
    }

    fn workspace_job_rc(cargo_rc: i32) -> (i32, bool) {
        attr_job_rc("workspace", cargo_rc, "workspace-build.result")
    }

    #[test_id(t8ubdoj)]
    #[test]
    fn the_unit_probe_runs_the_command_the_round_runs_its_unit_phase_with() {
        let cfg = "\"$HOME/round-nextest.toml\"";
        let unit_arm = REMOTE_ATTR_SCRIPT.split("\nunit)").nth(1).and_then(|a| a.split("exit 0 ;;").next()).expect("a unit arm");
        let round_cmd = crate::run::REMOTE_SCRIPT.split("unit_nextest() {").nth(1).and_then(|a| a.split("\n}\n").next()).expect("unit_nextest");
        let lib_bins = spira_config::unit_tier::LIB_BINS;
        assert!(unit_arm.contains(&spira_config::unit_tier::unit_words(cfg, Some("unittest"), "$scope", lib_bins)), "{unit_arm}");
        assert!(round_cmd.contains(&spira_config::unit_tier::unit_words(cfg, Some("unittest"), "--workspace", lib_bins)), "{round_cmd}");
    }

    #[test_id(t1fou1o)]
    #[test]
    fn a_unit_job_naming_packages_tests_only_those_and_a_bare_one_the_whole_workspace() {
        let (_, _, args) = attr_job("unit", "loom,gate", 0, "unit-tests.result");
        assert!(args.contains("-p loom -p gate") && !args.contains("--workspace"), "{args}");
        let (_, _, args) = attr_job("unit", "unit", 0, "unit-tests.result");
        assert!(args.contains("--workspace") && !args.contains("-p "), "{args}");
    }

    #[test_id(tskxrlp)]
    #[test]
    fn a_unit_job_exits_5_when_the_tests_fail_and_0_with_a_result_when_they_pass() {
        let r = parse_request("u1", "branch=spira/batcher-attr/x-u1\nsuites=unit\nbuild=unit\n").unwrap();
        assert_eq!(r.build, Build::Unit);
        assert_eq!(attr_job_rc("unit", 101, "unit-tests.result"), (5, true));
        assert_eq!(attr_job_rc("unit", 100, "unit-tests.result"), (5, true), "nextest's own exit for a failed test run");
        assert_eq!(attr_job_rc("unit", 137, "unit-tests.result"), (137, false), "a killed run is a fault, not red");
        assert_eq!(attr_job_rc("unit", 0, "unit-tests.result"), (0, true));
    }

    #[test_id(t2zhxk2)]
    #[test]
    fn an_integration_job_runs_the_integration_targets_of_its_packages_and_exits_5_when_they_fail() {
        let r = parse_request("i1", "branch=spira/batcher-attr/x-i1\nsuites=spira-runtime\nbuild=integration\n").unwrap();
        assert_eq!(r.build, Build::Integration);
        let (_, _, args) = attr_job("integration", "spira-runtime,gate", 0, "integration-tests.result");
        assert!(args.contains("-p spira-runtime -p gate") && args.contains("--test *") && args.contains("--features integration"), "{args}");
        assert_eq!(attr_job_rc("integration", 101, "integration-tests.result"), (5, true));
        assert_eq!(attr_job_rc("integration", 0, "integration-tests.result"), (0, true));
    }

    #[test_id(t8ijgof)]
    #[test]
    fn an_integration_probe_builds_the_test_bins_of_its_packages_in_attr_target_before_it_runs_their_tests() {
        let d = TempDir::new();
        fs::create_dir_all(d.path().join("home")).unwrap();
        fs::write(d.path().join("home/test-bins"), "spira-runtime spira-lc\nother spira-x\n").unwrap();
        let (rc, made, ran) = attr_job_in(&d, "integration", "spira-runtime", 0, "integration-tests.result");
        assert_eq!((rc, made), (0, true));
        let calls = fs::read_to_string(d.path().join("home/cargo.calls")).unwrap();
        let calls: Vec<&str> = calls.lines().collect();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].starts_with("build --profile unittest --config profile.unittest.incremental=false") && calls[0].contains("-p spira-lc"), "{calls:?}");
        assert!(calls[1].starts_with("test ") && calls[1].contains("-p spira-runtime"), "{calls:?}");
        assert_eq!(ran.trim_end(), calls[1]);
        let asked = fs::read_to_string(d.path().join("home/testenv.calls")).unwrap();
        assert_eq!(asked.trim(), "test-bins spira-runtime");
        assert!(!calls[0].contains("spira-x"), "{calls:?}");
        let env = fs::read_to_string(d.path().join("home/build.env")).unwrap();
        assert!(env.lines().any(|l| l.starts_with("CARGO_TARGET_DIR=") && l.ends_with("home/attr-target")), "{env}");
    }

    #[test_id(t1x5qjs)]
    #[test]
    fn an_integration_probe_whose_test_bin_build_fails_exits_4_and_leaves_no_result() {
        let d = TempDir::new();
        fs::create_dir_all(d.path().join("home")).unwrap();
        fs::write(d.path().join("home/test-bins"), "spira-runtime spira-lc\nother spira-x\n").unwrap();
        fs::write(d.path().join("home/build-fails"), "").unwrap();
        let (rc, made, _) = attr_job_in(&d, "integration", "spira-runtime", 0, "integration-tests.result");
        assert_eq!((rc, made), (4, false));
        assert_eq!(fs::read_to_string(d.path().join("home/cargo.calls")).unwrap().lines().count(), 1, "the tests are not run");
    }

    #[test_id(tbhhfwl)]
    #[test]
    fn an_integration_probe_whose_test_bin_listing_fails_exits_4_and_runs_no_cargo() {
        let d = TempDir::new();
        fs::create_dir_all(d.path().join("home")).unwrap();
        fs::write(d.path().join("home/listing-fails"), "").unwrap();
        let (rc, made, _) = attr_job_in(&d, "integration", "spira-runtime", 0, "integration-tests.result");
        assert_eq!((rc, made), (4, false));
        assert!(!d.path().join("home/cargo.calls").exists());
    }

    #[test_id(tfl5nis)]
    #[test]
    fn an_integration_probe_without_test_bins_or_over_the_workspace_makes_one_cargo_call() {
        for suites in ["spira-runtime", "integration"] {
            let d = TempDir::new();
            let (rc, made, _) = attr_job_in(&d, "integration", suites, 0, "integration-tests.result");
            assert_eq!((rc, made), (0, true), "{suites}");
            assert_eq!(fs::read_to_string(d.path().join("home/cargo.calls")).unwrap().lines().count(), 1, "{suites}");
        }
        let d = TempDir::new();
        fs::create_dir_all(d.path().join("home")).unwrap();
        fs::write(d.path().join("home/test-bins"), "spira-runtime spira-lc\nother spira-x\n").unwrap();
        attr_job_in(&d, "integration", "integration", 0, "integration-tests.result");
        assert_eq!(fs::read_to_string(d.path().join("home/cargo.calls")).unwrap().lines().count(), 1, "workspace scope builds nothing extra");
    }

    #[test_id(t9m95tn)]
    #[test]
    fn an_integration_job_naming_a_failing_test_runs_that_test_alone_and_a_bare_package_keeps_them_all() {
        let (_, _, args) = attr_job("integration", "testenv=lint_x,testenv=lint_y", 0, "integration-tests.result");
        assert!(args.contains("-p testenv ") && args.contains("-- --exact lint_x lint_y") && !args.contains("testenv="), "{args}");
        assert_eq!(args.matches("-p testenv").count(), 1, "{args}");
        let (_, _, args) = attr_job("integration", "testenv=lint_x,gate", 0, "integration-tests.result");
        assert!(args.contains("-p testenv -p gate") && !args.contains("--exact"), "{args}");
        let (_, _, args) = attr_job("unit", "loom=a::b", 0, "unit-tests.result");
        assert!(args.contains("-p loom") && args.contains("--lib --bins") && args.contains("-- --exact a::b"), "{args}");
    }

    #[test_id(tdzasy6)]
    #[test]
    fn the_integration_probe_runs_the_command_the_round_runs_its_integration_phase_with() {
        let arm = REMOTE_ATTR_SCRIPT.split("\nintegration)").nth(1).and_then(|a| a.split("exit 0 ;;").next()).expect("an integration arm");
        let round_cmd = crate::run::REMOTE_SCRIPT.split("integ_cargo() {").nth(1).and_then(|a| a.split("\n}\n").next()).expect("integ_cargo");
        assert!(arm.contains(&spira_config::unit_tier::integration_words(Some("unittest"), "$scope")), "{arm}");
        assert!(round_cmd.contains(&spira_config::unit_tier::integration_words(Some("unittest"), "--workspace")), "{round_cmd}");
    }

    #[test_id(tv48huu)]
    #[test]
    fn a_lint_job_builds_spira_lint_from_its_own_tree_and_exits_5_when_it_finds_something() {
        let r = parse_request("l1", "branch=spira/batcher-attr/x-l1\nsuites=spira-lint\nbuild=lint\n").unwrap();
        assert_eq!(r.build, Build::Lint);
        let (rc, _, ran) = attr_job("lint", "spira-lint", 0, "spira-lint.result");
        assert!(ran.contains("-p spira-lint") && ran.contains("--locked"), "{ran}");
        assert_ne!(rc, 4, "a built linter is not a build failure");
        assert_eq!(attr_job_rc("lint", 101, "spira-lint.result"), (4, false), "a tree that does not build is a fault, not a lint verdict");
    }

    #[test_id(ta0ejib)]
    #[test]
    fn a_workspace_job_exits_4_when_cargo_fails_and_0_with_a_result_when_it_builds() {
        assert_eq!(workspace_job_rc(101), (4, false));
        assert_eq!(workspace_job_rc(0), (0, true));
    }

    #[test_id(tai20tg)]
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

    #[test_id(tnkccry)]
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
