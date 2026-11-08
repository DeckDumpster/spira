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
    /// The tree's fences (`gate fences`, `name<TAB>command` lines), listed on the host before the
    /// VM is taken; never a command-line option.
    pub fences: Option<String>,
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
    /// The fences `tree`'s own gate definition runs (`gate fences`), their bins read from the
    /// VM's build: `name<TAB>command` lines.
    fn fences(&self, tree: &Path) -> Result<String, String>;
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
    /// `s3://<bucket>/<prefix>` the VM records each passed suite to (`SPIRA_TESTENV_RESUME`); a
    /// dead VM loses nothing, and the next pass for the tree reads the same prefix.
    pub resume_url: Option<String>,
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
    /// Where ssh's own diagnostics for this pass's connections are written.
    pub ssh_log: PathBuf,
    /// The mirror ref holding this run's tree, and the commit it must resolve to.
    pub round_ref: String,
    pub round_sha: String,
    /// Set: the VM runs [`FOREIGN_SCRIPT`] with this command rather than the harness round.
    pub ci_entry: Option<String>,
    /// The tree's fences, `name<TAB>command` lines, run after the lint when `lint` is set.
    pub fences: String,
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
    /// After the batch's ssh dropped: reconnects until the runner's exit code is recorded on the
    /// VM and returns it, or `None` when `within` passes first. `Err`: the VM cannot be reached again.
    fn await_exit(&self, addr: &str, within: Duration, ssh_log: &Path) -> Result<Option<i32>, String>;
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
pub const REMOTE_SCRIPT: &str = concat!(r#"set -euo pipefail
host_addr="$1" port="$2" suites="$3" maxpar="$4" toolchain="$5" cache_home="$6" registry="$7" setup_alarm="$8" cap="$9" post_guard="${10}" lint="${11}" build_image="${12}" round_ref="${13}" round_sha="${14}"
testenv_limits="${16:-}" resume_url="${17:-}" fences="${18:-}"
rm -rf ~/round-bins ~/round-launcher.env ~/round-setup.log ~/round-script.log
exec > >(tee -a ~/round-script.log)
script_out_tee=${!:-}
exec 2> >(tee -a ~/round-script.log ~/round-setup.log >&2)
script_err_tee=${!:-}
# Every exit leaves its phase and the script's log in the results, where the host names a pass
# that ended with no verdict from them (round-phase: the first phase that failed, else the last).
round_phase=start fault_phase="" fault_rc=""
phase() { round_phase="$1"; echo "round-vm: PHASE $1" >&2; }
phase_failed() {
    [ -n "$fault_phase" ] || { fault_phase="$1"; fault_rc="$2"; }
    echo "round-vm: PHASE-FAILED phase=$1 rc=$2" >&2
}
round_exit() {
    local rc=$? dir="$HOME/round-work/.runtime/spira/batch-results" i
    trap - EXIT
    exec >&- 2>&-
    for i in {1..20}; do
        { [ -n "$script_out_tee" ] && kill -0 "$script_out_tee" 2>/dev/null; } || { [ -n "$script_err_tee" ] && kill -0 "$script_err_tee" 2>/dev/null; } || break
        sleep 0.1
    done
    { mkdir -p "$dir" && cp -f ~/round-script.log "$dir/round-script.log" \
        && printf 'phase=%s rc=%s\n' "${fault_phase:-$round_phase}" "${fault_rc:-$rc}" > "$dir/round-phase"; } 2>/dev/null || true
    exit "$rc"
}
trap round_exit EXIT
phase preflight
[ -n "$testenv_limits" ] || { echo "round-vm: the host sent no testenv limits; a round never takes them from the tree's test fixture" >&2; exit 2; }
t_start=$(date +%s)
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin"
if ! command -v cargo >/dev/null 2>&1; then
    echo "round-vm: cargo not found on PATH ($PATH) — the template is missing a Rust toolchain" >&2
    exit 127
fi
phase checkout
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
disk_out="$HOME/round-work/.runtime/spira/batch-results/disk-usage"
mkdir -p "$(dirname "$disk_out")"
# Bytes used per directory, one line each, into the pass's results. A directory du cannot read
# whole is counted as a lower bound and says so; one it cannot read at all is `unread`, never a
# zero it did not measure, and the total then says it is incomplete. A measurement never ends the pass.
disk_report() {
    local phase="$1" label path bytes total=0 whole=yes graph
    graph="$(podman info --format '{{.Store.GraphRoot}}' 2>/dev/null || true)"
    for label in round-work tmpdir podman-graphroot cargo-target; do
        case "$label" in
            round-work) path="$HOME/round-work" ;;
            tmpdir) path="${TMPDIR:-/tmp}" ;;
            podman-graphroot) path="$graph" ;;
            cargo-target) path="$HOME/round-work/target" ;;
        esac
        if [ -z "$path" ]; then
            bytes=unread path="(podman named no graph root)"
        elif [ ! -d "$path" ]; then
            bytes=0
        elif ! bytes="$(du -sxB1 "$path" 2>/dev/null | cut -f1)"; then
            path="$path (du could not read all of it: a lower bound)"
        fi
        case "$bytes" in ''|*[!0-9]*) bytes=unread ;; esac
        if [ "$bytes" = unread ]; then whole=no; elif [ "$label" != cargo-target ]; then total=$(( total + bytes )); fi
        echo "round-vm: DISK $phase $label $bytes $path" | tee -a "$disk_out" >&2
    done
    [ "$whole" = yes ] || total="$total+unread"
    echo "round-vm: DISK $phase total $total -" | tee -a "$disk_out" >&2
}
# The pass's last report; coverage profiles go once the line report has read them, or once a red
# unit tier means none will.
disk_report_last() {
    disk_report "$1"
    find "$HOME/round-work/target" \( -name '*.profraw' -o -name '*.profdata' \) -delete 2>/dev/null || true
}
disk_report start
phase spira-env
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
phase toolchain
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
# sp-bt6ep3: a tree's test-binary sandbox (tools/test-sandbox/runner) runs every test binary under
# bwrap and refuses without it; an image built before the recipe carried bubblewrap has none,
# so the round installs it here, as it does a toolchain the image lacks.
command -v bwrap >/dev/null || { echo "round-vm: installing bubblewrap (the image has none)" >&2; DEBIAN_FRONTEND=noninteractive apt-get update -qq >&2 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq bubblewrap >&2; }
# sp-7dmcls: the suites run that sandbox inside rootful podman's container, as spirauser, and
# Ubuntu's restrict_unprivileged_userns moves an unprivileged userns creator into a profile
# that strips mount, so bwrap could make none of its own. The VM is the round's alone.
sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 >&2 || { echo "round-vm: cannot lift kernel.apparmor_restrict_unprivileged_userns; bwrap cannot sandbox a test in the container" >&2; exit 5; }
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
phase build
art_base="$SCCACHE_WEBDAV_ENDPOINT/artifacts"
"#, include_str!("../artifact-store.sh"), r#"art_tree="$(git -C ~/round-work rev-parse 'HEAD^{tree}')"
art_id="$(art_key "$art_tree" "$(rustc -vV | sed -n 's/^\(release\|commit-hash\): //p' | paste -sd' ')")"
t0=$(date +%s)
if art_fetch "$art_id" ~/round-work/target/release; then
    echo "round-vm: used the stored build of this tree; cargo did not run" >&2
else
    if ! cargo build -q --profile release --workspace --config profile.release.incremental=false; then
        echo "round-vm: the round's workspace build failed, built $sccache_mode" >&2
        tail -40 "$SCCACHE_ERROR_LOG" >&2 2>/dev/null || true
        exit 4
    fi
    art_store "$art_id" "$art_tree" "$(rustc -vV | sed -n 's/^\(release\|commit-hash\): //p' | paste -sd' ')" ~/round-work/target/release || true
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
phase unit-build
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
# The test binary a cargo log last started, by file name; empty before the first.
last_binary() {
    awk '/^ *Running / { if (match($0, /\(.*\)/)) b = substr($0, RSTART + 1, RLENGTH - 2); else b = $0 } END { sub(/.*\//, "", b); print b }' "$1"
}
# Bounds a test phase's job: past <secs> it is killed and named hung (<label>: HUNG guard=<guard>).
test_phase_bound() {
    local pid="$1" secs="$2" log="$3" hung="$4" label="$5" guard="$6" end running
    end=$(( $(date +%s) + secs ))
    while kill -0 "$pid" 2>/dev/null && [ "$(date +%s)" -lt "$end" ]; do sleep 2; done
    kill -0 "$pid" 2>/dev/null || return 0
    running="$(last_binary "$log")"
    echo "round-vm: $label: HUNG guard=$guard elapsed=${secs}s; binary=${running:-none-started}; killed" | tee -a "$hung" >&2
    pkill -TERM -P "$pid" 2>/dev/null; kill "$pid" 2>/dev/null
    return 0
}
json_str() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr -d '\000-\037'; }
# A red test phase's evidence: <out> (its cause, the failures and the log's tail) and <json>, the
# verdict as a record the host judges the pass from, never an exit code:
# {"stage","verdict":"red","ran_before_suites","cause","secs","last_binary","failed":[test names]}.
test_phase_red() {
    local stage="$1" log="$2" rc="$3" secs="$4" hung="$5" out="$6" json="$7" before="$8" cause last n first=1
    if [ "$rc" -gt 128 ]; then cause="killed by signal $(( rc - 128 ))"; else cause="exit status $rc"; fi
    [ ! -s "$hung" ] || cause="$cause, hung past its guard"
    last="$(last_binary "$log")"
    {
        echo "${stage^^}-CAUSE: $cause after ${secs}s; last binary=${last:-none-started}; failures=$(grep -c -E '^test .* FAILED|^test result: FAILED' "$log")"
        grep -E -A8 '^(test .* FAILED|---- |error(\[|:))|panicked at|^test result: FAILED|^failures:' "$log" | head -400
        echo "--- last output of the log ---"
        tail -100 "$log"
        [ ! -s "$hung" ] || { echo "round-vm: ${stage^^}: HUNG (test<TAB>binary):"; cat "$hung"; }
    } > "$out"
    {
        printf '{"stage":"%s","verdict":"red","ran_before_suites":%s,"cause":"%s","secs":%s,"last_binary":"%s","failed":[' \
            "$stage" "$before" "$(json_str "$cause")" "$secs" "$(json_str "$last")"
        while IFS= read -r n; do
            [ -n "$n" ] || continue
            [ -n "$first" ] || printf ','
            first=""
            printf '"%s"' "$(json_str "$n")"
        done < <(grep -oE '^test [^ ]+ \.\.\. FAILED' "$log" | awk '{print $2}' | sort -u | head -200)
        printf ']}\n'
    } > "$json"
}
phase release
# The round's one source of config: the tree's complete fixture, with this VM's own paths
# declared over it. Nothing here is searched for or defaulted: a tree's binaries build every
# child PATH from the declared release and `path`, never from this script's own PATH, so the
# VM's dolt, bd and cargo (outside the release) are declared here.
mkdir -p "$HOME/round-work/.runtime/spira" "$HOME/round-scratch"
cat > "$HOME/round-config.toml" <<ROUNDCFG
[spira]
run = "$HOME/round-work/.runtime/spira"   # where REMOTE_RESULTS pulls batch-results from
releases = "$HOME/round-releases"
path = "$HOME/spira-env/bin:$CARGO_HOME/bin:$HOME/.cargo/bin"
testenv_scratch_root = "$HOME/round-scratch"
home_repo = "spira"
batch_maxpar = $maxpar
$testenv_limits

