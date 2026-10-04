# round-vm — design

Replaces `spira/round-vm.sh` and `spira/round-vm-provider-pve.sh` (sp-o3o6z;
`law-new-subsystems-are-rust`). The approved design is the brain wiki's
`projects/spira/designs/round-vm-2026-09-28.md`. This file is the crate's own contract:
what the program is for, what it promises, and the shape of every piece of data it handles.
The code is written against this document rather than ported from the bash, which is
where the six live bugs came from.

## 1. Intent

A round's full test corpus runs on a **dedicated VM cloned from the CI template**, not on
the host the live aeons share. The verdict, the per-suite results, the `--with-bins`
binaries and the suite telemetry come back to the host, to the places landing already
reads them. Landing, production and publish do not move.

A **pool of one** keeps a warm VM booted in advance, so a round does not pay for the
clone. A used VM is destroyed and never reused, so no cleanup step exists.

There is **no local mode**. If the hypervisor cannot produce a VM, the round waits and
retries, and the operator is alarmed once per outage. Every recorded round time therefore
means the same thing.

## 2. Contract

### 2.1 Commands (the interface callers depend on)

| command | stdout | exit |
|---|---|---|
| `round-vm teardown` | — | 0; stops the mirror daemon for the state dir (nothing running is success) |
| `round-vm acquire` | `<handle> <addr> <warm\|cold>` | 0 success; 1 gave up (only when `SPIRA_ROUND_VM_MAX_RETRIES` > 0) |
| `round-vm release <handle>` | nothing | 0 destroyed and verified gone; 1 failed; 2 usage |
| `round-vm run <tree-dir> [--suites CSV] [--maxpar N] [--toolchain V] [--results-dir D]` | the remote batch's own output | see 2.2 |
| `round-vm status` | exactly three lines: `ready: <handle> <addr>\|none`, `provisioning: pid <pid>\|none`, `outage: <reason>\|none` | 0 |
| `round-vm _provision-bg` | internal: the one background provision | 0 |
| `round-vm template <tree-dir>` | `<new-template-vmid> <image-ref>` | 0 built, verified a template; 1 failed (the half-built VM destroyed, or named if it could not be); 2 usage/preflight |

Every option also accepts the `--opt=value` spelling. An unknown verb or no verb exits 1.

`warm` means a ready VM was handed out without waiting. `cold` means `acquire` had to
wait for a VM, whether it provisioned one itself or waited on the provision in flight.

### 2.2 `run`

1. Preflight, each exiting **2** with a message naming what is wrong: `<tree-dir>` is not
   a git checkout (the path is named); `SPIRA_ROUND_VM_HOST_ADDR` is unset (the variable is
   named); `SPIRA_ROUND_VM_HOST_KEY` is unreadable (the path is named);
   `SPIRA_ROUND_VM_CACHE_HOME` is unset (sp-xjnzl's own follow-up, sp-xjnzl-2: this binary
   resolves its OWN config for the VM-side `CARGO_HOME`, never the caller's ambient
   `CARGO_HOME` — a systemd unit, unlike an interactive shell, never sets one, and a
   production sweep run that way faulted the moment it defaulted to the VM's own ambient
   `/root/.cargo`, which does not match where the template actually installed `sccache`;
   `law-a-binary-resolves-the-config-it-reads`).
2. Update the read-only bare mirror `<state>/mirror.git` so its `round` branch (and a
   symbolic `HEAD`) is `<tree-dir>`'s HEAD. Make sure an anonymous read-only `git daemon`
   serves `<state>` on `SPIRA_ROUND_VM_MIRROR_PORT`.
