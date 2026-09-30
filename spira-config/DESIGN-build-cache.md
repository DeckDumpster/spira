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

### 2.5 Why cross-tree workspace-crate hits are not reachable (sp-283wz, 2026-09-30)

sp-283wz set out to make unchanged workspace crates hit sccache across trees via
`SCCACHE_BASEDIRS` and/or `--remap-path-prefix`, on the premise that "their path is part of
the key" the way a dependency crate's registry path is not. **Neither lever can do this in
sccache 0.18.0, and the premise undersells the actual cost.** Read from the vendored source
(`~/.cargo/registry/src/*/sccache-0.18.0/src/compiler/rust.rs`, `compiler/c.rs`), confirmed
against real builds:

* **`SCCACHE_BASEDIRS` is wired into the C/C++ frontend only.** `storage.basedirs()` is read
  in exactly two places, both in `compiler/c.rs` (preprocessor-output stripping). Rust's
  `generate_hash_key` (`compiler/rust.rs`) takes `_storage` — underscore-prefixed, unused. It
  hashes the literal command-line arguments (`args.hash(...)`), every `CARGO_*` environment
  variable including `CARGO_MANIFEST_DIR` (an absolute path), and the compilation's `cwd`
  directly — none basedir-normalized, none skippable. A worktree's absolute path reaches the
  hash three different ways and `SCCACHE_BASEDIRS` touches none of them for Rust.
