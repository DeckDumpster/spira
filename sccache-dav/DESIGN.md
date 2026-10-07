# sccache-dav — design

Bead: **sp-xjnzl** (P0). One compilation cache, reachable from this box's own builds (gate,
testenv, landing — `spira_config::build`, sp-z61hj) and from every ephemeral round-vm, so a
round is warm from its *first* build rather than its second.

## 1. Intent

`round-vm run` faulted on every call: the VM's own `cargo build` (via `testenv`'s in-place
build step) refused with `no-build-cache` because `sccache` was not on the VM's PATH — a
refusal `spira_config::build` enforces everywhere on purpose (sp-z61hj, the host disk-IO
incident it followed). The first fix considered was giving the VM template its own local
sccache and disk cache (sp-dvfea's template already warms the registry). Ryan's correction
(2026-10-01): a VM-local cache only ever warms itself — the VM's dependency crates and the
host's never share a hit even though the `Cargo.lock`, the profile and the toolchain are
identical. **One store both sides read and write makes a round warm from the first build.**

## 2. Why WebDAV, and why this crate rather than an existing server

sccache 0.18.0 ships a `webdav` cache backend (`src/cache/webdav.rs`, via `opendal`'s
`services::Webdav`) that needs nothing more than an HTTP endpoint speaking a small slice of
RFC4918 — no NFS/9p plumbing across the Proxmox guest-agent boundary a shared mount would
need, and no in-RAM service competing with the agents already running on this box (Redis
was considered and rejected for exactly that reason). Nothing already on this box serves WebDAV
(checked: no `wsgidav`, no `rclone`, no system package), and `law-new-subsystems-are-rust`
(round-vm's own DESIGN.md cites it) rules out reaching for a quick Python one. The actual
wire surface sccache issues is small enough that writing it is the cheap option, not the
ambitious one — confirmed by reading sccache 0.18.0's `RemoteStorage` (`src/cache/cache.rs`)
and `opendal-service-webdav` 0.56.0's client (`src/core.rs`, `src/backend.rs`) rather than
assumed:

* `RemoteStorage::get`/`get_raw` → `Operator::read` → one `GET`. A 404 is a cache miss, not
  an error (`err.kind() == ErrorKind::NotFound`).
* `RemoteStorage::put_raw` → `Operator::write` → `write()`'s `create_dir` step first
  **walks the key's ancestors with depth-0 `PROPFIND`** (`webdav_mkcol`, stops at the first
  ancestor that already exists or at `/`, which is never queried) and **`MKCOL`s every
  missing one**, then `PUT`s the body.
* `RemoteStorage::check` → one `GET` of `.sccache_check` (`NotFound` tolerated) then, unless
  read-only, one `PUT` of the same key to confirm write access.
* **Never used**: `COPY`, `MOVE`, `PROPPATCH`, or a listing `PROPFIND` (depth 1/infinity).
  `current_size`/`max_size` return `None` without ever listing the store. A server that only
  answers depth-0 `PROPFIND` is a correct implementation of what this client asks, not a
  shortcut around what it asks.
* Keys are sharded (`src/cache/utils.rs::normalize_key`: `abcdef` → `a/b/c/abcdef`), which is
  exactly why `MKCOL` and depth-0 `PROPFIND` are load-bearing and not dead code: every write
  is at least three directories deep from a cache that starts empty.
* `D:getlastmodified` is parsed as an RFC 2822 date (`Timestamp::parse_rfc2822`); the exact
  shape in every one of opendal's own test fixtures is RFC 7231's IMF-fixdate (`Tue, 01 May
  2022 06:39:47 GMT`), which `httpdate::fmt_http_date` produces — confirmed against the
  vendored fixtures rather than assumed compatible.

## 3. Contract

```
GET    <key>   -> 200 + body, or 404
HEAD   <key>   -> 200 (no body), or 404
PUT    <key>   -> 201; creates every missing ancestor directory first; atomic (tmp + rename)
DELETE <key>   -> 204, or 404
MKCOL  <dir>   -> 201, always (idempotent; RFC4918 9.3.1 also allows 405 for "already exists",
                  but this store never needs the caller to tell the two apart)
PROPFIND <path>, Depth: 0 -> 207 one <D:response> (collection or sized file), or 404
```

Any other method: 405. A path containing `..` (after percent-decoding) is refused with 400
rather than resolved — defense in depth on a socket reachable from more than one box, even
though both are on a private LAN. `Authorization: Bearer <token>` is enforced on every
request when `SCCACHE_DAV_TOKEN` is configured.

## 4. Configuration (environment only, for this SERVER)

| variable | meaning |
|---|---|
| `SCCACHE_DAV_ADDR` | `ip:port` to bind. Refused if it starts with `0.0.0.0` or `*` — this store binds the LAN address the round VMs and this host's builds already share, never a wildcard. |
| `SCCACHE_DAV_ROOT` | directory the cache entries live under; created if missing. |
| `SCCACHE_DAV_TOKEN` | optional bearer token; unset means no auth (LAN-only is the only guard). |

This server binary never reads `spira.toml` itself — `SCCACHE_DAV_ADDR` above is the
*unit's* own `Environment=` line (`systemd/sccache-dav.service`), rendered by
`units-install` from the `SPIRA_SCCACHE_DAV_ADDR` conf.d key (sp-xtdqi; no default — absent
means `install/src/manifest.rs` declines the unit outright, the same as it would a template
with an unfillable placeholder), same as every other value a systemd template substitutes.
The server binary still only ever sees a bare environment variable; it is the CLIENT side
(§5) that now also reads config.

## 5. What a build on either side needs set — UPDATED, sp-xtdqi (reverses the call below)

~~`spira_config::build::Wrapper::env()` sets only `RUSTC_WRAPPER` and
`SCCACHE_IGNORE_SERVER_IO_ERROR`; it does not — and must not — set `SCCACHE_WEBDAV_*`... an
**ambient environment change**, never a code change in `spira_config::build`.~~ That call did
not survive contact with a gate restarting the box's one sccache CLIENT daemon: the daemon
fixes its backend at spawn time and ignores every later invocation's environment, so a
daemon a stale build started before anyone had exported `SCCACHE_WEBDAV_ENDPOINT` kept every
later build on the LOCAL DISK cache until an operator noticed and re-exported the vars by
hand — the exact failure sp-xtdqi exists to close.

`Wrapper::env`/`Wrapper::admitted_env` now take an `Option<&spira_config::build::Store>` and,
when `Some`, set `SCCACHE_WEBDAV_ENDPOINT`/`SCCACHE_WEBDAV_KEY_PREFIX` themselves — resolved
from `SPIRA_SCCACHE_DAV_ADDR` (`Store::from_values`/`Store::from_env`, in-process, never a
bare env read) — AND synchronise the client daemon's own backend first (`ensure_store_backend`
inside `spira-config/src/build.rs`): a daemon already on the store is left alone, one on a
different backend is stopped (never refused — see that function's own doc) so the very next
cargo invocation this call is about to make starts a fresh daemon on the right one. A build
whose box names no store (`SPIRA_SCCACHE_DAV_ADDR` unset) is unaffected either way — still
whatever backend an operator's own `~/.cargo/config.toml` happens to name, if anything.

For a cross-machine cache hit on a dependency crate, sccache's Rust frontend hashes the
literal compiler arguments and `CARGO_*` environment variables — including the absolute path
under `CARGO_HOME` every registry source file is compiled from (confirmed in
`spira-config/DESIGN-build-cache.md` §2.5, re-verified against `rust.rs::generate_hash_key`
for this bead: `SCCACHE_BASEDIRS` is wired for the C/C++ frontend only, never Rust, in
0.18.0). Two boxes only share a dependency-crate hit if `CARGO_HOME` **and** the rustc
version are byte-identical on both sides — not normalized, not approximated.

## 6. Test strategy

Rust integration tests only, driven over a real socket with hand-rolled HTTP (the methods
under test have no constructor on a typed client already in this workspace) — the same idiom
`loom/tests/endpoint.rs` already uses. Covers: a miss, a sharded-key PUT/GET round trip (the
exact shape sccache writes), PROPFIND on a file vs. a directory vs. a miss, MKCOL's
idempotence, DELETE's two-call contract, the bearer-token check (absent/wrong/right), path
traversal, and `config_from_env`'s fail-closed checks. Nothing here reaches a real VM or a
real Proxmox: that is `round-vm`'s own test suite and the proof run in the bead.

## Size cap

`SPIRA_SCCACHE_DAV_MAX_GB` bounds the store. Every `SWEEP_EVERY` the process deletes the
least recently used files until the store is under the cap; a GET refreshes the entry's mtime,
which is the recency it orders by. In-flight PUT tmp files and directories are left alone. A
missing or zero cap refuses to start.