3. `acquire`. The VM is **leased to this process** from this point on.
4. Wait until ssh reaches the VM. On the VM: clone `git://<host-addr>:<port>/mirror.git`,
   then act as a **launcher** (runtime-is-a-release, sp-dvfea; the GitHub CI twin is
   sp-6cbna): `cargo build --profile release --workspace` (a failure exits 4, testenv's
   build-failure code), stage that build as a release with the round's own `release build
   <sha> --bin-dir target/release --releases ~/round-releases`, and set `SPIRA_RELEASE`,
   `SPIRA_REPO=~/round-work` (a release is not a checkout; without it testenv, run from the
   release, cannot resolve the base ref) and `PATH` **outright** (`$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:~/.cargo/bin:` the system
   directories), refusing (exit 2) unless `spira-config` then resolves into that release.
   Both are written to `~/round-launcher.env` for the attribution jobs (§2.2a), which source
   it. Without this every conf.sh load on the VM printed `spira-config: command not found`
   (conf.sh fails closed without it, sp-c7b85). Then `testenv --mode parallel --profile
   release [--suites CSV] round` by name from the release, with `SPIRA_BATCH_MAXPAR` and, if
   given, `RUSTUP_TOOLCHAIN`, and if configured `SPIRA_TESTENV_REGISTRY` (so a changed test-image tag costs the VM one pull rather than a full rebuild); then stage `target/release`'s executables into `~/round-bins/`.
5. rsync back `batch-results/` (flattened out of any one-level `BATCH_KEY` nesting into the
   results dir, default `$SPIRA_RUN/batch-results`), `tsd/*.jsonl` (merged into
   `$SPIRA_RUN/tsd`, tagged `ran_on`, `vcpus`, `maxpar`) and `round-bins/`.
6. Write `<state>/manifests/<tree-sha>.json` (schema 3.4).
7. Install the pulled executables into `<tree-dir>/target/release` — where `queue
   land-local --worktree <tree-dir>` reads them — **only if** the VM's `batch.meta`
   reports (`tree=`) this tree's own sha. A mismatch, or no report, installs nothing and
   names both shas.
8. Release the VM (destroy, verify gone).

Exit code: the remote `testenv-batch.sh`'s own code when it ran and was non-zero (1 red,
2/3 its harness faults, 4 `--with-bins` build failure), so a caller reads `round-vm run`
exactly as it would read `testenv-batch.sh`. Otherwise **2** if round-vm itself failed
(preflight, mirror, acquire, the VM never became reachable over ssh, ssh transport error
255, results missing after a green run, refused binaries), else 0.

**The exit set is closed: {0, 1, 2, 3, 4}** (sp-dp872). A remote exit outside testenv's own
contract — cargo's 101, git's 128, a signal — never reached a verdict and is round-vm's own
fault: **2**, with the remote code named on stderr. A panic anywhere in round-vm is also
**2**, never Rust's 101 (`cli::no_panic`); a panic during `run` first releases the run's VM
(G2). Scar: the first two rounds cut from the release ended "unexpected exit 101" — cargo's
code, because testenv had grown a second binary (`bd-meter`) and the remote `cargo run -p
testenv` could no longer choose one. Every remote `cargo run` names `--bin testenv`, and
testenv declares `default-run`.

### 2.2a `run --attr-spool <dir>` — streaming and attribution reruns (sp-hvtgs)

The batcher attributes a red round **while its corpus runs** (batcher-cut DESIGN.md §4). Two
additions to `run`, both active only when `--attr-spool <dir>` is given; without it `run` is
unchanged.

1. **Streaming.** While the remote batch runs, every `SPIRA_ROUND_VM_STREAM_SECS` (default 10)
   `run` pulls `round-work/.runtime/spira/batch-results/` and copies each suite's `.out` and then
   its `.result` (flattened out of the batch-key directory) into the results dir, so a
   `.result` there still means complete. The final pull is unchanged.
2. **The spool.** The VM stays leased after the corpus until the batcher is done with it:

| file | writer | content |
|---|---|---|
| `<dir>/req/<job>.req` | batcher (tmp + rename) | `branch=<ref in the tree-dir repo>`, `suites=<csv>`, `build=artifacts\|aeon\|round` |
| `<dir>/res/<job>/` | round-vm | the job's `<suite>.result`/`.out`, flattened; for `build=round` also `bins/` (the staged executables) and `batch.meta` |
| `<dir>/res/<job>.done` | round-vm (tmp + rename) | `rc=<the job's testenv exit code, 255 ssh, 2 round-vm's own failure>` |
| `<dir>/corpus.done` | round-vm (tmp + rename) | `rc=<what run would have exited with>` — written after results, tsd, manifest and binaries are in place |
| `<dir>/close` | batcher | present: finish in-flight jobs, release the VM, exit |

A request is served as soon as it appears, concurrently with the corpus and with other jobs:
the job's branch is fetched into the mirror as `refs/heads/attr-<job>`, then on the VM:
- `artifacts`: clone that ref into `~/attr/<job>` (objects shared with `~/round-work`) and run
  the round's own `~/round-work/target/release/testenv --artifacts ~/round-work/target/release
  --suites <csv>` there — no build;
- `aeon`: the same clone, `--profile aeon` (a debug build of that tree);
- `round`: waits for the corpus; checks the ref out in `~/round-work` itself and runs
  `cargo build -p testenv --bin testenv`, then that `testenv --profile release --suites <csv>` (incremental on
  the corpus's own target), then stages `target/release` into `~/round-bins/` as the corpus does.

Every job runs with `SPIRA_VERDICT_TTL=0` (a rerun is never a cache hit nor a refused repeat)
and `SPIRA_BATCH_RESULTS=~/attr-results/<job>`. After `corpus.done`, `run` keeps serving until
`close` exists and no job is in flight, or `SPIRA_ROUND_VM_ATTR_LINGER` seconds (default 3600)
pass, then releases the VM (G2 unchanged: a signal still releases it). The exit code is the
corpus's, as without the spool.

### 2.2b `template` — a round template whose test image is built, not loaded (sp-dvfea)

**Why.** A round's VM must hold `localhost/spira-testenv:<tag>` (the tag is the hash of the
Containerfile, the bd pin and deps.toml), or testenv builds the image on the VM before any
suite runs. Measured 2026-09-30 (sp-dp872's proof round): `up:1139` of a 1615 s round — a
cold `podman build` (a Go toolchain, bd compiled from source, a Rust toolchain). Template 108
did carry an image, but a **loaded** one (`podman load`) of an older tag: a loaded image has
no build cache, so when the tag moved — though only 4 of its 23 layers differ, all of them
from `COPY conf.sh`/`deps.toml` onward — the VM rebuilt all 23 from nothing. Shipping the
host's image instead (`podman save | ssh podman load`) was measured and rejected: the host's
disk is the contended resource (100% util, 2 s read waits during a round), and the 2 GB
stream ran past 17 minutes.

**What.** `round-vm template <tree-dir>` makes a new template from the current one
(`PVE_TEMPLATE_VMID`) whose image is **built on the template itself**, so the template keeps
podman's layer cache. A later closure change then rebuilds only the steps it touches — for a
`conf.sh`/`deps.toml`/doctor change, the last few layers, in seconds — and a round whose tag
equals the template's builds nothing. The cargo registry is warmed with the tree's
`Cargo.lock` too.

It **never repoints anything**: it prints the new VMID; switching `PVE_TEMPLATE_VMID` in
`pve.env` is the operator's infrastructure change, and so is destroying the old template.

1. Preflight (exit 2): `<tree-dir>` is a git checkout; `SPIRA_ROUND_VM_HOST_PUBKEY` and
   `SPIRA_ROUND_VM_HOST_KEY` are readable; `SPIRA_ROUND_VM_CACHE_HOME` is set (sp-xjnzl-2:
   the template it builds must agree with every round cloned from it on where `CARGO_HOME`
   lives, or §2.2's own preflight refuses every round the moment it is run the way
   production actually runs it — with no ambient `CARGO_HOME` to fall back to at all).
2. VMID: always the hypervisor's `/cluster/nextid`; the caller never chooses one (per Ryan 2026-10-04, sp-bb0l2).
3. **Linked** clone of `PVE_TEMPLATE_VMID` to it, named `round-template-<commit12>` — the
   same clone a round VM gets, so the old template's base disk stays referenced and cannot
   be destroyed while the new one exists. A full clone was tried first and abandoned: it
   copies the whole 250 GB disk (24% after 9 minutes) on the storage the Spira host's own
   disk shares, and the host's load went to 57 with its API calls timing out; the aborted
   clone also left a `lock: clone` placeholder that only root@pam can remove. It is never named
   `round-<vmid>`, so no `acquire`, `release` or reap can destroy it (G4).
4. Start, wait for the address, deliver the key (the same code as provision, §2.4), wait for
   ssh.
5. Stream `git archive <commit>` of the tree to `/root/template-work` on the VM, then run
   `TEMPLATE_SCRIPT` there: `cargo run -q --locked --release -p testenv -- container image`
   (the tree's own driver — the VM has the tree and cargo but no release, sp-s0e1k; builds
   with podman's default `--layers`, keeping the cache), `podman rmi` every other `localhost/spira-testenv:*` tag, `cargo
   fetch --locked`, then delete the work tree and `/root/.ssh/authorized_keys`. The script
   prints `image=<ref>`; a missing line is a failure.
6. Graceful shutdown (the guest flushes its image store), verify stopped, convert to a
   template, verify `template: 1` in its config.
7. Any failure from step 3 on destroys the VM — fenced on the exact name this run gave it —
   and reports the reason; a VM that will not die is named on stderr for the operator.

**Operator procedure** (infrastructure — an agent builds and proves, the operator switches):
1. `round-vm template <a checkout of local/main>` (the hypervisor assigns the VMID);
   stdout is `<vmid> <image-ref>`. About 17 minutes, a cold build with its heartbeat.
2. Prove it without touching production: `SPIRA_PVE_ENV=<a copy of pve.env naming the new
   VMID> SPIRA_ROUND_VM_STATE_DIR=<scratch> SPIRA_ROUND_VM_MIRROR_PORT=<a free port> round-vm
   run <tree> --suites test-batcher.sh`; the round's `phases:` line must show `up:` in seconds
   and the stderr `template image: ... present`. Release the scratch pool's ready VM after.
3. Switch: set `PVE_TEMPLATE_VMID=<vmid>` in `pve.env` (G7: the next acquire reads it; the
   ready VM already cloned from the old template is used once more).
4. The old template stays while the new one's linked base references it; destroying it is
   the operator's call and `qm destroy` refuses while a linked clone depends on it.

**How the operator knows to run it.** `REMOTE_SCRIPT` checks, before testenv starts, whether
the VM already holds the round's tag (`testenv container tag`, from the staged release), and says so on the round's stderr either way:
`round-vm: template image: <ref> present` or `... absent — this round builds it; refresh
the template with round-vm template`.

### 2.3 Guarantees

- **G1 Pool of one.** At most one ready VM and at most one provision in flight, ever.
  This is a property of the state type (`Option`, not a list), not a check.
- **G2 No leak.** Every VM round-vm clones is, at every moment, recorded in exactly one
  place in the pool state: the provision in flight, `ready`, a lease, or `doomed`. Every
  failure path after the clone destroys it. A VM whose owning process died (a killed `run`,
  a killed background provision) is destroyed by the next `acquire`. A `run` interrupted by
  SIGTERM, SIGINT or SIGHUP destroys its leased VM before exiting.
- **G3 Destroy is verified.** Destroy = stop, delete (purge), then poll until the VMID is
  absent from the node's VM list. A VM that is still listed is an error and stays in
  `doomed`, retried on every `acquire`.
- **G4 Destroy is fenced.** round-vm only ever destroys a VM named `round-<vmid>`. A
  handle naming anything else (the template, another client's VM that won a `nextid`
  race) is refused and never touched. The one other name it destroys is the
  `round-template-<commit12>` a `template` run gave its own half-built clone (§2.2b), and
  only from inside that run.
- **G5 No local mode.** No path runs the batch anywhere but the VM.
- **G6 One alarm per outage.** The first failure of an outage mails the operator
  (`$SPIRA_HOME/mail send <mailbox> --kind alert`, mailbox `SPIRA_ROUND_VM_MAIL_MAILBOX`,
  default `operator`). Later failures are silent until an acquire succeeds, which ends the
  outage.
- **G7 Nothing cached.** Config and `pve.env` are re-read on every attempt. A fixed
  `pve.env` is picked up by the next retry, never shadowed by the failure it caused.
- **G8 Binaries by tree sha.** Nothing is installed whose tree sha differs from the round
  head's own.

### 2.4 Key delivery (the live bugs)

The host's public key goes into the ssh user's `authorized_keys` through the qemu guest
agent, at an **absolute path**: `/root/.ssh/authorized_keys` for `root`, otherwise
`/home/<user>/.ssh/authorized_keys`. The steps are `guest-exec mkdir -p -m 700 <home>/.ssh`,
then `agent/file-write`, then `guest-exec chmod 600` (and `chown -R` for a non-root user).
The agent's working directory is not the user's home, so a relative path lands somewhere
sshd never reads (live bug 1).

`agent/file-write` takes `file` and `content`. Its `encode` flag defaults to true, which
means the API base64-encodes the content itself, so the content is sent **raw** and
neither `encode` nor `encoding` is sent (`encoding=base64` is an unknown parameter:
HTTP 400, live bug 2). `agent/exec` takes repeated `command` values and nothing else:
`capture-output` is an unknown property on this Proxmox.

### 2.5 Files

| path | owner | purpose |
|---|---|---|
| `<state>/pool.json` | round-vm | pool state (3.2), rewritten atomically (tmp + rename) under the lock |
| `<state>/lock` | round-vm | `flock` guarding every read-modify-write of `pool.json` |
| `<state>/provisioning.out` | round-vm | stdout/stderr of the background provision |
| `<state>/mirror.git` | round-vm | bare mirror the VM clones from |
| `<state>/git-daemon.pid` | git daemon | the mirror daemon's pid |
| `<state>/manifests/<tree-sha>.json` | round-vm | round result (3.4) |
| `<state>/.pulled-*.<pid>` | round-vm | scratch, removed after use |
| `$SPIRA_RUN/tsd/<family>.jsonl` | shared | VM rows appended, deduped by line under `<file>.lock` |
| `~/.config/spira/pve.env` (or `SPIRA_PVE_ENV`) | operator | Proxmox credentials (3.1), read only |
| `spira.toml` (`spira_config::discover`) | operator | `[spira] round_vm_*` keys, read only |

`<state>` is `SPIRA_ROUND_VM_STATE_DIR`, default `$SPIRA_RUN/round-vm`.

## 3. Schema

### 3.1 Configuration

Every key is looked up in the environment first, then in `spira.toml`'s `[spira]` table
through the `spira-config` crate (never parsed by hand), then defaulted.

```rust
pub struct Config {
    pub state_dir: PathBuf,          // SPIRA_ROUND_VM_STATE_DIR   / round_vm_state_dir  / $SPIRA_RUN/round-vm
    pub run_dir: PathBuf,            // SPIRA_RUN                  / run                 / required
    pub spira_home: Option<PathBuf>, // SPIRA_HOME (where mail lives)
    pub pve_env_path: PathBuf,       // SPIRA_PVE_ENV              / pve_env             / $XDG_CONFIG_HOME/spira/pve.env
    pub ssh_user: String,            // SPIRA_ROUND_VM_SSH_USER    / round_vm_ssh_user   / root
    pub ssh_port: u16,               // SPIRA_ROUND_VM_SSH_PORT    / round_vm_ssh_port   / 22
    pub host_key: PathBuf,           // SPIRA_ROUND_VM_HOST_KEY    / round_vm_host_key   / <state>/host_key
    pub host_pubkey: PathBuf,        // SPIRA_ROUND_VM_HOST_PUBKEY / round_vm_host_pubkey/ <host_key>.pub
    pub host_addr: Option<String>,   // SPIRA_ROUND_VM_HOST_ADDR   / round_vm_host_addr
    pub vcpus: u32,                  // SPIRA_ROUND_VM_VCPUS       / round_vm_vcpus      / 16
    pub maxpar: u32,                 // SPIRA_ROUND_VM_MAXPAR      / round_vm_maxpar     / 16
    pub retry_interval: Duration,    // SPIRA_ROUND_VM_RETRY_INTERVAL / round_vm_retry_interval / 60 s
    pub max_retries: u32,            // SPIRA_ROUND_VM_MAX_RETRIES / round_vm_max_retries/ 0 = forever
    pub acquire_deadline: Duration,  // SPIRA_ROUND_VM_ACQUIRE_DEADLINE secs / 3600; 0 = forever (env only)
    pub mirror_port: u16,            // SPIRA_ROUND_VM_MIRROR_PORT / round_vm_mirror_port/ 9430
    pub mailbox: String,             // SPIRA_ROUND_VM_MAIL_MAILBOX / operator
    pub net_iface: String,           // SPIRA_ROUND_VM_NET_IFACE   / ens18
    pub boot_tries: u32,             // SPIRA_ROUND_VM_BOOT_TRIES  / 60
    pub boot_poll: Duration,         // SPIRA_ROUND_VM_BOOT_POLL   / 2 s
    pub ssh_tries: u32,              // SPIRA_ROUND_VM_SSH_TRIES   / 30 (x boot_poll)
    pub wait_poll: Duration,         // how often acquire re-checks a provision in flight: 1 s
}

/// ~/.config/spira/pve.env — shell KEY=value lines; the file wins over the environment.
pub struct PveEnv {
    pub api_host: String,       // PVE_API_HOST, default localhost
    pub api_port: u16,          // PVE_API_PORT, default 8006
    pub node: String,           // PVE_NODE, required
    pub token_id: String,       // PVE_TOKEN_ID, required
    pub token_secret: String,   // PVE_TOKEN_SECRET, required
    pub cacert: PathBuf,        // PVE_CACERT, default /etc/pve/pve-root-ca.pem, must exist
    pub template_vmid: String,  // PVE_TEMPLATE_VMID, required
    pub pool: Option<String>,   // PVE_RUNNER_POOL
}
```

### 3.2 Pool state (`<state>/pool.json`)

```rust
#[derive(Serialize, Deserialize, Default)]
pub struct PoolState {
    pub ready: Option<Vm>,                 // G1: at most one
    pub provisioning: Option<Provisioning>,// G1: at most one
    pub leases: Vec<Lease>,                // VMs handed out and not yet released
    pub doomed: Vec<String>,               // handles whose verified destroy has not yet succeeded
    pub outage: Option<Outage>,            // G6: set by the first failure, cleared by success
}
pub struct Vm { pub handle: String, pub addr: String }
pub struct Provisioning { pub owner: ProcId, pub vmid: Option<String>, pub since: u64 }
pub struct Lease { pub vm: Vm, pub owner: Option<ProcId>, pub since: u64 } // None: handed to an external caller by `acquire`
pub struct Outage { pub reason: String, pub since: u64 }
/// A process identity that survives pid reuse: pid plus its start time from /proc/<pid>/stat.
pub struct ProcId { pub pid: u32, pub start: u64 }
```

### 3.3 Provider seam

```rust
pub trait Provider {
    fn next_id(&self) -> Result<String, String>;
    fn clone_to(&self, vmid: &str, name: &str) -> Result<(), String>;
    fn start(&self, vmid: &str) -> Result<(), String>;
    fn guest_addr(&self, vmid: &str, iface: &str) -> Result<Option<String>, String>;
    fn guest_exec(&self, vmid: &str, argv: &[&str]) -> Result<i32, String>;
    fn guest_file_write(&self, vmid: &str, path: &str, content: &str) -> Result<(), String>;
    fn alive(&self, vmid: &str) -> Result<bool, String>;
    fn stop(&self, vmid: &str) -> Result<(), String>;
    fn destroy(&self, vmid: &str) -> Result<(), String>;
    fn name_of(&self, vmid: &str) -> Result<Option<String>, String>; // None: the VMID is gone
    // `template` (§2.2b) only:
    fn shutdown(&self, vmid: &str) -> Result<(), String>;                // graceful, task-waited
    fn make_template(&self, vmid: &str) -> Result<(), String>;           // POST .../template
    fn is_template(&self, vmid: &str) -> Result<bool, String>;           // GET .../config: template == 1
}
```

Provision, key delivery and verified destroy are written once on top of this seam, so the
guarantees hold for every provider. The Proxmox provider speaks the HTTP API through a
`Transport` (`method, path, form -> data`), with TLS verified against `PVE_CACERT` and no
insecure fallback. Responses it reads, each wrapped in `{"data": ...}`:

| call | `data` |
|---|---|
| `GET /cluster/nextid` | `"123"` |
| `POST .../qemu/<t>/clone`, `.../status/start`, `.../status/stop`, `DELETE .../qemu/<id>` | a task UPID string |
| `GET /nodes/<n>/tasks/<upid>/status` | `{"status": "running"\|"stopped", "exitstatus": "OK"\|...}` |
| `GET .../qemu/<id>/status/current` | `{"status": "running"\|"stopped"}` |
| `GET /nodes/<n>/qemu` | `[{"vmid": 123, "name": "round-123", ...}]` |
| `GET .../agent/network-get-interfaces` | `{"result": [{"name": "ens18", "ip-addresses": [{"ip-address-type": "ipv4", "ip-address": "..."}]}]}` |
| `POST .../agent/exec` | `{"pid": 42}` |
| `GET .../agent/exec-status?pid=42` | `{"exited": 1\|true, "exitcode": 0}` |
| `POST .../agent/file-write` | `null` |
| `POST .../status/shutdown`, `POST .../template` | a task UPID string (or `null`) |
| `GET .../qemu/<id>/config` | `{"template": 1, ...}` (absent when not a template) |

### 3.4 Round result (`<state>/manifests/<tree-sha>.json`)

```rust
pub struct Manifest {
    pub tree_sha: String,
    pub tree_sha_found: Option<String>, // batch.meta `tree=` the VM's testenv reported
    pub commit_sha: String,
    pub vm: String,
    pub acquire: AcquireMode,           // "warm" | "cold"
    pub vcpus: u32,
    pub maxpar: u32,
    pub batch_wall_secs: u64,
    pub build_wall_secs: Option<u64>,   // runner.meta build_wall_s, present only when --with-bins built
    pub suite_wall_secs_sum: u64,       // sum of the third field of every <suite>.result
}
```

`<suite>.result` is testenv-batch.sh's line `<status> <epoch> <secs> <fp> <mode> <producer> <rc>`;
`runner.meta` is `key=value` lines. A tsd row is any JSON object; round-vm adds `ran_on`
(the VM handle), `vcpus` and `maxpar`.

## 4. Test strategy

Rust unit tests only, each well under a second, against a fake provider, a fake Proxmox
transport, a fake remote and a fake alarm. They are derived from section 2: every
guarantee G1-G8 and every live bug in 2.4 has a test named for it. Nothing in the test
suite reaches a real hypervisor, ssh or rsync.
