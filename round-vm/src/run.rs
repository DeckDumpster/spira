//! `round-vm run` (DESIGN.md §2.2): mirror → acquire → batch on the VM → pull results,
//! telemetry and binaries → manifest → install by tree sha → release.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use serde_json::Value;

use crate::config::{Config, PveEnv};
use crate::template::Record;
use crate::pool::{Deps, Pool};
use crate::procs::{command, FileLock};
use crate::schema::{AcquireMode, Manifest, ProcId, Vm};
use crate::spool::{linger, stream_into, Server, Spool};

pub const RUN_USAGE: &str =
    "round-vm run: usage: round-vm run <tree-dir> [--suites <csv>] [--maxpar <n>] [--toolchain <ver>] [--results-dir <dir>] [--attr-spool <dir>] [--base <ref>] [--round-batch <id> [--round-repo <repo>]] [--on-red <command>] [--ci-entry <command>]";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunArgs {
    pub tree_dir: PathBuf,
    pub suites: Option<String>,
    pub maxpar: Option<u32>,
    pub toolchain: Option<String>,
    pub results_dir: Option<PathBuf>,
    /// DESIGN.md §2.2a: stream results while the corpus runs, and serve attribution reruns.
    pub attr_spool: Option<PathBuf>,
    /// The landing ref the round is judged against: the VM runs `spira-lint --base` on it and
    /// attributes a hit to the member whose diff touches the file. Absent: no lint step.
    pub base: Option<String>,
    /// The spira-lc batch this pass belongs to: the suites boundary is recorded on it live.
    pub round_batch: Option<String>,
    pub round_repo: Option<String>,
    /// Words of a command run with each suite's name appended, the moment that suite goes red.
    pub on_red: Option<Vec<String>>,
    /// A foreign repository's own full CI entry point: the VM clones the tree and runs this
    /// command instead of the harness corpus, and its exit status is the round's one verdict.
    pub ci_entry: Option<String>,
}

/// Parses `run`'s arguments; `--opt value` and `--opt=value` both work.
pub fn parse_run_args(args: &[String]) -> Result<RunArgs, String> {
    let mut it = args.iter();
    let tree = it.next().filter(|a| !a.starts_with("--")).ok_or(RUN_USAGE)?;
    let mut r = RunArgs { tree_dir: PathBuf::from(tree), ..Default::default() };
    while let Some(a) = it.next() {
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut val = || inline.clone().or_else(|| it.next().cloned()).ok_or(format!("round-vm run: {key} needs a value"));
        match key {
            "--suites" => r.suites = Some(val()?).filter(|v| !v.is_empty()),
            "--maxpar" => {
                let v = val()?;
                r.maxpar = Some(v.parse().map_err(|_| format!("round-vm run: --maxpar not a number: {v:?}"))?);
            }
            "--toolchain" => r.toolchain = Some(val()?).filter(|v| !v.is_empty()),
            "--results-dir" => r.results_dir = Some(PathBuf::from(val()?)),
            "--attr-spool" => r.attr_spool = Some(PathBuf::from(val()?)),
            "--base" => r.base = Some(val()?).filter(|v| !v.is_empty()),
            "--round-batch" => r.round_batch = Some(val()?).filter(|v| !v.is_empty()),
            "--round-repo" => r.round_repo = Some(val()?).filter(|v| !v.is_empty()),
            "--on-red" => r.on_red = Some(val()?.split_whitespace().map(String::from).collect()).filter(|v: &Vec<String>| !v.is_empty()),
            "--ci-entry" => r.ci_entry = Some(val()?).filter(|v| !v.trim().is_empty()),
            other => return Err(format!("round-vm run: unknown option: {other}")),
        }
    }
    Ok(r)
}

/// The host side: the tree's identity and the mirror the VM clones from.
pub trait Host: Sync {
    fn is_checkout(&self, tree: &Path) -> bool;
    /// (commit sha, tree sha) of `tree`'s HEAD.
    fn head(&self, tree: &Path) -> Result<(String, String), String>;
    /// Publishes `tree`'s HEAD in the mirror as `round_ref` (one ref per run, never a shared
    /// HEAD) and makes sure the daemon serving it is up.
    fn prepare_mirror(&self, tree: &Path, round_ref: &str) -> Result<(), String>;
    /// Deletes `round_ref` from the mirror once its run is over.
    fn drop_round_ref(&self, round_ref: &str);
    /// The testenv image tag `commit` of `tree` needs, computed without a checkout.
    fn image_tag(&self, tree: &Path, commit: &str) -> Result<String, String>;
    /// Fetches `branch` of `tree`'s repository into the mirror as `refs/heads/<as_ref>`.
    fn mirror_ref(&self, tree: &Path, branch: &str, as_ref: &str) -> Result<(), String>;
    /// Every suite file name committed in `tree`'s HEAD.
    fn suites_in(&self, tree: &Path) -> Result<Vec<String>, String>;
    /// Recorded median wall seconds per suite (`testenv --report`).
    fn suite_medians(&self) -> Result<std::collections::HashMap<String, f64>, String>;
}

/// The explicit longest-first suite list for a full-corpus round, naming on stderr every suite
/// with no recorded time.
fn lpt_suites(host: &dyn Host, tree: &Path) -> Result<String, String> {
    let all = host.suites_in(tree)?;
    if all.is_empty() {
        return Err(format!("no spira/test-*.sh suites in {}", tree.display()));
    }
    let (list, unknown) = crate::lpt::order(&all, &host.suite_medians()?);
    if !unknown.is_empty() {
        eprintln!("round-vm run: ALARM — no recorded time, scheduled first: {}", unknown.join(" "));
    }
    Ok(list.join(","))
}

pub struct BatchJob {
    pub host_addr: String,
    pub mirror_port: u16,
    pub suites: Option<String>,
    pub maxpar: u32,
    pub toolchain: Option<String>,
    /// sp-xjnzl: this operator's own `CARGO_HOME` (read from the caller's own environment,
    /// never hardcoded — a literal path names one operator's box, which this crate ships to
    /// everyone who clones it). Empty: the VM's image records its own (EC2).
    pub cache_home: Option<String>,
    pub testenv_registry: Option<String>,
    /// Seconds of setup (script start to the suites launching) beyond which the run reports SETUP-SLOW.
    pub setup_alarm_secs: u64,
    /// The cap clock: first suite to the bare lane's end. Past it the VM kills the lane and exits 124.
    pub cap_secs: u64,
    /// `key = value` lines for the VM's `round-config.toml`: the operator's testenv limits.
    pub testenv_limits: String,
    /// Seconds the VM waits for the unit-test job once the cap clock has stopped.
    pub post_guard_secs: u64,
    /// Mirror ref `base` holds the landing ref; set: the round lints before its suites.
    pub lint: bool,
    /// The head's image tag differs from what the template (or the landing ref) holds: the VM
    /// builds `localhost/spira-testenv:<tag>` as a declared setup step instead of refusing it.
    pub build_image: bool,
    /// The mirror ref holding this run's tree, and the commit it must resolve to.
    pub round_ref: String,
    pub round_sha: String,
    /// Set: the VM runs [`FOREIGN_SCRIPT`] with this command rather than the harness round.
    pub ci_entry: Option<String>,
}

/// The VM side, reached only by address.
pub trait Remote: Sync {
    fn reachable(&self, addr: &str) -> bool;
    /// Runs the batch on the VM; the remote testenv runner's exit code (255: ssh itself).
    fn run_batch(&self, addr: &str, job: &BatchJob) -> Result<i32, String>;
    /// Copies the remote directory `remote_path` into `local`.
    fn pull(&self, addr: &str, remote_path: &str, local: &Path) -> Result<(), String>;
    /// Copies the local directory `local` into the remote `remote_path`.
    fn push(&self, addr: &str, local: &Path, remote_path: &str) -> Result<(), String>;
    /// Runs one attribution rerun (DESIGN.md §2.2a); its testenv exit code (255: ssh).
    fn run_attr(&self, addr: &str, job: &crate::spool::AttrJob) -> Result<i32, String>;
}

/// Single-quotes `s` for a POSIX shell, so an empty argument survives ssh's re-joining of
/// the remote command line (the bash lost an empty --suites and shifted every later one).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The VM side of a round for a registered repository that is not the harness: clone the round's
/// tree and run the repository's own CI entry point (`${15}`). Its exit status is the one verdict,
/// written as `ci.result`, so the round covers what CI covers by construction.
pub const FOREIGN_SCRIPT: &str = r#"set -euo pipefail
host_addr="$1" port="$2" toolchain="$5" round_ref="${13}" round_sha="${14}" entry="${15}"
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin"
rm -rf ~/round-bins ~/round-work ~/round-setup.log ~/round-shims
exec 2> >(tee ~/round-setup.log >&2)
exec 1>&2
res="$HOME/round-work/.runtime/spira/batch-results"
trap 'rc=$?; if [ "$rc" -ne 0 ] && [ ! -s "$res/ci.out" ]; then sleep 1; mkdir -p "$res"; { cat ~/round-setup.log; echo "round-vm: round script exited $rc before ci_entry left output"; } > "$res/ci.out"; fi' EXIT
git clone --quiet --no-checkout "git://${host_addr}:${port}/mirror.git" ~/round-work
git -C ~/round-work fetch --quiet origin "$round_ref"
git -C ~/round-work checkout --quiet --force --detach FETCH_HEAD
got="$(git -C ~/round-work rev-parse HEAD)"
if [ "$got" != "$round_sha" ]; then
    echo "round-vm: checked out $got from $round_ref, expected the round's commit $round_sha" >&2
    exit 2
fi
cd ~/round-work
unset RUSTC RUSTDOC CARGO_BUILD_RUSTC CARGO_BUILD_RUSTDOC RUSTUP_TOOLCHAIN
rustup_bin="$(command -v rustup || true)"
if [ -n "$rustup_bin" ]; then
    if [ -n "$toolchain" ]; then
        echo "round-vm: round toolchain $toolchain (the tree's pin)" >&2
        rustup toolchain install "$toolchain" --profile minimal -c clippy -c rustfmt --no-self-update >&2
        export RUSTUP_TOOLCHAIN="$toolchain"
    fi
    mkdir -p ~/round-shims
    for t in cargo rustc rustdoc rustfmt cargo-fmt clippy-driver cargo-clippy; do ln -sf "$rustup_bin" ~/round-shims/$t; done
    export PATH="$HOME/round-shims:$PATH"
fi
rustc -vV >&2 || echo "round-vm: no rustc on PATH" >&2
mkdir -p "$res"
printf 'tree=%s\n' "$(git rev-parse 'HEAD^{tree}')" > "$res/batch.meta"
t0=$(date +%s)
rc=0
bash -c "$entry" 2>&1 | tee "$res/ci.out" || rc=${PIPESTATUS[0]}
if [ "$rc" -eq 0 ]; then status=ok; else status=red; fi
printf 'round-vm: ci_entry exited %s\n' "$rc" | tee -a "$res/ci.out"
printf '%s %s %s - - - %s\n' "$status" 1 "$(( $(date +%s) - t0 ))" "$rc" > "$res/ci.result"
[ "$rc" -eq 0 ]
"#;

/// The VM side of a round. The VM is a LAUNCHER (design runtime-is-a-release, sp-dvfea): it
/// builds the round's tree, stages that build as a release (`release build --bin-dir`, the
/// same layout production and GitHub CI run, sp-6cbna) and sets `SPIRA_RELEASE` and `PATH`
/// outright — the release's `bin/` and `spira/`, then cargo and the system directories —
/// for everything after, so conf.sh finds `spira-config` and every harness script runs this
/// round's tools by name. [`LAUNCHER_ENV`] records the two for the attribution jobs.
/// Then it runs the round's own `testenv` (release profile — its binaries ship; its in-place
/// build is incremental on the one just made) and stages `target/release`'s executables
/// into `~/round-bins/` so the host pulls the binaries and not cargo's target directory.
pub const REMOTE_SCRIPT: &str = r#"set -euo pipefail
host_addr="$1" port="$2" suites="$3" maxpar="$4" toolchain="$5" cache_home="$6" registry="$7" setup_alarm="$8" cap="$9" post_guard="${10}" lint="${11}" build_image="${12}" round_ref="${13}" round_sha="${14}"
testenv_limits="${16:-}"
[ -n "$testenv_limits" ] || { echo "round-vm: the host sent no testenv limits; a round never takes them from the tree's test fixture" >&2; exit 2; }
t_start=$(date +%s)
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin"
if ! command -v cargo >/dev/null 2>&1; then
    echo "round-vm: cargo not found on PATH ($PATH) — the template is missing a Rust toolchain" >&2
    exit 127
fi
rm -rf ~/round-bins ~/round-launcher.env ~/round-setup.log
exec 2> >(tee ~/round-setup.log >&2)
mirror="git://${host_addr}:${port}/mirror.git"
checkout_round() {
    git -C ~/round-work fetch --quiet origin "$round_ref" \
        && git -C ~/round-work checkout --quiet --force -B round FETCH_HEAD
}
if [ -d ~/round-work/.git ] \
    && git -C ~/round-work remote set-url origin "$mirror" \
    && git -C ~/round-work fetch --quiet --prune origin \
    && checkout_round \
    && git -C ~/round-work clean --quiet -fdx -e /target; then
    :
else
    rm -rf ~/round-work
    git clone --quiet --no-checkout "$mirror" ~/round-work
    checkout_round
fi
got="$(git -C ~/round-work rev-parse HEAD)"
if [ "$got" != "$round_sha" ]; then
    echo "round-vm: checked out $got from $round_ref, expected the round's commit $round_sha" >&2
    exit 2
fi
cd ~/round-work
nix_bin=/nix/var/nix/profiles/default/bin
flake_ref="path:$HOME/round-work#spira-env"
build_env() { PATH="$nix_bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" nix --extra-experimental-features 'nix-command flakes' build "$flake_ref" -o "$HOME/spira-env" >&2; }
if ! build_env; then
    echo "round-vm: spira-env build failed for flake ref $flake_ref (git checkout: $(git -C ~/round-work rev-parse --short HEAD 2>&1); flake.nix on disk: $([ -f ~/round-work/flake.nix ] && echo present || echo MISSING)) — dropping nix's fetcher cache and retrying once" >&2
    rm -f ~/.cache/nix/fetcher-cache-v*.sqlite*
    if ! build_env; then
        echo "round-vm: cannot build spira-env from the round's flake.lock (flake ref $flake_ref)" >&2
        exit 3
    fi
fi
env_path="$(readlink -f "$HOME/spira-env")"
export PATH="$HOME/spira-env/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin"
echo "round-vm: spira-env $env_path ($(dolt version | head -n1); bd $(bd --version 2>&1 | head -n1))" >&2
# sp-xjnzl-2: warm conf.sh's own generated fragments (conf.d.keys.generated.sh,
# conf.d.defaults.generated.sh — gitignored, never present after a fresh clone) ONCE,
# sequentially, before the suite batch starts. Every suite's own container sources
# conf.sh (directly, or through a tool like watchd that shells into it), and conf.sh
# regenerates these itself when they are stale or missing (conf-gen.sh) — on the HOST's
# long-lived checkout that is already warm almost always, so the regeneration path is
# barely exercised; on a FRESH VM clone every single one of 400+ suites hits "missing"
# on its first conf.sh sourcing, all at once, at --maxpar. Warming it here, before any
# suite runs, means every one of them finds it already fresh and never regenerates at
# all — this is what test-install-migrate.sh's intermittent "the watcher manifest is
# malformed" (watchd's own conf.sh sourcing failing) traced back to.
bash spira/conf-gen.sh >&2 || true
if [ -n "$toolchain" ]; then export RUSTUP_TOOLCHAIN="$toolchain"; fi
# sp-xjnzl: ONE compilation cache shared with the host itself, not a VM-local one — the
# box's own address, which this VM already reaches for the mirror, is reused for the cache
# store too (sccache-dav/DESIGN.md). CARGO_HOME is overridden to $6, this binary's OWN
# resolved config (SPIRA_ROUND_VM_CACHE_HOME, or the operator's config file — Config::load's
# own doc comment names where) — never
# the caller's ambient CARGO_HOME, and never empty: run()'s own preflight already refused
# before this script was ever sent (sp-xjnzl-2, law-a-binary-resolves-the-config-it-reads).
# sccache hashes a dependency's registry source path into its cache key, so a hit across
# machines needs that path byte-identical, not merely consistent (spira-config/DESIGN-
# build-cache.md §2.5; re-verified against sccache 0.18.0's own generate_hash_key for this
# bead). An empty cache_home (an EC2 image) is read from the image's own record, never
# defaulted. A read or write this box's store refuses degrades to a
# cache miss, never a build failure (sccache's own RemoteStorage tolerates both) — so an
# unreachable store costs speed, not a round.
if [ -z "$cache_home" ]; then
    # `|| true`: under pipefail a missing record fails the substitution, and set -e then
    # exits here silently, before the refusal below can name the cause.
    cache_home=$(sed -n 's/^CARGO_HOME=//p' /etc/spira-round-ami.env 2>/dev/null | head -n1 || true)
    if [ -z "$cache_home" ]; then
        echo "round-vm: no cache home given and this image records none (/etc/spira-round-ami.env); rebuild the round image" >&2
        exit 5
    fi
fi
export CARGO_HOME="$cache_home"
# The image links $CARGO_HOME/bin straight to ITS toolchain (template.sh runs only at image
# build), so RUSTUP_TOOLCHAIN alone changes nothing: a round whose tree pins another release
# installs it here and relinks, or a toolchain upgrade can never certify.
have="$("$CARGO_HOME/bin/rustc" --version 2>/dev/null | cut -d' ' -f2)"
if [ -n "$toolchain" ] && [ "$have" != "$toolchain" ]; then
    echo "round-vm: round toolchain $toolchain (installed at round time; the image has ${have:-none})" >&2
    export RUSTUP_HOME="$CARGO_HOME/rustup"
    rustup toolchain install "$toolchain" --profile minimal --no-self-update >&2
    for b in "$RUSTUP_HOME"/toolchains/"$toolchain"-*/bin/*; do ln -sf "$b" "$CARGO_HOME/bin/${b##*/}"; done
fi
export PATH="$CARGO_HOME/bin:$HOME/spira-env/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin"
rustc_id="$(rustc -vV | sed -n 's/^\(release\|commit-hash\): //p' | tr '\n' ' ')"
echo "round-vm: round rustc $rustc_id" >&2
if [ -d ~/round-work/target ] && [ "$(cat ~/round-work/target/.rustc-id 2>/dev/null)" != "$rustc_id" ]; then
    echo "round-vm: warm target was built by another rustc; clearing it" >&2
    find ~/round-work/target -mindepth 1 -maxdepth 1 -exec rm -rf {} +
fi
mkdir -p ~/round-work/target
printf '%s' "$rustc_id" > ~/round-work/target/.rustc-id
export RUSTC_WRAPPER="$CARGO_HOME/bin/sccache"
if [ ! -x "$RUSTC_WRAPPER" ]; then
    echo "round-vm: sccache is missing on this VM: $RUSTC_WRAPPER" >&2
    exit 5
fi
export SCCACHE_IGNORE_SERVER_IO_ERROR=1
export SCCACHE_WEBDAV_ENDPOINT="http://${host_addr}:9431"
export SCCACHE_WEBDAV_KEY_PREFIX="/"
export SCCACHE_ERROR_LOG="$HOME/sccache-error.log"
start_sccache() {
    export SCCACHE_STARTUP_TIMEOUT="${SCCACHE_STARTUP_TIMEOUT:-120}"
    for attempt in 1 2; do
        if "$RUSTC_WRAPPER" --start-server >&2; then
            echo "round-vm: sccache server started (attempt $attempt)" >&2
            return 0
        fi
        echo "round-vm: sccache server start failed (attempt $attempt)" >&2
        "$RUSTC_WRAPPER" --stop-server >/dev/null 2>&1 || true
    done
    return 1
}
sccache_mode="with sccache"
if ! start_sccache; then
    echo "round-vm: sccache server would not start; building WITHOUT sccache (cold, slower)" >&2
    export RUSTC_WRAPPER=""
    sccache_mode="without sccache (fallback after two failed server starts)"
fi
t0=$(date +%s)
if ! cargo build -q --profile release --workspace --config profile.release.incremental=false; then
    echo "round-vm: the round's workspace build failed, built $sccache_mode" >&2
    tail -40 "$SCCACHE_ERROR_LOG" >&2 2>/dev/null || true
    exit 4
fi
echo "round-vm: built the round in $(( $(date +%s) - t0 ))s" >&2
# THE UNIT-TEST BINARIES COMPILE HERE, in the build phase and outside the suite clock; the unit
# run below is the same command, which then compiles nothing. A scrubbed environment: the
# launcher's SPIRA_RELEASE/SPIRA_REPO/PATH leak into tests that resolve configuration.
unit_cargo() {
    env -i HOME="$HOME" PATH="$HOME/spira-env/bin:$CARGO_HOME/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" \
        CARGO_HOME="$CARGO_HOME" RUSTC_WRAPPER="$RUSTC_WRAPPER" SCCACHE_IGNORE_SERVER_IO_ERROR=1 \
        SCCACHE_WEBDAV_ENDPOINT="$SCCACHE_WEBDAV_ENDPOINT" SCCACHE_WEBDAV_KEY_PREFIX="$SCCACHE_WEBDAV_KEY_PREFIX" \
        GIT_AUTHOR_NAME=round GIT_AUTHOR_EMAIL=round@spira GIT_COMMITTER_NAME=round GIT_COMMITTER_EMAIL=round@spira \
        UNIT_COV_ENV="$unit_cov_env" bash -c 'eval "$UNIT_COV_ENV"; exec cargo test "$@"' _ --profile unittest --workspace --config profile.unittest.incremental=false "$@"
}
unit_cov_init() {
    unit_cov_env=""
    if [ -x "$CARGO_HOME/bin/cargo-llvm-cov" ]; then
        # A clean environment: an inherited CARGO_TARGET_DIR, LLVM_PROFILE_FILE or CARGO_LLVM_COV_*
        # (e.g. from an outer coverage run) would point the profiles somewhere the report never reads.
        unit_cov_env=$(env -i HOME="$HOME" CARGO_HOME="$CARGO_HOME" PATH="$HOME/spira-env/bin:$CARGO_HOME/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" cargo llvm-cov show-env --export-prefix) || unit_cov_env=""
    fi
    [ -n "$unit_cov_env" ] || echo "round-vm: COVERAGE: lines unavailable (the image has no cargo-llvm-cov)" >&2
}
unit_cov_init
unit_build_mark="$HOME/round-work/.runtime/spira/batch-results/unit-build"
mkdir -p "$(dirname "$unit_build_mark")"
echo building > "$unit_build_mark"
t_ut=$(date +%s)
if ! unit_cargo --no-run > ~/round-unit-build.log 2>&1; then
    echo "round-vm: the round's unit-test build failed:" >&2
    tail -60 ~/round-unit-build.log >&2
    exit 4