# home_repo names the repo; this section says where it is on the VM and what its base is
# (origin/base, the mirror ref the lint already compares against). Without it testenv cannot
# resolve the base and every pass faults reason=base-ref.
[repo.spira]
path = "$HOME/round-work"
base = "origin/base"
ROUNDCFG
export SPIRA_TOML="$HOME/round-work/spira/testlib/complete.toml:$HOME/round-config.toml"
SPIRA_TOML="$(target/release/release config-stage "$HOME/round-work")" || { echo "round-vm: cannot apply the head's config delta" >&2; exit 2; }
export SPIRA_TOML
release_red() {
    echo "round-vm: RELEASE-VERIFY: RED at $1 — the head's release cannot be built and verified, so it could not land:" >&2
    grep -v '^ *$' ~/round-release.err | head -20 >&2
    exit 4
}
SPIRA_HOME="$HOME/round-work/spira" SPIRA_TOML="$SPIRA_TOML" target/release/spira-config validate > /dev/null 2> ~/round-release.err || release_red "config"
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
# The attribution reruns (spool.rs REMOTE_ATTR_SCRIPT) source this and run the batch, which
# refuses without the round's config: SPIRA_TOML rides with the release (sp-anhnd6).
printf 'export SPIRA_RELEASE=%q\nexport SPIRA_REPO=%q\nexport SPIRA_TOML=%q\nexport PATH=%q\n' "$SPIRA_RELEASE" "$SPIRA_REPO" "$SPIRA_TOML" "$PATH" > ~/round-launcher.env
echo "round-vm: launcher: SPIRA_RELEASE=$SPIRA_RELEASE" >&2
phase image
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
# sp-7dmcls: the test sandbox (tools/test-sandbox/runner) runs bwrap inside this image under
# rootful podman, as testenv starts it (unmasked /proc, AppArmor-unconfined). Proved here,
# before any suite, in bwrap's own words: a refusal now names its cause instead of a red suite.
if bwrap_probe="$(podman run --rm --security-opt 'unmask=/proc/*' --security-opt apparmor=unconfined "localhost/spira-testenv:$tag" runuser -u spirauser -- bwrap --unshare-user --unshare-pid --ro-bind / / --proc /proc true 2>&1)"; then
    echo "round-vm: BWRAP-PROBE: ok — bwrap made its namespaces and /proc inside localhost/spira-testenv:$tag as spirauser" >&2
else
    echo "round-vm: BWRAP-PROBE-FAILED: bwrap cannot sandbox a test inside localhost/spira-testenv:$tag: $bwrap_probe" >&2
    exit 3
fi
setup_secs=$(( $(date +%s) - t_start - unit_build_secs ))
if [ -n "$registry" ]; then export SPIRA_TESTENV_REGISTRY="$registry"; fi
disk_report build
# Each passed suite is recorded to the resume prefix as it passes, so a VM that dies loses none.
# A prefix the VM cannot write is named here, once, with the cause: a round that records nothing
# otherwise looks the same as one that recorded everything until its re-run skips nothing.
if [ -z "$resume_url" ]; then
    echo "round-vm: RESUME-OFF: no resume prefix is configured (round_vm_resume_url); a re-run of this tree runs every suite" >&2
else
    if [ -d "$HOME/round-resume-aws" ]; then
        export AWS_SHARED_CREDENTIALS_FILE="$HOME/round-resume-aws/credentials"
        [ ! -f "$HOME/round-resume-aws/config" ] || export AWS_CONFIG_FILE="$HOME/round-resume-aws/config"
    fi
    resume_probe() {
        local obj="${resume_url%/}/.probe" cp="aws s3 cp --only-show-errors --cli-connect-timeout 5 --cli-read-timeout 15"
        printf probe | $cp - "$obj" || return 1
        [ "$($cp "$obj" -)" = probe ] || { echo "the probe object was written but does not read back" >&2; return 1; }
    }
    if probe_err="$(resume_probe 2>&1)"; then
        export SPIRA_TESTENV_RESUME="$resume_url"
        echo "round-vm: RESUME: recording passed suites to $resume_url" >&2
    else
        echo "round-vm: RESUME-FAILED: the resume prefix $resume_url cannot be written from this VM, so a re-run of this tree skips nothing: ${probe_err//$'\n'/ }" >&2
    fi
fi
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
# A finding's culprit: the first member whose diff touches the flagged file, else the first whose
# diff changes the finding's rule or its allow list (the finding exists only because of it), else the base.
lint_culprit() {
    local tree="$1" finding="$2" rule path src c subj who a wanted
    local -a rule_paths=()
    rule="${finding%%: *}"
    path="${finding#*: }"; path="${path%%:*}"
    src="$(grep -lF -- "const NAME: &str = \"$rule\";" "$tree"/spira-lint/src/rules/*.rs 2>/dev/null | head -1)"
    if [ -n "$src" ]; then
        rule_paths+=("${src#"$tree"/}")
        while IFS= read -r a; do rule_paths+=("$a"); done < <(grep -oE '"spira[a-z-]*/[^"]*-allow[^"]*"' "$src" | tr -d '"' | sort -u)
    fi
    lint_touches() {
        local f want
        while IFS= read -r f; do
            for want in "$@"; do
                [ "$f" = "$want" ] || [ "${f#"$want"/}" != "$f" ] && return 0
            done
        done < <(git -C "$tree" diff --name-only "$c^1" "$c")
        return 1
    }
    for wanted in file rule; do
        for c in $(git -C "$tree" rev-list --first-parent --reverse origin/base..HEAD); do
            if { [ "$wanted" = file ] && lint_touches "$path"; } || { [ "$wanted" = rule ] && [ "${#rule_paths[@]}" -gt 0 ] && lint_touches "${rule_paths[@]}"; }; then
                subj="$(git -C "$tree" log -1 --format=%s "$c")"
                who="$(printf '%s\n' "$subj" | sed -n 's/.*merge \([^ ]*\) .*/\1/p')"
                echo "${who:-commit ${c:0:12} $subj}"
                return 0
            fi
        done
    done
    echo "the base itself (no member's diff touches $path or its rule $rule)"
}
# THE ROUND'S LINT: the full spira-lint on the merged tree against the landing ref, before the suites.
# A hit is a red round, named for its culprit (lint_culprit). Recorded as a suite result after the suites, so a certifier reading results sees it.
phase lint
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
                echo "member=$(lint_culprit "$HOME/round-work" "$finding") $finding"
            done < ~/round-lint.out
            cat ~/round-lint.err
        } > ~/round-lint.report
        phase_failed lint "$lint_rc"
        echo "round-vm: LINT: RED (spira-lint rc=$lint_rc)" >&2
        head -60 ~/round-lint.report >&2
    fi
    echo "round-vm: LINT: ${lint_secs}s of the 900s cap (rc=$lint_rc)" >&2
fi
# THE TREE'S FENCES, on the merged tree: the host's `gate fences` list (name<TAB>command per line),
# each run in the tree root against the mirrored landing ref, one after another and all before the
# unit tier starts, and without the round's sccache: a fence that compiles (build-fence) must not
# load the server the unit tests' own cargo builds connect to. Each one's seconds go to the
# fence-times file; a red one is recorded as fence-<name>.result and makes the round red. A
# green one writes no .result: the top directory of the results holds .result files only when
# the pass ended before the suites.
fence_fail=""
fence_res="$HOME/round-work/.runtime/spira/batch-results"
fence_run() {
    local name="$1" cmd="$2" t0 frc
    t0=$(date +%s)
    ( cd "$HOME/round-work" && env -u RUSTC_WRAPPER SPIRA_GATE_BASE=origin/base SPIRA_GATE_BRANCH=HEAD bash -c "$cmd" ) < /dev/null > ~/round-fence-"$name".out 2>&1
    frc=$?
    mkdir -p "$fence_res"
    echo "$name $(( $(date +%s) - t0 )) $frc" >> "$fence_res/fence-times"
    if [ "$frc" -ne 0 ]; then
        echo "round-vm: FENCE $name: RED (rc=$frc)" >&2
        head -40 ~/round-fence-"$name".out >&2
        fence_fail="$fence_fail $name"
        phase_failed "fence-$name" "$frc"
        cp ~/round-fence-"$name".out "$fence_res/fence-$name.out"
        echo "red $(date +%s) $(( $(date +%s) - t0 )) - - - $frc" > "$fence_res/fence-$name.result"
    fi
}
phase fences
if [ -n "$lint" ]; then
    while IFS=$'\t' read -r fname fcmd; do
        [ -n "$fname" ] || continue
        fence_run "$fname" "$fcmd"
    done <<< "$fences"
fi
# LINT AND FENCES ARE LOW-PASS FILTERS: a red one ends the pass HERE, before any unit test or
# suite runs, so no validation work is spent on a tree they already refuse (per Ryan 2026-10-29).
if [ -n "$lint" ] && { [ "${lint_rc:-0}" -ne 0 ] || [ -n "$fence_fail" ]; }; then
    early="$HOME/round-work/.runtime/spira/batch-results"
    mkdir -p "$early"
    if [ "${lint_rc:-0}" -ne 0 ]; then
        cp ~/round-lint.report "$early/spira-lint.out" 2>/dev/null || cp ~/round-lint.out "$early/spira-lint.out"
        echo "red 1 ${lint_secs:-0} - - - $lint_rc" > "$early/spira-lint.result"
    fi
    echo "$(date +%s) verdict RED lint-or-fence" >> "$early/run.events"
    echo "round-vm: LINT/FENCE RED before the suites: no unit test or suite was run" >&2
    kill "$sampler_pid" 2>/dev/null
    exit 1
fi
# THE WORKSPACE'S OWN TESTS, once per round (per Ryan 2026-10-05: no suite invokes cargo), in the
# standard two phases (sp-cddf76). UNIT FIRST: the tests inside src/ (--lib --bins) run to the end
# BEFORE any suite. They are the cheapest red the round can find, so a red there ends the pass:
# unit.json records it and no suite is run. THEN THE INTEGRATION TESTS (every crate's tests/ and
# its doc-tests) run beside the suites, and a red there makes the round red: integration.json.
# No suite has run when either record is written, so both go at the top of the results, where
# the host pulls them by name (TEST_PHASE_FILES).
phase unit-tests
rdir="$HOME/round-work/.runtime/spira/batch-results"
unit_progress="$rdir/unit-progress"
mkdir -p "$rdir"
unit_lib_total=$(grep -c '^ *Executable unittests ' ~/round-unit-build.log)
: > ~/round-unit-tests.log
unit_hung="$HOME/round-unit-hung"
: > "$unit_hung"
unit_t0=$(date +%s)
unit_cargo --no-fail-fast --lib --bins > ~/round-unit-tests.log 2>&1 &
unit_pid=$!
tail -n +1 -f --pid="$unit_pid" ~/round-unit-tests.log | unit_stream "$unit_lib_total" "$unit_progress" &
stream_pid=$!
unit_hang_watch ~/round-unit-tests.log "$unit_hung" "$unit_pid" &
hang_pid=$!
test_phase_bound "$unit_pid" "$cap" ~/round-unit-tests.log "$unit_hung" UNIT-TESTS unit-cap
wait "$unit_pid"; unit_rc=$?
unit_secs=$(( $(date +%s) - unit_t0 ))
wait "$stream_pid"
kill "$hang_pid" 2>/dev/null; wait "$hang_pid" 2>/dev/null
[ ! -s "$unit_hung" ] || [ "$unit_rc" -ne 0 ] || unit_rc=1
echo "round-vm: UNIT-TESTS took ${unit_secs}s (--lib --bins, ${unit_lib_total} test binaries)" >&2
if [ "$unit_rc" -eq 0 ]; then
    echo "round-vm: UNIT-TESTS: PASS ($(grep -c '^test result: ok' ~/round-unit-tests.log) test binaries)" >&2
else
    phase_failed unit-tests "$unit_rc"
    echo "round-vm: UNIT-TESTS: FAIL (unit run rc=$unit_rc):" >&2
    grep -E -A3 '^(test .* FAILED|---- |error(\[|:))|panicked at' ~/round-unit-tests.log | head -120 >&2
    test_phase_red unit-tests ~/round-unit-tests.log "$unit_rc" "$unit_secs" "$unit_hung" "$rdir/unit-tests.out" "$rdir/unit.json" true
    echo "round-vm: UNIT-TESTS RED before the suites: no suite was run" >&2
    disk_report_last unit-red
    kill "$sampler_pid" 2>/dev/null
    exit 1
