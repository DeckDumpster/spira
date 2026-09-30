# Build IO — one compilation cache, no cold dependency builds, ephemeral targets on tmpfs

Bead: **sp-z61hj** (P0). Module: `spira_config::build` (`src/build.rs`); callers in `gate`,
`testenv`, `release`, `aeon`; the reaper is `testenv`'s `target-reap` binary.

## 1. Intent

The box wrote ~19 GiB/hour to one consumer SATA SSD (2.25 TB in 118 h), and the cause was
compilation, not data: every worktree — aeons, agents, gate trees, landings — compiled the
whole workspace, **every third-party dependency included**, cold into its own `target/`
(159 target dirs, 236 GiB). Dependencies are the cacheable part and were rebuilt every time.

Per Ryan (2026-09-30): *"other than highly cachable dependency pulls and builds, we should not
be generating that much IO."* So:

1. **One shared compilation cache for the box**: every Spira build path compiles through
   `sccache` with its local disk cache. A dependency crate is compiled at most once per
   (Cargo.lock, profile) and read from the cache after that.
2. **Non-interactive builds are not incremental.** Incremental caches are write-heavy
   (~270–350 MB per tree) and useless to a one-shot build.
3. **Ephemeral trees build on tmpfs.** A gate tree's `target/` lives on a RAM-backed tmpfs
   (testenv's slots already do, sp-t26yx); a trial that cannot get the room refuses
   (NO_VERDICT), never falls back to the disk.
4. **No redundant landing build.** The landing pass no longer runs `build.sh` (a
   `cargo build --release --workspace`) after a landing; a landing publishes a release from
   tested binaries (`queue land-local` → `release build --bin-dir`).
5. **Build state of finished work is reaped**: `target-reap` removes the `target/` of a
   worktree whose bead is closed.

## 2. Contract

### 2.1 `spira_config::build`

```text
wrapper(path, setting)  -> Ok(Wrapper::Sccache(<abs path>)) | Ok(Wrapper::Off) | Err(refusal)
Wrapper::env()          -> [("RUSTC_WRAPPER", <abs>|""), ("SCCACHE_IGNORE_SERVER_IO_ERROR","1")]
one_shot(profile)       -> ["--config", "profile.<profile>.incremental=false"]
```

* `path` is the PATH the build will run under (the gate's is built outright from
  `SPIRA_RELEASE` + `spira.path`, sp-31gtu; everyone else's is their own). `sccache` is
  resolved on it to an absolute path — never an ambient lookup at compile time.
* `setting` is `$SPIRA_BUILD_CACHE`. Unset or empty or `sccache` → the wrapper is required.
  `off` → `Wrapper::Off` (explicit, loud opt-out: `RUSTC_WRAPPER=""`, which overrides any
  cargo config). Anything else is a refusal naming the value.
* **Absent sccache is a refusal (fail closed)** for every build *tool*: the gate, for a
  trial that builds in its tree — a definition with `bin` lines (the spira repository's, always),
  a unit composition, or `--release-bins` (`NO_VERDICT reason=no-build-cache`; a column-gated
  repository whose gate builds nothing is judged without it), testenv (`VERDICT FAULT rc=3 reason=no-build-cache`),
  `release build` (error). The refusal names `spira/deps.toml` and the opt-out.
* **The aeon falls back, loudly.** An aeon session is not a build; it hands its agent an
  environment. With sccache absent the session still runs, with a log line saying its builds
  are uncached (`aeon: sccache not on PATH …`). Refusing a session over a cache would stop
  the world over a performance tool.

### 2.2 Why `--config profile.<p>.incremental=false` and not `CARGO_INCREMENTAL=0`

**Measured (2026-09-30):** sccache hashes every `CARGO_*` environment variable into a
compilation's key. A dependency built with `CARGO_INCREMENTAL=0` and the same dependency
built without it (an aeon's interactive `cargo test`) are **two cache entries** — 0 hits
across the two. `--config profile.aeon.incremental=false` is a command-line switch: the
dependency's key is unchanged (3/3 hits), and cargo still passes no `-C incremental` to the
workspace crates. The same finding forbids `CARGO_TARGET_DIR` as an **environment variable**
on any cached build: two target dirs set that way never share a key (0 hits measured);
`--target-dir` (or linked build directories) does (hits). `release` therefore passes
`--target-dir`, and the gate moves its build directories by symlink.

### 2.3 The gate tree's target on tmpfs

`<gate tree>/target` stays a real directory, and each of its build directories — `aeon`,
`release`, `debug`, `gate-tools` — is a **symlink** to `<root>/<gate tree basename>/<dir>`,
where `<root>` is `$SPIRA_GATE_TARGET_ROOT`, else `/tmp/spira-gate-target-<sha256(SPIRA_RUN)[..6]>`
when `/tmp` is a tmpfs. Every path consumer (`target/aeon/<pkg>`, `target/gate-tools/<tree
id>`, `git clean -e target`, `make build`) is unchanged; only where the bytes land moves.
**Why the links are inside `target/` and not `target` itself:** `.gitignore` says `target/`,
which matches a directory only. A `target` symlink would be an *untracked file* to every
fence that walks `git ls-files --others` (spira-lint) — in the base trial too, whose older
`.gitignore` no change here can reach. Anything inside an ignored directory is invisible to
git in every revision.

Before each trial (under the trial's tree lock):

1. **Orphans**: a directory under `<root>` whose gate tree is gone is removed.
2. **Cap**: while the directories under `<root>` total more than
   `$SPIRA_GATE_TARGET_CAP_MIB` (default 12288), the least recently used one whose tree lock
   is free (a non-blocking `flock` succeeds) is removed. A locked one is a running trial and
   is never touched.
3. **Room**: `<root>` needs `$SPIRA_GATE_TARGET_MIN_FREE_MIB` (default 4096) free and
   `MemAvailable` at least `$SPIRA_GATE_TARGET_MIN_MEM_MIB` (default 4096); short is
   `NO_VERDICT reason=scratch-short` — **never a fall back to the disk**.
4. A real build directory left in the tree (a tree from before this change) is removed (its
   disk is freed) and replaced by the link; a dangling link gets its directory back.

When no tmpfs is available at all (`/tmp` is not one and no root is configured), the target
stays in the tree, and the gate says so on stderr — a host without a tmpfs is not short of
room, it has none to offer.

### 2.4 `target-reap`

```text
target-reap [--dry-run] [--worktrees DIR]
```

Candidates: each `<DIR>/<name>/target` (default `$SPIRA_RUN/worktree`) where `<name>` is a
bead id (`sp-…`, optionally `.N` children) — the aeon and Concierge worktree convention.
One `bd show <ids…> --json` reads their status; a target whose bead is `closed` is removed.
A bead bd does not know, or a failed `bd` call, removes nothing (fail closed: never guess).
Gate trees and testenv slots are not candidates — the gate and testenv own those.
Output: `target-reap: removed <n> target dir(s), <MiB> MiB (<ids>)`, and one line per kept
open bead count. Called by the landing pass once per pass (it replaced `land-build-ensure.sh`
in that slot), and by hand.

## 3. Schema

No new config keys in `spira.toml`. Environment only:

| variable | read by | meaning |
|---|---|---|
| `SPIRA_BUILD_CACHE` | gate, testenv, release, aeon | unset/`sccache`: required; `off`: explicit opt-out |
| `SPIRA_GATE_TARGET_ROOT` | gate | tmpfs root for gate-tree targets |
| `SPIRA_GATE_TARGET_CAP_MIB` | gate | total size cap before LRU eviction (12288) |
| `SPIRA_GATE_TARGET_MIN_FREE_MIB` / `_MIN_MEM_MIB` | gate | room a trial needs (4096 / 4096) |

`sccache` is declared in `spira/deps.toml` (tier `operator`: doctor checks it on an operated
box; the fixture image waives it, `spira/testenv/doctor-waivers`): the cache lives in its default
`~/.cache/sccache` (10 GiB LRU), the one place a dependency build is written.

## 4. Decisions

* **sccache, not a shared `CARGO_TARGET_DIR`.** cargo takes an exclusive lock on a target
  directory for a whole build, so a shared one serialises every concurrent gate, aeon and
  landing (certify_par=4 would become 1). It would also put every tree's `target/aeon/<pkg>`
  at one path, which is exactly what sp-g9f3t's tree-keyed tools exist to stop. sccache
  keeps each tree's own target (attribution unchanged) and shares only compiler outputs,
  keyed by their full inputs; concurrent builds are safe.
* **Workspace crates are not shared across trees.** Their keys include their absolute path
  (`CARGO_MANIFEST_DIR`), which differs per worktree. sccache 0.18's `SCCACHE_BASEDIRS` is a
  server-wide setting and cannot name a per-build root. Accepted: the bead's measure is
  dependency crates, and a workspace crate's build is cheap next to its dependencies.
* **Proc-macro, build-script and binary crates are not cacheable by sccache** (35 of 92
  requests in the probe). They are compiled per tree — on tmpfs for a gate tree.
* **`SCCACHE_IGNORE_SERVER_IO_ERROR=1`.** The server is spawned by whichever client first
  needs it and dies with that client's systemd unit; a compile whose server vanished falls
  back to a local rustc instead of failing the build.
* **Dropped: `land-build-ensure.sh` and its two suites** (`test-land-build-ensure.sh`,
  `test-landing-build.sh`). It ran `build.sh` (`make build` = `cargo build --release
  --workspace`, 5–6 minutes, measured 2026-09-29 in `landing.log`) in `SPIRA_REPO` after
  every landing that touched `.rs`. Since the runtime became a release (sp-6p20x) its trigger
  can no longer fire (`SPIRA_REPO` is a release directory with no reflog), and the release a
  landing publishes is built from the tested binaries (`--bin-dir`). A dead rebuild waiting
  for the next checkout to wake it is removed, not kept.
* **The hand landing's release build** (`cargo build --release --workspace` in the landing
  worktree, which `queue land-local --worktree` reads) is the one release-profile build a
  landing needs — nothing else built that profile of that tree. It now compiles through the
  cache: its dependencies are cache reads.
* **Not in scope:** the round VM (its own disk, its own cargo cache kept in the template,
  sp-dvfea), the fixture container's in-container `cargo` (its `CARGO_HOME`/target are
  container volumes, and the fixture sets `SPIRA_BUILD_CACHE=off`, so a suite driving a Spira
  build tool in there builds uncached on purpose rather than being refused — testenv itself
  builds on the host), and interactive shells of agents and the Concierge (an operator may point
  `~/.cargo/config.toml`'s `build.rustc-wrapper` at the same sccache).