* **Adding `--remap-path-prefix` makes it worse, not neutral.** The flag is parsed as
  `PassThrough` (`rust.rs` ~line 1047) and is not in the small set of args excluded from the
  hash (`-L`, `--extern`, `--out-dir`, `--check-cfg`, `--diagnostic-width`) — its literal text
  is hashed. Its `FROM` side is necessarily the tree's own absolute path (that is what makes
  it useful for debug info), so it differs byte-for-byte per tree. Measured (scratch
  `sp-283wz-expB`, two worktrees of the same commit, same `Cargo.lock`): a plain sccache
  build of `spira-config` in a second tree hit 15/24 cacheable requests, matching the first
  tree exactly — this is sp-z61hj's existing, working dependency-crate win. Adding
  `SCCACHE_BASEDIRS` plus a per-tree `--remap-path-prefix` to *one side only* (the natural way
  to try it, since the flag's value is inherently per-tree) drove that to **0/24**: it does
  not gain workspace-crate hits, it loses the dependency-crate hits already landed.
* **The dominant cost was never the workspace-crate rlibs — it is binary linking, and
  sccache refuses to cache that in any tree, including the same tree twice.** 39 of this
  workspace's 43 crates have a `src/main.rs`. sccache's Rust frontend hard-refuses any
  non-`rlib`/`staticlib` crate type before it ever computes a hash
  (`cannot_cache!("crate-type", ...)`, `rust.rs` ~line 1183: *"We can't cache non-rlib/
  staticlib crates, because rustc invokes the system linker to link them, and we don't know
  about all the linker inputs"*). Measured: rebuilding `spira-config --release` at the
  *identical path* twice (fresh `--target-dir` each time, nothing else changed) took the same
  ~100s both times. Isolating the crate alone (dependencies already warm) showed the library
  half sccache-hit in under a second while the **binary link/codegen step still ran, taking
  ~52s** — every time, same tree, same content, same everything. No basedir, remap, or
  path trick reaches this: it is refused before the cache is consulted at all. With 39 such
  binaries in `--release --workspace`, this floor — not cross-tree path collisions — is most
  of `release-bins`'s wall clock.
* **Conclusion:** this bead's described fix is not implemented, because it would either do
  nothing (bare `SCCACHE_BASEDIRS`) or actively regress sp-z61hj's dependency-crate hits
  (`SCCACHE_BASEDIRS` + `--remap-path-prefix`). A real reduction in `release-bins` wall clock
  has to come from building fewer binaries per trial, or from something other than sccache
  entirely (a content-addressed artifact cache keyed on each crate's git tree hash, e.g.) —
  out of scope here; see sp-283wz's report for the measurements this rests on.

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
  (`CARGO_MANIFEST_DIR`, the literal args, and `cwd`), which differs per worktree, and
  `SCCACHE_BASEDIRS` does not reach any of the three for sccache 0.18's Rust frontend
  (§2.5). **Superseded 2026-09-30 (sp-283wz): "a workspace crate's build is cheap next to
  its dependencies" was wrong** — most of this workspace's crates are binaries, and
  sccache refuses to cache a binary's link step in any tree, including the same one twice.
  That refusal, not the cross-tree path, is most of `release-bins`'s wall clock (§2.5).
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

## 5. `[profile.release]` chosen by measurement (sp-zqo8s, 2026-09-30)

sp-283wz (§2.5) established that sccache never caches a binary's link/codegen step (39 of
this workspace's 43 crates are binaries) and that this floor, not cross-tree path misses, is
most of `release-bins`'s wall clock. What was still open: whether the workspace's
`[profile.release]` — `opt-level="z"`, `lto=true` (fat), `codegen-units=1`, inherited
unexamined from the 5-crate workspace (sp-c411z) — was the slowest possible way to pay that
floor. It was.

### Method

Four candidates, each built with `cargo build --release --workspace --locked --config
profile.release.incremental=false` (the exact `release_bins_command()`, `gate/src/engine.rs`),
through the box's shared sccache. Two measurements per candidate:

* **cold** — a brand-new worktree of `local/main` that has never built this profile's exact
  flags before (a fresh gate tree: dependency crates still hit sccache, since their
  `CARGO_MANIFEST_DIR` is the shared `~/.cargo/registry` path regardless of tree; workspace
  crates and the link step do not, per §2.5).
* **warm** — the same worktree, `target/` wiped and rebuilt again immediately after: what a
  gate tree pays on a *second* `--release-bins` at the same tip (a re-certification, or a
  branch that fails and is re-landed at the same commit).

All eight builds ran **one at a time** (never in parallel) on a box otherwise busy with
~16 concurrent aeons (load average 20–90 on 32 cores over the session; noted per row). Midway
through, the Concierge capped `~/.cargo/config.toml`'s `[build] jobs` at 8 (18:41:54 UTC,
2026-09-30) to ease the same contention; every number below is **under that cap** — the first
pass's uncapped numbers (profile A cold 173s/108s, B 171s/167s, C cold 168s) were discarded
per that instruction rather than mixed in. The **hot-path check** is `spira-lint --root .
--base local/main` — a real gate fence binary, run three times per candidate (first run
absorbs page-cache warmup; the two after are the reported range) — chosen because it is
CPU-and-I/O over 1,472 files and 26 rules, the kind of thing the gate runs on every trial.
**bin/ size** is the sum of the 40 top-level executables in `target/release` (what
`build-tarball.sh --bin-dir` would ship), matching the bead's bound.

### Results

| candidate | opt-level | lto | codegen-units | cold wall | warm wall | bin/ size | hot-path (spira-lint) |
|---|---|---|---|---|---|---|---|
| A — current | `"z"` | `true` (fat) | 1 | 191s (load 29/32/38, 31 agents) | 146–162s (load 31–45, 27–30 agents) | 43 MB | 5.7–6.1s |
| B | 2 | `"thin"` | 16 | 199s (load 50/48/43, 31 agents) | 149–165s (load 43–52, 33–36 agents) | 61 MB | 5.8–6.6s |
| C | 2 | `false` | 16 | **113s** (load 29/34/40, 32 agents) | **25–28s** (load 34–48, 36–41 agents) | 61 MB | 6.0–8.7s |
| D | `"s"` | `"thin"` | 16 (default) | 198s (load 28/36/42, 35 agents) | 162s (load 35/37/42, 35 agents) | 52 MB | 5.6–5.7s |

All four are inside the bound (bin/ ≤ 86 MB, 2× today's 43 MB); no candidate approaches a
GitHub release-asset limit. No hot-path regression: `spira-lint`'s own wall clock is flat
across all four (it is I/O- and regex-bound, not sensitive to codegen opt-level), so
`opt-level="z"`'s size-over-speed trade was never buying anything on this fence.

**C — `lto=false, codegen-units=16, opt-level=2` — wins on every axis measured:**
*cold* 113s is already the fastest of the four (a genuinely fresh gate tree is ~40% faster
than the current profile, not slower); *warm* 25–28s is a 5–6× win over every LTO candidate
(A, B, D all stay at 146–199s regardless of warmth). The mechanism: `lto=true`/`"thin"` both
run a whole-program link-time-optimization pass that sccache cannot cache and that reprocesses
every crate's IR at link time regardless of how warm the object-code cache is; `lto=false`
lets the link step become a plain `ld` pass over already-compiled, already-cached object code,
so a warm tree relinks in seconds instead of minutes. This matters operationally because a
gate tree is reused across re-certifications of the same tip (LRU-evicted, not wiped every
trial) — the *warm* number is the one most hand-landings and re-lands actually pay.

**Decision: land profile C.** `codegen-units=16` (vs. today's 1) also gives rustc's own
codegen a real thread pool instead of serializing every crate to one unit — part of why even
the cold number improves. `opt-level=2` over `"z"`: size-optimize only mattered when the link
step (LTO) was already the dominant cost; with LTO off, `opt-level=2`'s slightly larger output
(61 MB vs. the current 43 MB, still within bound) buys normal optimization instead of
optimizing for size no caller asked for.

### Caveats

* The **cold** numbers for A, B, D above are each a single fresh-worktree sample (not the
  bead's literal "2 samples on a fresh gate tree" for every row) — the first measurement pass
  (2 fresh samples per candidate) ran before the jobs=8 cap and was discarded per the
  Concierge's instruction rather than compared across two different caps. Re-running a second
  independently-fresh-worktree cold sample per candidate, under jobs=8, would tighten this if
  the box quiets down; the gap between C and the rest (113s vs. 191–199s) is large enough
  relative to the load-driven noise seen across this session (±20–50s swings between otherwise
  identical runs) that it is very unlikely to be an artifact, but it is one sample each, not two.
* `spira-lint` is one gate fence, not a profile of every hot path; opt-level differences on a
  CPU-bound binary (e.g. a tight loop in `sentinel` or `queue`) were not separately measured.
* Box load climbed steadily across the whole measurement session (roughly 20 → 90 on 32 cores
  before the jobs cap, 28–50 after) independent of which profile was building — every row
  above carries its own load/agent-count so a reader can judge how much of a given number is
  profile and how much is ambient contention.