fi
integ_cargo() {
    local rc=0 doc_rc=0
    unit_cargo --no-fail-fast --test '*' || rc=$?
    unit_cargo --no-fail-fast --doc || doc_rc=$?
    [ "$rc" -ne 0 ] || rc=$doc_rc
    return "$rc"
}
integ_total=$(( unit_total - unit_lib_total ))
: > ~/round-integration-tests.log
integ_hung="$HOME/round-integration-hung"
: > "$integ_hung"
integ_t0=$(date +%s)
integ_end="$HOME/round-integration-end"
rm -f "$integ_end"
# The job stamps its own end: it is collected only after the suites, so the wait says nothing of its length.
( integ_cargo; r=$?; date +%s > "$integ_end"; exit "$r" ) > ~/round-integration-tests.log 2>&1 &
integ_pid=$!
tail -n +1 -f --pid="$integ_pid" ~/round-integration-tests.log | unit_stream "$integ_total" "$unit_progress" &
integ_stream_pid=$!
unit_hang_watch ~/round-integration-tests.log "$integ_hung" "$integ_pid" &
integ_hang_pid=$!
# THE CAP CLOCK runs from testenv's first suite to the bare lane's end: container setup, post-processing,
# the unit tier before it and the pulls are outside it, each under its own guard. testenv's "running N
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
runner_out="$HOME/round-runner.out"
: > "$runner_out"
cap_fired="$HOME/round-cap-fired"
rm -f "$suites_mark" "$cap_fired"
phase suites
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
    testenv --mode parallel --artifacts "$HOME/round-work/target/release" --suites "$suites" round > >(suites_mark_stream | tee -a "$runner_out") 2> >(suites_mark_stream | tee -a "$runner_out" >&2) &
else
    testenv --mode parallel --artifacts "$HOME/round-work/target/release" round > >(suites_mark_stream | tee -a "$runner_out") 2> >(suites_mark_stream | tee -a "$runner_out" >&2) &
fi
testenv_pid=$!
echo "$testenv_pid" > "$testenv_pidfile"
suites_cap_guard "$suites_mark" "$cap" "$cap_fired" "$testenv_pid" "$bare_pid" &
guard_pid=$!
wait "$testenv_pid"
rc=$?
[ "$rc" -eq 0 ] || phase_failed suites "$rc"
wait "$bare_pid"
bare_rc=$?
[ "$bare_rc" -eq 0 ] || phase_failed bare-suites "$bare_rc"
phase integration-tests
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
    echo "over-cap guard=suites-cap elapsed=${suites_clock}s cap=${cap}s — the suites and bare lane ran past the cap" > "$bare_rdir/round-fault-kind"
fi
if kill -0 "$integ_pid" 2>/dev/null; then echo "round-vm: INTEGRATION-TESTS: waiting (suites done, integration tests still running)" >&2; fi
test_phase_bound "$integ_pid" "$post_guard" ~/round-integration-tests.log "$integ_hung" INTEGRATION-TESTS integration-post
wait "$integ_pid"; integ_rc=$?
integ_secs=$(( $(cat "$integ_end" 2>/dev/null || date +%s) - integ_t0 ))
wait "$integ_stream_pid"
kill "$integ_hang_pid" 2>/dev/null; wait "$integ_hang_pid" 2>/dev/null
[ ! -s "$integ_hung" ] || [ "$integ_rc" -ne 0 ] || integ_rc=1
echo "round-vm: INTEGRATION-TESTS took ${integ_secs}s (tests/ and doc-tests, ${integ_total} test binaries, beside the suites)" >&2
if [ "$integ_rc" -eq 0 ]; then
    echo "round-vm: INTEGRATION-TESTS: PASS ($(grep -c '^test result: ok' ~/round-integration-tests.log) test binaries)" >&2
else
    phase_failed integration-tests "$integ_rc"
    echo "round-vm: INTEGRATION-TESTS: FAIL (integration run rc=$integ_rc):" >&2
    grep -E -A3 '^(test .* FAILED|---- |error(\[|:))|panicked at' ~/round-integration-tests.log | head -120 >&2
    test_phase_red integration-tests ~/round-integration-tests.log "$integ_rc" "$integ_secs" "$integ_hung" "$rdir/integration-tests.out" "$rdir/integration.json" false
    [ "$rc" -eq 0 ] && rc=1
fi
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
phase post
disk_report_last suites
suites_secs=$suites_clock
kill "$sampler_pid" 2>/dev/null; wait "$sampler_pid" 2>/dev/null
echo "round-vm: setup ${setup_secs}s, suites ${suites_secs}s" >&2
if [ "$setup_secs" -gt "$setup_alarm" ]; then
    echo "round-vm: SETUP-SLOW: setup took ${setup_secs}s, over the ${setup_alarm}s limit — something is being built or fetched that the template should hold" >&2
fi
if find "$HOME/round-work/.runtime/spira/batch-results" -name run.events -exec grep -l ' verdict FAULT' {} + >/dev/null 2>&1; then
    { systemctl list-units --all --no-pager; systemctl --user list-units --all --no-pager; systemctl --failed --no-pager; } > ~/round-fault-units.txt 2>&1 || true
fi
mkdir -p ~/round-bins
find target/release -maxdepth 1 -type f -executable -exec cp {} ~/round-bins/ \; 2>/dev/null
if [ -n "$over_cap" ]; then exit 124; fi
exit "$rc"
"#);

/// What REMOTE_SCRIPT leaves on the VM for the attribution jobs: the launcher's
/// `SPIRA_RELEASE` and `PATH`, as `export` lines.
pub const LAUNCHER_ENV: &str = "round-launcher.env";

/// Where REMOTE_SCRIPT stages the round's executables, relative to the VM user's home.
pub const REMOTE_BINS: &str = "round-bins/";

/// The VM script's own stderr, kept so a round that never produced binaries can say why.
pub const FIXTURE_FAULT_FILE: &str = "fixture-fault.out";

pub const REMOTE_SETUP_LOG: &str = "round-setup.log";

/// The unit states the VM script snapshots when the batch ends in a FAULT.
pub const REMOTE_FAULT_UNITS: &str = "round-fault-units.txt";

/// What a faulted batch leaves in the results directory: the VM's setup log and unit states.
pub const BATCH_FAULT_FILE: &str = "batch-fault.log";

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

/// Writes the VM's whole setup log (where testenv logs the failing install step's command and
/// stderr) and its unit-state snapshot to `results_dir`/[`BATCH_FAULT_FILE`]; returns the path.
fn keep_fault_evidence(remote: &dyn Remote, cfg: &Config, addr: &str, scratch: &Path, results_dir: &Path) -> Option<PathBuf> {
    let mut body = String::new();
    for (name, title) in [(REMOTE_SETUP_LOG, "the VM's setup log"), (REMOTE_FAULT_UNITS, "unit states at the fault")] {
        let _ = fs::remove_dir_all(scratch);
        let got = pull_retry(remote, cfg, addr, name, scratch).then(|| fs::read_to_string(scratch.join(name)).ok()).flatten();
        body.push_str(&format!("==== {title} ({name}) ====\n{}\n", got.as_deref().unwrap_or("(could not be pulled)")));
    }
    let _ = fs::remove_dir_all(scratch);
    let path = results_dir.join(BATCH_FAULT_FILE);
    fs::create_dir_all(results_dir).and_then(|_| fs::write(&path, body)).map(|_| path).ok()
}

/// The Proxmox template's cache home is `SPIRA_ROUND_VM_CACHE_HOME`; an EC2 image records its
/// own, which the VM reads, so the template's path is never sent to it.
pub(crate) fn job_cache_home(ec2: bool, configured: Option<&str>) -> Option<String> {
    if ec2 { None } else { configured.map(str::to_string) }
}

/// The ssh session's side of a batch: it stages the script on the VM, starts it detached in its
/// own session, then follows its output and exits with its code. The script's exit code lands
/// in [`REMOTE_EXIT_FILE`] whichever way it ends, so a dropped session loses nothing.
pub const REMOTE_LAUNCHER: &str = r#"script="$HOME/round-script.sh"
out="$HOME/round-batch.out" err="$HOME/round-batch.err" exit_file="$HOME/round-exit"
rm -f "$exit_file" "$exit_file.tmp"
cat > "$script"
: > "$out"; : > "$err"
setsid nohup bash -c 'rc=0; bash "$@" </dev/null >"$HOME/round-batch.out" 2>"$HOME/round-batch.err" || rc=$?; printf "rc=%s\n" "$rc" > "$HOME/round-exit.tmp"; mv -f "$HOME/round-exit.tmp" "$HOME/round-exit"' runner "$script" "$@" </dev/null >/dev/null 2>&1 &
runner_pid=$!
tail -n +1 -F "$out" 2>/dev/null &
tail_out=$!
tail -n +1 -F "$err" >&2 2>/dev/null &
tail_err=$!
while [ ! -e "$exit_file" ]; do
    if ! kill -0 "$runner_pid" 2>/dev/null && [ ! -e "$exit_file" ]; then
        sleep 1
        [ -e "$exit_file" ] || { echo "round-vm: the runner died without recording an exit code" >&2; kill "$tail_out" "$tail_err" 2>/dev/null; exit 137; }
    fi
    sleep 1
done
sleep 1
kill "$tail_out" "$tail_err" 2>/dev/null
rc="$(sed -n 's/^rc=//p' "$exit_file")"
exit "${rc:-1}"
"#;

/// The runner's exit code on the VM, `rc=N`, relative to the VM user's home.
pub const REMOTE_EXIT_FILE: &str = "round-exit";

/// Where ssh's diagnostics for the batch's connections are kept, in the results dir.
pub const SSH_LOG_FILE: &str = "ssh-batch.log";

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
    args.push(job.resume_url.clone().unwrap_or_default());
    args.push(job.fences.clone());
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    format!("bash -c {} launcher {}", shell_quote(REMOTE_LAUNCHER), quoted.join(" "))
}

/// The last diagnostic line ssh wrote to `log`: why a connection ended with exit 255.
pub fn ssh_cause(log: &Path) -> String {
    fs::read_to_string(log)
        .ok()
        .and_then(|t| t.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(str::to_string))
        .unwrap_or_else(|| "ssh wrote no diagnostic".to_string())
}

/// How often [`SshRemote::await_exit`] looks, and how long it tolerates an unreachable VM.
const AWAIT_POLL: Duration = Duration::from_secs(5);
const AWAIT_UNREACHABLE: Duration = Duration::from_secs(120);

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
            "-o", "ServerAliveInterval=10", "-o", "ServerAliveCountMax=3",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

impl SshRemote {
    /// [`Self::ssh_opts`] with ssh's own diagnostics (a keepalive timeout is INFO) sent to `log`.
    fn ssh_logged_opts(&self, log: &Path) -> Vec<String> {
        let mut o = self.ssh_opts();
        for v in o.iter_mut() {
            if v == "LogLevel=ERROR" {
                *v = "LogLevel=INFO".to_string();
            }
        }
        o.extend(["-E".to_string(), log.to_string_lossy().into_owned()]);
        o
    }
}