fi
echo built > "$unit_build_mark"
unit_build_secs=$(( $(date +%s) - t_ut ))
unit_total=$(grep -c '^ *Executable ' ~/round-unit-build.log)
echo "round-vm: built ${unit_total} unit-test binaries in $(( $(date +%s) - t_ut ))s" >&2
# One line per unit-test binary as it finishes, from cargo's own "Running" and "test result:"
# lines on stdin: UNIT-TEST <binary> ok|FAILED (k/M). The totals are the --no-run listing's.
unit_stream() {
    awk -v total="$1" -v prog="$2" '
        /^ *Running / { name = $0; if (match(name, /\(.*\)/)) { name = substr(name, RSTART + 1, RLENGTH - 2); sub(/.*\//, "", name); sub(/-[0-9a-f]+$/, "", name) } else { sub(/^ *Running /, "", name) } next }
        /^ *Doc-tests / { name = "doc-tests " $2; next }
        /^test result: / { k++; st = ($3 == "ok.") ? "ok" : "FAILED"; if (st == "FAILED") reds = reds " " name
            printf "round-vm: UNIT-TEST %s %s (%d/%d)\n", name, st, k, total > "/dev/stderr"; fflush("/dev/stderr")
            printf "%d %d%s\n", k, total, reds > prog; close(prog) }'
}
# A test libtest has flagged as running over its slow threshold is a hung test: the watcher
# kills that test binary, so cargo fails the crate by name and the run moves on to the next.
# stdin: a cargo log; prints "<test>\t<binary path>" per flagged test.
unit_hang_scan() {
    awk '/^ *Running / { bin = ""; if (match($0, /\(.*\)/)) bin = substr($0, RSTART + 1, RLENGTH - 2); next }
         /^test .* has been running for over [0-9]+ seconds$/ { print $2 "\t" bin }'
}
unit_kill_binary() {
    local want="${1##*/}" exe p
    [ -n "$want" ] || return 0
    for p in /proc/[0-9]*; do
        exe="$(readlink "$p/exe" 2>/dev/null)" || continue
        [ "${exe##*/}" = "$want" ] && kill -KILL "${p#/proc/}" 2>/dev/null
    done
    return 0
}
unit_hang_watch() {
    local log="$1" report="$2" pid="$3" t b
    while kill -0 "$pid" 2>/dev/null; do
        sleep 2
        while IFS=$'\t' read -r t b; do
            grep -qxF -- "$t	$b" "$report" 2>/dev/null && continue
            printf '%s\t%s\n' "$t" "$b" >> "$report"
            echo "round-vm: UNIT-TESTS: HUNG test=$t binary=${b##*/} past libtest's slow threshold; the binary is killed so the run goes on" >&2
            unit_kill_binary "$b"
        done < <(unit_hang_scan < "$log")
    done
}
# The round's one source of config: the tree's complete fixture, with this VM's own paths
# declared over it. Nothing here is searched for or defaulted.
mkdir -p "$HOME/round-work/.runtime/spira"
cat > "$HOME/round-config.toml" <<ROUNDCFG
[spira]
run = "$HOME/round-work/.runtime/spira"   # where REMOTE_RESULTS pulls batch-results from
releases = "$HOME/round-releases"
home_repo = "$HOME/round-work"
batch_maxpar = $maxpar
$testenv_limits
ROUNDCFG
export SPIRA_TOML="$HOME/round-work/spira-config/tests/fixtures/complete.toml:$HOME/round-config.toml"
SPIRA_TOML="$(target/release/release config-stage "$HOME/round-work")" || { echo "round-vm: cannot apply the head's config delta" >&2; exit 2; }
export SPIRA_TOML
release_red() {
    echo "round-vm: RELEASE-VERIFY: RED at $1 — the head's release cannot be built and verified, so it could not land:" >&2
    grep -v '^ *$' ~/round-release.err | head -20 >&2
    exit 4
}
SPIRA_TOML="$SPIRA_TOML" target/release/spira-config validate > /dev/null 2> ~/round-release.err || release_red "config"
rel_sha="$(SPIRA_HOME="$HOME/round-work/spira" target/release/release build "$(git rev-parse HEAD)" --repo "$HOME/round-work" --bin-dir "$HOME/round-work/target/release" --releases "$HOME/round-releases" 2> ~/round-release.err)" || release_red "build"
SPIRA_HOME="$HOME/round-work/spira" target/release/release verify "$rel_sha" --no-pre-activate --releases "$HOME/round-releases" > /dev/null 2> ~/round-release.err || release_red "verify"
export SPIRA_RELEASE="$HOME/round-releases/$rel_sha"
export SPIRA_REPO="$HOME/round-work"
export PATH="$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:$HOME/spira-env/bin:$CARGO_HOME/bin:$HOME/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
got="$(command -v spira-config || true)"
if [ "$got" != "$SPIRA_RELEASE/bin/spira-config" ]; then
    echo "round-vm: spira-config resolves to ${got:-nothing}, not the staged release $SPIRA_RELEASE" >&2
    exit 2
fi
printf 'export SPIRA_RELEASE=%q\nexport SPIRA_REPO=%q\nexport PATH=%q\n' "$SPIRA_RELEASE" "$SPIRA_REPO" "$PATH" > ~/round-launcher.env
echo "round-vm: launcher: SPIRA_RELEASE=$SPIRA_RELEASE" >&2
tag="$(testenv container tag)" || tag=""
# A ROUND NEVER COLD-BUILDS ITS IMAGE (law-unexpected-image-builds-are-killed-then-fixed,
# sp-lktok). An absent image means this VM predates the template, or the template predates
# the tree: refuse at once, naming it, rather than compile toolchains for twenty minutes in
# the critical path. Exit 3 (cannot tell): no verdict was reached.
if [ -z "$tag" ]; then
    echo "round-vm: IMAGE-ABSENT: testenv could not compute the image tag for this tree — refusing to run (a round never builds its image)" >&2
    exit 3
elif podman image exists "localhost/spira-testenv:$tag" 2>/dev/null; then
    echo "round-vm: template image: localhost/spira-testenv:$tag present" >&2
elif [ -n "$build_image" ]; then
    echo "round-vm: IMAGE-BUILD: localhost/spira-testenv:$tag is not on this VM; the round's head changed the image closure, so it is built here as a declared setup step" >&2
    t_img=$(date +%s)
    if ! testenv container image >&2 || ! podman image exists "localhost/spira-testenv:$tag" 2>/dev/null; then
        echo "round-vm: IMAGE-BUILD-FAILED: localhost/spira-testenv:$tag could not be built on this VM" >&2
        exit 3
    fi
    echo "round-vm: IMAGE-BUILD: built localhost/spira-testenv:$tag in $(( $(date +%s) - t_img ))s (counted as setup)" >&2
else
    echo "round-vm: IMAGE-ABSENT: localhost/spira-testenv:$tag is not on this VM — refusing to cold-build it (law-unexpected-image-builds-are-killed-then-fixed); refresh the template with round-vm template and recycle the warm pool" >&2
    exit 3
fi
setup_secs=$(( $(date +%s) - t_start - unit_build_secs ))
if [ -n "$registry" ]; then export SPIRA_TESTENV_REGISTRY="$registry"; fi
mkdir -p "$HOME/round-resume"
export SPIRA_TESTENV_RESUME="dir:$HOME/round-resume"
sig_out="$HOME/round-work/.runtime/spira/tsd/vm-signals.jsonl"
mkdir -p "$(dirname "$sig_out")"
cpu_now() { awk '/^cpu /{t=0; for(i=2;i<=9;i++) t+=$i; print t, $6, $9}' /proc/stat; }
(
    prev="$(cpu_now)"
    while sleep 10; do
        cur="$(cpu_now)"
        psi="$(awk '/^some/{sub("avg10=","",$2); print $2}' /proc/pressure/io 2>/dev/null)"
        infl="$(cat /sys/block/*/inflight 2>/dev/null | awk '{s+=$1+$2} END{print s+0}')"
        printf '%s %s %s %s\n' "$prev" "$cur" "${psi:-0}" "$infl" | awk -v ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)" -v host="$(hostname)" \
            '{dt=$4-$1; if (dt<=0) next; printf "{\"ts\":\"%s\",\"host\":\"%s\",\"family\":\"vm-signals\",\"iowait_pct\":%.2f,\"steal_pct\":%.2f,\"io_psi_some_avg10\":%s,\"disk_inflight\":%d}\n", ts, host, 100*($5-$2)/dt, 100*($6-$3)/dt, $7, $8}' >> "$sig_out"
        prev="$cur"
    done
) &
sampler_pid=$!
set +e
# THE ROUND'S LINT: the full spira-lint on the merged tree against the landing ref, before the suites.
# A hit is a red round, named for the member whose diff touches the file (a hit in a file no
# member touched is the base's own). Recorded as a suite result after the suites, so a certifier reading results sees it.
lint_rc=0
lint_secs=0
if [ -n "$lint" ]; then
    t_lint=$(date +%s)
    "$HOME/round-work/target/release/spira-lint" --root "$HOME/round-work" --base origin/base > ~/round-lint.out 2> ~/round-lint.err
    lint_rc=$?
    lint_secs=$(( $(date +%s) - t_lint ))
    if [ "$lint_rc" -ne 0 ]; then
        {
            while IFS= read -r finding; do
                path="${finding#*: }"; path="${path%%:*}"
                who="the base itself (no member's diff touches $path)"
                for c in $(git -C "$HOME/round-work" rev-list --first-parent --reverse origin/base..HEAD); do
                    if git -C "$HOME/round-work" diff --name-only "$c^1" "$c" | grep -qxF -- "$path"; then
                        subj="$(git -C "$HOME/round-work" log -1 --format=%s "$c")"
                        who="$(printf '%s\n' "$subj" | sed -n 's/.*merge \([^ ]*\) .*/\1/p')"
                        who="${who:-commit ${c:0:12} $subj}"
                        break
                    fi
                done
                echo "member=$who $finding"
            done < ~/round-lint.out
            cat ~/round-lint.err
        } > ~/round-lint.report
        echo "round-vm: LINT: RED (spira-lint rc=$lint_rc)" >&2
        head -60 ~/round-lint.report >&2
    fi
    echo "round-vm: LINT: ${lint_secs}s of the 900s cap (rc=$lint_rc)" >&2
fi
# THE BASE FENCES the gate runs on every branch beyond the lint (gate.steps), on the merged
# tree: a round that passes the suites but breaks one is red. build-fence is the workspace
# build above and the unit tests below.
fence_fail=""
fence_run() {
    local name="$1"; shift
    ( cd "$HOME/round-work" && "$@" ) > ~/round-fence-"$name".out 2>&1
    local frc=$?
    if [ "$frc" -ne 0 ]; then
        echo "round-vm: FENCE $name: RED (rc=$frc)" >&2
        head -40 ~/round-fence-"$name".out >&2
        fence_fail="$fence_fail $name"
    fi
}
if [ -n "$lint" ]; then
    fence_run lifecycle-guard "$HOME/round-work/target/release/lifecycle-guard" --gate .
    fence_run boundary bash spira/boundary.sh check
fi
# THE WORKSPACE'S OWN UNIT TESTS, once per round (per Ryan 2026-10-05: no suite invokes cargo).
# They run beside the suites, on the binaries built above; a red here makes the round red.
unit_progress="$HOME/round-work/.runtime/spira/batch-results/unit-progress"
mkdir -p "$(dirname "$unit_progress")"
: > ~/round-unit-tests.log
unit_t0=$(date +%s)
unit_cargo --no-fail-fast > ~/round-unit-tests.log 2>&1 &
unit_pid=$!
tail -n +1 -f --pid="$unit_pid" ~/round-unit-tests.log | unit_stream "$unit_total" "$unit_progress" &
stream_pid=$!
unit_hung="$HOME/round-unit-hung"
: > "$unit_hung"
unit_hang_watch ~/round-unit-tests.log "$unit_hung" "$unit_pid" &
hang_pid=$!
# THE CAP CLOCK runs from testenv's first suite to the bare lane's end: container setup, post-processing,
# the unit-test wait and the pulls are outside it, each under its own guard. testenv's "running N
# suite(s)" line is the start mark; one guard times the corpus and the bare lane from it.
suites_cap_guard() {
    local mark="$1" cap="$2" fired="$3" pid alive; shift 3
    until [ -e "$mark" ]; do
        alive=""
        for pid in "$@"; do kill -0 "$pid" 2>/dev/null && alive=1; done
        [ -n "$alive" ] || return 0
        sleep 1
    done
    sleep "$cap"
    for pid in "$@"; do
        if kill -0 "$pid" 2>/dev/null; then
            echo "$(( $(date +%s) - $(stat -c %Y "$mark") ))" > "$fired"
            kill -TERM "$pid" 2>/dev/null
        fi
    done
    sleep 5
    for pid in "$@"; do kill -KILL "$pid" 2>/dev/null; done
}
suites_mark_stream() {
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        printf '%s\n' "$line"
        case "$line" in *"batch: running "*" suite(s)"*) [ -e "$suites_mark" ] || : > "$suites_mark" ;; esac
    done
}
suites_mark="$HOME/round-suites-mark"
cap_fired="$HOME/round-cap-fired"
rm -f "$suites_mark" "$cap_fired"
t_suites=$(date +%s)
echo "round-vm: SUITES-CLOCK: start (cap ${cap}s from the first suite)" >&2
over_cap=""
# THE BARE LANE runs beside the corpus: the bare-host suites SKIP inside the corpus container (it
# has no podman), so the VM itself runs them, with the shared script the publish gate runs.
# Their results replace the corpus's skips once the corpus has written its own; a red makes the
# round red.
bare_out="$HOME/round-bare-results"
rm -rf "$bare_out"; mkdir -p "$bare_out"
if [ -n "$suites" ]; then bare_sel=(--selected "$suites"); else bare_sel=(--suites all); fi
testenv_pidfile="$HOME/round-testenv-pid"
rm -f "$testenv_pidfile"
# The lane boots podman containers of its own: it starts only once the batch has finished its own setup.
bare_after_setup() {
    until [ -e "$suites_mark" ]; do
        [ -s "$testenv_pidfile" ] && ! kill -0 "$(cat "$testenv_pidfile")" 2>/dev/null && break
        sleep 1
    done
    bash spira/bare-suites.sh "$@"
}
bare_after_setup "${bare_sel[@]}" --results "$bare_out" >&2 &
bare_pid=$!
# The suites run on the build above (--artifacts: testenv never runs cargo a second time).
if [ -n "$suites" ]; then
    testenv --mode parallel --artifacts "$HOME/round-work/target/release" --suites "$suites" round > >(suites_mark_stream) 2> >(suites_mark_stream >&2) &
else
    testenv --mode parallel --artifacts "$HOME/round-work/target/release" round > >(suites_mark_stream) 2> >(suites_mark_stream >&2) &
fi
testenv_pid=$!
echo "$testenv_pid" > "$testenv_pidfile"
suites_cap_guard "$suites_mark" "$cap" "$cap_fired" "$testenv_pid" "$bare_pid" &
guard_pid=$!
wait "$testenv_pid"
rc=$?
wait "$bare_pid"
bare_rc=$?
pkill -P "$guard_pid" 2>/dev/null; kill "$guard_pid" 2>/dev/null; wait "$guard_pid" 2>/dev/null
[ -e "$cap_fired" ] && over_cap=1
case "$rc" in 124|137) over_cap=1 ;; esac
case "$bare_rc" in 124|137) over_cap=1 ;; esac
bare_rdir="$HOME/round-work/.runtime/spira/batch-results"
ff="$(find "$bare_rdir" -name fixture-fault.out 2>/dev/null | head -1)"
if [ -n "$ff" ] && [ "$ff" != "$bare_rdir/fixture-fault.out" ]; then cp -f "$ff" "$bare_rdir/fixture-fault.out"; fi
bare_leaf="$(find "$bare_rdir" -name '*.result' -printf '%h\n' 2>/dev/null | head -1)"
bare_leaf="${bare_leaf:-$bare_rdir}"
mkdir -p "$bare_leaf"
cp -rf "$bare_out"/. "$bare_leaf"/
if [ -e "$suites_mark" ]; then suites_clock=$(( $(date +%s) - $(stat -c %Y "$suites_mark") )); else suites_clock=$(( $(date +%s) - t_suites )); fi
echo "round-vm: SUITES-CLOCK: ${suites_clock}s of the ${cap}s cap" >&2
if [ "$bare_rc" -ne 0 ]; then
    echo "round-vm: BARE-SUITES: FAIL (rc=$bare_rc)" >&2
    [ "$rc" -eq 0 ] && rc=1
elif [ -z "$over_cap" ]; then
    echo "round-vm: BARE-SUITES: PASS" >&2
fi
if [ -n "$over_cap" ]; then
    echo "round-vm: OVER-CAP: guard=suites-cap elapsed=${suites_clock}s cap=${cap}s — the suites and bare lane ran past the cap" >&2
fi
if kill -0 "$unit_pid" 2>/dev/null; then echo "round-vm: UNIT-TESTS: waiting (suites done, unit job still running)" >&2; fi
unit_deadline=$(( $(date +%s) + post_guard ))
while kill -0 "$unit_pid" 2>/dev/null && [ "$(date +%s)" -lt "$unit_deadline" ]; do sleep 2; done
if kill -0 "$unit_pid" 2>/dev/null; then
    unit_running="$(awk '/^ *Running / { if (match($0, /\(.*\)/)) b = substr($0, RSTART + 1, RLENGTH - 2); else b = $0 } END { sub(/.*\//, "", b); print b }' ~/round-unit-tests.log)"
    echo "round-vm: UNIT-TESTS: HUNG guard=unit-post elapsed=${post_guard}s past the ${post_guard}s guard after the cap clock stopped; binary=${unit_running:-none-started}; killed" | tee -a "$unit_hung" >&2
    pkill -TERM -P "$unit_pid" 2>/dev/null; kill "$unit_pid" 2>/dev/null
fi
wait "$unit_pid"; unit_rc=$?
unit_secs=$(( $(date +%s) - unit_t0 ))
wait "$stream_pid"
if [ -n "$unit_cov_env" ]; then
    # The line report is read off the profile data the unit run just wrote; it reports and never gates.
    # The report must read the same show-env environment the unit run used, or it looks in the
    # wrong target dir and finds no binaries ("No filenames specified").
    if unit_cargo_report=$(env -i HOME="$HOME" PATH="$HOME/spira-env/bin:$CARGO_HOME/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" CARGO_HOME="$CARGO_HOME" \
        UNIT_COV_ENV="$unit_cov_env" bash -c 'eval "$UNIT_COV_ENV"; exec cargo llvm-cov report --profile unittest --json --summary-only --output-path "$HOME/coverage-lines.json"' 2>&1); then
        cov_rdir="$HOME/round-work/.runtime/spira/batch-results"
        cov_leaf="$(find "$cov_rdir" -name '*.result' -printf '%h\n' 2>/dev/null | head -1)"
        cov_leaf="${cov_leaf:-$cov_rdir}"
        mkdir -p "$cov_leaf"
        cp "$HOME/coverage-lines.json" "$cov_leaf/coverage-lines.json"
        printf '%s\n' "$HOME/round-work" > "$cov_leaf/coverage-root"
    else
        echo "round-vm: COVERAGE: lines unavailable (cargo llvm-cov report failed): $(printf '%s' "$unit_cargo_report" | tail -5)" >&2
    fi
fi
kill "$hang_pid" 2>/dev/null; wait "$hang_pid" 2>/dev/null
[ ! -s "$unit_hung" ] || [ "$unit_rc" -ne 0 ] || unit_rc=1
if [ "$lint_rc" -ne 0 ]; then
    rdir="$HOME/round-work/.runtime/spira/batch-results"
    leaf="$(find "$rdir" -name '*.result' -printf '%h\n' 2>/dev/null | head -1)"
    leaf="${leaf:-$rdir}"
    mkdir -p "$leaf"
    cp ~/round-lint.report "$leaf/spira-lint.out"
    echo "red 1 $lint_secs - - - $lint_rc" > "$leaf/spira-lint.result"
    [ "$rc" -eq 0 ] && rc=1
fi
if [ -n "$fence_fail" ]; then
    rdir="$HOME/round-work/.runtime/spira/batch-results"
    leaf="$(find "$rdir" -name '*.result' -printf '%h\n' 2>/dev/null | head -1)"
    leaf="${leaf:-$rdir}"
    mkdir -p "$leaf"
    for name in $fence_fail; do
        cp ~/round-fence-"$name".out "$leaf/fence-$name.out"
        echo "red 1 0 - - - 1" > "$leaf/fence-$name.result"
    done
    [ "$rc" -eq 0 ] && rc=1
fi
if [ "$unit_rc" -eq 0 ]; then
    echo "round-vm: UNIT-TESTS: PASS ($(grep -c '^test result: ok' ~/round-unit-tests.log) test binaries)" >&2
else
    echo "round-vm: UNIT-TESTS: FAIL (unit run rc=$unit_rc):" >&2
    grep -E -A3 '^(test .* FAILED|---- |error(\[|:))|panicked at' ~/round-unit-tests.log | head -120 >&2
    rdir="$HOME/round-work/.runtime/spira/batch-results"
    leaf="$(find "$rdir" -name '*.result' -printf '%h\n' 2>/dev/null | head -1)"
    leaf="${leaf:-$rdir}"
    mkdir -p "$leaf"
    if [ "$unit_rc" -gt 128 ]; then unit_cause="killed by signal $(( unit_rc - 128 ))"; else unit_cause="exit status $unit_rc"; fi
    [ ! -s "$unit_hung" ] || unit_cause="$unit_cause, hung past its guard"
    unit_last="$(awk '/^ *Running / { if (match($0, /\(.*\)/)) b = substr($0, RSTART + 1, RLENGTH - 2); else b = $0 } END { sub(/.*\//, "", b); print b }' ~/round-unit-tests.log)"
    unit_failed="$(grep -c -E '^test .* FAILED|^test result: FAILED' ~/round-unit-tests.log)"
    {
        echo "UNIT-TESTS-CAUSE: $unit_cause after ${unit_secs}s; last binary=${unit_last:-none-started}; failures=$unit_failed"
        grep -E -A8 '^(test .* FAILED|---- |error(\[|:))|panicked at|^test result: FAILED|^failures:' ~/round-unit-tests.log | head -400
        echo "--- last output of the log ---"
        tail -100 ~/round-unit-tests.log
    } > "$leaf/unit-tests.out"
    [ ! -s "$unit_hung" ] || { echo "round-vm: UNIT-TESTS: HUNG (test<TAB>binary):"; cat "$unit_hung"; } >> "$leaf/unit-tests.out"
    [ "$rc" -eq 0 ] && rc=1
fi
suites_secs=$suites_clock
kill "$sampler_pid" 2>/dev/null; wait "$sampler_pid" 2>/dev/null
echo "round-vm: setup ${setup_secs}s, suites ${suites_secs}s" >&2
if [ "$setup_secs" -gt "$setup_alarm" ]; then
    echo "round-vm: SETUP-SLOW: setup took ${setup_secs}s, over the ${setup_alarm}s limit — something is being built or fetched that the template should hold" >&2
fi
mkdir -p ~/round-bins
find target/release -maxdepth 1 -type f -executable -exec cp {} ~/round-bins/ \; 2>/dev/null
if [ -n "$over_cap" ]; then exit 124; fi
exit "$rc"
"#;

/// What REMOTE_SCRIPT leaves on the VM for the attribution jobs: the launcher's
/// `SPIRA_RELEASE` and `PATH`, as `export` lines.
pub const LAUNCHER_ENV: &str = "round-launcher.env";

/// Where REMOTE_SCRIPT stages the round's executables, relative to the VM user's home.
pub const REMOTE_BINS: &str = "round-bins/";

/// The VM script's own stderr, kept so a round that never produced binaries can say why.
pub const FIXTURE_FAULT_FILE: &str = "fixture-fault.out";

pub const REMOTE_SETUP_LOG: &str = "round-setup.log";

const SETUP_LOG_TAIL: usize = 60;

/// The last lines of `log`, or `None` when it is empty.
pub fn log_tail(log: &str, n: usize) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines[lines.len().saturating_sub(n)..].join("\n"))
}

/// Prints the VM's setup log tail: a failure carries its own explanation.
fn show_setup_log(remote: &dyn Remote, cfg: &Config, addr: &str, scratch: &Path) {
    let _ = fs::remove_dir_all(scratch);
    if pull_retry(remote, cfg, addr, REMOTE_SETUP_LOG, scratch) {
        let text = fs::read_to_string(scratch.join(REMOTE_SETUP_LOG)).unwrap_or_default();
        match log_tail(&text, SETUP_LOG_TAIL) {
            Some(t) => eprintln!("round-vm run: no round-bins came back; the VM's setup log, last {SETUP_LOG_TAIL} lines:\n{t}"),
            None => eprintln!("round-vm run: no round-bins came back and the VM's setup log is empty"),
        }
    } else {
        eprintln!("round-vm run: no round-bins came back and the VM's setup log could not be pulled");
    }
    let _ = fs::remove_dir_all(scratch);
}

/// The Proxmox template's cache home is `SPIRA_ROUND_VM_CACHE_HOME`; an EC2 image records its
/// own, which the VM reads, so the template's path is never sent to it.
pub(crate) fn job_cache_home(ec2: bool, configured: Option<&str>) -> Option<String> {
    if ec2 { None } else { configured.map(str::to_string) }
}

pub fn remote_command(job: &BatchJob) -> String {
    let mut args = vec![
        job.host_addr.clone(),
        job.mirror_port.to_string(),
        job.suites.clone().unwrap_or_default(),
        job.maxpar.to_string(),
        job.toolchain.clone().unwrap_or_default(),
        job.cache_home.clone().unwrap_or_default(),
        job.testenv_registry.clone().unwrap_or_default(),
        job.setup_alarm_secs.to_string(),
        job.cap_secs.to_string(),
        job.post_guard_secs.to_string(),
        if job.lint { "1" } else { "" }.to_string(),
        if job.build_image { "1" } else { "" }.to_string(),
    ];
    args.push(job.round_ref.clone());
    args.push(job.round_sha.clone());
    args.push(job.ci_entry.clone().unwrap_or_default());
    args.push(job.testenv_limits.clone());
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    format!("bash -s -- {}", quoted.join(" "))
}

pub struct SshRemote {
    pub user: String,
    pub port: u16,
    pub key: PathBuf,
}

impl SshRemote {
    pub(crate) fn ssh_opts(&self) -> Vec<String> {
        [
            "-i", &self.key.to_string_lossy(), "-p", &self.port.to_string(),
            "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

impl Remote for SshRemote {
    fn reachable(&self, addr: &str) -> bool {
        command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run_batch(&self, addr: &str, job: &BatchJob) -> Result<i32, String> {
        let mut child = command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(remote_command(job))
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ssh: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            si.write_all(if job.ci_entry.is_some() { FOREIGN_SCRIPT } else { REMOTE_SCRIPT }.as_bytes()).map_err(|e| format!("ssh stdin: {e}"))?;
        }
        let st = child.wait().map_err(|e| format!("ssh: {e}"))?;
        Ok(st.code().unwrap_or(255))
    }

    fn run_attr(&self, addr: &str, job: &crate::spool::AttrJob) -> Result<i32, String> {
        let mut child = command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(crate::spool::attr_command(job))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| format!("ssh: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            si.write_all(crate::spool::REMOTE_ATTR_SCRIPT.as_bytes()).map_err(|e| format!("ssh stdin: {e}"))?;
        }
        let st = child.wait().map_err(|e| format!("ssh: {e}"))?;
        Ok(st.code().unwrap_or(255))
    }

    fn push(&self, addr: &str, local: &Path, remote_path: &str) -> Result<(), String> {
        let st = command("rsync")
            .arg("-a")
            .arg("-e")
            .arg(format!("ssh {}", self.ssh_opts().join(" ")))
            .arg(format!("{}/", local.display()))
            .arg(format!("{}@{addr}:{remote_path}", self.user))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("rsync: {e}"))?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("rsync to {remote_path} exited {}", st.code().unwrap_or(-1)))
        }
    }

    fn pull(&self, addr: &str, remote_path: &str, local: &Path) -> Result<(), String> {
        fs::create_dir_all(local).map_err(|e| format!("{}: {e}", local.display()))?;
        let st = command("rsync")
            .arg("-a")
            .arg("-e")
            .arg(format!("ssh {}", self.ssh_opts().join(" ")))
            .arg(format!("{}@{addr}:{remote_path}", self.user))
            .arg(format!("{}/", local.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("rsync: {e}"))?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("rsync {remote_path} exited {}", st.code().unwrap_or(-1)))
        }
    }
}

