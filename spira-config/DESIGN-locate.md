# spira-config locator — one search for the operator's spira.toml

Bead: **sp-hconl** (P0), wave 4 of the lib.sh/conf.sh decomposition (`run/wave4-decomposition.md`
row C1, beads 4-10). Module: `spira_config::locate` (new file, re-exported from `lib.rs`);
touches `discover()` (existing, `lib.rs`) and `main.rs`'s `read_input`.

## 1. Intent

`conf.sh`'s `spira_toml_file`/`spira_toml_resolve` are, today, the only code that finds the
operator's real `spira.toml` for every one of the ~50 bash scripts that source `conf.sh`, and
`pre-activate.sh`'s `check_config` independently duplicates a four-tier search for the same file
because it runs before `conf.sh` can be sourced (it is validating the release `conf.sh` would
activate). sp-4bw2i made every shell *reader* of an already-resolved file go through
`spira-config get`/`export`, but left both locators in place because spira-config had no search
of its own to replace them with — deleting either one first would have stopped every shell
script from finding the real config file on this box (the blocking note on sp-4bw2i).

This bead gives `spira-config` that search, as the single place both bash copies can eventually
call into (future beads: #5 folds `conf.sh` into `eval "$(spira-config resolve --sh)"`; a
later one repoints `pre-activate.sh`). It does **not** delete either bash copy — that is out of
this bead's scope, per sp-hconl's own description, and this box's production config is not
touched by this change (no code path here writes anything).

### What this bead actually found