fn rsync_said(stderr: &[u8]) -> String {
    let t = String::from_utf8_lossy(stderr);
    let t = t.trim();
    if t.is_empty() { "no output".to_string() } else { t.lines().collect::<Vec<_>>().join(" | ") }
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
        let _ = fs::create_dir_all(job.ssh_log.parent().unwrap_or(Path::new(".")));
        let _ = fs::remove_file(&job.ssh_log);
        let mut child = command("ssh")
            .args(self.ssh_logged_opts(&job.ssh_log))
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

    fn await_exit(&self, addr: &str, within: Duration, ssh_log: &Path) -> Result<Option<i32>, String> {
        let deadline = Instant::now() + within;
        let mut unreachable_since: Option<Instant> = None;
        loop {
            let out = command("ssh")
                .args(self.ssh_logged_opts(ssh_log))
                .arg(format!("{}@{addr}", self.user))
                .arg(format!("if [ -e \"$HOME/{REMOTE_EXIT_FILE}\" ]; then cat \"$HOME/{REMOTE_EXIT_FILE}\"; else exit 3; fi"))
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("ssh: {e}"))?;
            match out.status.code() {
                Some(0) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    return match text.trim().strip_prefix("rc=").and_then(|n| n.parse::<i32>().ok()) {
                        Some(rc) => Ok(Some(rc)),
                        None => Err(format!("{REMOTE_EXIT_FILE} holds {:?}, not rc=N", text.trim())),
                    };
                }
                Some(3) => unreachable_since = None,
                _ => {
                    let since = *unreachable_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= AWAIT_UNREACHABLE {
                        return Err(format!("ssh to {addr} cannot be restored: {}", ssh_cause(ssh_log)));
                    }
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(AWAIT_POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
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
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("rsync: {e}"))?;
        if st.status.success() {
            Ok(())
        } else {
            Err(format!("rsync to {remote_path} exited {}: {}", st.status.code().unwrap_or(-1), rsync_said(&st.stderr)))
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
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("rsync: {e}"))?;
        if st.status.success() {
            Ok(())
        } else {
            Err(format!("rsync {remote_path} exited {}: {}", st.status.code().unwrap_or(-1), rsync_said(&st.stderr)))
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

    fn fences(&self, tree: &Path) -> Result<String, String> {
        let o = command("gate")
            .arg("fences")
            .arg("--tree")
            .arg(tree)
            // path-ok: the VM's own workspace build, relative to the round tree the fences run in.
            .args(["--bin-dir", "target/release"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("gate fences: {e}"))?;
        if !o.status.success() {
            return Err(format!("gate fences exited {}: {}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
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

/// How the batch itself ended, from the last `verdict` line of `run.events` (in `dir` or one
/// subdirectory), with the last phase before it: `"FAULT install (died in phase up)"`.
pub fn batch_cause(dir: &Path) -> Option<String> {
    let text = result_dirs(dir).iter().find_map(|d| fs::read_to_string(d.join("run.events")).ok())?;
    let (mut phase, mut verdict) = (None, None);
    for f in text.lines().filter_map(|l| l.split_once(' ').map(|(_, r)| r)) {
        if let Some(p) = f.strip_prefix("phase ") {
            phase = Some(p.trim().to_string());
        } else if let Some(v) = f.strip_prefix("verdict ") {
            verdict = Some(v.trim().to_string());
        }
    }
    let verdict = verdict?;
    Some(match phase {
        Some(p) => format!("{verdict} (last phase {p})"),
        None => verdict,
    })
}

fn result_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    if let Ok(rd) = fs::read_dir(dir) {
        let mut subs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subs.sort();
        dirs.extend(subs);
    }
    dirs
}

/// How many suites testenv recorded as `unreached` in `dir`.
pub fn unreached_count(dir: &Path) -> usize {
    result_dirs(dir)
        .iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten().map(|e| e.path()).collect::<Vec<_>>())
        .filter(|p| p.extension().is_some_and(|x| x == "result"))
        .filter(|p| {
            fs::read_to_string(p).is_ok_and(|t| t.split_whitespace().next() == Some("unreached"))
        })
        .count()
}

fn suite_cap_s(dir: &Path) -> Option<String> {
    result_dirs(dir).iter().find_map(|d| {
        let t = fs::read_to_string(d.join("runner.meta")).ok()?;
        t.lines().find_map(|l| l.strip_prefix("suite_cap_s=").map(|v| v.trim().to_string()))
    })
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
pub fn install_bins(pulled: &Path, expected: &str, reported: Option<&str>, batch: Option<&str>, target: &Path) -> Result<(), String> {
    let exes = executables(pulled);
    if exes.is_empty() {
        return Ok(());
    }
    match reported {
        None => {
            let how = batch.map_or(String::new(), |b| format!(": batch verdict {b}"));
            return Err(format!("round-vm run: the VM reported no tested tree (batch did not complete{how}) — nothing installed"));
        }
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
    /// One red step of the running pass, so its mender is filed (`queue round failed`).
    fn failed(&self, batch: &str, repo: Option<&str>, step: &str) -> Result<(), String>;
    /// A red pass, naming its red steps.
    fn pass_red(&self, batch: &str, repo: Option<&str>, red: &[String]) -> Result<(), String>;
    fn progress(&self, batch: &str, body: &str) -> Result<(), String>;

    /// The batch's current pass number and members, for the pass's record; `None` when the
    /// lifecycle machine cannot say.
    fn pass_context(&self, batch: &str) -> Option<(u64, Vec<String>)> {
        let out = command("timeout").args(["-k", "5", "30", spira_config::lifecycle_row::lc_bin().ok()?.as_str(), "show-batch", batch]).stdin(Stdio::null()).output().ok()?;
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
        cmd.args(["-k", "5", "30", spira_config::lifecycle_row::lc_bin()?.as_str(), "batch-progress", batch]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
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
        cmd.args(["-k", "5", "60", spira_config::release_env::tool("queue")?.as_str(), "round", "pass-start", "--resume", batch]).args(repo).stdin(Stdio::null());
        let out = cmd.output().map_err(|e| format!("cannot run queue: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!("queue round pass-start refused for {batch} (rc={}): {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
    }

    fn pass_incomplete(&self, batch: &str, repo: Option<&str>, reason: &str) -> Result<(), String> {
        let mut cmd = command("timeout");
        cmd.args(["-k", "5", "60", spira_config::release_env::tool("queue")?.as_str(), "round", "pass-verdict", "--verdict", "incomplete", "--reason", reason, batch]).args(repo).stdin(Stdio::null());
        let out = cmd.output().map_err(|e| format!("cannot run queue: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!("queue round pass-verdict incomplete refused for {batch} (rc={}): {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
    }

    fn failed(&self, batch: &str, repo: Option<&str>, step: &str) -> Result<(), String> {
        let mut cmd = command("timeout");
        cmd.args(["-k", "5", "60", spira_config::release_env::tool("queue")?.as_str(), "round", "failed", "--suite", step, batch]).args(repo).stdin(Stdio::null());
        let out = cmd.output().map_err(|e| format!("cannot run queue: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!("queue round failed {step} refused for {batch} (rc={}): {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
    }

    fn pass_red(&self, batch: &str, repo: Option<&str>, red: &[String]) -> Result<(), String> {
        let mut cmd = command("timeout");
        cmd.args(["-k", "5", "60", spira_config::release_env::tool("queue")?.as_str(), "round", "pass-verdict", "--verdict", "red", "--red-suites", &red.join(","), batch]).args(repo).stdin(Stdio::null());
        let out = cmd.output().map_err(|e| format!("cannot run queue: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!("queue round pass-verdict red refused for {batch} (rc={}): {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim()))
    }

    fn suites_started(&self, batch: &str, repo: Option<&str>) -> Result<(), String> {
        let mut cmd = command("timeout");
        cmd.args(["-k", "5", "60", spira_config::release_env::tool("queue")?.as_str(), "round", "suites-started", batch]).args(repo).stdin(Stdio::null());
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

/// The suites boundary, the first time the pass is past its build. A refusal faults the pass (the
/// row cannot follow it), unless it is late or the pass is a ci_entry, whose certify writes it.
fn record_boundary(env: &RunEnv, args: &RunArgs, progress: &mut crate::progress::Progress, refused: &mut Option<String>) {
    let Some(batch) = args.round_batch.as_deref() else { return };
    publish_progress(env, args, progress);
    if progress.take_suites_began() {
        if let Err(e) = env.record.suites_started(batch, args.round_repo.as_deref()) {
            eprintln!("round-vm run: {e}");
            if args.ci_entry.is_none() && !e.contains("LateBoundary") {
                refused.get_or_insert(e);
            }
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

/// Seconds per fence from this pass's pulled `fence-times` (`<name> <secs> <rc>` lines); a file
/// older than the pass is a previous pass's and is not counted.
fn fence_seconds(results: &Path, started: u64) -> Vec<(String, u64)> {
    let path = results.join(FENCE_TIMES);
    let fresh = fs::metadata(&path).and_then(|m| m.modified()).is_ok_and(|t| t >= std::time::UNIX_EPOCH + Duration::from_secs(started));
    let text = if fresh { fs::read_to_string(&path).unwrap_or_default() } else { String::new() };
    let mut out: Vec<(String, u64)> = text
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.to_string(), f.next()?.parse().ok()?))
        })
        .collect();
    out.sort();
    out
}

/// What REMOTE_SCRIPT records each fence's seconds in, at the top of the results.
pub const FENCE_TIMES: &str = "fence-times";

/// What REMOTE_SCRIPT leaves at the top of the results when a test phase goes red (sp-cddf76):
/// each phase's verdict as a record and its failures' output — the unit phase's, before any
/// suite, and the integration phase's, beside them. They sit outside testenv's leaf, so the host
/// carries them over by name, as it does [`FENCE_TIMES`].
pub const TEST_PHASE_FILES: [&str; 4] = ["unit.json", "unit-tests.out", "integration.json", "integration-tests.out"];

/// What the VM script leaves at the top of the results on every exit: its phase marker and its log.
pub const SCRIPT_FILES: [&str; 3] = [crate::vm_fault::PHASE_FILE, crate::vm_fault::SCRIPT_LOG_FILE, crate::vm_fault::FAULT_KIND_FILE];

/// Carries the files the VM leaves outside testenv's leaf from `pulled` to `results_dir`, by name.
pub fn keep_top_files(pulled: &Path, results_dir: &Path) {
    for top in [FIXTURE_FAULT_FILE, FENCE_TIMES].into_iter().chain(TEST_PHASE_FILES).chain(SCRIPT_FILES) {
        if pulled.join(top).is_file() {
            let _ = fs::create_dir_all(results_dir);
            let _ = fs::copy(pulled.join(top), results_dir.join(top));
        }
    }
}

/// A failed exit that left no testenv verdict, no result and no test-phase record has no verdict;
/// a failed workspace build (4) is a verdict whose cause certify reads from the output.
pub fn verdict_missing(pulled: &Path, remote_rc: i32) -> bool {
    !matches!(remote_rc, 0 | 4)
        && results_leaf(pulled).is_none()
        && batch_cause(pulled).is_none()
        && TEST_PHASE_FILES.iter().all(|f| !pulled.join(f).exists())
}

/// A pass with no verdict is named from the VM script's own evidence, said aloud and kept as
/// [`crate::vm_fault::ROUND_FAULT_FILE`] in `results_dir` for certify.
pub fn settle_missing_verdict(pulled: &Path, results_dir: &Path, remote_rc: i32) -> Option<crate::vm_fault::RoundFault> {
    if !verdict_missing(pulled, remote_rc) {
        return None;
    }
    let fault = crate::vm_fault::name(results_dir);
    eprintln!("round-vm run: {} — the VM script's log, last {} lines:\n{}", fault.cause, crate::vm_fault::FAULT_TAIL, fault.tail);
    let path = results_dir.join(crate::vm_fault::ROUND_FAULT_FILE);
    if let Err(e) = fs::create_dir_all(results_dir).and_then(|_| fs::write(&path, fault.render())) {
        eprintln!("round-vm run: {}: {e}", path.display());
    }
    Some(fault)
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
        fences: fence_seconds(&args.results_dir.clone().unwrap_or_else(|| env.cfg.run_dir.join("batch-results")), started),
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
    let fenced;
    let args = if lints(args) {
        match env.host.fences(&args.tree_dir) {
            Ok(list) => {
                fenced = RunArgs { fences: Some(list), ..args.clone() };
                &fenced
            }
            Err(e) => {
                eprintln!("round-vm run: cannot list the tree's fences, so the round would judge nothing they guard: {e}");
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
    progress.place(&results_dir, place_of(&vm.handle));
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
        eprintln!("{}", held_vm_line(&vm.handle, &vm.addr));
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

/// The round is judged against a landing ref, so it runs the lint and the tree's fences.
fn lints(args: &RunArgs) -> bool {
    args.base.is_some() && args.ci_entry.is_none()
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

fn place_of(handle: &str) -> &'static str {
    if crate::ec2::is_ec2_handle(handle) {
        "ec2"
    } else {
        "proxmox"
    }
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
        lint: lints(args),
        build_image,
        round_ref: round_ref.to_string(),
        round_sha: commit_sha.to_string(),
        ssh_log: results_dir.join(SSH_LOG_FILE),
        ci_entry: args.ci_entry.clone(),
        resume_url: args.ci_entry.is_none().then(|| cfg.resume_url.clone()).flatten(),
        fences: args.fences.clone().unwrap_or_default(),
    };
    if let (Some(dir), None) = (&cfg.resume_credentials, &args.ci_entry) {
        if job.resume_url.is_some() && !crate::ec2::is_ec2_handle(&vm.handle) {
            push_resume_credentials(env.remote, &vm.addr, dir);
        }
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
                    stream_pull(env.remote, &vm.addr, &stream_scratch, &results_dir);
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
            Ok(255) if vm_lost(env, vm) => {
                lose_vm(env, args, &mut progress);
                progress.finish(&results_dir, VM_LOST_EXIT);
                return VM_LOST_EXIT;
            }
            Ok(255) => match recover_dropped_ssh(env, args, vm, &job, &mut progress, &results_dir, keep) {
                Some(rc) => rc,
                None => {
                    progress.finish(&results_dir, 2);
                    return 2;
                }
            },
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
                stream_pull(env.remote, &vm.addr, &stream_scratch, &results_dir);
                progress.update(&results_dir);
                record_boundary(env, args, &mut progress, &mut refused);
                last = Some(Instant::now());
            }
            server.serve(sc, false);
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = fs::remove_dir_all(&stream_scratch);
        let judged = match batch.join() {
            Ok(Ok(255)) if vm_lost(env, vm) => {
                lose_vm(env, args, &mut progress);
                Err(VM_LOST_EXIT)
            }
            Ok(Ok(255)) => recover_dropped_ssh(env, args, vm, &job, &mut progress, &results_dir, keep).ok_or(2),
            Ok(Ok(rc)) => Ok(rc),
            Ok(Err(e)) => {
                eprintln!("round-vm run: {e}");
                Err(2)
            }
            Err(_) => {
                eprintln!("round-vm run: the batch thread panicked");
                Err(2)
            }
        };
        let (code, usable) = match judged {
            Ok(rc) => {
                progress.update(&results_dir);
                record_boundary(env, args, &mut progress, &mut refused);
                progress.enter(&results_dir, "salvage", None);
                let code = after_batch(env, args, vm, mode, commit_sha, tree_sha, maxpar, rc, t0.elapsed().as_secs(), keep);
                (settle_boundary(env, args, &mut progress, &results_dir, &mut refused, code), true)
            }
            Err(code) => (code, false),
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

/// The batch's ssh returned 255 on a VM that is still running. Reconnects and waits, within the
/// pass's own budgets, for the runner's recorded exit code and returns it. `None`: the pass is
/// incomplete, the VM is kept, and the cause is on every report.
fn recover_dropped_ssh(env: &RunEnv, args: &RunArgs, vm: &Vm, job: &BatchJob, progress: &mut crate::progress::Progress, results_dir: &Path, keep: &AtomicBool) -> Option<i32> {
    let cause = ssh_cause(&job.ssh_log);
    let within = Duration::from_secs(job.cap_secs + job.post_guard_secs);
    match env.remote.await_exit(&vm.addr, within, &job.ssh_log) {
        Ok(Some(rc)) => {
            eprintln!("round-vm run: ssh to {} dropped ({cause}); reconnected and the runner had finished with exit {rc}", vm.addr);
            return Some(rc);
        }
        Ok(None) => eprintln!("round-vm run: ssh to {} failed (exit 255): {cause}; the runner recorded no exit within {}s", vm.addr, within.as_secs()),
        Err(e) => eprintln!("round-vm run: ssh to {} failed (exit 255): {cause}; {e}", vm.addr),
    }
    progress.marks().lose(&format!("ssh dropped: {cause}"));
    keep_after_lost_ssh(env, vm, results_dir, progress.total(), keep);
    record_incomplete(env, args, &format!("the batch's ssh dropped: {cause}"));
    None
}

/// `run`'s exit when the VM died under the pass: no verdict, nothing to re-pull, retry the pass.
pub const VM_LOST_EXIT: i32 = 75;

/// Whether the hypervisor says `vm` is no longer running; a provider that cannot answer says no.
fn vm_lost(env: &RunEnv, vm: &Vm) -> bool {
    (env.deps.factory)().ok().is_some_and(|a| matches!(a.provider.alive(&vm.handle), Ok(false)))
}

/// The VM is gone, so its lease is not kept: `run_pass` releases it on the way out.
fn lose_vm(env: &RunEnv, args: &RunArgs, progress: &mut crate::progress::Progress) {
    eprintln!("round-vm run: INCOMPLETE reason=vm-lost: the VM stopped running under the pass");
    progress.marks().lose("vm-lost");
    record_incomplete(env, args, "vm-lost");
}

/// One streaming pass: every verdict the VM has so far lands in `results_dir`, so a run cut
/// before its final pull keeps them.
fn stream_pull(remote: &dyn Remote, addr: &str, scratch: &Path, results_dir: &Path) {
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

/// A batch that ended before testing a tree: a RED cause is a red step (lint, a fence) and the
/// pass's verdict, naming the steps; any other cause, or a RED no red result names, is no verdict.
/// Each step is reported failed first: after the verdict a Failed is late and opens no mender.
fn record_treeless_end(env: &RunEnv, args: &RunArgs, cause: &str, results: &Path) {
    let red = red_steps(results);
    if !cause.starts_with("RED") || red.is_empty() {
        return record_incomplete(env, args, &format!("batch verdict {cause}"));
    }
    let Some(batch) = args.round_batch.as_deref() else { return };
    for step in &red {
        if let Err(e) = env.record.failed(batch, args.round_repo.as_deref(), step) {
            eprintln!("round-vm run: {e}");
        }
    }
    if let Err(e) = env.record.pass_red(batch, args.round_repo.as_deref(), &red) {
        eprintln!("round-vm run: {e}");
    }
}

/// The steps and suites whose result in `dir` (or one subdirectory) is `red`, sorted.
fn red_steps(dir: &Path) -> Vec<String> {
    let mut red: Vec<String> = result_dirs(dir)
        .iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten().map(|e| e.path()).collect::<Vec<_>>())
        .filter(|p| fs::read_to_string(p).is_ok_and(|t| t.split_whitespace().next() == Some("red")))
        .filter_map(|p| p.file_name()?.to_str()?.strip_suffix(".result").map(str::to_string))
        .collect();
    red.sort();
    red.dedup();
    red
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
fn keep_after_lost_ssh(env: &RunEnv, vm: &Vm, results_dir: &Path, expected: usize, keep: &AtomicBool) {
    keep_vm(env, vm, keep);
    let _pin = env.pool.pin_pull(&vm.handle);
    let scratch = env.cfg.state_dir.join(format!(".pulled-lost.{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    if pull_retry(env.remote, env.cfg, &vm.addr, REMOTE_RESULTS, &scratch) {
        let n = stream_into(&scratch, results_dir).unwrap_or(0);
        eprintln!("{}", incomplete_count_line(results_present(results_dir), expected, n));
        env.pool.mark_pulled(&vm.handle);
    } else {
        eprintln!("round-vm run: INCOMPLETE: the batch's ssh dropped and the results pull failed");
    }
    let _ = fs::remove_dir_all(&scratch);
    keep_runner_output(env.remote, env.cfg, &vm.addr, &env.cfg.state_dir.join(format!(".pulled-runner.{}", std::process::id())), results_dir);
    keep_launcher_output(env.remote, env.cfg, &vm.addr, &env.cfg.state_dir.join(format!(".pulled-launcher.{}", std::process::id())), results_dir);
}

fn incomplete_count_line(present: usize, expected: usize, new: usize) -> String {
    format!("round-vm run: INCOMPLETE: {present} of {expected} suite results present ({new} new in the final pull)")
}

pub fn held_vm_line(handle: &str, addr: &str) -> String {
    format!("round-vm run: INCOMPLETE: holding VM {handle} at {addr}; release it with `round-vm release {handle}`")
}

fn results_present(dir: &Path) -> usize {
    fs::read_dir(dir).map(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "result")).count()).unwrap_or(0)
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

/// The scoped credentials the VM reads the resume prefix with, pushed beside the round and
/// never passed on a command line.
pub const REMOTE_RESUME_CREDENTIALS: &str = "round-resume-aws/";

fn push_resume_credentials(remote: &dyn Remote, addr: &str, dir: &Path) {
    match remote.push(addr, dir, REMOTE_RESUME_CREDENTIALS) {
        Ok(()) => eprintln!("round-vm run: RESUME: credentials delivered to {addr}"),
        Err(e) => eprintln!("round-vm run: RESUME-FAILED: credentials for the resume prefix could not be delivered to {addr}: {e}"),
    }
}

/// The whole stdout and stderr of the VM's testenv run, kept with the round's results.
pub const REMOTE_RUNNER_OUT: &str = "round-runner.out";
pub const RUNNER_OUT_FILE: &str = "runner.out";

/// The launcher's capture of the script's stdout and stderr, kept with the results on every exit.
pub const LAUNCHER_FILES: [(&str, &str); 2] = [("round-batch.out", crate::vm_fault::LAUNCHER_OUT_FILE), ("round-batch.err", crate::vm_fault::LAUNCHER_ERR_FILE)];

fn keep_launcher_output(remote: &dyn Remote, cfg: &Config, addr: &str, scratch: &Path, results_dir: &Path) {
    for (remote_name, kept) in LAUNCHER_FILES {
        let _ = fs::remove_dir_all(scratch);
        let copied = pull_retry(remote, cfg, addr, remote_name, scratch)
            .then(|| fs::create_dir_all(results_dir).and_then(|_| fs::copy(scratch.join(remote_name), results_dir.join(kept))));
        match copied {
            Some(Ok(_)) => {}
            Some(Err(e)) => eprintln!("round-vm run: {kept}: {e}"),
            None => eprintln!("round-vm run: the VM's {remote_name} could not be pulled"),
        }
    }
    let _ = fs::remove_dir_all(scratch);
}

fn keep_runner_output(remote: &dyn Remote, cfg: &Config, addr: &str, scratch: &Path, results_dir: &Path) {
    let _ = fs::remove_dir_all(scratch);
    if pull_retry(remote, cfg, addr, REMOTE_RUNNER_OUT, scratch) {
        let kept = fs::create_dir_all(results_dir).and_then(|_| fs::copy(scratch.join(REMOTE_RUNNER_OUT), results_dir.join(RUNNER_OUT_FILE)));
        if let Err(e) = kept {
            eprintln!("round-vm run: {RUNNER_OUT_FILE}: {e}");
        }
    } else {
        eprintln!("round-vm run: the VM's runner output could not be pulled");
    }
    let _ = fs::remove_dir_all(scratch);
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
    if args.ci_entry.is_none() {
        keep_runner_output(remote, cfg, &vm.addr, &scratch("runner"), &results_dir);
    }
    let _ = fs::remove_file(results_dir.join(crate::vm_fault::FAULT_KIND_FILE));
    keep_top_files(&pulled, &results_dir);
    keep_launcher_output(remote, cfg, &vm.addr, &scratch("launcher"), &results_dir);
    if pulled_ok {
        settle_missing_verdict(&pulled, &results_dir, remote_rc);
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
    let cause = batch_cause(&results_dir);
    if cause.as_deref().is_some_and(|c| c.starts_with("FAULT")) {
        match keep_fault_evidence(remote, cfg, &vm.addr, &scratch("fault-evidence"), &results_dir) {
            Some(p) => eprintln!("round-vm run: the batch faulted; the VM's setup log and unit states are in {}", p.display()),
            None => eprintln!("round-vm run: the batch faulted and its evidence could not be written to {}", results_dir.display()),
        }
    }
    if reported.is_none() && remote_rc != 0 {
        if let Some(c) = &cause {
            eprintln!("round-vm run: the batch ended without a tested tree: {c}");
            record_treeless_end(env, args, c, &results_dir);
        }
    }
    if let Err(e) = install_bins(&bins, tree_sha, reported.as_deref(), cause.as_deref(), &target) {
        eprintln!("{e}");
        own_fault = true;
    }
    let _ = fs::remove_dir_all(&bins);

    if cause.as_deref().is_some_and(|c| c.starts_with("FAULT over-cap")) {
        let detail = format!(
            "FAULT over-cap guard=testenv-suite-cap cap={}s unreached={} — testenv's suite cap fired; no member is implicated",
            suite_cap_s(&results_dir).unwrap_or_else(|| "?".into()),
            unreached_count(&results_dir),
        );
        eprintln!("round-vm run: OVER-CAP: {detail}");
        if let Err(e) = crate::vm_fault::write_kind(&results_dir, crate::vm_fault::KIND_OVER_CAP, &detail) {
            eprintln!("round-vm run: {FAULT_KIND}: {e}", FAULT_KIND = crate::vm_fault::FAULT_KIND_FILE);
        }
        return 124;
    }
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
    fn a_pass_is_placed_on_ec2_exactly_when_its_handle_is_an_ec2_one() {
        assert_eq!(place_of(&format!("{}i-0abc", crate::ec2::HANDLE_PREFIX)), "ec2");
        assert_eq!(place_of("vm-9001"), "proxmox");
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
    fn the_round_installs_bubblewrap_before_any_test_binary_runs() {
        // sp-bt6ep3: a tree's test-binary sandbox (tools/test-sandbox/runner) refuses without bwrap,
        // and an image built before the recipe carried it has none.
        let install = REMOTE_SCRIPT.find("command -v bwrap >/dev/null || ").expect("the round installs bubblewrap when the image lacks it");
        let step = &REMOTE_SCRIPT[install..install + REMOTE_SCRIPT[install..].find('\n').unwrap()];
        assert!(step.contains("apt-get install -y -qq bubblewrap"), "{step}");
        assert!(install > REMOTE_SCRIPT.find("phase toolchain").unwrap(), "beside the toolchain install");
        assert!(install < REMOTE_SCRIPT.find("phase unit-tests").unwrap(), "before the unit stage");
    }

    #[test]
    fn the_round_lets_bwrap_run_in_the_rootful_test_container_and_proves_it_before_any_suite() {
        // sp-7dmcls: Ubuntu's restrict_unprivileged_userns strips mount from a bwrap the
        // container's user starts, and only a probe in the round's own image shows the VM
        // grants what the test sandbox needs.
        let sysctl = REMOTE_SCRIPT.find("sysctl -w kernel.apparmor_restrict_unprivileged_userns=0").expect("the round lifts the userns restriction");
        assert!(sysctl > REMOTE_SCRIPT.find("command -v bwrap >/dev/null || ").unwrap(), "beside the bubblewrap install");
        let probe = REMOTE_SCRIPT
            .find(r#"podman run --rm --security-opt 'unmask=/proc/*' --security-opt apparmor=unconfined "localhost/spira-testenv:$tag" runuser -u spirauser -- bwrap --unshare-user --unshare-pid --ro-bind / / --proc /proc true"#)
            .expect("the round probes bwrap inside its image");
        assert!(probe > REMOTE_SCRIPT.find("phase image").unwrap(), "after the image is present or built");
        assert!(probe < REMOTE_SCRIPT.find("phase unit-tests").unwrap(), "before any suite");
        let line = &REMOTE_SCRIPT[probe..probe + REMOTE_SCRIPT[probe..].find('\n').unwrap()];
        let after = &REMOTE_SCRIPT[probe..probe + 600];
        assert!(line.contains("2>&1"), "bwrap's stderr is captured: {line}");
        assert!(after.contains("BWRAP-PROBE-FAILED") && after.contains("$bwrap_probe") && after.contains("exit 3"), "a refused probe fails the round naming bwrap's words: {after}");
    }

    #[test]
    fn the_round_runs_the_bare_host_suites_on_the_vm_after_the_corpus() {
        let corpus = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        let bare = REMOTE_SCRIPT.find("spira/bare-suites.sh").unwrap();
        let integ_wait = REMOTE_SCRIPT.find("wait \"$integ_pid\"").unwrap();
        assert!(bare < corpus && corpus < integ_wait, "the bare lane starts before the corpus and is waited on after it");
        let lane = &REMOTE_SCRIPT[bare..integ_wait];
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
    fn the_remote_script_reports_bytes_used_per_directory_at_start_build_and_suites() {
        let a = REMOTE_SCRIPT.find("disk_report() {").expect("the disk report");
        let start = REMOTE_SCRIPT.find("disk_report start").unwrap();
        let func = &REMOTE_SCRIPT[a..start];
        let build = REMOTE_SCRIPT.find("disk_report build").expect("reported after the build");
        let suites = REMOTE_SCRIPT.find("disk_report_last suites").expect("reported after the suites");
        let batch = REMOTE_SCRIPT.find("testenv --mode parallel").unwrap();
        assert!(start < build && build < batch && batch < suites, "at start, after the build and after the suites");
        let red = REMOTE_SCRIPT.find("UNIT-TESTS RED before the suites").unwrap();
        let red_exit = red + REMOTE_SCRIPT[red..].find("exit 1").unwrap();
        assert!(REMOTE_SCRIPT[red..red_exit].contains("disk_report_last unit-red"), "a pass that ends at a red unit tier still reports its last usage");
        for (label, path) in [("round-work", "\"$HOME/round-work\""), ("tmpdir", "\"${TMPDIR:-/tmp}\""), ("podman-graphroot", "\"$graph\""), ("cargo-target", "\"$HOME/round-work/target\"")] {
            assert!(func.contains(&format!("{label}) path={path}")), "{label} is measured at {path}: {func}");
        }
        assert!(func.contains("[ \"$label\" != cargo-target ]; then total=$(( total + bytes ))"), "cargo-target lies inside round-work and is not added twice");
        assert!(func.contains("| tee -a \"$disk_out\" >&2"), "the stderr log and the results file carry the same lines");
        assert!(REMOTE_SCRIPT.contains("disk_out=\"$HOME/round-work/.runtime/spira/batch-results/disk-usage\""), "the lines land in the pass's results");
    }

    #[test]
    fn a_directory_du_cannot_read_is_never_a_zero_and_never_ends_the_pass() {
        let a = REMOTE_SCRIPT.find("disk_report() {").unwrap();
        let func = &REMOTE_SCRIPT[a..REMOTE_SCRIPT.find("disk_report start").unwrap()];
        assert!(func.contains("elif ! bytes=\"$(du -sxB1 \"$path\" 2>/dev/null | cut -f1)\"; then"), "du's failure is tested, so errexit cannot end the pass: {func}");
        assert!(func.contains("a lower bound"), "a partial read says it is partial");
        assert!(func.contains("''|*[!0-9]*) bytes=unread"), "no number read is unread, not 0");
        assert!(func.contains("bytes=unread path=\"(podman named no graph root)\""), "an unnamed graph root is unread, not 0");
        assert!(func.contains("[ \"$whole\" = yes ] || total=\"$total+unread\""), "an incomplete total says so");
        assert!(!func.contains("bytes=0 ;;"), "no fallback to zero");
    }

    #[test]
    fn coverage_profiles_are_deleted_only_after_the_line_report_has_read_them() {
        let report = REMOTE_SCRIPT.find("cargo llvm-cov report").expect("the line report");
        let last = REMOTE_SCRIPT.find("disk_report_last suites").expect("the pass's last report");
        assert!(report < last);
        let f = REMOTE_SCRIPT.find("disk_report_last() {").unwrap();
        assert!(REMOTE_SCRIPT[f..f + 300].contains("-name '*.profraw' -o -name '*.profdata' \\) -delete"), "profiles deleted with the last report");
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

    /// Before the launcher declares SPIRA_RELEASE (the release the round just built), the
    /// script runs only the steps that need no declared release: staging config, validating
    /// it, and building and verifying the release itself, whose children run from the stage.
    #[test]
    fn nothing_before_the_built_release_is_declared_needs_spira_release() {
        let declared = REMOTE_SCRIPT.find("export SPIRA_RELEASE=\"$HOME/round-releases/$rel_sha\"").expect("the launcher declares the built release");
        let allowed = ["target/release/release config-stage", "target/release/spira-config validate", "target/release/release build", "target/release/release verify"];
        let before = &REMOTE_SCRIPT[..declared];
        for (at, _) in before.match_indices("target/release/") {
            let step = &before[at..];
            assert!(allowed.iter().any(|a| step.starts_with(a)) || step.starts_with("target/release\""), "runs a binary before SPIRA_RELEASE is declared: {}", step.lines().next().unwrap_or(""));
        }
    }

    #[test]
    fn the_heads_config_delta_is_staged_before_the_release_build_reads_config() {
        let export = REMOTE_SCRIPT.find("export SPIRA_TOML=\"$HOME/round-work/spira/testlib/complete.toml:").unwrap();
        let stage = REMOTE_SCRIPT.find("release config-stage \"$HOME/round-work\"").expect("the delta must be applied");
        let build = REMOTE_SCRIPT.find("release build").unwrap();
        assert!(export < stage && stage < build);
    }

    #[test]
    fn the_round_validates_the_staged_config_then_builds_and_verifies_the_release_before_any_suite() {
        let stage = REMOTE_SCRIPT.find("release config-stage \"$HOME/round-work\"").unwrap();
        let validate = REMOTE_SCRIPT.find("target/release/spira-config validate").expect("the staged layers are validated by the head's own schema");
        assert!(
            REMOTE_SCRIPT.contains("SPIRA_HOME=\"$HOME/round-work/spira\" SPIRA_TOML=\"$SPIRA_TOML\" target/release/spira-config validate"),
            "the validate step names the head's home: a registry read at run time needs it"
        );
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

    #[test]
    fn the_build_is_skipped_on_a_hit_and_a_miss_stores_what_it_builds() {
        let fetch = REMOTE_SCRIPT.find("if art_fetch \"$art_id\"").expect("fetched before building");
        let build = REMOTE_SCRIPT.find("cargo build -q --profile release --workspace").unwrap();
        let store = REMOTE_SCRIPT.find("art_store \"$art_id\"").expect("a build is stored");
        assert!(fetch < build && build < store);
        assert!(REMOTE_SCRIPT[fetch..build].contains("\nelse\n"), "cargo runs only in the miss branch");
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
        let f = REMOTE_SCRIPT.find("cause=\"killed by signal").expect("the cause");
        let tail = &REMOTE_SCRIPT[f..];
        assert!(tail.contains("killed by signal") && tail.contains("${secs}s"), "{tail}");
        assert!(tail.contains("tail -100 \"$log\""), "the saved output ends with the log's own tail");
        assert!(!REMOTE_SCRIPT.contains("^test result:' ~/round-unit-tests.log | head"), "passing binaries no longer fill the head");
    }

    #[test]
    fn the_unit_run_is_watched_for_hung_tests_and_a_hang_makes_the_round_red() {
        let run = REMOTE_SCRIPT.find("unit_cargo --no-fail-fast").unwrap();
        let watch = REMOTE_SCRIPT.find("unit_hang_watch ~/round-unit-tests.log").unwrap();
        assert!(run < watch);
        assert!(REMOTE_SCRIPT.contains("[ ! -s \"$unit_hung\" ] || [ \"$unit_rc\" -ne 0 ] || unit_rc=1"));
        assert!(REMOTE_SCRIPT.contains("binary=${running:-none-started}; killed"), "the post-guard kill names the binary running");
    }

    #[test]
    fn the_fences_finish_before_the_unit_tier_starts() {
        let fences = REMOTE_SCRIPT.find("fence_run \"$fname\" \"$fcmd\"").expect("the fence loop");
        let early = REMOTE_SCRIPT.find("LINT/FENCE RED before the suites").unwrap();
        let units = REMOTE_SCRIPT.find("unit_cargo --no-fail-fast --lib --bins > ~/round-unit-tests.log 2>&1 &").expect("the unit tier's launch");
        assert!(fences < early && early < units, "the fences run in the foreground and the unit tier starts only after them");
        assert!(!REMOTE_SCRIPT[fences..fences + 80].contains('&'), "the fence loop is not backgrounded");
    }

    #[test]
    fn the_unit_tier_is_waited_for_before_the_bare_lane_or_any_suite_starts() {
        let launch = REMOTE_SCRIPT.find("unit_cargo --no-fail-fast --lib --bins > ~/round-unit-tests.log 2>&1 &").expect("the unit tier's launch");
        let waited = REMOTE_SCRIPT.find("wait \"$unit_pid\"; unit_rc=$?").expect("the unit wait");
        let early = REMOTE_SCRIPT.find("UNIT-TESTS RED before the suites: no suite was run").expect("the early exit");
        let bare = REMOTE_SCRIPT.find("bare_after_setup \"${bare_sel[@]}\"").expect("the bare lane's start");
        let corpus = REMOTE_SCRIPT.find("testenv --mode parallel").expect("the corpus's start");
        assert!(launch < waited && waited < early && early < bare && early < corpus, "the unit verdict is settled before any suite runs");
        assert_eq!(REMOTE_SCRIPT.matches("wait \"$unit_pid\"").count(), 1, "one unit wait, before the suites; none after them");
        assert!(!REMOTE_SCRIPT.contains("beside the suites, on the binaries"), "the unit phase no longer runs beside the suites");
        let integ = REMOTE_SCRIPT.find("( integ_cargo; r=$?;").expect("the integration phase's launch");
        let integ_wait = REMOTE_SCRIPT.find("wait \"$integ_pid\"; integ_rc=$?").expect("the integration wait");
        let stop = REMOTE_SCRIPT.find("if [ -e \"$suites_mark\" ]; then suites_clock=").unwrap();
        assert!(early < integ && integ < corpus && stop < integ_wait, "the integration phase starts once the unit phase passed and runs beside the suites");
    }

    #[test]
    fn the_unit_phase_runs_only_the_tests_inside_src_and_the_integration_phase_the_rest() {
        let f = REMOTE_SCRIPT.find("integ_cargo() {").unwrap();
        let integ = &REMOTE_SCRIPT[f..f + REMOTE_SCRIPT[f..].find("\n}\n").unwrap()];
        assert!(integ.contains("unit_cargo --no-fail-fast --test '*'") && integ.contains("unit_cargo --no-fail-fast --doc"), "{integ}");
        assert!(REMOTE_SCRIPT.contains("unit_lib_total=$(grep -c '^ *Executable unittests ' ~/round-unit-build.log)"));
        for line in ["round-vm: UNIT-TESTS took ${unit_secs}s", "round-vm: INTEGRATION-TESTS took ${integ_secs}s"] {
            assert!(REMOTE_SCRIPT.contains(line), "each phase says how long it took: {line}");
        }
    }

    #[test]
    fn base_is_parsed_and_asks_the_job_to_lint() {
        let a = parse_run_args(&["/t".into(), "--base".into(), "local/main".into()]).unwrap();
        assert_eq!(a.base.as_deref(), Some("local/main"));
        assert!(parse_run_args(&["/t".into()]).unwrap().base.is_none());
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
            resume_url: None,
            testenv_registry: Some("registry.example/spira".into()),
            setup_alarm_secs: 45,
            cap_secs: 900,
            testenv_limits: "testenv_suite_cap = 600\n".into(),
            post_guard_secs: 600,
            lint: false,
            build_image: false,
            ssh_log: PathBuf::from("ssh.log"),
            round_ref: "refs/rounds/7-abc".into(),
            round_sha: "abc".into(),
            ci_entry: Some("deploy/ci.sh all".into()),
            fences: String::new(),
        });
        assert_eq!(cmd, format!("bash -c {} launcher '10.0.0.1' '9430' '' '16' '1.82.0' '/opt/spira/cargo' 'registry.example/spira' '45' '900' '600' '' '' 'refs/rounds/7-abc' 'abc' 'deploy/ci.sh all' 'testenv_suite_cap = 600\n' '' ''", shell_quote(REMOTE_LAUNCHER)));
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
    fn the_round_config_declares_testenvs_scratch_root_outside_the_checkout() {
        let cfg = REMOTE_SCRIPT.find("cat > \"$HOME/round-config.toml\"").unwrap();
        let end = cfg + REMOTE_SCRIPT[cfg..].find("ROUNDCFG\nexport").unwrap();
        assert!(
            REMOTE_SCRIPT[cfg..end].contains("\ntestenv_scratch_root = \"$HOME/round-scratch\"\n"),
            "testenv refuses a round whose config leaves its scratch root undeclared"
        );
    }

    #[test]
    fn an_early_lint_red_exit_stops_the_sampler_that_holds_the_ssh_channel_open() {
        let red = REMOTE_SCRIPT.find("LINT/FENCE RED before the suites").expect("the early exit");
        let exit = red + REMOTE_SCRIPT[red..].find("exit 1").unwrap();
        assert!(REMOTE_SCRIPT[red..exit].contains("kill \"$sampler_pid\""), "a live background sampler keeps ssh waiting after the script exits");
    }

    #[test]
    fn the_round_config_places_the_home_repo_at_the_vm_checkout_with_its_base() {
        let start = REMOTE_SCRIPT.find("<<ROUNDCFG").unwrap();
        let end = start + REMOTE_SCRIPT[start..].find("\nROUNDCFG\n").unwrap();
        let cfg = &REMOTE_SCRIPT[start..end];
        let sec = cfg.find("[repo.spira]").expect("the round config declares where spira is on the VM");
        assert!(cfg[sec..].contains("path = \"$HOME/round-work\""), "{cfg}");
        assert!(cfg[sec..].contains("base = \"origin/base\""), "{cfg}");
    }

    /// The VM's own tool directories, declared: every child PATH a tree's binary builds is
    /// the declared release plus `spira.path`, never the script's inherited PATH, and the
    /// VM's dolt, bd and cargo live outside the release. The scratch root is declared with it.
    #[test]
    fn the_round_config_declares_the_vms_toolchain_and_scratch_root() {
        let start = REMOTE_SCRIPT.find("<<ROUNDCFG").unwrap();
        let end = start + REMOTE_SCRIPT[start..].find("\nROUNDCFG\n").unwrap();
        let cfg = &REMOTE_SCRIPT[start..end];
        let spira = &cfg[..cfg.find("[repo.spira]").unwrap()];
        assert!(spira.contains("\npath = \"$HOME/spira-env/bin:$CARGO_HOME/bin:$HOME/.cargo/bin\"\n"), "{cfg}");
        assert!(spira.contains("\ntestenv_scratch_root = \"$HOME/round-scratch\"\n"), "{cfg}");
        let mk = REMOTE_SCRIPT.find("mkdir -p \"$HOME/round-work/.runtime/spira\" \"$HOME/round-scratch\"").expect("the scratch root is made before the config names it");
        assert!(mk < start, "the scratch root exists before any tree binary reads the config");
    }

    /// The rendered round config parses as strict TOML: release config-stage on the VM refuses
    /// a duplicate key, so two writers landing the same line kill every round before its suites.
    #[test]
    fn the_rendered_round_config_parses_as_strict_toml_with_each_key_once() {
        let start = REMOTE_SCRIPT.find("<<ROUNDCFG\n").unwrap() + "<<ROUNDCFG\n".len();
        let end = start + REMOTE_SCRIPT[start..].find("\nROUNDCFG\n").unwrap();
        let rendered = REMOTE_SCRIPT[start..end]
            .replace("$maxpar", "16")
            .replace("$testenv_limits", "testenv_suite_cap = 600")
            .replace("$HOME", "/root")
            .replace("$CARGO_HOME", "/opt/spira/cargo");
        assert!(!rendered.contains('$'), "every heredoc variable is rendered: {rendered}");
        let cfg: toml::Table = toml::from_str(&rendered).unwrap_or_else(|e| panic!("{e}\n{rendered}"));
        assert_eq!(cfg["spira"]["testenv_scratch_root"].as_str(), Some("/root/round-scratch"));
    }

    #[test]
    fn the_round_config_names_the_home_repo_not_its_path() {
        let scripts = [("run.rs", REMOTE_SCRIPT, "ROUNDCFG\nexport"), ("template.sh", include_str!("template.sh"), "TEMPLATECFG\nexport")];
        for (name, script, end) in scripts {
            let cfg = script.find("home_repo").unwrap_or_else(|| panic!("{name} declares home_repo"));
            let line = script[cfg..].lines().next().unwrap();
            assert!(script[cfg..].contains(end), "{name}");
            assert!(!line.contains('/') && !line.contains('$'), "a path as the repo name is an unsafe resume key segment ({name}): {line}");
        }
    }

    #[test]
    fn resume_credentials_reach_only_a_proxmox_vm_and_never_the_command_line() {
        let job = BatchJob {
            host_addr: "h".into(), mirror_port: 1, suites: None, maxpar: 1, toolchain: None, cache_home: None,
            testenv_registry: None, resume_url: Some("s3://bkt/pre".into()), setup_alarm_secs: 1, cap_secs: 1,
            testenv_limits: "x".into(), post_guard_secs: 1, lint: false, build_image: false,
            ssh_log: PathBuf::from("ssh.log"), round_ref: "r".into(), round_sha: "s".into(), ci_entry: None, fences: String::new(),
        };
        assert!(remote_command(&job).ends_with("'x' 's3://bkt/pre' ''"));
        assert!(!REMOTE_SCRIPT.contains("AWS_SECRET_ACCESS_KEY"), "the script never carries a key");
        let remote = FakeRemote::green();
        push_resume_credentials(&remote, "10.0.0.2", Path::new("/creds"));
        assert_eq!(*remote.pushes.lock().unwrap(), vec![(PathBuf::from("/creds"), REMOTE_RESUME_CREDENTIALS.to_string())]);
    }

    #[test]
    fn remote_script_checks_cargo_before_paying_for_the_clone() {
        let check = REMOTE_SCRIPT.find("command -v cargo").expect("checks for cargo");
        let clone = REMOTE_SCRIPT.find("git clone").expect("clones the mirror");
        assert!(check < clone, "cargo must be checked before the clone, not after it fails");
        let path_export = REMOTE_SCRIPT.find(r#"export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin""#).expect("puts cargo's install dirs on PATH");
        assert!(path_export < check, "PATH must be widened before the check that reads it");
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
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        exe_with(p, "x");
    }

    /// An executable-moded fixture written in-process: no test here runs it, so no forked
    /// child can hold its write descriptor across an exec (the ETXTBSY race testkit::write_exe
    /// exists for), and a unit test spawns no shell to make it.
    fn exe_with(p: &Path, body: &str) {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o755).open(p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn binaries_of_another_tree_are_refused_and_nothing_installed() {
        let d = TempDir::new();
        let (pulled, target) = (d.path().join("pulled"), d.path().join("wt/target/release"));
        exe(&pulled.join("fakebin"));
        let e = install_bins(&pulled, "expectedsha", Some("wrongsha"), None, &target).unwrap_err();
        assert!(e.contains("wrongsha") && e.contains("expectedsha"), "{e}");
        assert!(e.contains("refusing binaries"), "{e}");
        let none = install_bins(&pulled, "expectedsha", None, Some("FAULT install (last phase up)"), &target).unwrap_err();
        assert!(none.contains("batch verdict FAULT install (last phase up)"), "{none}");
        let none = install_bins(&pulled, "expectedsha", None, None, &target).unwrap_err();
        assert!(none.contains("reported no tested tree") && !none.contains("refusing") && !none.contains("<none"), "{none}");
        assert!(!target.exists());
    }

    #[test]
    fn the_batch_cause_is_its_last_verdict_and_the_phase_it_died_in() {
        let d = TempDir::new();
        let sub = d.path().join("key");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(batch_cause(d.path()), None);
        fs::write(sub.join("run.events"), "1 phase resolve\n2 phase up\n3 verdict FAULT install\n").unwrap();
        assert_eq!(batch_cause(d.path()).as_deref(), Some("FAULT install (last phase up)"));
    }

    #[test]
    fn binaries_of_this_tree_land_where_land_local_reads_them() {
        let d = TempDir::new();
        let (pulled, target) = (d.path().join("pulled"), d.path().join("wt/target/release"));
        exe(&pulled.join("fakebin"));
        fs::write(pulled.join("not-executable"), "x").unwrap();
        install_bins(&pulled, "expectedsha", Some("expectedsha"), None, &target).unwrap();
        assert!(target.join("fakebin").is_file());
        assert!(!target.join("not-executable").exists());
        install_bins(&d.path().join("absent"), "expectedsha", None, None, &target).unwrap();
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
    fn host_addresses_reads_ip_br_addr() {
        let out = "lo UNKNOWN 127.0.0.1/8 ::1/128\neth0 UP 192.168.15.174/22 fe80::1/64\n";
        let a = host_addresses(out);
        assert!(a.iter().any(|x| x == "192.168.15.174") && a.iter().any(|x| x == "127.0.0.1"));
        assert!(!a.iter().any(|x| x == "192.168.1.56"));
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
        fn fences(&self, tree: &Path) -> Result<String, String> {
            if tree.ends_with("fences-unreadable") { Err("gate.steps: unknown directive".into()) } else { Ok(FAKE_FENCES.into()) }
        }
    }

    const FAKE_FENCES: &str = "lifecycle-guard\texport SPIRA_GUARD_BIN=target/release/lifecycle-guard; \"$SPIRA_GUARD_BIN\" --gate .\nboundary\tbash spira/boundary.sh check\n";

    #[test]
    fn a_tree_whose_fences_cannot_be_listed_is_refused_before_a_vm_is_taken() {
        let fx = fixture();
        let remote = FakeRemote::green();
        let a = RunArgs { tree_dir: fx.d.path().join("fences-unreadable"), maxpar: Some(24), base: Some("main".into()), ..Default::default() };
        assert_eq!(go(&fx, &remote, &a), 2);
        assert!(remote.jobs.lock().unwrap().is_empty(), "no batch ran");
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
        /// the VM is stopped under the batch, as a hard-stop would
        kill_vm: Option<crate::testutil::FakeProvider>,
        /// the exit code the runner recorded on the VM, as a dropped session finds it
        recorded_exit: Option<i32>,
        /// what ssh logged for the batch connection before it dropped
        ssh_log_text: Option<&'static str>,
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
            files.insert(REMOTE_RUNNER_OUT, vec![(REMOTE_RUNNER_OUT, "batch: resumed: test-a.sh passed in run/attempt local\n")]);
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
                kill_vm: None,
                recorded_exit: None,
                ssh_log_text: None,
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
            if let Some(text) = self.ssh_log_text {
                fs::create_dir_all(job.ssh_log.parent().unwrap()).unwrap();
                fs::write(&job.ssh_log, text).unwrap();
            }
            if let Some(fp) = &self.kill_vm {
                for vm in fp.live_vms() {
                    crate::provider::Provider::stop(fp, &vm).unwrap();
                }
            }
            self.rc.clone()
        }
        fn await_exit(&self, _: &str, _: Duration, _: &Path) -> Result<Option<i32>, String> {
            Ok(self.recorded_exit)
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
                    exe_with(&p, body);
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
            resume_url: None,
            resume_credentials: None,
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

    #[derive(Default)]
    struct FakeRecorder {
        calls: Mutex<Vec<String>>,
        bodies: Mutex<Vec<(String, String)>>,
        reasons: Mutex<Vec<String>>,
        refuse: bool,
        /// The suites boundary is refused as late: the pass already has its verdict.
        late: bool,
        refuse_record: bool,
    }

    impl Recorder for FakeRecorder {
        fn failed(&self, batch: &str, _: Option<&str>, step: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("failed {batch} {step}"));
            Ok(())
        }
        fn pass_red(&self, batch: &str, _: Option<&str>, red: &[String]) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("red {batch} {}", red.join(",")));
            Ok(())
        }
        fn pass_started(&self, batch: &str, _: Option<&str>) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("start {batch}"));
            Ok(())
        }
        fn pass_incomplete(&self, batch: &str, _: Option<&str>, why: &str) -> Result<(), String> {
            self.reasons.lock().unwrap().push(why.to_string());
            self.calls.lock().unwrap().push(format!("incomplete {batch}"));
            Ok(())
        }
        fn suites_started(&self, batch: &str, repo: Option<&str>) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("{batch} {}", repo.unwrap_or("-")));
            if self.late {
                return Err(format!("queue round suites-started refused for {batch} (rc=1): refused: Some(LateBoundary {{ state: \"ATTRIBUTING (pass 3)\", event: \"SuitesStarted {{ n: 3 }}\", pass: 3 }})"));
            }
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

    #[test]
    fn the_round_flags_are_parsed() {
        let a = parse_run_args(&args(&["t", "--round-batch", "r-1", "--round-repo=sp"])).unwrap();
        assert_eq!((a.round_batch.as_deref(), a.round_repo.as_deref()), (Some("r-1"), Some("sp")));
    }

    fn tree(fx: &Fixture) -> RunArgs {
        RunArgs { tree_dir: fx.d.path().join("tree"), maxpar: Some(24), ..Default::default() }
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
        assert!(unit_wait < start && start < bare && bare < corpus && corpus < guard && guard < stop && stop < bins, "the unit tier is settled before the clock starts");
        assert!(REMOTE_SCRIPT[guard..stop].contains("\"$testenv_pid\" \"$bare_pid\""), "the lane runs under the same cap, beside the corpus");
        assert!(!REMOTE_SCRIPT[start..stop].contains("timeout -k 5 \"$cap\""), "no timer counts from before the first suite");
        assert!(REMOTE_SCRIPT.contains("round-vm: BARE-SUITES: PASS"));
        assert!(REMOTE_SCRIPT.contains("round-vm: OVER-CAP: guard=suites-cap elapsed=") && REMOTE_SCRIPT.contains("if [ -n \"$over_cap\" ]; then exit 124; fi"));
        assert!(REMOTE_SCRIPT.contains("> \"$bare_rdir/round-fault-kind\"") && REMOTE_SCRIPT.contains("echo \"over-cap guard=suites-cap"), "the script records the typed kind beside its stderr line");
        assert_eq!(crate::vm_fault::FAULT_KIND_FILE, "round-fault-kind");
        assert!(REMOTE_SCRIPT[..unit_wait].contains("test_phase_bound \"$unit_pid\" \"$cap\""), "the unit phase's wait has its own guard, outside the cap clock");
        assert!(REMOTE_SCRIPT[stop..bins].contains("test_phase_bound \"$integ_pid\" \"$post_guard\""), "the integration phase's wait after the suites has its own guard");
        assert_eq!(exit_for_remote(124), 124, "over-cap reaches the caller as itself");
    }

    #[test]
    fn the_vms_runner_output_is_kept_with_the_results() {
        let fx = fixture();
        let remote = FakeRemote::green();
        let results = fx.cfg.run_dir.join("kept");
        keep_runner_output(&remote, &fx.cfg, "addr", &fx.cfg.state_dir.join(".runner-test"), &results);
        let kept = fs::read_to_string(results.join(RUNNER_OUT_FILE)).unwrap();
        assert!(kept.contains("batch: resumed: test-a.sh"), "{kept}");
    }

    fn session_ending_after_the_build() -> FakeRemote {
        let mut log: Vec<String> = (1..=60).map(|i| format!("round-vm: setup line {i}")).collect();
        log.push("round-vm: built the round in 12s".into());
        log.push("round-vm: cannot apply the head's config delta".into());
        let log: &'static str = Box::leak(log.join("\n").into_boxed_str());
        let mut remote = FakeRemote::green();
        remote.rc = Ok(2);
        remote.files.insert(REMOTE_RESULTS, vec![(crate::vm_fault::PHASE_FILE, "phase=release rc=2\n"), (crate::vm_fault::SCRIPT_LOG_FILE, log)]);
        remote.files.insert("round-batch.err", vec![("round-batch.err", "release: config-delta.toml: TOML parse error at line 3\nround-vm: cannot apply the head's config delta\n")]);
        remote.files.insert("round-batch.out", vec![("round-batch.out", "")]);
        remote
    }

    #[test]
    fn a_session_that_ends_after_the_build_phase_is_a_named_fault_carrying_its_logs_tail() {
        let fx = fixture();
        let remote = session_ending_after_the_build();
        let pulled = fx.cfg.state_dir.join(".pulled-results-test");
        let results = fx.cfg.run_dir.join("faulted");
        remote.pull("addr", REMOTE_RESULTS, &pulled).unwrap();
        keep_top_files(&pulled, &results);
        keep_launcher_output(&remote, &fx.cfg, "addr", &fx.cfg.state_dir.join(".launcher-test"), &results);
        assert!(fs::read_to_string(results.join(crate::vm_fault::LAUNCHER_ERR_FILE)).unwrap().contains("TOML parse error"), "the launcher's stderr is kept");
        let fault = settle_missing_verdict(&pulled, &results, 2).expect("a pass with no verdict is a named fault");
        assert_eq!(fault.cause, "FAULT phase=release rc=2: round-vm: cannot apply the head's config delta");
        let kept = fs::read_to_string(results.join(crate::vm_fault::ROUND_FAULT_FILE)).unwrap();
        assert!(kept.starts_with("ROUND-FAULT: FAULT phase=release rc=2: "), "{kept}");
        assert!(kept.contains("round-vm: setup line 23\n") && !kept.contains("round-vm: setup line 22\n"), "the last 40 lines: {kept}");
        assert!(kept.contains("round-vm: built the round in 12s"), "{kept}");
    }

    #[test]
    fn an_over_cap_kind_the_vm_left_reaches_the_results_and_a_stale_one_does_not_survive_the_next_pull() {
        let fx = fixture();
        let mut remote = FakeRemote::green();
        remote.files.insert(REMOTE_RESULTS, vec![(crate::vm_fault::FAULT_KIND_FILE, "over-cap guard=suites-cap cap=900s\n")]);
        let pulled = fx.cfg.state_dir.join(".pulled-cap-test");
        let results = fx.cfg.run_dir.join("capped");
        remote.pull("addr", REMOTE_RESULTS, &pulled).unwrap();
        keep_top_files(&pulled, &results);
        let kind = fs::read_to_string(results.join(crate::vm_fault::FAULT_KIND_FILE)).unwrap();
        assert_eq!(kind, "over-cap guard=suites-cap cap=900s\n");
        let clean = fx.cfg.state_dir.join(".pulled-clean-test");
        FakeRemote::green().pull("addr", REMOTE_RESULTS, &clean).unwrap();
        let _ = fs::remove_file(results.join(crate::vm_fault::FAULT_KIND_FILE));
        keep_top_files(&clean, &results);
        assert!(!results.join(crate::vm_fault::FAULT_KIND_FILE).exists());
    }

    #[test]
    fn a_pass_with_a_verdict_or_a_build_red_names_no_fault() {
        let fx = fixture();
        let pulled = fx.cfg.state_dir.join(".pulled-green-test");
        FakeRemote::green().pull("addr", REMOTE_RESULTS, &pulled).unwrap();
        assert_eq!(settle_missing_verdict(&pulled, &fx.cfg.run_dir.join("green"), 1), None, "testenv's results are the verdict");
        let empty = fx.cfg.state_dir.join(".pulled-empty-test");
        fs::create_dir_all(&empty).unwrap();
        assert!(!verdict_missing(&empty, 0) && !verdict_missing(&empty, 4));
        fs::write(empty.join("unit.json"), "{}").unwrap();
        assert!(!verdict_missing(&empty, 1), "a unit red is its own record");
    }

    #[test]
    fn every_exit_of_the_remote_script_leaves_its_phase_and_its_log_in_the_results() {
        let s = REMOTE_SCRIPT;
        let tee = s.find("exec 2> >(tee -a ~/round-script.log ~/round-setup.log >&2)").expect("stderr is kept in the script log");
        let out = s.find("exec > >(tee -a ~/round-script.log)").expect("stdout too: testenv's VERDICT line is on it");
        let trap = s.find("trap round_exit EXIT").expect("an EXIT trap");
        let first_exit = s.find("exit 2;").unwrap();
        assert!(out < trap && tee < trap && trap < first_exit, "the log and the trap are in place before the first exit");
        let exit_fn = &s[s.find("round_exit() {").unwrap()..trap];
        assert!(exit_fn.contains("cp -f ~/round-script.log \"$dir/round-script.log\""), "{exit_fn}");
        assert!(exit_fn.contains("printf 'phase=%s rc=%s\\n' \"${fault_phase:-$round_phase}\" \"${fault_rc:-$rc}\" > \"$dir/round-phase\""), "{exit_fn}");
        assert!(exit_fn.contains("dir=\"$HOME/round-work/.runtime/spira/batch-results\""), "into the dir REMOTE_RESULTS pulls");
        assert_eq!(REMOTE_RESULTS, "round-work/.runtime/spira/batch-results/");
        let wait = s.find("wait \"$testenv_pid\"").unwrap();
        assert!(s[wait..].starts_with("wait \"$testenv_pid\"\nrc=$?\n[ \"$rc\" -eq 0 ] || phase_failed suites \"$rc\""), "testenv's refusal is the failed phase, not whatever ran after it");
        for p in ["checkout", "build", "release", "unit-tests", "suites"] {
            assert!(s.contains(&format!("\nphase {p}\n")), "phase {p} is marked");
        }
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
        let stop = REMOTE_SCRIPT.rfind("kill \"$sampler_pid\"").expect("and stopped");
        assert!(sampler < batch && batch < stop, "it brackets the suites");
        for f in ["vm-signals", "iowait_pct", "steal_pct", "io_psi_some_avg10", "disk_inflight", "tsd/vm-signals.jsonl"] {
            assert!(REMOTE_SCRIPT.contains(f), "{f}");
        }
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

    #[test]
    fn every_ssh_and_rsync_to_a_round_vm_carries_keepalives() {
        let opts = SshRemote { user: "u".into(), port: 22, key: "k".into() }.ssh_opts().join(" ");
        assert!(opts.contains("-o ServerAliveInterval=10 -o ServerAliveCountMax=3"), "{opts}");
    }

    #[test]
    fn an_empty_ssh_log_says_ssh_wrote_no_diagnostic() {
        let fx = fixture();
        let log = fx.d.path().join("ssh.log");
        assert_eq!(ssh_cause(&log), "ssh wrote no diagnostic");
        fs::write(&log, "\n").unwrap();
        assert_eq!(ssh_cause(&log), "ssh wrote no diagnostic");
    }

    #[test]
    fn a_dropped_pass_reports_one_count_and_never_promises_a_re_pull() {
        assert_eq!(incomplete_count_line(2, 2, 0), "round-vm run: INCOMPLETE: 2 of 2 suite results present (0 new in the final pull)");
        let held = held_vm_line("h-1", "10.0.0.5");
        assert!(held.contains("round-vm release h-1") && !held.contains("re-pull"), "{held}");
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


}