pub struct GitHost {
    pub state_dir: PathBuf,
    pub mirror_port: u16,
    /// The LAN address the VMs reach; the daemon binds only there and on `also_listen`.
    pub listen: String,
    /// Further addresses of this host the daemon binds, e.g. the tailnet one a spilled VM dials.
    pub also_listen: Vec<String>,
}

impl GitHost {
    fn listen_set(&self) -> Vec<&str> {
        let mut v = vec![self.listen.as_str()];
        v.extend(self.also_listen.iter().map(String::as_str).filter(|a| !a.is_empty() && *a != self.listen));
        v
    }
}

pub(crate) fn git(args: &[&str]) -> Result<String, String> {
    let out = command("git").args(args).stdin(Stdio::null()).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The addresses on each line of `ip -br addr`, prefix lengths stripped.
fn host_addresses(ip_br_addr: &str) -> Vec<String> {
    ip_br_addr
        .lines()
        .flat_map(|l| l.split_whitespace().skip(2))
        .map(|a| a.split('/').next().unwrap_or(a).to_string())
        .collect()
}

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// Stops the mirror daemon `prepare_mirror` left running for `state_dir`, and waits for it
/// to be gone. Nothing running is success.
pub fn stop_mirror(state_dir: &Path) -> Result<(), String> {
    let pidfile = state_dir.join("git-daemon.pid");
    let Some(pid) = fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok()) else {
        return Ok(());
    };
    if pid_alive(pid) {
        // SAFETY: plain SIGTERM to the pid the daemon wrote for this state dir.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        for _ in 0..50 {
            if !pid_alive(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if pid_alive(pid) {
            return Err(format!("git daemon {pid} did not exit after SIGTERM"));
        }
    }
    let _ = fs::remove_file(&pidfile);
    Ok(())
}

impl Host for GitHost {
    fn is_checkout(&self, tree: &Path) -> bool {
        git(&["-C", &tree.to_string_lossy(), "rev-parse", "--git-dir"]).is_ok()
    }

    fn head(&self, tree: &Path) -> Result<(String, String), String> {
        let t = tree.to_string_lossy();
        Ok((git(&["-C", &t, "rev-parse", "HEAD"])?, git(&["-C", &t, "rev-parse", "HEAD^{tree}"])?))
    }

    fn image_tag(&self, tree: &Path, commit: &str) -> Result<String, String> {
        let scratch = self.state_dir.join(format!(".tag.{}", std::process::id()));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        let r = (|| {
            let mut archive = command("git")
                .args(["-C", &tree.to_string_lossy(), "archive", "--format=tar", commit, "spira/testenv/Containerfile", "spira/deps.toml", "rust-toolchain.toml"])
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|e| format!("git archive: {e}"))?;
            let out = archive.stdout.take().ok_or("git archive: no stdout")?;
            let st = command("tar").arg("-x").arg("-C").arg(&scratch).stdin(out).status().map_err(|e| format!("tar: {e}"))?;
            if !archive.wait().map_err(|e| e.to_string())?.success() || !st.success() {
                return Err(format!("cannot read the image closure of {commit}"));
            }
            let o = command("testenv")
                .args(["container", "tag"])
                .env("SPIRA_TESTENV_HARNESS", &scratch)
                .output()
                .map_err(|e| format!("testenv container tag: {e}"))?;
            let tag = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && !tag.is_empty() {
                Ok(tag)
            } else {
                Err(format!(
                    "`SPIRA_TESTENV_HARNESS={} testenv container tag` exited {}: {}",
                    scratch.display(),
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).trim()
                ))
            }
        })();
        let _ = fs::remove_dir_all(&scratch);
        r
    }

    fn suites_in(&self, tree: &Path) -> Result<Vec<String>, String> {
        let out = git(&["-C", &tree.to_string_lossy(), "ls-tree", "--name-only", "HEAD", "spira/"])?;
        let mut v: Vec<String> = out
            .lines()
            .filter_map(|l| l.strip_prefix("spira/"))
            .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
            .map(str::to_string)
            .collect();
        v.sort();
        Ok(v)
    }

    fn suite_medians(&self) -> Result<std::collections::HashMap<String, f64>, String> {
        let o = command("testenv").arg("--report").stdin(Stdio::null()).output().map_err(|e| format!("testenv --report: {e}"))?;
        if !o.status.success() {
            return Err(format!("testenv --report exited {}: {}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).trim()));
        }
        crate::lpt::parse_medians(&String::from_utf8_lossy(&o.stdout))
    }

    fn prepare_mirror(&self, tree: &Path, round_ref: &str) -> Result<(), String> {
        if self.listen.is_empty() {
            return Err("no listen address for the mirror daemon (SPIRA_ROUND_VM_HOST_ADDR)".into());
        }
        let out = command("ip").args(["-br", "addr"]).stdin(Stdio::null()).output().map_err(|e| format!("ip -br addr: {e}"))?;
        let addrs = host_addresses(&String::from_utf8_lossy(&out.stdout));
        let wanted = self.listen_set();
        if let Some(gone) = wanted.iter().find(|w| !out.status.success() || !addrs.iter().any(|a| a == *w)) {
            return Err(format!(
                "{gone} is not an address of this host (ip -br addr: {}); a pinned round_vm_host_addr no longer names this host — \
                 set it to auto, which resolves the address each round",
                addrs.join(" ")
            ));
        }
        let listen_key = wanted.join(",");
        fs::create_dir_all(&self.state_dir).map_err(|e| e.to_string())?;
        let mirror = self.state_dir.join("mirror.git");
        let m = mirror.to_string_lossy().to_string();
        if !mirror.is_dir() {
            git(&["init", "--quiet", "--bare", &m])?;
        }
        git(&["--git-dir", &m, "fetch", "--quiet", &tree.to_string_lossy(), &format!("+HEAD:{round_ref}")])?;

        let pidfile = self.state_dir.join("git-daemon.pid");
        let running = |p: &Path| fs::read_to_string(p).ok().and_then(|s| s.trim().parse().ok()).map(pid_alive).unwrap_or(false);
        let addrfile = self.state_dir.join("git-daemon.addr");
        let bound_to = fs::read_to_string(&addrfile).ok().map(|s| s.trim().to_string());
        if running(&pidfile) {
            if bound_to.as_deref() == Some(listen_key.as_str()) {
                return Ok(());
            }
            stop_mirror(&self.state_dir)?;
        }
        let _ = fs::remove_file(&pidfile);
        let _ = fs::remove_file(&addrfile);
        let mut daemon = command("git");
        daemon.arg("daemon").arg("--reuseaddr");
        for a in &wanted {
            daemon.arg(format!("--listen={a}"));
        }
        let st = daemon
            .arg(format!("--port={}", self.mirror_port))
            .arg(format!("--base-path={}", self.state_dir.display()))
            .arg("--export-all")
            .arg(format!("--pid-file={}", pidfile.display()))
            .arg("--detach")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("git daemon: {e}"))?;
        for _ in 0..20 {
            if running(&pidfile) {
                return fs::write(&addrfile, &listen_key).map_err(|e| format!("{}: {e}", addrfile.display()));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(format!("git daemon did not start on port {} (exit {:?})", self.mirror_port, st.code()))
    }

    fn drop_round_ref(&self, round_ref: &str) {
        let m = self.state_dir.join("mirror.git").to_string_lossy().to_string();
        let _ = git(&["--git-dir", &m, "update-ref", "-d", round_ref]);
    }

    fn mirror_ref(&self, tree: &Path, branch: &str, as_ref: &str) -> Result<(), String> {
        let m = self.state_dir.join("mirror.git").to_string_lossy().to_string();
        git(&["--git-dir", &m, "fetch", "--quiet", &tree.to_string_lossy(), &format!("+{branch}:refs/heads/{as_ref}")]).map(|_| ())
    }
}

/// The directory that holds the pulled `.result` files: `dir` itself, or the one
/// BATCH_KEY subdirectory the testenv runner nests them under.
pub fn results_leaf(dir: &Path) -> Option<PathBuf> {
    let has_result = |d: &Path| {
        fs::read_dir(d)
            .map(|rd| rd.flatten().any(|e| e.path().extension().map(|x| x == "result").unwrap_or(false)))
            .unwrap_or(false)
    };
    if has_result(dir) {
        return Some(dir.to_path_buf());
    }
    let mut subs: Vec<PathBuf> = fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    subs.sort();
    subs.into_iter().find(|p| has_result(p))
}

/// Sum of every suite's own wall time: the third field of each `<suite>.result`.
pub fn suite_wall_sum(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter(|e| e.path().extension().map(|x| x == "result").unwrap_or(false))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| t.split_whitespace().nth(2).and_then(|s| s.parse::<u64>().ok()))
        .sum()
}

/// testenv's `build_wall_s` from `runner.meta`, present only when it built.
pub fn build_wall(dir: &Path) -> Option<u64> {
    let t = fs::read_to_string(dir.join("runner.meta")).ok()?;
    t.lines().find_map(|l| l.strip_prefix("build_wall_s=")).and_then(|v| v.trim().parse().ok())
}

/// The tree the VM's testenv run built and tested: `tree=` in the pulled `batch.meta`.
pub fn reported_tree(results: &Path) -> Option<String> {
    let t = fs::read_to_string(results.join("batch.meta")).ok()?;
    t.lines().find_map(|l| l.strip_prefix("tree=")).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The executables directly in `dir` (the pulled `round-bins/`).
fn executables(dir: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for e in fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?.flatten() {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_symlink() {
            let target = fs::read_link(&from).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&to);
            std::os::unix::fs::symlink(target, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        } else {
            fs::copy(&from, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
    }
    Ok(())
}

/// Installs the pulled executables into `<target>` — the round worktree's own
/// `target/release`, where `queue land-local --worktree` reads them (G8). The VM's testenv
/// must report (batch.meta `tree=`) exactly the tree the host sent; any other tree is a
/// refusal naming both, and no tree at all (the run died first) is a distinct error;
/// either installs nothing. No executables at all (the batch never
/// reached its build) is not a refusal.
pub fn install_bins(pulled: &Path, expected: &str, reported: Option<&str>, target: &Path) -> Result<(), String> {
    let exes = executables(pulled);
    if exes.is_empty() {
        return Ok(());
    }
    match reported {
        None => return Err("round-vm run: the VM reported no tested tree (batch did not complete) — nothing installed".into()),
        Some(r) if r != expected => {
            return Err(format!("round-vm run: refusing binaries: tree sha {r} does not match expected {expected} — nothing installed"))
        }
        Some(_) => {}
    }
    fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    for exe in exes {
        let to = target.join(exe.file_name().unwrap());
        let _ = fs::remove_file(&to);
        fs::copy(&exe, &to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

/// Takes the VM's tsd rows to the host, each tagged with `ran_on`, `vcpus` and `maxpar`: a row of
/// a family held in the metrics store is pushed to `store`, any other is appended to
/// `<run>/tsd/<family>.jsonl` unless already present, under `<file>.lock`.
pub fn merge_tsd(src: &Path, run_dir: &Path, vm: &str, vcpus: u32, maxpar: u32, store: Option<&str>) -> Result<usize, String> {
    let Ok(rd) = fs::read_dir(src) else { return Ok(0) };
    let mut added = 0;
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false)).collect();
    files.sort();
    let mut refused: Option<String> = None;
    for f in files {
        let family = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if tsd::store::moved(&family) {
            for line in fs::read_to_string(&f).unwrap_or_default().lines() {
                let Ok(Value::Object(mut row)) = serde_json::from_str::<Value>(line) else { continue };
                row.insert("ran_on".into(), Value::from(vm));
                row.insert("vcpus".into(), Value::from(vcpus));
                row.insert("maxpar".into(), Value::from(maxpar));
                match tsd::store::push_row(store, &family, &Value::Object(row).to_string()) {
                    Ok(()) => added += 1,
                    Err(e) => refused = Some(e),
                }
            }
            continue;
        }
        let dest = run_dir.join("tsd").join(f.file_name().unwrap());
        let mut lock_path = dest.clone().into_os_string();
        lock_path.push(".lock");
        let _lock = FileLock::exclusive(Path::new(&lock_path))?;
        let mut seen: HashSet<String> = fs::read_to_string(&dest).unwrap_or_default().lines().map(str::to_string).collect();
        let mut out = fs::OpenOptions::new().create(true).append(true).open(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        for line in fs::read_to_string(&f).unwrap_or_default().lines() {
            let Ok(Value::Object(mut row)) = serde_json::from_str::<Value>(line) else { continue };
            row.insert("ran_on".into(), Value::from(vm));
            row.insert("vcpus".into(), Value::from(vcpus));
            row.insert("maxpar".into(), Value::from(maxpar));
            let s = Value::Object(row).to_string();
            if seen.insert(s.clone()) {
                writeln!(out, "{s}").map_err(|e| format!("{}: {e}", dest.display()))?;
                added += 1;
            }
        }
    }
    match refused {
        Some(e) => Err(e),
        None => Ok(added),
    }
}

pub struct RunEnv<'a> {
    pub cfg: &'a Config,
    pub pool: &'a Pool,
    pub deps: &'a Deps<'a>,
    pub host: &'a dyn Host,
    pub remote: &'a dyn Remote,
    pub record: &'a dyn Recorder,
}

/// Writes a pass boundary onto the round's batch row.
pub trait Recorder: Sync {
    fn pass_started(&self, batch: &str, repo: Option<&str>) -> Result<(), String>;
    fn suites_started(&self, batch: &str, repo: Option<&str>) -> Result<(), String>;
    fn pass_incomplete(&self, batch: &str, repo: Option<&str>, reason: &str) -> Result<(), String>;
    fn progress(&self, batch: &str, body: &str) -> Result<(), String>;

    /// The batch's current pass number and members, for the pass's record; `None` when the
    /// lifecycle machine cannot say.
    fn pass_context(&self, batch: &str) -> Option<(u64, Vec<String>)> {
        let out = command("timeout").args(["-k", "5", spira_config::release_env::tool("30").as_str(), "spira-lc", "show-batch", batch]).stdin(Stdio::null()).output().ok()?;
        let v: Value = serde_json::from_slice(&out.stdout).ok()?;
        let members = v.get("members").and_then(Value::as_array).map(|a| a.iter().filter_map(|m| m.as_str().or_else(|| m.get("bead_id").and_then(Value::as_str)).map(str::to_string)).collect()).unwrap_or_default();
        Some((v.get("pass")?.as_u64()?, members))
    }

    /// Appends the pass's one record under `<root>/tsd`.
    fn pass_record(&self, root: &Path, rec: &tsd::pass::PassRecord) -> Result<(), String> {
        rec.write(root, &tsd::default_host(), &tsd::now_iso())
    }
}

/// `queue round suites-started`: the lifecycle machine's own verb, so a refusal names the phase.
pub struct QueueRecorder;

impl Recorder for QueueRecorder {
    fn progress(&self, batch: &str, body: &str) -> Result<(), String> {
        use std::io::Write;
        let mut cmd = command("timeout");
        cmd.args(["-k", "5", spira_config::release_env::tool("30").as_str(), "spira-lc", "batch-progress", batch]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("cannot run spira-lc: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| format!("spira-lc batch-progress: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(progress_refusal(batch, out.status.code(), &out.stdout, &out.stderr))
    }

    fn pass_started(&self, batch: &str, repo: Option<&str>) -> Result<(), String> {
        let mut cmd = command("timeout");
        cmd.args(["-k", "5", spira_config::release_env::tool("60").as_str(), "queue", "round", "pass-start", "--resume", batch]).args(repo).stdin(Stdio::null());
        let out = cmd.output().map_err(|e| format!("cannot run queue: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!("queue round pass-start refused for {batch} (rc={}): {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
    }

    fn pass_incomplete(&self, batch: &str, repo: Option<&str>, reason: &str) -> Result<(), String> {
        let mut cmd = command("timeout");
        cmd.args(["-k", "5", spira_config::release_env::tool("60").as_str(), "queue", "round", "pass-verdict", "--verdict", "incomplete", "--reason", reason, batch]).args(repo).stdin(Stdio::null());
        let out = cmd.output().map_err(|e| format!("cannot run queue: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!("queue round pass-verdict incomplete refused for {batch} (rc={}): {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
    }

    fn suites_started(&self, batch: &str, repo: Option<&str>) -> Result<(), String> {
        let mut cmd = command("timeout");
        cmd.args(["-k", "5", spira_config::release_env::tool("60").as_str(), "queue", "round", "suites-started", batch]).args(repo).stdin(Stdio::null());
        let out = cmd.output().map_err(|e| format!("cannot run queue: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!("queue round suites-started refused for {batch} (rc={}): {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// spira-lc states a refusal's reason on stdout, so both streams are the reason.
fn progress_refusal(batch: &str, rc: Option<i32>, stdout: &[u8], stderr: &[u8]) -> String {
    let reason = [stdout, stderr].map(|b| String::from_utf8_lossy(b).trim().to_string()).into_iter().filter(|r| !r.is_empty()).collect::<Vec<_>>().join(" | ");
    let reason = if reason.is_empty() { "(spira-lc said nothing)".to_string() } else { reason };
    format!("spira-lc batch-progress refused for {batch} (rc={}): {reason}", rc.unwrap_or(-1))
}

/// The pass's live counts onto its batch row; telemetry, so a refusal is logged and never fails the pass.
fn publish_progress(env: &RunEnv, args: &RunArgs, progress: &mut crate::progress::Progress) {
    let (Some(batch), Some(body)) = (args.round_batch.as_deref(), progress.take_publish()) else { return };
    if let Err(e) = env.record.progress(batch, &body) {
        eprintln!("round-vm run: {e}");
    }
}

/// The suites boundary, written the first time the pass is seen past its build. A refused write is
/// kept so the pass ends a fault: a pass the batch row cannot follow is not one to certify.
fn record_boundary(env: &RunEnv, args: &RunArgs, progress: &mut crate::progress::Progress, refused: &mut Option<String>) {
    let Some(batch) = args.round_batch.as_deref() else { return };
    publish_progress(env, args, progress);
    if progress.take_suites_began() {
        if let Err(e) = env.record.suites_started(batch, args.round_repo.as_deref()) {
            eprintln!("round-vm run: {e}");
            refused.get_or_insert(e);
        }
    }
}

/// A batch that finished between two polls is still recorded; a refused record turns a judged pass
/// (green or red) into a fault.
fn settle_boundary(env: &RunEnv, args: &RunArgs, progress: &mut crate::progress::Progress, results_dir: &Path, refused: &mut Option<String>, code: i32) -> i32 {
    progress.update(results_dir);
    record_boundary(env, args, progress, refused);
    match refused {
        Some(e) if code == 0 || code == 1 => {
            eprintln!("round-vm run: the pass could not be recorded on its batch ({e}); it is not judged");
            2
        }
        _ => code,
    }
}

/// What the pass learned about the VM it ran on, for its record.
#[derive(Default)]
struct Facts {
    started: Option<u64>,
    wait_s: u64,
    handle: Option<String>,
    instance_type: Option<String>,
    marks: Option<crate::progress::Marks>,
}

/// `round-vm run`. Returns the process exit code (DESIGN.md §2.2). A pass that began is recorded
/// exactly once, whatever way it ended; a record that cannot be written ends the pass unjudged.
pub fn run(env: &RunEnv, args: &RunArgs) -> i32 {
    let facts = Mutex::new(Facts::default());
    let code = run_pass(env, args, &facts);
    let facts = facts.into_inner().unwrap_or_else(|e| e.into_inner());
    let (Some(batch), Some(started)) = (args.round_batch.as_deref(), facts.started) else { return code };
    let marks = facts.marks.clone().unwrap_or_default();
    let (boot_s, build_s, suites_s) = marks.phases(facts.wait_s, crate::progress::now_secs());
    let lost = marks.get().lost;
    let place = if facts.handle.as_deref().is_some_and(crate::ec2::is_ec2_handle) { tsd::pass::Place::Ec2 } else { tsd::pass::Place::Proxmox };
    let vcpus = match place {
        tsd::pass::Place::Ec2 => facts.instance_type.as_deref().and_then(crate::ec2::vcpus_of),
        _ => Some(env.cfg.vcpus),
    };
    let instance_type = facts.instance_type.clone().unwrap_or_else(|| format!("{}vcpu", env.cfg.vcpus));
    crate::telemetry::push_placement(place.as_str(), &instance_type, facts.wait_s, boot_s);
    let (pass, members) = match env.record.pass_context(batch) {
        Some((n, m)) => (n.to_string(), m),
        None => (format!("t{started}"), Vec::new()),
    };
    let record = tsd::pass::PassRecord {
        repo: args.round_repo.clone().unwrap_or_else(|| "-".into()),
        batch: batch.to_string(),
        pass,
        members,
        instance_type,
        vcpus,
        place,
        capacity_wait_s: facts.wait_s,
        boot_s,
        build_s,
        suites_s,
        cause: tsd::pass::Cause::from_exit(code, lost.is_some()),
        detail: lost.unwrap_or_default(),
    };
    match env.record.pass_record(&env.cfg.run_dir, &record) {
        Ok(()) => {
            crate::telemetry::push_pass(&record);
            code
        }
        Err(e) => {
            let why = format!("the pass's record could not be written ({e}); its verdict is lost");
            eprintln!("round-vm run: {why}");
            record_incomplete(env, args, &why);
            if code == 0 || code == 1 { 2 } else { code }
        }
    }
}

struct RoundRef<'a> {
    host: &'a dyn Host,
    name: &'a str,
}

impl Drop for RoundRef<'_> {
    fn drop(&mut self) {
        self.host.drop_round_ref(self.name);
    }
}

fn run_pass(env: &RunEnv, args: &RunArgs, facts: &Mutex<Facts>) -> i32 {
    let cfg = env.cfg;
    if !env.host.is_checkout(&args.tree_dir) {
        eprintln!("round-vm run: not a git checkout: {}", args.tree_dir.display());
        return 2;
    }
    let Some(host_addr) = cfg.host_addr.clone() else {
        eprintln!("round-vm run: this host's address is unresolved (SPIRA_ROUND_VM_HOST_ADDR, auto by default)");
        return 2;
    };
    if fs::File::open(&cfg.host_key).is_err() {
        eprintln!("round-vm run: SPIRA_ROUND_VM_HOST_KEY not readable: {}", cfg.host_key.display());
        return 2;
    }
    if cfg.cache_home.is_none() {
        eprintln!(
            "round-vm run: SPIRA_ROUND_VM_CACHE_HOME is not set, by environment or in the \
             operator's own config file — cannot resolve the VM-side CARGO_HOME; the \
             template's own sccache lives wherever `round-vm template` was told to put it, \
             and this binary refuses to guess (law-a-binary-resolves-the-config-it-reads). \
             Set it to match."
        );
        return 2;
    }
    let (commit_sha, tree_sha) = match env.host.head(&args.tree_dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("round-vm run: {e}");
            return 2;
        }
    };
    let image_missing = image_build_declared(env, args, &commit_sha);
    let build_image = args.ci_entry.is_none() && image_missing;
    let ordered;
    let args = if args.suites.is_none() && args.ci_entry.is_none() {
        match lpt_suites(env.host, &args.tree_dir) {
            Ok(list) => {
                ordered = RunArgs { suites: Some(list), ..args.clone() };
                &ordered
            }
            Err(e) => {
                eprintln!("round-vm run: cannot order the corpus longest-first: {e}");
                return 2;
            }
        }
    } else {
        args
    };
    let round_ref = format!("refs/rounds/{}-{commit_sha}", std::process::id());
    let _round_ref = RoundRef { host: env.host, name: &round_ref };
    if let Err(e) = env.host.prepare_mirror(&args.tree_dir, &round_ref) {
        eprintln!("round-vm run: cannot prepare the mirror from {}: {e}", args.tree_dir.display());
        return 2;
    }
    if let Some(base) = &args.base {
        if let Err(e) = env.host.mirror_ref(&args.tree_dir, base, "base") {
            eprintln!("round-vm run: cannot mirror the base {base}: {e}");
            return 2;
        }
    }
    let results_dir = args.results_dir.clone().unwrap_or_else(|| cfg.run_dir.join("batch-results"));
    let total = args.suites.as_deref().map_or(0, |l| l.split(',').filter(|x| !x.is_empty()).count());
    let budgets = crate::progress::Budgets { vm: cfg.vm_budget_secs, build: cfg.build_budget_secs };
    let progress = Mutex::new(crate::progress::Progress::start(&cfg.run_dir, &results_dir, &commit_sha, &tree_sha, total, cfg.cap_secs, budgets).on_red(args.on_red.clone().filter(|_| args.ci_entry.is_none())));
    if let Some(batch) = args.round_batch.as_deref() {
        if let Err(e) = env.record.pass_started(batch, args.round_repo.as_deref()) {
            eprintln!("round-vm run: {e}; the pass is not run unrecorded");
            return 2;
        }
    }
    let leased_from = Instant::now();
    {
        let mut f = facts.lock().unwrap_or_else(|e| e.into_inner());
        f.started = Some(crate::progress::now_secs());
        f.marks = Some(progress.lock().unwrap_or_else(|e| e.into_inner()).marks());
    }
    let acquired = {
        let leasing = AtomicBool::new(true);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                let mut beat = 0u32;
                while leasing.load(Ordering::SeqCst) {
                    beat += 1;
                    if beat % 10 != 1 {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    let detail = pool_detail(&env.pool.state_file());
                    let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                    match detail {
                        Some(d) => p.detail(&results_dir, d),
                        None => p.tick(&results_dir),
                    }
                    drop(p);
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
            let got = env.pool.acquire_placed(env.deps, Some(ProcId::current()), None, image_missing);
            leasing.store(false, Ordering::SeqCst);
            got
        })
    };
    let mut progress = progress.into_inner().unwrap_or_else(|e| e.into_inner());
    let (vm, mode) = match acquired {
        Ok(v) => v,
        Err(e) => {
            eprintln!("round-vm run: acquire failed: {e}");
            progress.finish(&results_dir, 2);
            return 2;
        }
    };
    env.pool.label(&vm.handle, &run_label(args));
    let instance_type = (env.deps.factory)().ok().and_then(|a| a.provider.instance_type(&vm.handle));
    {
        let mut f = facts.lock().unwrap_or_else(|e| e.into_inner());
        f.wait_s = leased_from.elapsed().as_secs();
        f.handle = Some(vm.handle.clone());
        f.instance_type = instance_type.clone();
    }
    match &instance_type {
        Some(t) => eprintln!("round-vm run: {} {} {} type={t}", vm.handle, vm.addr, mode.as_str()),
        None => eprintln!("round-vm run: {} {} {}", vm.handle, vm.addr, mode.as_str()),
    }
    let scaled;
    let args = match spilled_maxpar(args.maxpar, instance_type.as_deref(), crate::config::declared("SPIRA_ROUND_VM_MAXPAR").is_some()) {
        Some(n) => {
            scaled = RunArgs { maxpar: Some(n), ..args.clone() };
            &scaled
        }
        None => args,
    };
    let host_addr = if crate::ec2::is_ec2_handle(&vm.handle) {
        match crate::config::declared("SPIRA_ROUND_VM_EC2_HOST_ADDR") {
            Some(a) => a,
            None => {
                eprintln!("round-vm run: SPIRA_ROUND_VM_EC2_HOST_ADDR is not set; a spilled VM cannot reach this host");
                let _ = env.pool.release(&vm.handle, ProcId::current(), env.deps.factory);
                return 2;
            }
        }
    } else {
        host_addr
    };
    let keep = AtomicBool::new(false);
    let code = on_vm(env, args, &vm, mode, &host_addr, &commit_sha, &tree_sha, build_image, &round_ref, progress, results_dir, &keep);
    if keep.load(Ordering::SeqCst) {
        let pulled = fs::read_dir(args.results_dir.clone().unwrap_or_else(|| cfg.run_dir.join("batch-results"))).map(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "result")).count()).unwrap_or(0);
        eprintln!("round-vm run: INCOMPLETE: {pulled} results pulled; results not fully pulled; keeping VM {} at {} for a re-pull (release it with `round-vm release {}`)", vm.handle, vm.addr, vm.handle);
        if let Err(e) = env.pool.hold(&vm.handle) {
            eprintln!("round-vm run: warning: could not hold {}: {e}", vm.handle);
        }
        return code;
    }
    if let Err(e) = env.pool.release(&vm.handle, ProcId::current(), env.deps.factory) {
        eprintln!("round-vm run: warning: release of {} failed: {e}", vm.handle);
    }
    code
}

/// The suite slots for a spilled VM: one per vCPU of its type, unless the operator or the caller
/// named a number.
fn spilled_maxpar(requested: Option<u32>, instance_type: Option<&str>, configured: bool) -> Option<u32> {
    if requested.is_some() || configured {
        return None;
    }
    instance_type.and_then(crate::ec2::vcpus_of)
}

/// True when the head's image is one the template does not hold, so the VM must build it:
/// the recorded template names another tag, or (no usable record) the `--base` ref's tag
/// differs from the head's. Only this declaration lets the VM build an image; any other
/// absent image is still refused there.
fn image_build_declared(env: &RunEnv, args: &RunArgs, commit: &str) -> bool {
    let tree = &args.tree_dir;
    let tag = match env.host.image_tag(tree, commit) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("round-vm run: cannot compute the tree's image tag ({e}) — the VM will check it");
            return false;
        }
    };
    if let Some(rec) = Record::read(&env.cfg.state_dir) {
        let names_it = PveEnv::load(&env.cfg.pve_env_path, &|k| std::env::var(k).ok()).map(|pe| pe.template_vmid == rec.vmid).unwrap_or(true);
        if names_it {
            if let Ok(want) = crate::recipe::sha_of_tree(tree) {
                if !rec.built_from(&want) {
                    eprintln!("round-vm run: template {} was not built from this tree's recipe ({want}) — stale; run `round-vm refresh`", rec.vmid);
                }
            }
            if rec.holds(&tag) {
                return false;
            }
            eprintln!(
                "round-vm run: IMAGE-BUILD declared: template {} holds {}, this tree needs localhost/spira-testenv:{tag} — the VM builds it in setup",
                rec.vmid, rec.image
            );
            return true;
        }
        eprintln!(
            "round-vm run: STALE TEMPLATE RECORD: template.json describes template {} but pve.env names another, so what the template holds is unknown — placing the pass as image-missing; fix: `round-vm refresh <tree-dir>` (never hand-edit PVE_TEMPLATE_VMID)",
            rec.vmid
        );
        return true;
    }
    let Some(base) = &args.base else { return false };
    match env.host.image_tag(tree, base) {
        Ok(b) if b != tag => {
            eprintln!("round-vm run: IMAGE-BUILD declared: the head's image tag {tag} differs from {base}'s {b} — the VM builds it in setup");
            true
        }
        _ => false,
    }
}

/// What the pool is doing for the lease, from its own state: the VM being provisioned, else the
/// latest transition it recorded. `None` while the state file says nothing.
fn pool_detail(state_file: &Path) -> Option<String> {
    let s = crate::pool::read_state(state_file).ok()?;
    if let Some(pr) = &s.provisioning {
        return Some(match &pr.vmid {
            Some(id) => format!("provisioning VM {id}"),
            None => "provisioning a VM".to_string(),
        });
    }
    let e = s.events.last()?;
    Some(format!("VM {} {:?}: {}", e.vm, e.to, e.reason).to_lowercase())
}

#[allow(clippy::too_many_arguments)]
fn on_vm(env: &RunEnv, args: &RunArgs, vm: &Vm, mode: AcquireMode, host_addr: &str, commit_sha: &str, tree_sha: &str, build_image: bool, round_ref: &str, progress: crate::progress::Progress, results_dir: PathBuf, keep: &AtomicBool) -> i32 {
    let watch = crate::load::Watch::start(&vm.handle);
    let rc = on_vm_run(env, args, vm, mode, host_addr, commit_sha, tree_sha, build_image, round_ref, progress, results_dir, keep);
    watch.finish();
    rc
}

#[allow(clippy::too_many_arguments)]
fn on_vm_run(
    env: &RunEnv,
    args: &RunArgs,
    vm: &Vm,
    mode: AcquireMode,
    host_addr: &str,
    commit_sha: &str,
    tree_sha: &str,
    build_image: bool,
    round_ref: &str,
    mut progress: crate::progress::Progress,
    results_dir: PathBuf,
    keep: &AtomicBool,
) -> i32 {
    let cfg = env.cfg;
    let maxpar = args.maxpar.unwrap_or(cfg.maxpar);
    progress.detail(&results_dir, format!("adopting VM {} at {}", vm.handle, vm.addr));
    let reachable = (0..cfg.ssh_tries.max(1)).any(|i| {
        if i > 0 {
            std::thread::sleep(cfg.boot_poll);
            progress.tick(&results_dir);
        }
        env.remote.reachable(&vm.addr)
    });
    if !reachable {
        eprintln!("round-vm run: VM {} at {} never became reachable over ssh", vm.handle, vm.addr);
        progress.marks().lose("never reachable over ssh");
        progress.finish(&results_dir, 2);
        return 2;
    }
    progress.enter(&results_dir, "build", Some(format!("VM {}", vm.handle)));
    let job = BatchJob {
        host_addr: host_addr.to_string(),
        mirror_port: cfg.mirror_port,
        suites: args.suites.clone(),
        maxpar,
        toolchain: args.toolchain.clone().or_else(|| crate::recipe::tree_channel(&args.tree_dir)),
        cache_home: job_cache_home(crate::ec2::is_ec2_handle(&vm.handle), cfg.cache_home.as_deref()),
        testenv_registry: cfg.testenv_registry.clone(),
        setup_alarm_secs: cfg.setup_alarm_secs,
        cap_secs: cfg.cap_secs,
        testenv_limits: cfg.testenv_limits.clone(),
        post_guard_secs: cfg.post_guard_secs,
        lint: args.base.is_some() && args.ci_entry.is_none(),
        build_image,
        round_ref: round_ref.to_string(),
        round_sha: commit_sha.to_string(),
        ci_entry: args.ci_entry.clone(),
    };
    let resume = args.ci_entry.is_none().then(|| resume_dir(cfg, tree_sha));
    if let Some(dir) = &resume {
        push_resume(env.remote, &vm.addr, dir);
    }
    let t0 = Instant::now();
    let stream_scratch = cfg.state_dir.join(format!(".pulled-stream.{}", std::process::id()));
    let mut refused: Option<String> = None;
    let Some(spool_dir) = args.attr_spool.clone() else {
        let streamed = std::thread::scope(|sc| {
            let batch = sc.spawn(|| env.remote.run_batch(&vm.addr, &job));
            let mut last: Option<Instant> = None;
            while !batch.is_finished() {
                if last.is_none_or(|l| l.elapsed() >= cfg.stream_every) {
                    stream_pull(env.remote, &vm.addr, &stream_scratch, &results_dir, resume.as_deref());
                    progress.update(&results_dir);
                    record_boundary(env, args, &mut progress, &mut refused);
                    last = Some(Instant::now());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = fs::remove_dir_all(&stream_scratch);
            batch.join().unwrap_or_else(|_| Err("the batch thread panicked".into()))
        });
        let remote_rc = match streamed {
            Ok(255) => {
                eprintln!("round-vm run: ssh to {} failed (exit 255)", vm.addr);
                progress.marks().lose("ssh dropped");
                keep_after_lost_ssh(env, vm, &results_dir, resume.as_deref(), keep);
                record_incomplete(env, args, "the batch's ssh dropped");
                progress.finish(&results_dir, 2);
                return 2;
            }
            Ok(rc) => rc,
            Err(e) => {
                eprintln!("round-vm run: {e}");
                progress.finish(&results_dir, 2);
                return 2;
            }
        };
        progress.update(&results_dir);
        record_boundary(env, args, &mut progress, &mut refused);
        progress.enter(&results_dir, "salvage", None);
        let code = after_batch(env, args, vm, mode, commit_sha, tree_sha, maxpar, remote_rc, t0.elapsed().as_secs(), keep);
        let code = settle_boundary(env, args, &mut progress, &results_dir, &mut refused, code);
        progress.finish(&results_dir, code);
        publish_progress(env, args, &mut progress);
        return code;
    };

    // DESIGN.md §2.2a: stream results while the corpus runs; serve attribution reruns on this
    // VM until the batcher closes the spool.
    let pid = std::process::id();
    let in_flight = AtomicUsize::new(0);
    let mirror_lock = Mutex::new(());
    std::thread::scope(|sc| {
        let mut server = Server {
            spool: Spool { dir: spool_dir },
            host: env.host,
            remote: env.remote,
            tree_dir: &args.tree_dir,
            addr: &vm.addr,
            host_addr,
            mirror_port: cfg.mirror_port,
            toolchain: args.toolchain.clone().or_else(|| crate::recipe::tree_channel(&args.tree_dir)),
            scratch: cfg.state_dir.join(format!(".spool.{pid}")),
            taken: BTreeSet::new(),
            in_flight: &in_flight,
            mirror_lock: &mirror_lock,
        };
        let batch = sc.spawn(|| env.remote.run_batch(&vm.addr, &job));
        let mut last: Option<Instant> = None;
        while !batch.is_finished() {
            if last.is_none_or(|l| l.elapsed() >= cfg.stream_every) {
                stream_pull(env.remote, &vm.addr, &stream_scratch, &results_dir, resume.as_deref());
                progress.update(&results_dir);
                record_boundary(env, args, &mut progress, &mut refused);
                last = Some(Instant::now());
            }
            server.serve(sc, false);
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = fs::remove_dir_all(&stream_scratch);
        let (code, usable) = match batch.join() {
            Ok(Ok(255)) => {
                eprintln!("round-vm run: ssh to {} failed (exit 255)", vm.addr);
                progress.marks().lose("ssh dropped");
                keep_after_lost_ssh(env, vm, &results_dir, resume.as_deref(), keep);
                record_incomplete(env, args, "the batch's ssh dropped");
                (2, false)
            }
            Ok(Ok(rc)) => {
                progress.update(&results_dir);
                record_boundary(env, args, &mut progress, &mut refused);
                progress.enter(&results_dir, "salvage", None);
                let code = after_batch(env, args, vm, mode, commit_sha, tree_sha, maxpar, rc, t0.elapsed().as_secs(), keep);
                (settle_boundary(env, args, &mut progress, &results_dir, &mut refused, code), true)
            }
            Ok(Err(e)) => {
                eprintln!("round-vm run: {e}");
                (2, false)
            }
            Err(_) => {
                eprintln!("round-vm run: the batch thread panicked");
                (2, false)
            }
        };
        progress.finish(&results_dir, code);
        publish_progress(env, args, &mut progress);
        if let Err(e) = server.spool.corpus_done(code) {
            eprintln!("round-vm run: {e}");
        }
        if usable {
            linger(&mut server, sc, cfg.attr_linger, Duration::from_millis(200));
        }
        code
    })
}

/// One streaming pass: every verdict the VM has so far lands in `results_dir`, so a run cut
/// before its final pull keeps them.
fn stream_pull(remote: &dyn Remote, addr: &str, scratch: &Path, results_dir: &Path, resume: Option<&Path>) {
    if let Some(dir) = resume {
        pull_resume(remote, addr, dir);
    }
    if remote.pull(addr, REMOTE_RESULTS, scratch).is_ok() {
        if let Err(e) = stream_into(scratch, results_dir) {
            eprintln!("round-vm run: streaming results: {e}");
        }
    }
}

/// `run`'s exit code for a non-zero remote exit (DESIGN.md §2.2): testenv's own contract
/// (1 red, 2/3 its harness faults, 4 workspace build) passes through; anything else — cargo's
/// 101, git's 128, a signal — never reached a testenv verdict and is round-vm's own fault, 2,
/// named on stderr (sp-dp872).
pub fn exit_for_remote(rc: i32) -> i32 {
    match rc {
        1..=4 | 124 => rc,
        _ => {
            eprintln!("round-vm run: the remote runner exited {rc}, outside testenv's contract (0-4) — no verdict; see its output above");
            2
        }
    }
}

/// A failed workspace build (testenv's code 4) ran no suite, so missing results are the verdict's
/// whole story, not a pull fault: the VM has nothing to re-pull.
fn build_failed_without_results(remote_rc: i32, has_results: bool) -> bool {
    remote_rc == 4 && !has_results
}

/// One pull, retried: a dropped ssh must not cost a finished corpus its results.
fn pull_retry(remote: &dyn Remote, cfg: &Config, addr: &str, remote_path: &str, local: &Path) -> bool {
    let tries = cfg.ssh_tries.max(3);
    for i in 0..tries {
        if i > 0 {
            std::thread::sleep(cfg.boot_poll);
            let _ = fs::remove_dir_all(local);
        }
        match remote.pull(addr, remote_path, local) {
            Ok(()) => return true,
            Err(e) => eprintln!("round-vm run: pull {remote_path} try {} of {tries}: {e}", i + 1),
        }
    }
    false
}

/// An incomplete pass leaves the batch's CI_RUNNING so the next pass can start; a refusal is said aloud.
fn record_incomplete(env: &RunEnv, args: &RunArgs, why: &str) {
    let Some(batch) = args.round_batch.as_deref() else { return };
    if let Err(e) = env.record.pass_incomplete(batch, args.round_repo.as_deref(), why) {
        eprintln!("round-vm run: {e}");
    }
}

/// What a lease serves: the round's batch, else the run's tree directory.
fn run_label(args: &RunArgs) -> String {
    args.round_batch.clone().unwrap_or_else(|| args.tree_dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default())
}

/// Marks the VM kept and takes it off this process's lease at once, so a signal arriving
/// before `run` returns cannot release a VM that `run` has said it is keeping.
fn keep_vm(env: &RunEnv, vm: &Vm, keep: &AtomicBool) {
    keep.store(true, Ordering::SeqCst);
    if let Err(e) = env.pool.hold(&vm.handle) {
        eprintln!("round-vm run: warning: could not hold {}: {e}", vm.handle);
    }
}

/// The batch's ssh dropped, so what the VM holds is unknown: take what it has and keep the VM.
fn keep_after_lost_ssh(env: &RunEnv, vm: &Vm, results_dir: &Path, resume: Option<&Path>, keep: &AtomicBool) {
    keep_vm(env, vm, keep);
    let _pin = env.pool.pin_pull(&vm.handle);
    if let Some(dir) = resume {
        pull_resume(env.remote, &vm.addr, dir);
    }
    let scratch = env.cfg.state_dir.join(format!(".pulled-lost.{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    if pull_retry(env.remote, env.cfg, &vm.addr, REMOTE_RESULTS, &scratch) {
        let n = stream_into(&scratch, results_dir).unwrap_or(0);
        eprintln!("round-vm run: INCOMPLETE: the batch's ssh dropped; salvaged {n} results");
        env.pool.mark_pulled(&vm.handle);
    } else {
        eprintln!("round-vm run: INCOMPLETE: the batch's ssh dropped and the results pull failed");
    }
    let _ = fs::remove_dir_all(&scratch);
}

/// A run cut by a signal still holds the verdicts its VM produced: pulls them into
/// `results_dir` (results already there are kept) before the VM is released. Bounded by
/// `within`, because the caller's kill-after is seconds away. Returns how many were new, or `None` when the pull itself failed or timed out.
pub fn salvage_results(remote: &dyn Remote, addr: &str, results_dir: &Path, scratch: &Path, within: Duration) -> Option<usize> {
    let pulled = std::thread::scope(|sc| {
        let (tx, rx) = std::sync::mpsc::channel();
        sc.spawn(move || {
            let _ = fs::remove_dir_all(scratch);
            let ok = remote.pull(addr, REMOTE_RESULTS, scratch).is_ok();
            let _ = tx.send(ok);
        });
        rx.recv_timeout(within).unwrap_or(false)
    });
    let n = pulled.then(|| stream_into(scratch, results_dir).unwrap_or(0));
    let _ = fs::remove_dir_all(scratch);
    n
}

/// Where the VM's runner writes its time-series rows.
const TSD_DIR: &str = "round-work/.runtime/spira/tsd/";

/// The VM's testenv resume store (`SPIRA_TESTENV_RESUME=dir:$HOME/round-resume`), carried to a
/// re-acquired VM so a round whose spot VM was reclaimed skips the suites the first one passed.
pub const REMOTE_RESUME: &str = "round-resume/";

/// The host's copy of a tree's resume store.
fn resume_dir(cfg: &Config, tree_sha: &str) -> PathBuf {
    cfg.state_dir.join("resume").join(tree_sha)
}

fn push_resume(remote: &dyn Remote, addr: &str, dir: &Path) {
    let held = fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some());
    if !held {
        return;
    }
    match remote.push(addr, dir, REMOTE_RESUME) {
        Ok(()) => eprintln!("round-vm run: RESUME: pushed {} to {addr}", dir.display()),
        Err(e) => eprintln!("round-vm run: RESUME: push to {addr} failed, the VM runs everything: {e}"),
    }
}

/// A failed pull costs only the skips; the round's verdict never depends on it.
fn pull_resume(remote: &dyn Remote, addr: &str, dir: &Path) {
    if remote.pull(addr, REMOTE_RESUME, dir).is_err() {
        let _ = fs::remove_dir(dir);
    }
}

/// Where the VM's testenv writes the corpus's results.
pub const REMOTE_RESULTS: &str = "round-work/.runtime/spira/batch-results/";

/// Everything after the remote corpus returned `remote_rc`: pull results, tsd and binaries,
/// write the manifest, install by tree sha. Returns `run`'s exit code.
#[allow(clippy::too_many_arguments)]
/// The round's coverage artifacts and its delta against the last green round; a fault here is
/// printed and never changes the round's verdict.
fn record_coverage(args: &RunArgs, state_dir: &Path, results_dir: &Path, round: &str, green: bool) {
    let script = args.tree_dir.join("spira").join("coverage-round.sh");
    let out = command("bash")
        .arg(&script)
        .arg(&args.tree_dir)
        .arg(results_dir)
        .arg(state_dir)
        .arg(round)
        .arg(if green { "1" } else { "0" })
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) => eprint!("{}", String::from_utf8_lossy(&o.stderr)),
        Err(e) => eprintln!("round-vm run: coverage: {e}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn after_batch(
    env: &RunEnv,
    args: &RunArgs,
    vm: &Vm,
    mode: AcquireMode,
    commit_sha: &str,
    tree_sha: &str,
    maxpar: u32,
    remote_rc: i32,
    wall: u64,
    keep: &AtomicBool,
) -> i32 {
    let cfg = env.cfg;
    let pid = std::process::id();
    let scratch = |name: &str| cfg.state_dir.join(format!(".pulled-{name}.{pid}"));
    let results_dir = args.results_dir.clone().unwrap_or_else(|| cfg.run_dir.join("batch-results"));
    let mut own_fault = false;

    let pulled = scratch("results");
    let tsd = scratch("tsd");
    let bins = scratch("bins");
    for d in [&pulled, &tsd, &bins] {
        let _ = fs::remove_dir_all(d);
    }
    let _pin = env.pool.pin_pull(&vm.handle);
    let remote = env.remote;
    if args.ci_entry.is_none() {
        let dir = resume_dir(cfg, tree_sha);
        if remote_rc == 0 {
            let _ = fs::remove_dir_all(&dir);
        } else {
            pull_resume(remote, &vm.addr, &dir);
        }
    }
    let timed = |what: &str, remote_path: &str, local: &Path| {
        let t0 = std::time::Instant::now();
        let ok = pull_retry(remote, cfg, &vm.addr, remote_path, local);
        eprintln!("round-vm run: PULL {what}: {}s ({})", t0.elapsed().as_secs(), if ok { "ok" } else { "failed" });
        ok
    };
    let (pulled_ok, tsd_ok) = std::thread::scope(|sc| {
        let tsd_pull = sc.spawn(|| timed("tsd", TSD_DIR, &tsd));
        let bins_pull = sc.spawn(|| timed("bins", REMOTE_BINS, &bins));
        let results_ok = timed("results", REMOTE_RESULTS, &pulled);
        let _ = bins_pull.join();
        (results_ok, tsd_pull.join().unwrap_or(false))
    });
    let leaf = results_leaf(&pulled);
    if pulled.join(FIXTURE_FAULT_FILE).is_file() {
        let _ = fs::create_dir_all(&results_dir);
        let _ = fs::copy(pulled.join(FIXTURE_FAULT_FILE), results_dir.join(FIXTURE_FAULT_FILE));
    }
    if executables(&bins).is_empty() {
        show_setup_log(remote, cfg, &vm.addr, &scratch("setup-log"));
    }
    if build_failed_without_results(remote_rc, leaf.is_some()) {
        eprintln!("round-vm run: the workspace build failed on {}: no suite ran, so there is nothing to pull; releasing it", vm.handle);
        for d in [&pulled, &tsd, &bins] {
            let _ = fs::remove_dir_all(d);
        }
        return 4;
    }
    if let Some(leaf) = &leaf {
        if let Err(e) = copy_tree(leaf, &results_dir) {
            eprintln!("round-vm run: {e}");
            own_fault = true;
            keep_vm(env, vm, keep);
        }
    } else if !pulled_ok || remote_rc == 0 {
        eprintln!("round-vm run: no suite results came back from {}", vm.handle);
        own_fault = true;
        keep_vm(env, vm, keep);
    }
    if !pulled_ok {
        if let Ok(a) = (env.deps.factory)() {
            eprintln!("round-vm run: {}", a.provider.diagnose(&vm.handle));
        }
        eprintln!("round-vm run: INCOMPLETE: the results pull from {} failed after retries; this is not a verdict", vm.handle);
        keep_vm(env, vm, keep);
        record_incomplete(env, args, "the results pull failed after retries");
        return 2;
    }
    let _ = fs::remove_dir_all(&pulled);
    let suite_sum = suite_wall_sum(&results_dir);
    let build = build_wall(&results_dir);

    if tsd_ok {
        if let Err(e) = merge_tsd(&tsd, &cfg.run_dir, &vm.handle, cfg.vcpus, maxpar, crate::telemetry::endpoint().as_deref()) {
            eprintln!("round-vm run: tsd merge: {e}");
        }
    }
    let _ = fs::remove_dir_all(&tsd);

    let reported = reported_tree(&results_dir);
    let manifest = Manifest {
        tree_sha: tree_sha.to_string(),
        tree_sha_found: reported.clone(),
        commit_sha: commit_sha.to_string(),
        vm: vm.handle.clone(),
        acquire: mode,
        vcpus: cfg.vcpus,
        maxpar,
        batch_wall_secs: wall,
        build_wall_secs: build,
        suite_wall_secs_sum: suite_sum,
        provider: if crate::ec2::is_ec2_handle(&vm.handle) { "ec2" } else { "proxmox" }.to_string(),
    };
    let mdir = cfg.state_dir.join("manifests");
    let written = fs::create_dir_all(&mdir)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::to_string(&manifest).map_err(|e| e.to_string()))
        .and_then(|j| fs::write(mdir.join(format!("{tree_sha}.json")), j).map_err(|e| e.to_string()));
    if let Err(e) = written {
        eprintln!("round-vm run: manifest: {e}");
        own_fault = true;
    }
    record_coverage(args, &cfg.state_dir, &results_dir, tree_sha, remote_rc == 0);
    let target = args.tree_dir.join("target").join("release");
    if let Err(e) = install_bins(&bins, tree_sha, reported.as_deref(), &target) {
        eprintln!("{e}");
        own_fault = true;
    }
    let _ = fs::remove_dir_all(&bins);

    if remote_rc != 0 {
        return exit_for_remote(remote_rc);
    }
    if own_fault {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_progress_refusal_carries_the_reason_spira_lc_wrote_to_stdout() {
        let m = progress_refusal("b1", Some(2), b"cannot tell: no such column\n", b"");
        assert!(m.contains("rc=2") && m.contains("no such column"), "{m}");
        assert!(progress_refusal("b1", Some(2), b"", b"").contains("said nothing"));
    }
    use super::*;

    #[test]
    fn the_image_tag_is_computed_from_a_tree_by_a_process_standing_elsewhere() {
        if std::process::Command::new("testenv").arg("--version").output().is_err() {
            eprintln!("skipped: no testenv on PATH");
            return;
        }
        let d = TempDir::new();
        let tree = d.path().join("tree");
        for (f, body) in [
            ("spira/testenv/Containerfile", "FROM scratch\n"),
            ("spira/deps.toml", "[meta]\n"),
            ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.90.0\"\n"),
        ] {
            let p = tree.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        let g = |a: &[&str]| {
            let st = std::process::Command::new("git").arg("-C").arg(&tree).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(a).status().unwrap();
            assert!(st.success());
        };
        g(&["init", "-q"]);
        g(&["add", "-A"]);
        g(&["commit", "-qm", "x"]);
        let host = GitHost { state_dir: d.path().to_path_buf(), mirror_port: 1, listen: String::new(), also_listen: vec![] };
        let (commit, _) = host.head(&tree).unwrap();
        let tag = host.image_tag(&tree, &commit);
        assert!(tag.as_ref().is_ok_and(|t| t.len() == 12), "{tag:?}");
    }

    use crate::config::Fields;
    use crate::pool::{Attempt, Spawner};
    use crate::provider::Timing;
    use crate::testutil::{FakeAlarm, FakeProvider, TempDir};
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_spilled_vm_runs_one_slot_per_vcpu_unless_a_number_was_named() {
        assert_eq!(spilled_maxpar(None, Some("c7i.4xlarge"), false), Some(16));
        assert_eq!(spilled_maxpar(None, Some("c7i.8xlarge"), false), Some(32));
        assert_eq!(spilled_maxpar(Some(8), Some("c7i.4xlarge"), false), None);
        assert_eq!(spilled_maxpar(None, Some("c7i.4xlarge"), true), None);
        assert_eq!(spilled_maxpar(None, None, false), None);
    }

    #[test]
    fn log_tail_keeps_the_last_lines_and_none_when_empty() {
        assert_eq!(log_tail("a\nb\nc\n", 2).as_deref(), Some("b\nc"));
        assert_eq!(log_tail("a\n", 60).as_deref(), Some("a"));
        assert_eq!(log_tail("", 60), None);
    }

    #[test]
    fn a_failed_build_with_no_results_is_a_verdict_not_a_pull_fault() {
        assert!(build_failed_without_results(4, false));
        assert!(!build_failed_without_results(4, true));
        assert!(!build_failed_without_results(1, false));
        assert!(!build_failed_without_results(0, false));
    }

    #[test]
    fn run_args_accept_both_spellings() {
        let r = parse_run_args(&args(&["/t", "--suites", "a,b", "--maxpar=24", "--toolchain", "1.82.0", "--results-dir=/r"])).unwrap();
        assert_eq!(r.tree_dir, PathBuf::from("/t"));
        assert_eq!(r.suites.as_deref(), Some("a,b"));
        assert_eq!(r.maxpar, Some(24));
        assert_eq!(r.toolchain.as_deref(), Some("1.82.0"));
        assert_eq!(r.results_dir, Some(PathBuf::from("/r")));
    }

    #[test]
    fn run_args_errors_name_the_problem() {
        assert_eq!(parse_run_args(&[]).unwrap_err(), RUN_USAGE);
        assert!(parse_run_args(&args(&["/t", "--bogus"])).unwrap_err().contains("unknown option: --bogus"));
        assert!(parse_run_args(&args(&["/t", "--maxpar", "x"])).unwrap_err().contains("--maxpar"));
        assert!(parse_run_args(&args(&["/t", "--suites"])).unwrap_err().contains("needs a value"));
    }

    #[test]
    fn the_round_runs_the_bare_host_suites_on_the_vm_after_the_corpus() {
        let corpus = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        let bare = REMOTE_SCRIPT.find("spira/bare-suites.sh").unwrap();
        let unit_wait = REMOTE_SCRIPT.find("wait \"$unit_pid\"").unwrap();
        assert!(bare < corpus && corpus < unit_wait, "the bare lane starts before the corpus and is waited on after it");
        let lane = &REMOTE_SCRIPT[bare..unit_wait];
        assert!(REMOTE_SCRIPT[bare..corpus].contains("&\nbare_pid=$!"), "the bare lane runs in the background beside the corpus");
        let gate = REMOTE_SCRIPT.find("bare_after_setup()").unwrap();
        let call = REMOTE_SCRIPT.find("bare_after_setup \"${bare_sel[@]}\"").unwrap();
        assert!(gate < call && REMOTE_SCRIPT[gate..call].contains("until [ -e \"$suites_mark\" ]"), "the bare lane's container boots wait for the batch's setup to finish");
        assert!(lane.contains("wait \"$bare_pid\""));
        assert!(lane.contains("--results \"$bare_out\"") || REMOTE_SCRIPT[bare..corpus].contains("--results \"$bare_out\""), "its results are copied beside the corpus's once the corpus has written its own");
        assert!(lane.contains("[ \"$rc\" -eq 0 ] && rc=1"), "a red bare suite makes the round red");
        assert!(REMOTE_SCRIPT.contains("--selected \"$suites\"") && REMOTE_SCRIPT.contains("--suites all"));
    }

    #[test]
    fn the_remote_script_says_whether_the_template_holds_the_rounds_image() {
        let clone = REMOTE_SCRIPT.find("git clone").unwrap();
        let check = REMOTE_SCRIPT.find("podman image exists").unwrap();
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(clone < check && check < batch, "checked after the clone, before testenv starts");
        assert!(REMOTE_SCRIPT.contains("template image: localhost/spira-testenv:$tag present"));
        assert!(!REMOTE_SCRIPT.contains("this round builds it"), "a round never cold-builds its image");
        assert!(REMOTE_SCRIPT.contains("IMAGE-ABSENT: localhost/spira-testenv:$tag is not on this VM — refusing to cold-build it"));
        let absent = REMOTE_SCRIPT.find("refusing to cold-build it").unwrap();
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(absent < batch, "the refusal precedes the batch");
        assert!(REMOTE_SCRIPT[absent..batch].contains("exit 3"), "an absent image exits 3 before any batch runs");
    }

    #[test]
    fn the_round_builds_once_in_a_clean_tree_with_testenvs_own_flags() {
        assert!(!REMOTE_SCRIPT.contains("ln -sfn"), "a target symlink dirties the tree and dangles in the container");
        assert!(REMOTE_SCRIPT.contains(&format!("cargo build -q --profile release --workspace {}", spira_config::build::one_shot_words("release"))));
    }

    fn env_build_step() -> String {
        let a = REMOTE_SCRIPT.find("nix_bin=").unwrap();
        let b = REMOTE_SCRIPT.find("env_path=").unwrap();
        format!("set -uo pipefail\n{}", &REMOTE_SCRIPT[a..b])
    }

    fn run_env_build(fail_first: usize, always_fail: bool) -> (i32, String) {
        let tmp = testkit::TempDir::new("rvm-envbuild");
        let d = tmp.path().to_path_buf();
        let nixdir = d.join("nixbin");
        std::fs::create_dir_all(&nixdir).unwrap();
        std::fs::create_dir_all(d.join("round-work")).unwrap();
        std::fs::create_dir_all(d.join(".cache/nix")).unwrap();
        std::fs::write(d.join(".cache/nix/fetcher-cache-v4.sqlite"), "x").unwrap();
        std::fs::write(d.join("round-work/flake.nix"), "{}").unwrap();
        testkit::write_exe(
            nixdir.join("nix"),
            "#!/bin/sh\nif [ -e \"$HOME/.cache/nix/fetcher-cache-v4.sqlite\" ] && [ \"$FAIL\" != 0 ]; then echo \"error: path '/nix/store/x-source/flake.nix' does not exist\" >&2; exit 1; fi\nif [ \"$ALWAYS\" = 1 ]; then exit 1; fi\nexit 0\n",
        );
        let script = env_build_step().replace("nix_bin=/nix/var/nix/profiles/default/bin", &format!("nix_bin={}", nixdir.display()));
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .env_clear()
            .env("HOME", &d)
            .env("PATH", "/usr/bin:/bin")
            .env("FAIL", fail_first.to_string())
            .env("ALWAYS", if always_fail { "1" } else { "0" })
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    #[test]
    fn a_stale_fetcher_cache_is_dropped_and_the_env_build_retried_naming_its_flake_ref() {
        let (code, err) = run_env_build(1, false);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("flake ref path:") && err.contains("round-work#spira-env"), "{err}");
        assert!(err.contains("flake.nix on disk: present"), "{err}");
    }

    #[test]
    fn a_clean_env_build_does_not_retry_and_a_persistent_failure_names_the_flake_ref() {
        let (code, err) = run_env_build(0, false);
        assert_eq!(code, 0);
        assert!(!err.contains("retrying"), "{err}");
        let (code, err) = run_env_build(0, true);
        assert_eq!(code, 3, "{err}");
        assert!(err.contains("cannot build spira-env from the round's flake.lock (flake ref path:"), "{err}");
    }

    #[test]
    fn conf_gen_is_warmed_once_before_the_workspace_build_and_the_suite_batch() {
        // sp-xjnzl-2: a fresh clone never carries conf.sh's gitignored generated fragments,
        // so every suite's own conf.sh sourcing would otherwise regenerate them independently
        // the moment the batch goes parallel — warming once, here, means none of them do.
        let clone = REMOTE_SCRIPT.find("git clone").unwrap();
        let warm = REMOTE_SCRIPT.find("bash spira/conf-gen.sh").unwrap();
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(clone < warm && warm < build && build < batch, "conf-gen.sh must be warmed after the clone but before either the workspace build or the suite batch");
        assert!(REMOTE_SCRIPT.contains("bash spira/conf-gen.sh >&2 || true"), "non-fatal: conf.sh's own per-suite self-heal is still the fallback if this one warm attempt fails");
    }

    #[test]
    fn the_heads_config_delta_is_staged_before_the_release_build_reads_config() {
        let export = REMOTE_SCRIPT.find("export SPIRA_TOML=\"$HOME/round-work/spira-config/tests/fixtures/complete.toml:").unwrap();
        let stage = REMOTE_SCRIPT.find("release config-stage \"$HOME/round-work\"").expect("the delta must be applied");
        let build = REMOTE_SCRIPT.find("release build").unwrap();
        assert!(export < stage && stage < build);
    }

    #[test]
    fn the_round_validates_the_staged_config_then_builds_and_verifies_the_release_before_any_suite() {
        let stage = REMOTE_SCRIPT.find("release config-stage \"$HOME/round-work\"").unwrap();
        let validate = REMOTE_SCRIPT.find("target/release/spira-config validate").expect("the staged layers are validated by the head's own schema");
        let build = REMOTE_SCRIPT.find("target/release/release build").unwrap();
        let verify = REMOTE_SCRIPT.find("target/release/release verify \"$rel_sha\" --no-pre-activate").expect("the built release is verified");
        let tenv = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(stage < validate && validate < build && build < verify && verify < tenv);
        for step in ["config", "build", "verify"] {
            assert!(REMOTE_SCRIPT.contains(&format!("release_red \"{step}\"")), "{step} failure is a named red");
        }
        assert!(REMOTE_SCRIPT.contains("RELEASE-VERIFY: RED at $1"));
        assert!(REMOTE_SCRIPT.contains("grep -v '^ *$' ~/round-release.err | head -20 >&2\n    exit 4"));
    }

    #[test]
    fn the_vm_is_a_launcher_release_path_set_outright_before_any_harness_script() {
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        let stage = REMOTE_SCRIPT.find("release build").unwrap();
        let path = REMOTE_SCRIPT.find("export PATH=\"$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:").unwrap();
        let first_script = REMOTE_SCRIPT.find("testenv container tag").unwrap();
        let tenv = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(build < stage && stage < path && path < first_script && path < tenv);
        assert!(REMOTE_SCRIPT.contains("export SPIRA_RELEASE=\"$HOME/round-releases/$rel_sha\""));
        // A release is not a checkout: testenv run from it names the repo by SPIRA_REPO, or
        // it cannot resolve the base ref (the first proof round: VERDICT FAULT reason=base-ref).
        assert!(REMOTE_SCRIPT.contains("export SPIRA_REPO=\"$HOME/round-work\""));
        assert!(REMOTE_SCRIPT.contains("export SPIRA_REPO=%q"));
        assert!(REMOTE_SCRIPT.contains("\"$SPIRA_RELEASE/bin/spira-config\""), "positive control on what PATH resolves");
        assert!(!REMOTE_SCRIPT.contains(":$PATH"), "PATH is set outright, never appended to");
        assert!(!REMOTE_SCRIPT.contains("cargo run"), "testenv runs from the staged release by name");
        assert!(REMOTE_SCRIPT.contains(LAUNCHER_ENV));
        assert!(REMOTE_SCRIPT.contains("exit 4"), "a failed workspace build is testenv's build-failure code");
    }

    #[test]
    fn the_round_runs_with_spira_env_from_its_own_tree_first_in_path() {
        let build = REMOTE_SCRIPT.find("flake_ref=\"path:$HOME/round-work#spira-env\"").expect("spira-env is built from the round's own tree");
        let path = REMOTE_SCRIPT.find("export PATH=\"$HOME/spira-env/bin:").expect("spira-env/bin first");
        let first_build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        assert!(build < path && path < first_build);
        assert!(REMOTE_SCRIPT.contains("round-vm: spira-env $env_path"), "the log prints the store path and versions");
        assert!(REMOTE_SCRIPT.contains("PATH=\"$HOME/spira-env/bin:$CARGO_HOME/bin:"), "unit tests see it too");
        assert!(REMOTE_SCRIPT.contains("$SPIRA_RELEASE/spira:$HOME/spira-env/bin:"), "the launcher PATH keeps it");
    }

    #[test]
    fn the_round_s_build_shares_the_hosts_own_cache_not_a_vm_local_one() {
        // sp-xjnzl: CARGO_HOME, RUSTC_WRAPPER and the webdav endpoint must all be set
        // BEFORE the launcher's first `cargo build` — that build, not just testenv's
        // in-place one, is the one the original fault actually hit.
        let cache_home = REMOTE_SCRIPT.find("export CARGO_HOME=\"$cache_home\"").expect("CARGO_HOME comes from the operator's own env (cache_home), never a hardcoded literal — a literal names one operator's box");
        let wrapper = REMOTE_SCRIPT.find("export RUSTC_WRAPPER=\"$CARGO_HOME/bin/sccache\"").expect("RUSTC_WRAPPER set outright, by absolute path");
        let endpoint = REMOTE_SCRIPT.find("export SCCACHE_WEBDAV_ENDPOINT=\"http://${host_addr}:9431\"").expect("the webdav endpoint reuses the VM's own host_addr, the same address the mirror already uses");
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        assert!(cache_home < build && wrapper < build && endpoint < build, "the cache must be wired up before the first build, not just before testenv's");
        // The VM's PATH must still find cargo/rustc (the pre-existing $HOME/.cargo/bin,
        // rustup's own shims) AND the new CARGO_HOME/bin (where sccache now installs) —
        // neither replaces the other.
        assert!(REMOTE_SCRIPT.contains("$CARGO_HOME/bin:$HOME/.cargo/bin:"));
        assert!(!REMOTE_SCRIPT.contains("/home/"), "no literal home directory — cache_home is an operator-supplied argument, not a hardcoded path");
    }

    #[test]
    fn an_ec2_vm_never_gets_the_templates_cache_home() {
        assert_eq!(job_cache_home(true, Some("/tmpl/.cargo")), None);
        assert_eq!(job_cache_home(false, Some("/tmpl/.cargo")), Some("/tmpl/.cargo".to_string()));
        let at = REMOTE_SCRIPT.find("/etc/spira-round-ami.env").expect("the script reads the image's own record");
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        let guard = REMOTE_SCRIPT.find("sccache is missing on this VM").expect("a missing sccache is named before the build");
        assert!(at < guard && guard < build);
    }

    fn sh(home: &Path, script: &str) -> std::process::Output {
        std::process::Command::new("bash").arg("-c").arg(script).env_clear().env("HOME", home).env("PATH", "/usr/bin:/bin")
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .output().unwrap()
    }

    fn fence_post_segment() -> String {
        let c = REMOTE_SCRIPT.find("if [ -n \"$fence_fail\" ]; then\n    rdir=").unwrap();
        let d = c + REMOTE_SCRIPT[c..].find("\nfi\n").unwrap() + 4;
        REMOTE_SCRIPT[c..d].to_string()
    }

    fn start_sccache_fn() -> String {
        let a = REMOTE_SCRIPT.find("start_sccache() {").unwrap();
        let b = a + REMOTE_SCRIPT[a..].find("\n}\n").unwrap() + 3;
        REMOTE_SCRIPT[a..b].to_string()
    }

    fn run_start_sccache(fail_first: u32) -> (bool, String) {
        let tmp = testkit::TempDir::new("sccache-start");
        let dir = tmp.path().to_path_buf();
        let stub = dir.join("sccache");
        testkit::write_exe(&stub, &format!(
            "#!/bin/bash\nif [ \"$1\" = --start-server ]; then n=$(cat {d}/n 2>/dev/null || echo 0); echo $((n+1)) > {d}/n; \
             [ \"$n\" -ge {fail_first} ] || {{ echo 'sccache: error: Timed out waiting for server startup' >&2; exit 2; }}; fi\nexit 0\n",
            d = dir.display()));
        let script = format!("{}\nRUSTC_WRAPPER={}\nif start_sccache; then echo STARTED; else echo FALLBACK; fi\nenv | grep -c ^SCCACHE_STARTUP_TIMEOUT=120$",
            start_sccache_fn(), stub.display());
        let out = sh(&dir, &script);
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (text.contains("STARTED"), text)
    }

    #[test]
    fn a_server_start_that_times_out_once_is_retried_and_the_build_proceeds() {
        let (started, log) = run_start_sccache(1);
        assert!(started, "{log}");
        assert!(log.contains("start failed (attempt 1)") && log.contains("started (attempt 2)"), "{log}");
        assert!(log.lines().any(|l| l == "1"), "start-up deadline raised for the server: {log}");
    }

    #[test]
    fn a_server_that_never_starts_falls_back_to_no_wrapper_and_the_log_says_so() {
        let (started, log) = run_start_sccache(99);
        assert!(!started, "{log}");
        let fb = REMOTE_SCRIPT.find("building WITHOUT sccache").expect("fallback is named in the log");
        let wrapper_off = REMOTE_SCRIPT.find("export RUSTC_WRAPPER=\"\"").expect("the wrapper is dropped");
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        assert!(fb < wrapper_off && wrapper_off < build);
        assert!(REMOTE_SCRIPT.contains("built $sccache_mode"), "the build failure line names which happened");
        assert!(REMOTE_SCRIPT.contains("tail -40 \"$SCCACHE_ERROR_LOG\""), "error log kept on build failure");
    }

    fn unit_stream_fn() -> String {
        let a = REMOTE_SCRIPT.find("unit_stream() {").unwrap();
        let b = a + REMOTE_SCRIPT[a..].find("\n}\n").unwrap() + 3;
        REMOTE_SCRIPT[a..b].to_string()
    }

    #[test]
    fn the_warm_target_is_cleared_when_the_rustc_that_built_it_differs() {
        let install = REMOTE_SCRIPT.find("rustup toolchain install \"$toolchain\"").unwrap();
        let stamp = REMOTE_SCRIPT.find("rustc -vV").expect("identifies the active rustc");
        let clear = REMOTE_SCRIPT.find("warm target was built by another rustc").expect("clears a foreign target");
        let build = REMOTE_SCRIPT.find("cargo build").unwrap_or(usize::MAX);
        assert!(install < stamp && stamp < clear && clear < build);
        assert!(REMOTE_SCRIPT.contains("target/.rustc-id"));
    }

    #[test]
    fn a_round_installs_its_trees_toolchain_when_the_image_differs() {
        let home = REMOTE_SCRIPT.find("export CARGO_HOME=\"$cache_home\"").unwrap();
        let install = REMOTE_SCRIPT.find("rustup toolchain install \"$toolchain\"").expect("installs the round's toolchain");
        let relink = REMOTE_SCRIPT.find("ln -sf \"$b\" \"$CARGO_HOME/bin/${b##*/}\"").expect("relinks cargo/rustc");
        assert!(home < install && install < relink);
        assert!(REMOTE_SCRIPT.contains("installed at round time; the image has"));
    }

    #[test]
    fn the_round_tree_is_fetched_into_a_kept_tree_and_only_cloned_when_that_fails() {
        let fetch = REMOTE_SCRIPT.find("fetch --quiet --prune origin").expect("fetches into the kept tree");
        let reset = REMOTE_SCRIPT.find("&& checkout_round").unwrap();
        assert!(REMOTE_SCRIPT.contains("checkout --quiet --force -B round FETCH_HEAD"), "testenv runs the branch `round`; a detached HEAD leaves it on the image's old commit");
        assert!(!REMOTE_SCRIPT.contains("origin/HEAD"), "the mirror's HEAD is shared and never read");
        let clean = REMOTE_SCRIPT.find("clean --quiet -fdx -e /target").expect("the clean spares the warm target");
        let clone = REMOTE_SCRIPT.find("git clone").unwrap();
        assert!(fetch < reset && reset < clean && clean < clone);
        assert!(!REMOTE_SCRIPT.contains("rm -rf ~/round-work ~/round-bins"), "the tree, and its target, is not deleted up front");
    }

    #[test]
    fn the_unit_test_binaries_compile_in_the_build_phase_and_the_run_is_the_same_command() {
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        let no_run = REMOTE_SCRIPT.find("unit_cargo --no-run").unwrap();
        let suites = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        let run = REMOTE_SCRIPT.find("unit_cargo --no-fail-fast").unwrap();
        assert!(build < no_run && no_run < suites && no_run < run, "the compile precedes the suites and the run");
        let f = REMOTE_SCRIPT.find("unit_cargo() {").unwrap();
        let body = &REMOTE_SCRIPT[f..f + REMOTE_SCRIPT[f..].find("\n}\n").unwrap()];
        assert!(body.contains("exec cargo test \"$@\"' _ --profile unittest --workspace --config profile.unittest.incremental=false"), "{body}");
        assert_eq!(REMOTE_SCRIPT.matches("exec cargo test").count(), 1, "one definition of the unit command, shared by compile and run");
    }

    #[test]
    fn line_coverage_is_the_unit_command_itself_when_the_image_has_llvm_cov_and_never_a_second_run() {
        assert!(REMOTE_SCRIPT.contains("cargo llvm-cov show-env --export-prefix"));
        assert!(!REMOTE_SCRIPT.contains("llvm-cov --no-report"), "no llvm-cov subcommand wraps the cargo test flags");
        assert!(REMOTE_SCRIPT.contains("lines unavailable (the image has no cargo-llvm-cov)"), "a missing tool is reported, not a failed round");
        let report = REMOTE_SCRIPT.find("cargo llvm-cov report --profile unittest --json --summary-only").unwrap();
        let waited = REMOTE_SCRIPT.find("wait \"$unit_pid\"; unit_rc=$?").unwrap();
        assert!(waited < report, "the report is read off the profile data after the unit run");
        assert_eq!(REMOTE_SCRIPT.matches("exec cargo test").count(), 1);
    }

    #[test]
    fn a_killed_unit_run_saves_its_cause_and_its_last_output() {
        let f = REMOTE_SCRIPT.find("unit_cause=\"killed by signal").expect("the cause");
        let tail = &REMOTE_SCRIPT[f..];
        assert!(tail.contains("killed by signal") && tail.contains("${unit_secs}s"), "{tail}");
        assert!(tail.contains("tail -100 ~/round-unit-tests.log"), "the saved output ends with the log's own tail");
        assert!(!REMOTE_SCRIPT.contains("^test result:' ~/round-unit-tests.log | head"), "passing binaries no longer fill the head");
    }

    #[test]
    fn a_cargo_log_streams_one_line_per_binary_in_order_and_a_red_when_it_fails() {
        let d = TempDir::new();
        let log = "   Running unittests src/lib.rs (target/release/deps/alpha-0123abcd)\nrunning 1 test\ntest result: ok. 1 passed; 0 failed\n\
                   \x20    Running tests/it.rs (target/release/deps/it-feedbeef)\ntest result: FAILED. 0 passed; 1 failed\n\
                   \x20  Doc-tests beta\ntest result: ok. 0 passed; 0 failed\n";
        fs::write(d.path().join("cargo.log"), log).unwrap();
        let script = format!("{}\nunit_stream 3 ~/prog < cargo.log", unit_stream_fn());
        let out = std::process::Command::new("bash").arg("-c").arg(&script).current_dir(d.path()).env_clear().env("HOME", d.path()).env("PATH", "/usr/bin:/bin").output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        let lines: Vec<&str> = err.lines().collect();
        assert_eq!(lines, ["round-vm: UNIT-TEST alpha ok (1/3)", "round-vm: UNIT-TEST it FAILED (2/3)", "round-vm: UNIT-TEST doc-tests beta ok (3/3)"], "{err}");
        assert_eq!(fs::read_to_string(d.path().join("prog")).unwrap().trim(), "3 3 it");
    }

    fn unit_fns() -> String {
        let a = REMOTE_SCRIPT.find("unit_hang_scan() {").unwrap();
        let b = REMOTE_SCRIPT.find("# The round's one source of config").unwrap();
        REMOTE_SCRIPT[a..b].to_string()
    }

    #[test]
    fn a_unit_test_libtest_flags_as_slow_is_named_with_its_binary() {
        let d = TempDir::new();
        let log = "   Running unittests src/lib.rs (target/unittest/deps/alpha-0123abcd)\nrunning 2 tests\ntest a::quick ... ok\n\
                   test a::sleeps_forever has been running for over 60 seconds\n";
        let quiet = "   Running unittests src/lib.rs (target/unittest/deps/alpha-0123abcd)\ntest a::quick ... ok\n";
        fs::write(d.path().join("hung.log"), log).unwrap();
        fs::write(d.path().join("quiet.log"), quiet).unwrap();
        let run = |f: &str| {
            let script = format!("{}\nunit_hang_scan < {f}", unit_fns());
            let o = std::process::Command::new("bash").arg("-c").arg(&script).current_dir(d.path()).output().unwrap();
            String::from_utf8_lossy(&o.stdout).to_string()
        };
        assert_eq!(run("hung.log"), "a::sleeps_forever\ttarget/unittest/deps/alpha-0123abcd\n");
        assert_eq!(run("quiet.log"), "", "a log with no slow test names nothing");
    }

    #[test]
    fn the_watcher_kills_a_hung_test_binary_and_reports_it_while_the_job_runs() {
        let d = TempDir::new();
        fs::write(d.path().join("cargo.log"), "   Running unittests src/lib.rs (target/unittest/deps/sleepy-feedbeef)\ntest t::hangs has been running for over 60 seconds\n").unwrap();
        let script = format!(
            "{}\ncp /bin/bash sleepy-feedbeef\n./sleepy-feedbeef -c 'read -t 300 && :' </dev/zero &\njob=$!\nunit_hang_watch cargo.log hung \"$job\" &\nw=$!\nwait \"$job\"; echo job-rc=$?\nkill $w 2>/dev/null; cat hung",
            unit_fns()
        );
        let o = std::process::Command::new("bash").arg("-c").arg(&script).current_dir(d.path()).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(out.contains("job-rc=137"), "{out}{err}");
        assert!(out.contains("t::hangs\ttarget/unittest/deps/sleepy-feedbeef"), "{out}");
        assert!(err.contains("UNIT-TESTS: HUNG test=t::hangs binary=sleepy-feedbeef"), "{err}");
    }

    #[test]
    fn the_unit_run_is_watched_for_hung_tests_and_a_hang_makes_the_round_red() {
        let run = REMOTE_SCRIPT.find("unit_cargo --no-fail-fast").unwrap();
        let watch = REMOTE_SCRIPT.find("unit_hang_watch ~/round-unit-tests.log").unwrap();
        assert!(run < watch);
        assert!(REMOTE_SCRIPT.contains("[ ! -s \"$unit_hung\" ] || [ \"$unit_rc\" -ne 0 ] || unit_rc=1"));
        assert!(REMOTE_SCRIPT.contains("binary=${unit_running:-none-started}; killed"), "the post-guard kill names the binary running");
    }

    fn lint_segments() -> (String, String) {
        let a = REMOTE_SCRIPT.find("lint_rc=0\nlint_secs=0").unwrap();
        let b = REMOTE_SCRIPT.find("# THE WORKSPACE'S OWN UNIT TESTS").unwrap();
        let c = REMOTE_SCRIPT.find("if [ \"$lint_rc\" -ne 0 ]; then\n    rdir=").unwrap();
        let d = c + REMOTE_SCRIPT[c..].find("\nfi\n").unwrap() + 4;
        (REMOTE_SCRIPT[a..b].to_string(), REMOTE_SCRIPT[c..d].to_string())
    }

    /// A round of two members on a base; the stub lint reports `hit` (exit 1) or nothing (exit 0).
    fn lint_fixture(d: &TempDir, hit: Option<&str>) -> std::process::Output {
        fence_fixture(d, hit, 0, 0)
    }

    /// As [`lint_fixture`], with the lifecycle-guard and boundary fences exiting `guard_rc` and `boundary_rc`.
    fn fence_fixture(d: &TempDir, hit: Option<&str>, guard_rc: i32, boundary_rc: i32) -> std::process::Output {
        let home = d.path();
        let w = home.join("round-work");
        fs::create_dir_all(w.join("target/release")).unwrap();
        let stub = match hit {
            Some(h) => format!("#!/bin/sh\necho '{h}'\nexit 1\n"),
            None => "#!/bin/sh\nexit 0\n".to_string(),
        };
        testkit::write_exe(w.join("target/release/spira-lint"), &stub);
        testkit::write_exe(w.join("target/release/lifecycle-guard"), &format!("#!/bin/sh\n[ \"$1\" = --gate ] || exit 9\necho 'guard finding'\nexit {guard_rc}\n"));
        fs::create_dir_all(w.join("spira")).unwrap();
        fs::write(w.join("spira/boundary.sh"), format!("[ \"$1\" = check ] || exit 9\necho 'README is stale'\nexit {boundary_rc}\n")).unwrap();
        let (lint, post) = lint_segments();
        let fences = fence_post_segment();
        let script = format!(
            "set -e; cd ~/round-work; git init -q -b main .; echo x > base.txt; git add base.txt; git commit -qm base; \
             git update-ref refs/remotes/origin/base HEAD; \
             echo 1 > ok.rs; git add ok.rs; git commit -qm 'spira: round r-1: merge sp-clean (aaa)'; \
             echo 2 > bad.rs; git add bad.rs; git commit -qm 'spira: round r-1: merge sp-dirty (bbb)'; \
             set +e; rc=0; lint=1; {lint}\n{post}\n{fences}\necho rc=$rc"
        );
        sh(home, &script)
    }

    #[test]
    fn a_round_whose_member_adds_a_lint_hit_goes_red_naming_that_member() {
        let d = TempDir::new();
        let (lint, _) = lint_segments();
        assert!(lint.contains("--base origin/base"), "the lint is judged against the mirrored landing ref");
        let o = lint_fixture(&d, Some("call-deadline: bad.rs:3: a call without a deadline"));
        let err = String::from_utf8_lossy(&o.stderr);
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(out.contains("rc=1"), "{out}{err}");
        assert!(err.contains("member=sp-dirty call-deadline: bad.rs:3"), "{err}");
        assert!(!err.contains("member=sp-clean"), "{err}");
        let res = fs::read_to_string(d.path().join("round-work/.runtime/spira/batch-results/spira-lint.result")).unwrap();
        assert!(res.starts_with("red "), "{res}");
        assert!(err.contains("of the 900s cap"), "the lint's wall time is reported: {err}");
    }

    #[test]
    fn a_round_that_breaks_only_a_base_fence_goes_red_naming_the_fence() {
        let d = TempDir::new();
        let o = fence_fixture(&d, None, 0, 1);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(String::from_utf8_lossy(&o.stdout).contains("rc=1"), "{err}");
        assert!(err.contains("FENCE boundary: RED"), "{err}");
        assert!(!err.contains("FENCE lifecycle-guard"), "{err}");
        let r = d.path().join("round-work/.runtime/spira/batch-results");
        assert!(fs::read_to_string(r.join("fence-boundary.result")).unwrap().starts_with("red "));
        assert!(fs::read_to_string(r.join("fence-boundary.out")).unwrap().contains("README is stale"));
        assert!(!r.join("spira-lint.result").exists(), "the lint was clean");
    }

    #[test]
    fn a_lifecycle_guard_finding_alone_makes_the_round_red() {
        let d = TempDir::new();
        let o = fence_fixture(&d, None, 1, 0);
        assert!(String::from_utf8_lossy(&o.stderr).contains("FENCE lifecycle-guard: RED"));
        assert!(d.path().join("round-work/.runtime/spira/batch-results/fence-lifecycle-guard.result").exists());
    }

    #[test]
    fn a_hit_in_a_file_no_member_touched_is_the_bases_own() {
        let d = TempDir::new();
        let o = lint_fixture(&d, Some("call-deadline: base.txt:1: old"));
        assert!(String::from_utf8_lossy(&o.stderr).contains("member=the base itself"), "{}", String::from_utf8_lossy(&o.stderr));
    }

    #[test]
    fn a_clean_round_is_unaffected_by_the_lint() {
        let d = TempDir::new();
        let o = lint_fixture(&d, None);
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(out.contains("rc=0"), "{out}{}", String::from_utf8_lossy(&o.stderr));
        assert!(!d.path().join("round-work/.runtime").exists());
        assert!(String::from_utf8_lossy(&o.stderr).contains("of the 900s cap"));
    }

    #[test]
    fn base_is_parsed_and_asks_the_job_to_lint() {
        let a = parse_run_args(&["/t".into(), "--base".into(), "local/main".into()]).unwrap();
        assert_eq!(a.base.as_deref(), Some("local/main"));
        assert!(parse_run_args(&["/t".into()]).unwrap().base.is_none());
    }

    fn git_out(dir: &Path, a: &[&str]) -> String {
        let o = std::process::Command::new("git").arg("-C").arg(dir).args(a).output().unwrap();
        assert!(o.status.success(), "{a:?}");
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn foreign_run(entry: &str) -> (std::process::Output, String) {
        foreign_run_expecting(entry, None)
    }

    fn foreign_run_expecting(entry: &str, expect: Option<&str>) -> (std::process::Output, String) {
        foreign_run_with(entry, expect, "", |_| {}, &[])
    }

    fn foreign_run_dir(entry: &str) -> (std::process::Output, String, String) {
        foreign_run_full(entry, None, "", |_| {}, &[])
    }

    fn foreign_run_with(entry: &str, expect: Option<&str>, toolchain: &str, setup: impl Fn(&Path), env: &[(&str, &str)]) -> (std::process::Output, String) {
        let (out, verdict, _) = foreign_run_full(entry, expect, toolchain, setup, env);
        (out, verdict)
    }

    fn foreign_run_full(entry: &str, expect: Option<&str>, toolchain: &str, setup: impl Fn(&Path), env: &[(&str, &str)]) -> (std::process::Output, String, String) {
        let home = TempDir::new();
        setup(home.path());
        let src = home.path().join("src");
        fs::create_dir_all(&src).unwrap();
        for a in [&["init", "-q"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&src).args(a).status().unwrap().success());
        }
        git_out(&src, &["update-ref", "refs/rounds/t", "HEAD"]);
        let sha = expect.map(str::to_string).unwrap_or_else(|| git_out(&src, &["rev-parse", "HEAD"]));
        let script = FOREIGN_SCRIPT.replace("\"git://${host_addr}:${port}/mirror.git\"", &format!("'{}'", src.display()));
        assert_ne!(script, FOREIGN_SCRIPT);
        let out = std::process::Command::new("bash")
            .args(["-c", &script, "x", "h", "1", "", "", toolchain, "", "", "", "", "", "", "", "refs/rounds/t", &sha, entry])
            .env_clear()
            .envs(env.iter().copied())
            .env("HOME", home.path())
            .env("PATH", std::env::var("PATH").unwrap())
            .output()
            .unwrap();
        let res = home.path().join("round-work/.runtime/spira/batch-results");
        let verdict = fs::read_to_string(res.join("ci.result")).unwrap_or_default();
        (out, verdict, fs::read_to_string(res.join("ci.out")).unwrap_or_default())
    }

    #[test]
    fn the_foreign_script_checks_out_the_round_commit_and_refuses_another_naming_both() {
        let (out, _) = foreign_run("exit 0");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let other = "0123456789012345678901234567890123456789";
        let (out, verdict) = foreign_run_expecting("exit 0", Some(other));
        assert_eq!(out.status.code(), Some(2));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(other) && err.contains("expected the round's commit"), "{err}");
        assert!(verdict.is_empty());
    }

    #[test]
    fn the_foreign_script_runs_the_repos_entry_and_its_exit_is_the_verdict() {
        let (out, verdict) = foreign_run("echo tier-one; exit 0");
        assert!(out.status.success());
        assert!(verdict.starts_with("ok "), "{verdict}");
        let (out, verdict) = foreign_run("echo isolation tier failed; exit 7");
        assert_eq!(out.status.code(), Some(1));
        assert!(verdict.starts_with("red ") && verdict.trim_end().ends_with(" 7"), "{verdict}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("isolation tier failed"));
    }

    #[test]
    fn the_foreign_script_leaves_the_entrys_output_and_exit_where_the_mender_reads_it() {
        let (_, _, out) = foreign_run_dir("echo tier-one; echo oops >&2; exit 7");
        let (_, _, early) = foreign_run_full("exit 0", Some("0123456789012345678901234567890123456789"), "", |_| {}, &[]);
        assert!(early.contains("expected the round's commit") && early.contains("exited 2"), "{early}");
        assert!(out.contains("tier-one") && out.contains("oops") && out.contains("ci_entry exited 7"), "{out}");
    }

    #[test]
    fn the_foreign_script_resolves_rust_tools_through_rustup_from_the_trees_pin() {
        use std::os::unix::fs::symlink;
        let setup = |home: &Path| {
            let bin = home.join(".cargo/bin");
            fs::create_dir_all(&bin).unwrap();
            let rustup = "#!/bin/sh\ncase \"${0##*/}\" in\n  rustup) echo \"$@\" > \"$HOME/rustup.args\" ;;\n  rustc) echo \"rustc via proxy pin=${RUSTUP_TOOLCHAIN:-none}\" ;;\nesac\n";
            testkit::write_exe(bin.join("rustup"), rustup);
            let image = home.join("image-rustc");
            testkit::write_exe(&image, "#!/bin/sh\necho rustc 1.99.1 direct\n");
            symlink(&image, bin.join("rustc")).unwrap();
        };
        let (out, _) = foreign_run_with("rustc -vV; echo \"[$RUSTUP_TOOLCHAIN]\"", None, "1.94", setup, &[("RUSTC", "/bin/false")]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{err}");
        assert!(err.contains("rustc via proxy pin=1.94"), "{err}");
        assert!(!err.contains("1.99.1"), "{err}");
        assert!(err.contains("[1.94]"), "{err}");
        let (out, _) = foreign_run_with("rustc -vV", None, "", setup, &[]);
        assert!(!String::from_utf8_lossy(&out.stderr).contains("1.99.1"));
    }

    #[test]
    fn remote_command_keeps_empty_arguments_in_place() {
        let cmd = remote_command(&BatchJob {
            host_addr: "10.0.0.1".into(),
            mirror_port: 9430,
            suites: None,
            maxpar: 16,
            toolchain: Some("1.82.0".into()),
            cache_home: Some("/opt/spira/cargo".into()),
            testenv_registry: Some("registry.example/spira".into()),
            setup_alarm_secs: 45,
            cap_secs: 900,
            testenv_limits: "testenv_suite_cap = 600\n".into(),
            post_guard_secs: 600,
            lint: false,
            build_image: false,
            round_ref: "refs/rounds/7-abc".into(),
            round_sha: "abc".into(),
            ci_entry: Some("deploy/ci.sh all".into()),
        });
        assert_eq!(cmd, "bash -s -- '10.0.0.1' '9430' '' '16' '1.82.0' '/opt/spira/cargo' 'registry.example/spira' '45' '900' '600' '' '' 'refs/rounds/7-abc' 'abc' 'deploy/ci.sh all' 'testenv_suite_cap = 600\n'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn the_round_config_carries_the_hosts_testenv_limits_not_the_fixtures() {
        let cfg = REMOTE_SCRIPT.find("cat > \"$HOME/round-config.toml\"").unwrap();
        let end = cfg + REMOTE_SCRIPT[cfg..].find("ROUNDCFG\nexport").unwrap();
        assert!(REMOTE_SCRIPT[cfg..end].contains("\n$testenv_limits\n"), "the heredoc declares the host's limits over the fixture");
        assert!(REMOTE_SCRIPT.contains("[ -n \"$testenv_limits\" ] ||"), "no limits sent is a refusal");
    }

    #[test]
    fn remote_script_exports_the_resume_dir_before_testenv_runs() {
        let export = REMOTE_SCRIPT.find(r#"export SPIRA_TESTENV_RESUME="dir:$HOME/round-resume""#).expect("exports the resume dir");
        let corpus = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(export < corpus);
        assert_eq!(REMOTE_RESUME, "round-resume/");
    }

    #[test]
    fn push_resume_sends_only_a_held_dir() {
        let d = testkit::TempDir::new("rvm-resume");
        let remote = FakeRemote::green();
        let held = d.path().join("held");
        push_resume(&remote, "10.0.0.2", &held);
        fs::create_dir_all(&held).unwrap();
        push_resume(&remote, "10.0.0.2", &held);
        assert!(remote.pushes.lock().unwrap().is_empty());
        fs::write(held.join("k"), "x").unwrap();
        push_resume(&remote, "10.0.0.2", &held);
        assert_eq!(*remote.pushes.lock().unwrap(), vec![(held, REMOTE_RESUME.to_string())]);
    }

    #[test]
    fn remote_script_checks_cargo_before_paying_for_the_clone() {
        let check = REMOTE_SCRIPT.find("command -v cargo").expect("checks for cargo");
        let clone = REMOTE_SCRIPT.find("git clone").expect("clones the mirror");
        assert!(check < clone, "cargo must be checked before the clone, not after it fails");
        let path_export = REMOTE_SCRIPT.find(r#"export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin""#).expect("puts cargo's install dirs on PATH");
        assert!(path_export < check, "PATH must be widened before the check that reads it");
    }

    /// Runs only REMOTE_SCRIPT's guard, with no `cargo` reachable — never reaching the git
    /// clone or the disk state a real round would leave behind.
    #[test]
    fn remote_script_fails_fast_and_named_with_no_cargo_on_path() {
        // REMOTE_SCRIPT resets PATH to fixed system directories; where one of them carries a
        // cargo (a round VM does), "no cargo on PATH" cannot be arranged here: nothing to judge.
        if ["/usr/local/bin", "/usr/local/sbin", "/usr/sbin", "/usr/bin", "/sbin", "/bin"]
            .iter()
            .any(|d| std::path::Path::new(d).join("cargo").exists())
        {
            return;
        }
        let d = TempDir::new();
        let cache_home = d.path().join("cargo-home");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(REMOTE_SCRIPT)
            .arg("round-vm-test")
            .args(["host", "9430", "", "16", ""])
            .arg(&cache_home)
            .args(["", "60", "900", "600", "", "", "refs/rounds/t", "abc", "", "testenv_suite_cap = 600\n"])
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/nonexistent-home")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(127));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("cargo not found"), "{stderr}");
    }

    #[test]
    fn an_image_without_a_cache_home_record_is_refused_out_loud() {
        // The block runs under REMOTE_SCRIPT's own `set -euo pipefail`: a `sed` of a missing
        // /etc/spira-round-ami.env inside `$(...)` failed the assignment, and the script
        // exited 2 with nothing on stderr — a round that died between conf-gen and the build
        // without a word (seen on an EC2 image that predates the record).
        if std::path::Path::new("/etc/spira-round-ami.env").exists() {
            return; // an image that HAS the record cannot show its absence
        }
        let start = REMOTE_SCRIPT.find("if [ -z \"$cache_home\" ]; then").expect("the cache-home block");
        let end = REMOTE_SCRIPT.find("export CARGO_HOME=\"$cache_home\"").expect("CARGO_HOME export");
        let block = format!("set -euo pipefail\ncache_home=\"\"\n{}", &REMOTE_SCRIPT[start..end]);
        let out = std::process::Command::new("bash").arg("-c").arg(&block).env("PATH", "/usr/bin:/bin").output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(5), "{stderr}");
        assert!(stderr.contains("this image records none"), "{stderr}");
    }

    #[test]
    fn measurement_reads_flat_nested_and_empty_results() {
        let d = TempDir::new();
        let flat = d.path().join("flat");
        fs::create_dir_all(&flat).unwrap();
        fs::write(flat.join("test-a.sh.result"), "ok 1 3 - serial explicit 0\n").unwrap();
        fs::write(flat.join("test-b.sh.result"), "ok 1 4 - serial explicit 0\n").unwrap();
        assert_eq!(results_leaf(&flat), Some(flat.clone()));
        assert_eq!(suite_wall_sum(&flat), 7);
        let nested = d.path().join("nested");
        fs::create_dir_all(nested.join("deadbeef")).unwrap();
        fs::write(nested.join("deadbeef/test-c.sh.result"), "ok 1 5 - serial explicit 0\n").unwrap();
        assert_eq!(results_leaf(&nested), Some(nested.join("deadbeef")));
        let empty = d.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(results_leaf(&empty), None);
        assert_eq!(suite_wall_sum(&empty), 0);
        fs::write(flat.join("runner.meta"), "nproc=8\nmaxpar=16\nbuild_wall_s=42\n").unwrap();
        assert_eq!(build_wall(&flat), Some(42));
        assert_eq!(build_wall(&empty), None);
    }

    fn exe(p: &Path) {
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        testkit::write_exe(p, "x");
    }

    #[test]
    fn binaries_of_another_tree_are_refused_and_nothing_installed() {
        let d = TempDir::new();
        let (pulled, target) = (d.path().join("pulled"), d.path().join("wt/target/release"));
        exe(&pulled.join("fakebin"));
        let e = install_bins(&pulled, "expectedsha", Some("wrongsha"), &target).unwrap_err();
        assert!(e.contains("wrongsha") && e.contains("expectedsha"), "{e}");
        assert!(e.contains("refusing binaries"), "{e}");
        let none = install_bins(&pulled, "expectedsha", None, &target).unwrap_err();
        assert!(none.contains("reported no tested tree") && !none.contains("refusing") && !none.contains("<none"), "{none}");
        assert!(!target.exists());
    }

    #[test]
    fn binaries_of_this_tree_land_where_land_local_reads_them() {
        let d = TempDir::new();
        let (pulled, target) = (d.path().join("pulled"), d.path().join("wt/target/release"));
        exe(&pulled.join("fakebin"));
        fs::write(pulled.join("not-executable"), "x").unwrap();
        install_bins(&pulled, "expectedsha", Some("expectedsha"), &target).unwrap();
        assert!(target.join("fakebin").is_file());
        assert!(!target.join("not-executable").exists());
        install_bins(&d.path().join("absent"), "expectedsha", None, &target).unwrap();
    }

    #[test]
    fn tsd_rows_are_tagged_and_deduplicated() {
        let d = TempDir::new();
        let src = d.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("suite.jsonl"), "{\"suite\":\"a\",\"secs\":3}\nnot json\n{\"suite\":\"b\"}\n").unwrap();
        let run = d.path().join("run");
        assert_eq!(merge_tsd(&src, &run, "123", 16, 24, None).unwrap(), 2);
        assert_eq!(merge_tsd(&src, &run, "123", 16, 24, None).unwrap(), 0, "a re-merge adds nothing");
        let rows: Vec<Value> = fs::read_to_string(run.join("tsd/suite.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["ran_on"], "123");
        assert_eq!(rows[0]["vcpus"], 16);
        assert_eq!(rows[0]["maxpar"], 24);
        assert_eq!(rows[0]["secs"], 3);
    }

    #[test]
    fn vm_signals_go_to_the_store_and_a_re_merge_adds_no_second_row() {
        let d = TempDir::new();
        let src = d.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("vm-signals.jsonl"), "{\"ts\":\"2026-10-22T10:00:00Z\",\"host\":\"vm\",\"family\":\"vm-signals\",\"iowait_pct\":4.5,\"steal_pct\":0.0}\n").unwrap();
        let run = d.path().join("run");
        let store = spira_otel::fixture::FakeStore::start();
        merge_tsd(&src, &run, "123", 16, 24, Some(store.addr())).unwrap();
        merge_tsd(&src, &run, "123", 16, 24, Some(store.addr())).unwrap();
        assert!(!run.join("tsd/vm-signals.jsonl").exists(), "a moved family has no file");
        let rows = tsd::store::Reader::new(Some(store.addr())).unwrap().rows("vm-signals", 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0]["iowait_pct"].as_f64(), rows[0]["ran_on"].as_str(), rows[0]["vcpus"].as_i64()), (Some(4.5), Some("123"), Some(16)));
        let err = merge_tsd(&src, &run, "123", 16, 24, None).unwrap_err();
        assert!(err.contains("not stored"), "an unreachable store is an error, not a silent drop: {err}");
    }

    // ── run end to end against fakes ────────────────────────────────────────────────

    #[test]
    fn teardown_leaves_no_listener_and_nothing_running_is_success() {
        let d = TempDir::new();
        assert!(stop_mirror(d.path()).is_ok(), "no pidfile");
        let pidfile = d.path().join("git-daemon.pid");
        let st = std::process::Command::new("sh").arg("-c").arg(format!("sleep 300 </dev/null >/dev/null 2>&1 & echo $! > {}", pidfile.display())).status().unwrap();
        assert!(st.success());
        let pid: i32 = fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(pid_alive(pid));
        stop_mirror(d.path()).unwrap();
        assert!(!pid_alive(pid), "the daemon survived teardown");
        assert!(!pidfile.exists());
    }

    #[test]
    fn host_addresses_reads_ip_br_addr() {
        let out = "lo UNKNOWN 127.0.0.1/8 ::1/128\neth0 UP 192.168.15.174/22 fe80::1/64\n";
        let a = host_addresses(out);
        assert!(a.iter().any(|x| x == "192.168.15.174") && a.iter().any(|x| x == "127.0.0.1"));
        assert!(!a.iter().any(|x| x == "192.168.1.56"));
    }

    fn accepts(addr: &str, port: u16) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if std::net::TcpStream::connect((addr, port)).is_ok() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    const RR: &str = "refs/rounds/t";

    #[test]
    fn a_kept_tree_with_an_old_round_branch_is_moved_to_the_round_commit() {
        let d = TempDir::new();
        let (src, kept) = (d.path().join("src"), d.path().join("kept"));
        fs::create_dir_all(&src).unwrap();
        let commit = |m: &str| {
            {
                let a = &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", m][..];
                assert!(std::process::Command::new("git").arg("-C").arg(&src).args(a).status().unwrap().success());
            }
        };
        assert!(std::process::Command::new("git").arg("-C").arg(&src).args(["init", "-q", "-b", "round"]).status().unwrap().success());
        commit("old");
        assert!(std::process::Command::new("git").args(["clone", "-q"]).arg(&src).arg(&kept).status().unwrap().success());
        commit("new");
        git_out(&src, &["update-ref", "refs/rounds/t", "HEAD"]);
        let want = git_out(&src, &["rev-parse", "HEAD"]);
        let line = REMOTE_SCRIPT.lines().find(|l| l.contains("checkout --quiet --force -B round FETCH_HEAD")).expect("the kept-tree checkout");
        let script = format!("set -e\ngit -C \"$K\" fetch --quiet \"$S\" refs/rounds/t\ngit -C \"$K\" {}", line.trim().trim_start_matches("&& git -C ~/round-work").trim_end_matches('}').trim());
        let out = std::process::Command::new("bash").args(["-c", &script]).env("K", &kept).env("S", &src).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(git_out(&kept, &["rev-parse", "round"]), want, "testenv's branch is the round's commit");
        assert_eq!(git_out(&kept, &["rev-parse", "HEAD"]), want);
    }

    #[test]
    fn two_repos_prepared_back_to_back_each_keep_their_own_ref_and_it_is_pruned() {
        let d = TempDir::new();
        let mk = |name: &str| {
            let t = d.path().join(name);
            fs::create_dir_all(&t).unwrap();
            for a in [vec!["init", "--quiet"], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "--allow-empty", "-m", name]] {
                assert!(std::process::Command::new("git").arg("-C").arg(&t).args(&a).status().unwrap().success());
            }
            t
        };
        let (a, b) = (mk("a"), mk("b"));
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let state = d.path().join("state");
        let host = GitHost { state_dir: state.clone(), mirror_port: port, listen: "127.0.0.1".into(), also_listen: vec![] };
        host.prepare_mirror(&a, "refs/rounds/1-a").unwrap();
        host.prepare_mirror(&b, "refs/rounds/2-b").unwrap();
        let m = state.join("mirror.git");
        assert_eq!(git_out(&m, &["rev-parse", "refs/rounds/1-a"]), git_out(&a, &["rev-parse", "HEAD"]));
        assert_eq!(git_out(&m, &["rev-parse", "refs/rounds/2-b"]), git_out(&b, &["rev-parse", "HEAD"]));
        host.drop_round_ref("refs/rounds/1-a");
        assert_eq!(git_out(&m, &["for-each-ref", "--format=%(refname)", "refs/rounds"]), "refs/rounds/2-b");
        stop_mirror(&state).unwrap();
    }

    #[test]
    fn a_real_mirror_daemon_listens_where_told_and_teardown_closes_it() {
        let d = TempDir::new();
        let tree = d.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        for a in [vec!["init", "--quiet"], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "--allow-empty", "-m", "x"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&tree).args(&a).status().unwrap().success());
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let state = d.path().join("state");
        let no_addr = GitHost { state_dir: state.clone(), mirror_port: port, listen: String::new(), also_listen: vec![] };
        assert!(no_addr.prepare_mirror(&tree, RR).is_err(), "no address means no daemon, never every interface");
        let host = GitHost { state_dir: state.clone(), mirror_port: port, listen: "127.0.0.1".into(), also_listen: vec![] };
        host.prepare_mirror(&tree, RR).unwrap();
        assert!(accepts("127.0.0.1", port), "daemon not listening");
        let old_pid = fs::read_to_string(state.join("git-daemon.pid")).unwrap();
        host.prepare_mirror(&tree, RR).unwrap();
        assert_eq!(old_pid, fs::read_to_string(state.join("git-daemon.pid")).unwrap(), "a daemon on the right address is kept");
        fs::write(state.join("git-daemon.addr"), "192.0.2.1").unwrap();
        host.prepare_mirror(&tree, RR).unwrap();
        assert_ne!(old_pid, fs::read_to_string(state.join("git-daemon.pid")).unwrap(), "a daemon recorded on another address is restarted");
        assert_eq!(fs::read_to_string(state.join("git-daemon.addr")).unwrap(), "127.0.0.1");
        assert!(accepts("127.0.0.1", port));
        let foreign = GitHost { state_dir: state.clone(), mirror_port: port, listen: "192.0.2.1".into(), also_listen: vec![] };
        let e = foreign.prepare_mirror(&tree, RR).unwrap_err();
        assert!(e.contains("not an address of this host") && e.contains("set it to auto"), "{e}");
        stop_mirror(&state).unwrap();
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "listener survived teardown");
    }

    #[test]
    fn a_mirror_with_a_second_address_binds_both_and_refuses_a_foreign_one() {
        let d = TempDir::new();
        let tree = d.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        for a in [vec!["init", "--quiet"], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "--allow-empty", "-m", "x"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&tree).args(&a).status().unwrap().success());
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let state = d.path().join("state");
        let host = GitHost { state_dir: state.clone(), mirror_port: port, listen: "127.0.0.1".into(), also_listen: vec!["127.0.0.1".into()] };
        assert_eq!(host.listen_set(), vec!["127.0.0.1"], "a duplicate address is bound once");
        let two = GitHost { state_dir: state.clone(), mirror_port: port, listen: "192.0.2.1".into(), also_listen: vec!["127.0.0.1".into()] };
        assert_eq!(two.listen_set(), vec!["192.0.2.1", "127.0.0.1"]);
        let bad = GitHost { state_dir: state.clone(), mirror_port: port, listen: "127.0.0.1".into(), also_listen: vec!["192.0.2.1".into()] };
        let e = bad.prepare_mirror(&tree, RR).unwrap_err();
        assert!(e.starts_with("192.0.2.1 is not an address of this host"), "{e}");
        let loopback2 = GitHost { state_dir: state.clone(), mirror_port: port, listen: "127.0.0.1".into(), also_listen: vec!["127.0.0.2".into()] };
        let r = loopback2.prepare_mirror(&tree, RR);
        if r.is_ok() {
            assert!(accepts("127.0.0.2", port), "second address not listening");
            assert!(accepts("127.0.0.1", port));
            assert_eq!(fs::read_to_string(state.join("git-daemon.addr")).unwrap(), "127.0.0.1,127.0.0.2");
        }
        stop_mirror(&state).unwrap();
    }

    struct FakeHost;
    impl Host for FakeHost {
        fn is_checkout(&self, tree: &Path) -> bool {
            !tree.ends_with("not-a-repo")
        }
        fn head(&self, _: &Path) -> Result<(String, String), String> {
            Ok(("commitsha".into(), "treesha".into()))
        }
        fn prepare_mirror(&self, _: &Path, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn drop_round_ref(&self, _: &str) {}
        fn image_tag(&self, tree: &Path, rev: &str) -> Result<String, String> {
            if rev == "main" { Ok("tagA".into()) } else if tree.ends_with("tag-unreadable") { Err("no closure".into()) } else if tree.ends_with("newer-image") { Ok("tagB".into()) } else { Ok("tagA".into()) }
        }
        fn mirror_ref(&self, _: &Path, branch: &str, _: &str) -> Result<(), String> {
            if branch == "missing" { Err("no such branch".into()) } else { Ok(()) }
        }
        fn suites_in(&self, _: &Path) -> Result<Vec<String>, String> {
            Ok(vec!["test-fast.sh".into(), "test-slow.sh".into()])
        }
        fn suite_medians(&self) -> Result<std::collections::HashMap<String, f64>, String> {
            Ok([("test-fast.sh".to_string(), 2.0), ("test-slow.sh".to_string(), 90.0)].into())
        }
    }

    fn wait_until(cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(60), "fake remote: condition never held");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A VM whose remote tree is a map of remote path → files it holds.
    struct FakeRemote {
        reachable: bool,
        rc: Result<i32, String>,
        files: BTreeMap<&'static str, Vec<(&'static str, &'static str)>>,
        jobs: Mutex<Vec<String>>,
        pushes: Mutex<Vec<(PathBuf, String)>>,
        /// the corpus runs until the first attribution rerun has started (no sleep to race)
        hold_for_attr: bool,
        /// (job id, whether the batch was still running when the job started)
        attrs: Mutex<Vec<(String, bool)>>,
        batch_running: std::sync::atomic::AtomicBool,
        /// the corpus runs until its results have been pulled once, then the connection drops
        hold_for_stream: bool,
        result_pulls: std::sync::atomic::AtomicUsize,
        /// how many results pulls fail before one succeeds
        fail_result_pulls: std::sync::atomic::AtomicUsize,
    }
    impl FakeRemote {
        fn green() -> FakeRemote {
            let mut files = BTreeMap::new();
            files.insert("round-work/.runtime/spira/batch-results/", vec![
                ("KEY/test-a.sh.result", "ok 1 30 - p e 0\n"),
                ("KEY/test-b.sh.result", "ok 1 12 - p e 0\n"),
                ("KEY/runner.meta", "build_wall_s=90\n"),
                ("KEY/batch.meta", "key=KEY\ntree=treesha\n"),
            ]);
            files.insert("round-work/.runtime/spira/tsd/", vec![("suite.jsonl", "{\"suite\":\"a\"}\n")]);
            files.insert(REMOTE_BINS, vec![("batcher", "bin")]);
            files.insert("attr-results/j1/", vec![("K2/test-a.sh.result", "ok 1 2 - p e 0\n")]);
            files.insert("attr-results/j2/", vec![("K3/test-a.sh.result", "red 1 2 fp p e 1\n"), ("K3/batch.meta", "tree=t2\n")]);
            FakeRemote {
                reachable: true,
                rc: Ok(0),
                files,
                jobs: Mutex::new(vec![]),
                pushes: Mutex::new(vec![]),
                hold_for_attr: false,
                attrs: Mutex::new(vec![]),
                batch_running: std::sync::atomic::AtomicBool::new(false),
                hold_for_stream: false,
                result_pulls: std::sync::atomic::AtomicUsize::new(0),
                fail_result_pulls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }
    impl Remote for FakeRemote {
        fn reachable(&self, _: &str) -> bool {
            self.reachable
        }
        fn run_batch(&self, _: &str, job: &BatchJob) -> Result<i32, String> {
            self.jobs.lock().unwrap().push(remote_command(job));
            self.batch_running.store(true, std::sync::atomic::Ordering::SeqCst);
            if self.hold_for_attr {
                wait_until(|| !self.attrs.lock().unwrap().is_empty());
            }
            if self.hold_for_stream {
                wait_until(|| self.result_pulls.load(std::sync::atomic::Ordering::SeqCst) > 0);
            }
            self.batch_running.store(false, std::sync::atomic::Ordering::SeqCst);
            self.rc.clone()
        }
        fn run_attr(&self, _: &str, job: &crate::spool::AttrJob) -> Result<i32, String> {
            if self.hold_for_attr && job.job == "j1" {
                wait_until(|| self.batch_running.load(std::sync::atomic::Ordering::SeqCst));
            }
            let running = self.batch_running.load(std::sync::atomic::Ordering::SeqCst);
            self.attrs.lock().unwrap().push((job.job.clone(), running));
            Ok(if job.job == "j2" { 1 } else { 0 })
        }
        fn push(&self, _: &str, local: &Path, remote_path: &str) -> Result<(), String> {
            self.pushes.lock().unwrap().push((local.to_path_buf(), remote_path.to_string()));
            Ok(())
        }
        fn pull(&self, _: &str, remote_path: &str, local: &Path) -> Result<(), String> {
            if remote_path == REMOTE_RESULTS {
                self.result_pulls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let failing = self.fail_result_pulls.try_update(std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst, |n| n.checked_sub(1)).is_ok();
                if failing {
                    return Err("rsync exited 255".into());
                }
            }
            let files = self.files.get(remote_path).ok_or("no such remote dir")?;
            for (rel, body) in files {
                let p = local.join(rel);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                if remote_path == REMOTE_BINS {
                    // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
                    testkit::write_exe(&p, body);
                } else {
                    fs::write(&p, body).unwrap();
                }
            }
            Ok(())
        }
    }

    struct NoSpawn;
    impl Spawner for NoSpawn {
        fn spawn_next(&self) -> Result<ProcId, String> {
            Err("not in tests".into())
        }
    }

    struct Fixture {
        d: TempDir,
        fp: FakeProvider,
        cfg: Config,
    }

    fn fixture() -> Fixture {
        let d = TempDir::new();
        let run_dir = d.path().join("run");
        let key = d.path().join("host_key");
        fs::write(&key, "k").unwrap();
        // Values passed directly to Config::build (DESIGN.md / round-vm's config migration
        // notes) — this used to be a fake env Config::load read through; now it is the
        // Fields literal the old defaults would have produced, with this fixture's own
        // overrides (host_key/host_addr/cache_home/ssh_tries/boot_poll) spelled out alongside
        // them instead of layered on top by Config::load itself.
        let cfg = Config::build(Fields {
            run: run_dir.to_string_lossy().to_string(),
            spira_home: None,
            pve_env: String::new(),
            ssh_user: "root".to_string(),
            ssh_port: 22,
            state_dir: run_dir.join("round-vm").to_string_lossy().to_string(),
            host_key: key.to_string_lossy().to_string(),
            host_pubkey: format!("{}.pub", key.to_string_lossy()),
            host_addr: Some("192.168.1.10".to_string()),
            cache_home: Some("/opt/spira/cargo".to_string()),
            testenv_registry: None,
            vcpus: 16,
            mem_mib: 0,
            maxpar: 16,
            max_retries: 0,
            retry_interval_secs: 60,
            mirror_port: 9430,
            setup_alarm_secs: 180,
            acquire_deadline_secs: 3600,
            mailbox: "operator".to_string(),
            net_iface: "ens18".to_string(),
            boot_tries: 60,
            boot_poll_secs: 0,
            ssh_tries: 2,
            stream_every_secs: 10,
            attr_linger_secs: 3600,
            cap_secs: 1234,
            testenv_limits: "testenv_suite_cap = 600\n".into(),
            post_guard_secs: 77,
            vm_budget_secs: 300,
            build_budget_secs: 600,
        });
        Fixture { d, fp: FakeProvider::new(), cfg }
    }

    fn go(fx: &Fixture, remote: &FakeRemote, a: &RunArgs) -> i32 {
        let pool = Pool { state_dir: fx.cfg.state_dir.clone(), retry_interval: Duration::ZERO, max_retries: 1, wait_poll: Duration::ZERO, acquire_deadline: Duration::ZERO };
        let fp = fx.fp.clone();
        let f = move || {
            Ok(Attempt {
                provider: Box::new(fp.clone()),
                spill: None,
                iface: "ens18".into(),
                ssh_user: "root".into(),
                pubkey: "k".into(),
                timing: Timing { boot_tries: 1, poll: Duration::ZERO, gone_tries: 1 },
            })
        };
        let alarm = FakeAlarm::default();
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &NoSpawn };
        run(&RunEnv { cfg: &fx.cfg, pool: &pool, deps: &deps, host: &FakeHost, remote, record: &FakeRecorder::default() }, a)
    }

    fn go_recorded(fx: &Fixture, remote: &FakeRemote, a: &RunArgs, rec: &FakeRecorder) -> i32 {
        let pool = Pool { state_dir: fx.cfg.state_dir.clone(), retry_interval: Duration::ZERO, max_retries: 1, wait_poll: Duration::ZERO, acquire_deadline: Duration::ZERO };
        let fp = fx.fp.clone();
        let f = move || {
            Ok(Attempt {
                provider: Box::new(fp.clone()),
                spill: None,
                iface: "ens18".into(),
                ssh_user: "root".into(),
                pubkey: "k".into(),
                timing: Timing { boot_tries: 1, poll: Duration::ZERO, gone_tries: 1 },
            })
        };
        let alarm = FakeAlarm::default();
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &NoSpawn };
        run(&RunEnv { cfg: &fx.cfg, pool: &pool, deps: &deps, host: &FakeHost, remote, record: rec }, a)
    }

    #[derive(Default)]
    struct FakeRecorder {
        calls: Mutex<Vec<String>>,
        bodies: Mutex<Vec<(String, String)>>,
        refuse: bool,
        refuse_record: bool,
    }

    impl Recorder for FakeRecorder {
        fn pass_started(&self, batch: &str, _: Option<&str>) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("start {batch}"));
            Ok(())
        }
        fn pass_incomplete(&self, batch: &str, _: Option<&str>, _: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("incomplete {batch}"));
            Ok(())
        }
        fn suites_started(&self, batch: &str, repo: Option<&str>) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("{batch} {}", repo.unwrap_or("-")));
            if self.refuse { Err("the batch is ATTRIBUTING, not CI_RUNNING".into()) } else { Ok(()) }
        }
        fn progress(&self, batch: &str, body: &str) -> Result<(), String> {
            self.bodies.lock().unwrap().push((batch.to_string(), body.to_string()));
            Ok(())
        }
        fn pass_context(&self, _: &str) -> Option<(u64, Vec<String>)> {
            Some((3, vec!["m1".into(), "m2".into()]))
        }
        fn pass_record(&self, root: &Path, rec: &tsd::pass::PassRecord) -> Result<(), String> {
            if self.refuse_record {
                return Err("disk full".into());
            }
            rec.write(root, "h", "2026-10-14T00:00:00Z")
        }
    }

    fn pass_rows(fx: &Fixture) -> Vec<Value> {
        let text = fs::read_to_string(tsd::family_path(&fx.cfg.run_dir, tsd::pass::FAMILY)).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn batch_args(fx: &Fixture, batch: &str) -> RunArgs {
        RunArgs { results_dir: Some(fx.d.path().join("results")), suites: Some("test-a.sh,test-b.sh".into()), round_batch: Some(batch.into()), round_repo: Some("sp".into()), ..tree(fx) }
    }

    #[test]
    fn every_pass_of_a_round_yields_exactly_one_record_with_its_cause() {
        let fx = fixture();
        let rec = FakeRecorder::default();
        let cases = [("b-green", 0, "green"), ("b-red", 1, "red-attributed"), ("b-cap", 124, "over-cap"), ("b-infra", 3, "infra")];
        for (batch, rc, _) in cases {
            let remote = FakeRemote { rc: Ok(rc), ..FakeRemote::green() };
            go_recorded(&fx, &remote, &batch_args(&fx, batch), &rec);
        }
        let lost = FakeRemote { rc: Ok(255), ..FakeRemote::green() };
        assert_eq!(go_recorded(&fx, &lost, &batch_args(&fx, "b-lost"), &rec), 2);
        let text = fs::read_to_string(tsd::family_path(&fx.cfg.run_dir, tsd::pass::FAMILY)).unwrap();
        let expected: Vec<(&str, &str, &str)> = ["b-green", "b-red", "b-cap", "b-infra", "b-lost"].iter().map(|b| ("sp", *b, "3")).collect();
        tsd::pass::exactly_one(&text, &expected).unwrap();
        let rows = pass_rows(&fx);
        let cause = |b: &str| rows.iter().find(|r| r["batch"] == b).unwrap()["cause"].as_str().unwrap().to_string();
        for (batch, _, want) in cases {
            assert_eq!(cause(batch), want, "{batch}");
        }
        assert_eq!(cause("b-lost"), "vm-lost");
        let g = rows.iter().find(|r| r["batch"] == "b-green").unwrap();
        assert_eq!((g["where"].as_str(), g["members"].as_array().map(Vec::len), g["verdict"].as_str()), (Some("proxmox"), Some(2), Some("green")));
        assert_eq!(g["vcpus"], 16);
    }

    #[test]
    fn a_pass_whose_record_cannot_be_written_is_a_loss_not_a_verdict() {
        let fx = fixture();
        let rec = FakeRecorder { refuse_record: true, ..Default::default() };
        assert_eq!(go_recorded(&fx, &FakeRemote::green(), &batch_args(&fx, "b-1"), &rec), 2, "a green pass nobody can record is not judged");
        assert_eq!(*rec.calls.lock().unwrap().last().unwrap(), "incomplete b-1");
    }

    #[test]
    fn a_run_with_no_batch_writes_no_pass_record() {
        let fx = fixture();
        let a = RunArgs { results_dir: Some(fx.d.path().join("results")), ..tree(&fx) };
        assert_eq!(go(&fx, &FakeRemote::green(), &a), 0);
        assert!(pass_rows(&fx).is_empty());
    }

    #[test]
    fn a_pass_with_a_batch_records_its_suites_boundary_once() {
        let fx = fixture();
        let rec = FakeRecorder::default();
        let a = RunArgs { results_dir: Some(fx.d.path().join("results")), suites: Some("test-a.sh,test-b.sh".into()), round_batch: Some("r-9".into()), round_repo: Some("sp".into()), ..tree(&fx) };
        assert_eq!(go_recorded(&fx, &FakeRemote::green(), &a, &rec), 0);
        assert_eq!(*rec.calls.lock().unwrap(), vec!["start r-9".to_string(), "r-9 sp".to_string()]);
        let bodies = rec.bodies.lock().unwrap();
        let (batch, last) = bodies.last().expect("the pass publishes its counts onto the batch row");
        let v: Value = serde_json::from_str(last).unwrap();
        assert_eq!((batch.as_str(), v["total"].as_u64(), v["phase"].as_str()), ("r-9", Some(2), Some("done")));
    }

    #[test]
    fn a_pass_with_no_batch_records_nothing() {
        let fx = fixture();
        let rec = FakeRecorder::default();
        let a = RunArgs { results_dir: Some(fx.d.path().join("results")), suites: Some("test-a.sh".into()), ..tree(&fx) };
        assert_eq!(go_recorded(&fx, &FakeRemote::green(), &a, &rec), 0);
        assert!(rec.calls.lock().unwrap().is_empty() && rec.bodies.lock().unwrap().is_empty());
    }

    #[test]
    fn a_refused_record_makes_the_pass_a_fault_not_a_verdict() {
        let fx = fixture();
        let rec = FakeRecorder { refuse: true, ..Default::default() };
        let a = RunArgs { results_dir: Some(fx.d.path().join("results")), suites: Some("test-a.sh,test-b.sh".into()), round_batch: Some("r-9".into()), ..tree(&fx) };
        assert_eq!(go_recorded(&fx, &FakeRemote::green(), &a, &rec), 2);
        let v: Value = serde_json::from_str(&fs::read_to_string(fx.cfg.run_dir.join("round-progress.json")).unwrap()).unwrap();
        assert_eq!(v["verdict"].as_str(), Some("fault"));
    }

    #[test]
    fn a_foreign_round_whose_entry_prints_errors_and_exits_1_is_red_and_fires_no_attribution_hook() {
        let fx = fixture();
        let rec = FakeRecorder::default();
        let marker = fx.d.path().join("hook-fired");
        let mut remote = FakeRemote { rc: Ok(1), ..FakeRemote::green() };
        remote.files.insert(
            "round-work/.runtime/spira/batch-results/",
            vec![
                ("KEY/ci.result", "red 1 5 - - - 1\n"),
                ("KEY/ci.out", "svelte-check found 2 errors\nround-vm: ci_entry exited 1\n"),
                ("KEY/batch.meta", "tree=treesha\n"),
            ],
        );
        remote.files.remove(REMOTE_BINS);
        let a = RunArgs {
            results_dir: Some(fx.d.path().join("results")),
            round_batch: Some("r-9".into()),
            round_repo: Some("cubby".into()),
            ci_entry: Some("npm run check".into()),
            on_red: Some(vec!["touch".into(), marker.display().to_string()]),
            ..tree(&fx)
        };
        assert_eq!(go_recorded(&fx, &remote, &a, &rec), 1);
        std::thread::sleep(Duration::from_millis(300)); // clock-safe: grace before an absence check; load can only delay, not flip it
        assert!(!marker.exists(), "a foreign round has one verdict and nothing to attribute");
        let v: Value = serde_json::from_str(&fs::read_to_string(fx.cfg.run_dir.join("round-progress.json")).unwrap()).unwrap();
        assert_eq!(v["verdict"].as_str(), Some("red"));
    }

    #[test]
    fn the_round_flags_are_parsed() {
        let a = parse_run_args(&args(&["t", "--round-batch", "r-1", "--round-repo=sp"])).unwrap();
        assert_eq!((a.round_batch.as_deref(), a.round_repo.as_deref()), (Some("r-1"), Some("sp")));
    }

    fn tree(fx: &Fixture) -> RunArgs {
        RunArgs { tree_dir: fx.d.path().join("tree"), maxpar: Some(24), ..Default::default() }
    }

    fn record_template(fx: &mut Fixture, vmid: &str, image: &str, pve_names: &str) {
        let ca = fx.d.path().join("ca.pem");
        fs::write(&ca, "x").unwrap();
        let env = fx.d.path().join("pve.env");
        fs::write(&env, format!("PVE_TOKEN_ID=t\nPVE_TOKEN_SECRET=s\nPVE_NODE=n\nPVE_CACERT={}\nPVE_TEMPLATE_VMID={pve_names}\n", ca.display())).unwrap();
        fx.cfg.pve_env_path = env;
        fs::create_dir_all(&fx.cfg.state_dir).unwrap();
        Record { vmid: vmid.into(), image: image.into(), recipe_sha: None }.write(&fx.cfg.state_dir).unwrap();
    }

    fn newer_tree(fx: &Fixture) -> RunArgs {
        RunArgs { tree_dir: fx.d.path().join("newer-image"), maxpar: Some(24), ..Default::default() }
    }

    #[test]
    fn a_head_whose_image_the_template_lacks_declares_the_build_and_a_matching_one_does_not() {
        let mut fx = fixture();
        record_template(&mut fx, "130", "localhost/spira-testenv:tagA", "130");
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &newer_tree(&fx)), 0);
        assert!(remote.jobs.lock().unwrap()[0].contains("'180' '1234' '77' '' '1' 'refs/rounds/"), "declared: {:?}", remote.jobs.lock().unwrap());
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 0);
        assert!(remote.jobs.lock().unwrap()[0].contains("'180' '1234' '77' '' '' 'refs/rounds/"), "no image change, no build: {:?}", remote.jobs.lock().unwrap());
    }

    #[test]
    fn without_a_record_the_base_tag_decides_whether_the_build_is_declared() {
        let fx = fixture();
        let remote = FakeRemote::green();
        let mut a = tree(&fx);
        a.base = Some("main".into());
        assert_eq!(go(&fx, &remote, &a), 0);
        assert!(remote.jobs.lock().unwrap()[0].contains("'1' '' 'refs/rounds/"), "same tag as base: {:?}", remote.jobs.lock().unwrap());
        let remote = FakeRemote::green();
        let mut a = newer_tree(&fx);
        a.base = Some("main".into());
        assert_eq!(go(&fx, &remote, &a), 0);
        assert!(remote.jobs.lock().unwrap()[0].contains("'1' '1' 'refs/rounds/"), "newer tag than base: {:?}", remote.jobs.lock().unwrap());
    }

    #[test]
    fn the_remote_script_builds_a_declared_image_in_setup_and_still_refuses_an_undeclared_one() {
        let declared = REMOTE_SCRIPT.find("elif [ -n \"$build_image\" ]").expect("a declared branch");
        let refuse = REMOTE_SCRIPT.find("refusing to cold-build it").unwrap();
        let setup = REMOTE_SCRIPT.find("setup_secs=$(( $(date +%s) - t_start - unit_build_secs ))").unwrap();
        assert!(declared < refuse && refuse < setup, "the build is before the setup clock stops, the refusal stays the fallback");
        assert!(REMOTE_SCRIPT[declared..refuse].contains("testenv container image"));
        assert!(REMOTE_SCRIPT[declared..refuse].contains("exit 3"), "a failed build gives no verdict");
    }

    #[test]
    fn a_record_for_a_template_pve_env_no_longer_names_places_the_pass_as_image_missing() {
        let mut fx = fixture();
        record_template(&mut fx, "130", "localhost/spira-testenv:tagA", "131");
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 0);
        assert!(remote.jobs.lock().unwrap()[0].contains("'180' '1234' '77' '' '1' 'refs/rounds/"), "stale record declares the build: {:?}", remote.jobs.lock().unwrap());
    }

    #[test]
    fn the_remote_script_reports_setup_against_suites_and_alarms_over_the_limit() {
        let setup = REMOTE_SCRIPT.find("setup_secs=$(( $(date +%s) - t_start - unit_build_secs ))").expect("setup is measured");
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(setup < batch, "setup ends where the suites start");
        assert!(REMOTE_SCRIPT.contains("round-vm: setup ${setup_secs}s, suites ${suites_secs}s"));
        assert!(REMOTE_SCRIPT.contains("SETUP-SLOW"));
        assert!(REMOTE_SCRIPT.contains("[ \"$setup_secs\" -gt \"$setup_alarm\" ]"));
    }

    #[test]
    fn the_cap_clock_covers_the_suites_and_the_bare_lane_only() {
        let start = REMOTE_SCRIPT.find("t_suites=$(date +%s)").expect("the clock starts");
        let corpus = REMOTE_SCRIPT.find("testenv --mode parallel").expect("the corpus runs");
        let bare = REMOTE_SCRIPT.find("bash spira/bare-suites.sh").unwrap();
        let guard = REMOTE_SCRIPT.find("suites_cap_guard \"$suites_mark\"").expect("one guard times both");
        let stop = REMOTE_SCRIPT.find("if [ -e \"$suites_mark\" ]; then suites_clock=").expect("the clock stops");
        let unit_wait = REMOTE_SCRIPT.find("wait \"$unit_pid\"").unwrap();
        let bins = REMOTE_SCRIPT.find("round-bins/ \\;").unwrap();
        assert!(start < bare && bare < corpus && corpus < guard && guard < stop && stop < unit_wait && unit_wait < bins);
        assert!(REMOTE_SCRIPT[guard..stop].contains("\"$testenv_pid\" \"$bare_pid\""), "the lane runs under the same cap, beside the corpus");
        assert!(!REMOTE_SCRIPT[start..stop].contains("timeout -k 5 \"$cap\""), "no timer counts from before the first suite");
        assert!(REMOTE_SCRIPT.contains("round-vm: BARE-SUITES: PASS"));
        assert!(REMOTE_SCRIPT.contains("round-vm: OVER-CAP: guard=suites-cap elapsed=") && REMOTE_SCRIPT.contains("if [ -n \"$over_cap\" ]; then exit 124; fi"));
        assert!(REMOTE_SCRIPT[stop..bins].contains("+ post_guard ))"), "the unit-test wait has its own guard");
        assert_eq!(exit_for_remote(124), 124, "over-cap reaches the caller as itself");
    }

    fn run_guard(setup_secs: u64, run_secs: u64, cap: u64) -> (bool, String) {
        let from = REMOTE_SCRIPT.find("suites_cap_guard() {").unwrap();
        let func = &REMOTE_SCRIPT[from..from + REMOTE_SCRIPT[from..].find("\n}\n").unwrap() + 3];
        let d = TempDir::new();
        let script = format!(
            "{func}\nmark={m}; fired={f}\n( sleep {setup_secs}; : > \"$mark\"; sleep {run_secs} ) &\nworker=$!\nsuites_cap_guard \"$mark\" {cap} \"$fired\" \"$worker\" &\nguard=$!\nwait \"$worker\"\nwait \"$guard\"\n",
            m = d.path().join("mark").display(),
            f = d.path().join("fired").display(),
        );
        let _o = std::process::Command::new("bash").arg("-c").arg(script).output().unwrap();
        let fired = d.path().join("fired");
        (fired.exists(), fs::read_to_string(fired).unwrap_or_default())
    }

    #[test]
    fn a_long_setup_does_not_spend_the_cap_and_a_run_past_it_is_killed_with_its_elapsed() {
        let (fired, _) = run_guard(3, 1, 2);
        assert!(!fired, "setup longer than the cap, suites inside it: not killed");
        let (fired, elapsed) = run_guard(1, 30, 2);
        assert!(fired, "a run past the cap from its mark is killed");
        assert!(elapsed.trim().parse::<u64>().unwrap() >= 2, "the elapsed counts from the mark: {elapsed}");
    }

    #[test]
    fn a_signalled_run_salvages_the_verdicts_its_vm_already_produced() {
        let fx = fixture();
        let remote = FakeRemote::green();
        let dest = fx.cfg.run_dir.join("salvaged");
        let scratch = fx.cfg.state_dir.join(".salvage-test");
        let n = salvage_results(&remote, "addr", &dest, &scratch, Duration::from_secs(5));
        assert_eq!(n, Some(2));
        assert!(dest.join("test-a.sh.result").is_file());
        assert!(!scratch.exists(), "scratch is removed");
        let mut none = FakeRemote::green();
        none.files.clear();
        assert_eq!(salvage_results(&none, "addr", &dest, &scratch, Duration::from_secs(5)), None, "an unreachable VM salvages nothing and says the pull failed");
    }

    #[test]
    fn the_remote_script_records_vm_wide_signals_beside_the_suite_times() {
        let sampler = REMOTE_SCRIPT.find("sampler_pid=$!").expect("a sampler is started");
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        let stop = REMOTE_SCRIPT.find("kill \"$sampler_pid\"").expect("and stopped");
        assert!(sampler < batch && batch < stop, "it brackets the suites");
        for f in ["vm-signals", "iowait_pct", "steal_pct", "io_psi_some_avg10", "disk_inflight", "tsd/vm-signals.jsonl"] {
            assert!(REMOTE_SCRIPT.contains(f), "{f}");
        }
    }

    #[test]
    fn green_run_pulls_everything_writes_the_manifest_installs_and_releases() {
        let fx = fixture();
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 0);
        assert!(fx.fp.live_vms().is_empty(), "released: {:?}", fx.fp.live_vms());
        let results = fx.cfg.run_dir.join("batch-results");
        assert!(results.join("test-a.sh.result").is_file(), "flattened out of the BATCH_KEY dir");
        let m: Manifest = serde_json::from_str(&fs::read_to_string(fx.cfg.state_dir.join("manifests/treesha.json")).unwrap()).unwrap();
        assert_eq!((m.vm.as_str(), m.acquire, m.vcpus, m.maxpar), ("100", AcquireMode::Cold, 16, 24));
        assert_eq!((m.build_wall_secs, m.suite_wall_secs_sum), (Some(90), 42));
        assert_eq!(m.tree_sha_found.as_deref(), Some("treesha"));
        assert_eq!(m.commit_sha, "commitsha");
        // path-ok: a test asserting where round-vm installs a fixture binary in a temp worktree
        assert!(tree(&fx).tree_dir.join("target/release/batcher").is_file(), "installed into the round worktree");
        assert!(fs::read_to_string(fx.cfg.run_dir.join("tsd/suite.jsonl")).unwrap().contains("\"ran_on\":\"100\""));
        assert!(remote.jobs.lock().unwrap()[0].contains("'test-slow.sh,test-fast.sh' '24' '' '/opt/spira/cargo' '' '180' '1234' '77' '' '' 'refs/rounds/"), "{:?}", remote.jobs.lock().unwrap());
    }

    #[test]
    fn the_configured_testenv_registry_is_forwarded_to_the_vm() {
        let mut fx = fixture();
        fx.cfg.testenv_registry = Some("registry.example/spira".into());
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 0);
        assert!(remote.jobs.lock().unwrap()[0].contains("'registry.example/spira'"), "{:?}", remote.jobs.lock().unwrap());
    }

    #[test]
    fn a_red_remote_exit_code_passes_through_and_the_vm_is_released() {
        for rc in [1, 3, 4] {
            let fx = fixture();
            let remote = FakeRemote { rc: Ok(rc), ..FakeRemote::green() };
            assert_eq!(go(&fx, &remote, &tree(&fx)), rc);
            assert!(fx.fp.live_vms().is_empty());
        }
    }

    /// sp-dp872: the first round cut from the release exited 101 — cargo's own exit code
    /// (`cargo run` could not pick a binary once testenv grew `bd-meter`), passed through as if
    /// it were a testenv verdict. The batcher had no row for it. A remote exit outside
    /// testenv's contract (0-4) is round-vm's own harness fault: exit 2, named, VM released —
    /// on the no-ready-VM (cold) acquire path the walkthrough rounds took.
    #[test]
    fn a_remote_exit_outside_testenv_s_contract_is_exit_2_on_the_cold_path() {
        for rc in [101, 128, 137, -1] {
            let fx = fixture();
            let remote = FakeRemote { rc: Ok(rc), files: BTreeMap::new(), ..FakeRemote::green() };
            assert_eq!(go(&fx, &remote, &tree(&fx)), 2, "remote rc {rc}");
            assert_eq!(fx.fp.live_vms().len(), 1, "no results came back, so the VM is kept: {:?}", fx.fp.live_vms());
        }
    }

    #[test]
    fn a_spool_run_whose_remote_exits_outside_the_contract_writes_corpus_done_rc_2() {
        let fx = fixture();
        let remote = FakeRemote { rc: Ok(101), files: BTreeMap::new(), ..FakeRemote::green() };
        let (a, sp) = spool_fixture(&fx);
        assert_eq!(go(&fx, &remote, &a), 2);
        assert_eq!(fs::read_to_string(sp.dir.join("corpus.done")).unwrap(), "rc=2\n");
        assert_eq!(fx.fp.live_vms().len(), 1, "kept: the pull found nothing");
    }

    /// sp-dp872: testenv has two binaries (testenv, bd-meter); an unqualified `cargo run -p
    /// testenv` cannot choose and exits 101 before a suite runs.
    #[test]
    fn every_remote_cargo_run_names_the_testenv_binary() {
        for script in [REMOTE_SCRIPT, crate::spool::REMOTE_ATTR_SCRIPT] {
            for line in script.lines().filter(|l| l.contains("cargo run")) {
                assert!(line.contains("--bin testenv"), "unqualified cargo run: {line}");
            }
        }
    }

    #[test]
    fn every_failure_on_the_vm_is_exit_2_and_releases_it() {
        let cases: Vec<(FakeRemote, usize)> = vec![
            (FakeRemote { reachable: false, ..FakeRemote::green() }, 0),
            (FakeRemote { rc: Ok(255), ..FakeRemote::green() }, 1),
            (FakeRemote { rc: Err("ssh: spawn failed".into()), ..FakeRemote::green() }, 0),
            (FakeRemote { files: BTreeMap::new(), ..FakeRemote::green() }, 1),
        ];
        for (remote, kept) in cases {
            let fx = fixture();
            assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
            assert_eq!(fx.fp.live_vms().len(), kept, "{:?}", fx.fp.live_vms());
        }
    }

    #[test]
    fn a_green_run_with_another_tree_s_binaries_is_exit_2_and_installs_nothing() {
        let fx = fixture();
        let mut remote = FakeRemote::green();
        remote.files.insert("round-work/.runtime/spira/batch-results/", vec![
            ("KEY/test-a.sh.result", "ok 1 30 - p e 0\n"),
            ("KEY/batch.meta", "key=KEY\ntree=othersha\n"),
        ]);
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        // path-ok: a test asserting where round-vm installs a fixture binary in a temp worktree
        assert!(!tree(&fx).tree_dir.join("target/release/batcher").exists());
        assert!(fx.fp.live_vms().is_empty());
    }

    #[test]
    fn preflight_errors_exit_2_before_any_vm_is_cloned() {
        let fx = fixture();
        let a = RunArgs { tree_dir: fx.d.path().join("not-a-repo"), ..Default::default() };
        assert_eq!(go(&fx, &FakeRemote::green(), &a), 2);

        let mut fx2 = fixture();
        fx2.cfg.host_addr = None;
        assert_eq!(go(&fx2, &FakeRemote::green(), &tree(&fx2)), 2);

        let mut fx3 = fixture();
        fx3.cfg.host_key = fx3.d.path().join("no-such-key");
        assert_eq!(go(&fx3, &FakeRemote::green(), &tree(&fx3)), 2);

        // sp-xjnzl-2: never an ambient CARGO_HOME fallback — unset is refused, named, before
        // any VM is touched. The exact fault a production sweep hit when its systemd unit
        // (unlike an interactive shell) never set CARGO_HOME at all.
        let mut fx4 = fixture();
        fx4.cfg.cache_home = None;
        assert_eq!(go(&fx4, &FakeRemote::green(), &tree(&fx4)), 2);

        for f in [&fx, &fx2, &fx3, &fx4] {
            assert_eq!(f.fp.count("clone"), 0);
        }
    }

    #[test]
    fn an_acquire_that_gives_up_is_exit_2_and_never_runs_the_batch() {
        let fx = fixture();
        fx.fp.fail(crate::testutil::Step::NextId);
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        assert!(remote.jobs.lock().unwrap().is_empty(), "no local mode, no batch without a VM");
    }

    // ── --attr-spool (DESIGN.md §2.2a) ───────────────────────────────────────────────

    fn spool_fixture(fx: &Fixture) -> (RunArgs, Spool) {
        let dir = fx.d.path().join("spool");
        let req = dir.join("req");
        fs::create_dir_all(&req).unwrap();
        fs::write(req.join("j1.req"), "branch=spira/batcher-attr/r-j1\nsuites=test-a.sh\nbuild=dwarf\n").unwrap();
        fs::write(req.join("j2.req"), "branch=spira/batcher-attr/r-j2\nsuites=test-a.sh\nbuild=round\n").unwrap();
        fs::write(req.join("j3.req"), "branch=missing\nsuites=test-a.sh\nbuild=artifacts\n").unwrap();
        fs::write(dir.join("close"), "").unwrap();
        let a = RunArgs { attr_spool: Some(dir.clone()), results_dir: Some(fx.d.path().join("results")), ..tree(fx) };
        (a, Spool { dir })
    }

    #[test]
    fn the_spool_serves_reruns_while_the_corpus_runs_and_a_round_build_after_it() {
        let fx = fixture();
        let remote = FakeRemote { hold_for_attr: true, ..FakeRemote::green() };
        let (a, sp) = spool_fixture(&fx);
        assert_eq!(go(&fx, &remote, &a), 0, "the exit code is the corpus's");
        let attrs = remote.attrs.lock().unwrap().clone();
        assert!(attrs.contains(&("j1".to_string(), true)), "j1 ran while the corpus did: {attrs:?}");
        assert!(attrs.contains(&("j2".to_string(), false)), "the round build waited for the corpus: {attrs:?}");
        let res = sp.res_dir();
        assert_eq!(fs::read_to_string(res.join("j1.done")).unwrap(), "rc=0\n");
        assert!(res.join("j1/test-a.sh.result").is_file(), "flattened out of the key dir");
        assert_eq!(fs::read_to_string(res.join("j2.done")).unwrap(), "rc=1\n");
        assert!(res.join("j2/bins/batcher").is_file(), "a round build brings its binaries back");
        assert_eq!(fs::read_to_string(res.join("j3.done")).unwrap(), "rc=2\n", "a branch that cannot be mirrored is a fault");
        assert_eq!(fs::read_to_string(sp.dir.join("corpus.done")).unwrap(), "rc=0\n");
        assert!(fs::read_to_string(fx.d.path().join("results/test-b.sh.result")).is_ok(), "the corpus's results streamed in");
        assert!(fx.fp.live_vms().is_empty(), "released once the spool closed");
    }

    #[test]
    fn a_failed_results_pull_is_retried_and_the_full_set_comes_back() {
        let fx = fixture();
        let remote = FakeRemote::green();
        remote.fail_result_pulls.store(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(go(&fx, &remote, &tree(&fx)), 0);
        let results = fx.cfg.run_dir.join("batch-results");
        assert!(results.join("test-a.sh.result").is_file() && results.join("test-b.sh.result").is_file());
        assert!(fx.fp.live_vms().is_empty(), "a complete pull releases the VM");
    }

    #[test]
    fn a_pull_that_never_completes_is_incomplete_and_the_vm_is_kept() {
        let fx = fixture();
        let remote = FakeRemote::green();
        remote.fail_result_pulls.store(1000, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        assert_eq!(fx.fp.live_vms().len(), 1, "kept for a re-pull: {:?}", fx.fp.live_vms());
    }

    #[test]
    fn a_dropped_batch_ssh_salvages_what_it_can_and_keeps_the_vm() {
        let fx = fixture();
        let mut remote = FakeRemote::green();
        remote.rc = Ok(255);
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        assert!(fx.cfg.run_dir.join("batch-results/test-a.sh.result").is_file());
        assert_eq!(fx.fp.live_vms().len(), 1);
    }

    #[test]
    fn a_kept_vm_is_off_this_runs_lease_so_a_signal_cannot_release_it() {
        let fx = fixture();
        let mut remote = FakeRemote::green();
        remote.rc = Ok(255);
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        let pool = Pool { state_dir: fx.cfg.state_dir.clone(), retry_interval: Duration::ZERO, max_retries: 1, wait_poll: Duration::ZERO, acquire_deadline: Duration::ZERO };
        assert!(pool.leased_to(ProcId::current()).is_empty(), "release_owned_by finds nothing to stop");
        assert_eq!(fx.fp.live_vms().len(), 1);
    }

    #[test]
    fn a_salvage_whose_pull_fails_is_none_not_zero() {
        let fx = fixture();
        let remote = FakeRemote::green();
        remote.fail_result_pulls.store(1, std::sync::atomic::Ordering::SeqCst);
        let dest = fx.cfg.run_dir.join("salvaged");
        let scratch = fx.cfg.state_dir.join(".salvage-fail");
        assert_eq!(salvage_results(&remote, "addr", &dest, &scratch, Duration::from_secs(5)), None);
        assert_eq!(salvage_results(&remote, "addr", &dest, &scratch, Duration::from_secs(5)), Some(2), "the re-pull recovers every result");
    }

    #[test]
    fn a_missing_local_file_mid_stream_does_not_abort_the_rest() {
        let d = TempDir::new();
        let pulled = d.path().join("pulled/KEY");
        fs::create_dir_all(&pulled).unwrap();
        for n in ["a", "b", "c"] {
            fs::write(pulled.join(format!("test-{n}.sh.out")), "o").unwrap();
            fs::write(pulled.join(format!("test-{n}.sh.result")), "ok 1 1 - p e 0\n").unwrap();
        }
        fs::write(pulled.join("test-b.sh.out.streaming"), "").unwrap();
        fs::create_dir(pulled.parent().unwrap().join("x")).unwrap();
        let out = d.path().join("out");
        fs::create_dir_all(out.join("test-b.sh.out.streaming")).unwrap();
        assert_eq!(stream_into(&d.path().join("pulled"), &out).unwrap(), 2, "a and c land; b's blocked write is skipped");
        assert!(out.join("test-c.sh.result").is_file());
    }

    #[test]
    fn a_run_publishes_its_progress_and_leaves_it_done_with_the_verdict() {
        let fx = fixture();
        let remote = FakeRemote::green();
        let a = RunArgs { results_dir: Some(fx.d.path().join("results")), suites: Some("test-a.sh,test-b.sh".into()), ..tree(&fx) };
        assert_eq!(go(&fx, &remote, &a), 0);
        let v: Value = serde_json::from_str(&fs::read_to_string(fx.cfg.run_dir.join("round-progress.json")).unwrap()).unwrap();
        assert_eq!((v["phase"].as_str(), v["verdict"].as_str()), (Some("done"), Some("green")));
        assert_eq!((v["done"].as_u64(), v["total"].as_u64(), v["cap"].as_u64()), (Some(2), Some(2), Some(1234)));
        assert!(v["suites_started"].as_u64().is_some());
        let seq: Vec<&str> = v["phases"].as_array().unwrap().iter().map(|e| e["phase"].as_str().unwrap()).collect();
        assert_eq!(seq, ["vm", "build", "suites", "salvage", "done"], "a pass through the fake pool walks every phase in order");
        let starts: Vec<u64> = v["phases"].as_array().unwrap().iter().map(|e| e["started"].as_u64().unwrap()).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
        assert_eq!(v["phase_started"], *starts.last().unwrap());
    }

    #[test]
    fn a_run_whose_ssh_fails_is_left_done_with_a_fault() {
        let fx = fixture();
        let remote = FakeRemote { rc: Ok(255), ..FakeRemote::green() };
        let a = RunArgs { results_dir: Some(fx.d.path().join("results")), ..tree(&fx) };
        assert_eq!(go(&fx, &remote, &a), 2);
        let v: Value = serde_json::from_str(&fs::read_to_string(fx.cfg.run_dir.join("round-progress.json")).unwrap()).unwrap();
        assert_eq!((v["phase"].as_str(), v["verdict"].as_str()), (Some("done"), Some("fault")));
    }

    #[test]
    fn a_pass_whose_ssh_drops_records_itself_incomplete_on_its_batch() {
        let fx = fixture();
        let rec = FakeRecorder::default();
        let remote = FakeRemote { rc: Ok(255), ..FakeRemote::green() };
        let a = RunArgs { results_dir: Some(fx.d.path().join("results")), suites: Some("test-a.sh".into()), round_batch: Some("r-9".into()), ..tree(&fx) };
        assert_eq!(go_recorded(&fx, &remote, &a, &rec), 2);
        assert_eq!(*rec.calls.lock().unwrap().last().unwrap(), "incomplete r-9");
    }

    #[test]
    fn a_run_cut_before_its_final_pull_keeps_the_verdicts_already_produced() {
        let mut fx = fixture();
        fx.cfg.stream_every = Duration::ZERO;
        let remote = FakeRemote { hold_for_stream: true, rc: Ok(255), ..FakeRemote::green() };
        let a = RunArgs { results_dir: Some(fx.d.path().join("results")), ..tree(&fx) };
        assert_eq!(go(&fx, &remote, &a), 2);
        for s in ["test-a.sh", "test-b.sh"] {
            assert!(fx.d.path().join("results").join(format!("{s}.result")).is_file(), "{s} streamed before the cut");
        }
        assert_eq!(fx.fp.live_vms().len(), 1, "kept for a re-pull");
    }

    #[test]
    fn a_spool_run_whose_ssh_fails_writes_corpus_done_and_does_not_linger() {
        let fx = fixture();
        let remote = FakeRemote { rc: Ok(255), ..FakeRemote::green() };
        let (a, sp) = spool_fixture(&fx);
        fs::remove_file(sp.dir.join("close")).unwrap();
        assert_eq!(go(&fx, &remote, &a), 2);
        assert_eq!(fs::read_to_string(sp.dir.join("corpus.done")).unwrap(), "rc=2\n");
        assert_eq!(fx.fp.live_vms().len(), 1, "kept for a re-pull");
    }

    #[test]
    fn an_unclosed_spool_releases_the_vm_after_the_linger() {
        let mut fx = fixture();
        fx.cfg.attr_linger = Duration::from_millis(300);
        let remote = FakeRemote::green();
        let (a, sp) = spool_fixture(&fx);
        fs::remove_file(sp.dir.join("close")).unwrap();
        let t0 = Instant::now();
        assert_eq!(go(&fx, &remote, &a), 0);
        assert!(t0.elapsed() >= Duration::from_millis(300));
        assert!(fx.fp.live_vms().is_empty());
    }
}
