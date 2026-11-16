# Repo-contract phase 1: per-check cost of a dev shell, and where `nix` is reachable

Subject: deckdumpster (`origin/main` e3f94f5). Design: brain
`wiki/projects/spira/designs/repo-landing-contract-2026-11-16.md`, "Phases" 1.
Raw files are under `repo-contract-phase-1-evidence/`.

## Result

```
per-check: 69.5 ms (row `true`) / 186 ms (row `uv run ruff check`) via print-dev-env cached (mechanism 2)
builder: reaches nix by /nix/var/nix/profiles/default/bin/nix (absolute path; nix is on no builder PATH, no env var or bind needed)
gate env -i: append /nix/var/nix/profiles/default/bin to the launcher PATH (spira.path tail); nothing else needed, not even HOME
testenv: podman -v /nix:/nix:ro (the daemon socket is under /nix/var, so one bind carries it) + NIX_CONFIG="experimental-features = nix-command flakes"; print-dev-env and develop both work under the isolated network
```

The bar ("well under a second") is met by both mechanisms. Phase 1 passes; drafts 2-8 go ahead
using mechanism 2.

## (a) Per-check cost

Run from this builder session's cgroup, read from `/proc/self/cgroup`:
`0::/user.slice/user-1000.slice/user@1000.service/app.slice/spira-dwarf-builder-directed-1794855745.service`
(`bench-raw.txt` line 1). nix 2.35.2, shell built once beforehand, 20 runs per cell,
`bench.sh flake <detached deckdumpster worktree>` (`N=20`).

| row | mechanism | median | max |
|---|---|---|---|
| `true` | 1. `nix develop <flake> --command` | 328.0 ms | 380.8 ms |
| `true` | 2. `print-dev-env --json` cached, exec with the JSON's variables | 69.5 ms | 143.4 ms |
| `uv run ruff check mtg_collector/` | 1. `nix develop` | 538.8 ms | 915.2 ms |
| `uv run ruff check mtg_collector/` | 2. cached print-dev-env | 186.3 ms | 285.6 ms |

Baseline with no nix at all (`baseline-no-shell.txt`): `true` 6.5 ms; the ruff row by absolute
`~/.local/bin/uv` 62.8 ms median (max 218.7). So the shell adds, by median:

- mechanism 1: about 320 ms (`true`), about 476 ms (ruff row);
- mechanism 2: about 63 ms (`true`), about 124 ms (ruff row).

All 80 timed runs exited 0 (`rcs` in `bench-raw.txt`). Output was discarded in the timed loop; the ruff row printed "All checks passed" in an untimed run, not kept as a file.

Mechanism 2 as measured: `nix print-dev-env --json <flake>` once, cached under
`sha256(flake.nix flake.lock)`; each row is `env -i <exported variables from the JSON> HOME=$HOME
TMPDIR=/tmp <argv>`. Two of the JSON's values must be overridden: `HOME=/homeless-shelter` (uv
then cannot create its cache; the first bench run with the JSON's HOME left in place had every
ruff-row run exit 2, so `bench.sh` overrides it; that first run's file was overwritten) and `TMPDIR=/build`. The JSON's PATH is the nix store's only; nothing
from the host PATH leaks in. The JSON parse (`python3 -I`) is inside the timed cost; the cache-key hash and file lookup are not (the key is computed once per `bench.sh` run, a `sha256sum` of two small files, a few ms).

Cold, once (`cold-build.txt`, `store-size.txt`): 9.1 s wall to fetch and build the shell from
cache.nixos.org, adding 526,224 KiB (about 514 MiB) to `/nix/store` (3,418,144 to 3,944,368 KiB).
Informational only. Two further costs not in the table: the first `uv run` creates deckdumpster's
`.venv` (PyPI traffic), and the first `print-dev-env` after a flake.lock change is the cold figure.

Findings worth carrying into drafts 2-8:

- The flake must be tracked by git for a `git+file` reference (`git add -N`), or referenced as
  `path:`. Untracked it fails with "not tracked by Git". Mechanism 1 on a flake inside a git tree
  also copies the tree to the store, which is part of its higher cost.
- The nix shell's uv is 0.13.0 (nixpkgs at the pinned revision); the hand install on the host is
  0.13.8. `uv.lock` resolved with 0.13.0 here. A repo needing a newer uv must pin a newer nixpkgs.
- Bare `uv` on this builder's PATH is exit 127 (`baseline-no-shell.txt`, last lines): the sp-28d4u3
  refusal reproduces, and a shell is what removes it.

## (b) Reachability

Evidence: `reach.sh` (commands) and `reach.txt` (their output and exit statuses).

**Builder session.** PATH is `/usr/bin:/bin:<release>/model-bin:~/.cargo/bin`; `command -v nix`
fails. `/nix/var/nix/profiles/default/bin/nix eval --expr 1+1` prints `2`. The socket is
world-writable, so no group or bind is needed. `$SPIRA_NIX_PROFILE_BIN` is unset in the builder
environment; it is the config key the PATH entry should come from, not something to read from the
environment.

**Gate `env -i`.** PATH as `release_path_from_env_with_tail` builds it: `<release>/bin:<release>/spira:/usr/local/bin:/usr/bin:/bin:` plus the `spira.path` tail (live: `~/.local/bin:~/.cargo/bin`). `nix` is not found there. Appending `/nix/var/nix/profiles/default/bin` to
the tail makes `nix eval` and `nix develop <flake> --command uv --version` work under `env -i`
with only PATH set. Variables nix needs: none. HOME unset works (nix falls back to the passwd
entry); `HOME=/nonexistent` fails ("creating directory /nonexistent/.cache/nix/fetcher-locks:
Permission denied"), so a gate that sets HOME must point it at a writable directory.
`XDG_CACHE_HOME` and `NIX_REMOTE` are not needed; a fresh empty HOME also works, at the cost of
re-fetching the flake input from the network. The experimental features come from
`/etc/nix/nix.conf` on the host.

**Testenv container.** Image `localhost/spira-testenv:93400812ae94`, network
`pasta:-T,none,--no-map-gw` as testenv uses, rootless. With nothing bound `nix` is absent.
With `-v /nix:/nix:ro` and `NIX_CONFIG="experimental-features = nix-command flakes"` (the
container has no `/etc/nix/nix.conf`; without it nix refuses `nix-command`):
`nix eval --expr 1+1` prints `2`, `nix print-dev-env --json path:/flake` returns the JSON, and
`nix develop path:/flake --command uv --version` prints `uv 0.13.0`. The daemon is reached through
`/nix/var/nix/daemon-socket/socket` inside the same bind; a read-only bind does not prevent
connecting. The container's root maps to the invoking host user, which the daemon accepts for a
shell already in the store. Use `path:` references, as the container sees no `.git` for the
flake. Not tested: a shell not already in the store (the daemon would have to build or fetch it),
and the rootful round-VM podman.