`spira-config` already has a Rust-side locator, `discover()` (`lib.rs`, landed by sp-cx0mj on
2026-09-28, "spira-config is the only door onto spira.toml" for **Rust** readers — gate, queue,
queue-watch, rebase-stale, release, round-vm, spira-claim, spira-world's `aeons`, strand, and
both of testenv's call sites). The next day, sp-9hwim rewrote `conf.sh`'s bash locator to stop
offering `$SPIRA_REPO/spira.toml` as a candidate at all ("design runtime-is-a-release #5: nothing
reads or writes the checkout at runtime") — and never touched `discover()`. The two Rust and
bash locators have disagreed since 2026-09-29 22:52: eleven Rust binaries still read a
`spira.toml` beside `$SPIRA_REPO` if one happens to be there (a worktree, a fixture checkout),
while every bash script no longer will. A box where `$SPIRA_REPO` is set and has its own stray
`spira.toml` — any aeon worktree, any gate tree — is exactly the shape that produces it.

`discover()` also returns the `$SPIRA_TOML`-pinned path **unconditionally**, even when that file
does not exist, where `conf.sh`'s `spira_toml_file` checks `-f` first and returns empty (no
config in force) if it does not. A caller that goes on to `load()` the result gets a hard read
error instead of "no config, use the derived default" — `gate::gate_mode`'s own comment
(`real.rs`) names the law this breaks: *"the library finds and validates the document"* is
supposed to mean an absent document is not a crash.

Both are fixed in this bead, because "spira-config's own path locator" is exactly the claim
sp-hconl makes, and these are the two ways it was already wrong for the callers it already has.

## 2. Contract

### 2.1 Tiers — conf.sh's search as it stands today, post-sp-9hwim

1. an explicit path the caller already has (a `--config` flag, `discover`'s `explicit` arg) —
   wins outright, no existence check needed because the caller named it on purpose.
2. `$SPIRA_TOML` if the variable is **set at all** (even empty) — exclusive: if the path it
   names is not a file, resolution stops here and reports "not found", it does **not** fall
   through to 3/4. Mirrors `spira_toml_file`'s own comment: "pointing it at a nonexistent path
   means 'read no file at all', not 'keep looking'."
3. `${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml`.
4. `/etc/spira/spira.toml`.

First candidate that is a file wins. **No `$SPIRA_REPO` tier** — removed from bash by sp-9hwim,
now removed from Rust by this bead. `conf.sh`'s matching legacy-file search, `spira_conf_file`,
runs the identical 2-3-4 shape (its own explicit var is `$SPIRA_CONF`) for `spira.conf`; the new
`locate()` reuses it only to name the ambiguous case below, never to read or convert it.

### 2.2 API (`spira_config::locate`, new module)

```rust
pub enum LocateOutcome {
    Found(PathBuf),
    NotFound { tried: Vec<PathBuf> },
    LegacyOnly { conf: PathBuf, tried: Vec<PathBuf> },
}
pub fn locate(explicit: Option<PathBuf>) -> LocateOutcome
```

* `Found` — exactly what `discover()` used to return as `Some`, tiers fixed as above.
* `NotFound` — no `spira.toml` at any tier, and no legacy `spira.conf` either. `tried` names
  every path actually checked (the pinned path alone, when `$SPIRA_TOML` was set; all of
  XDG+/etc otherwise) — **fail closed on the diagnostic, not on the caller**: this is not an
  error for a caller that means to fall back to derived defaults (see 2.3), only a report a
  human or script can act on.
* `LegacyOnly` — no `spira.toml`, but a `spira.conf` resolves at the same (SPIRA_REPO-free)
  tiers `spira_conf_file` searches. Conversion (`spira-config convert`) is needed before a toml
  exists; `locate` reports this rather than silently reporting absence, which is what the box's
  own real state would have produced from the old `discover()` (it never looked for
  `spira.conf` at all).

`discover(explicit) -> Option<PathBuf>` keeps its signature (eleven existing callers,
unchanged) and becomes `locate(explicit).found()` — `Found` only; `NotFound` and `LegacyOnly`
both collapse to `None`, which is exactly how every existing caller already treats absence
(`gate::gate_mode`, `queue`, `queue-watch`, `rebase-stale`, `release`, `round-vm`,
`spira-claim::store`, `spira-world::aeons`, `strand::config`, `testenv` ×2 — audited below).

### 2.3 CLI (`main.rs`)

```
spira-config locate                 the spira.toml path in force, or a refusal naming why
```

* `Found(p)` — prints `p`, exit 0.
* `NotFound{tried}` — prints nothing to stdout; stderr: `spira-config locate: no spira.toml
  found; tried: <path>[, <path>...]`; exit 1.
* `LegacyOnly{conf, tried}` — prints nothing to stdout; stderr: `spira-config locate: <conf>
  exists but no spira.toml — run \`spira-config convert\` first; tried: <path>[, <path>...]`;
  exit 2.

`validate`/`get`/`export --sh` **without a file argument** now resolve through `locate(None)`
too, and refuse (same two messages, `ExitCode::FAILURE`) instead of `read_input`'s old guesses:
silently reading `./spira.toml` if one happened to be sitting in the current directory, or else
blocking on stdin forever. Both guesses are exactly the "ambiguous or missing location" this
bead's brief calls out — a caller who wants stdin still gets it by naming `-` explicitly, and a
caller who wants a specific file still gets it by naming it; only the no-argument, no-resolvable-
file case changes, and no existing caller passes "no argument" in production (every bash site
that calls `get`/`export`/`validate` already resolves its own path and passes it explicitly —
`git grep` confirms this below).

## 3. Decisions

1. **Drop the `$SPIRA_REPO` tier from `discover()`.** Matches `conf.sh` as landed by sp-9hwim.
   Audited every one of its eleven callers (gate, queue, queue-watch, rebase-stale, release,
   round-vm, spira-claim, spira-world's `aeons`, strand, testenv ×2): none has a test that
   plants a `spira.toml` under `$SPIRA_REPO` expecting `discover` to find it there (the one test
   that did, in `spira-config` itself, is rewritten below, not ported). `cargo test` across all
   eleven crates after the change is this bead's parity proof for Rust readers.
2. **Check `$SPIRA_TOML`'s existence before returning it.** `conf.sh` has always done this;
   `discover()` had not. Fixes `gate::gate_mode`'s crash-on-bad-pin into the fall-to-defaults
   its own comment says it should be.
3. **The legacy `spira.conf` auto-convert, and the persona/fayth regeneration
   `spira_toml_resolve` layers on top of its own locator, are not ported.** Both are *write*
   behaviour on top of "which file wins" — sp-usxfl already retired every writer that targets
   `spira.conf`, so the auto-convert's only remaining job is a one-time read on a box that has
   only ever known that format, and the regeneration only matters once `conf.sh` is no longer
   the one calling `spira-config convert` itself (bead #5+). `locate` reporting `LegacyOnly`
   instead of writing anything is strictly more fail-closed than today's behaviour (which
   converts and writes a derived `spira.toml` the first time anything sources `conf.sh`), and
   this bead changes no production file. Future bead (#5, "conf.sh becomes an eval of resolve")
   decides whether the conversion moves into Rust or conf.sh keeps running it.
4. **`pre-activate.sh`'s `check_config` keeps its own four-tier copy for now.** It still
   includes a `$SPIRA_REPO` tier `conf.sh` no longer has — a divergence that predates this bead
   (sp-9hwim fixed `conf.sh`, not `pre-activate.sh`) and is out of this bead's scope to fix in
   bash. Flagged here, and in the delivery report, for whoever repoints `pre-activate.sh` onto
   this locator — at that point the `$SPIRA_REPO` tier must also drop, or `pre-activate`'s
   pre-flight and the activated `conf.sh` could still validate two different files.
5. **No change to `convert`, `set`, `unset`, `schema`, `path-tail`, `migrate`.** `path-tail`
   already calls `discover(None)`, so it inherits both fixes for free; nothing else in `main.rs`
   calls the old ad hoc `read_input` fallback except `validate`/`get`/`export` with no file arg.

## 4. Parity table

Every tier `conf.sh`'s `spira_toml_file` searches, run against the box's real environment and
against planted fixtures, must name the same file (or the same absence) as
`spira_config::locate`. Proven in `spira-config/tests/locate_parity.rs`, which sources the
**actual function bodies** of `spira_toml_file`/`spira_conf_file` out of this worktree's
`spira/conf.sh` (never a hand-copied re-statement of them) into a throwaway bash harness, so a
future edit to `conf.sh`'s search makes this test fail instead of silently drifting.

| tier | conf.sh (`spira_toml_file`/`spira_conf_file`) | `spira_config::locate` | same? |
|---|---|---|---|
| explicit arg | n/a (bash has no caller-supplied path concept here) | `explicit` param | n/a, Rust-only |
| `$SPIRA_TOML` set, file exists | that path | that path | yes |
| `$SPIRA_TOML` set, file missing | empty (no fallthrough) | `NotFound{tried:[that path]}` | yes |
| `$SPIRA_TOML` unset, `$SPIRA_REPO` set with a `spira.toml` beside it | not consulted (tier removed, sp-9hwim) | not consulted | yes (both ignore it) |
| `${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml` exists | that path | that path | yes |
| only `/etc/spira/spira.toml` exists | that path | that path | yes |
| nothing exists, no legacy conf either | empty | `NotFound{tried:[xdg, /etc]}` | yes (absence) |
| nothing exists, legacy `spira.conf` resolves | empty (bash's own `spira_toml_resolve` would auto-convert on top; `spira_toml_file` alone still says empty) | `LegacyOnly{conf, tried}` | **intentional difference** — `locate` names the legacy file bash's plain `spira_toml_file` call doesn't surface; see Decision 3 |

## 5. Non-goals

* Deleting `conf.sh`'s or `pre-activate.sh`'s own copies of this search (sp-4bw2i's remaining
  blocker resolves once this lands; the deletions themselves are future beads).
* The key registry (sp-g3uwp) and `spira-config resolve` (bead 4 in the wave, which depends on
  both sp-hconl and sp-g3uwp).
* Any write to this box's real `~/.config/spira/spira.toml` or `spira.conf` — this bead reads
  only, including every test, which runs against scratch directories or (for the parity test)
  this worktree's own `spira/conf.sh` text, never the operator's files.
