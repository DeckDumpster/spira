# rule — design

Replaces the logic of `rule.sh` (root of the harness, ~220 lines) with one Rust binary,
`rule`. `rule.sh` stays as the one entry point every caller already names — `concierge.sh`,
`cockpit/db.sh`'s comment, every runes brief that tells an agent `rule.sh enact <slug>
"<text>"`, and the suites — the same shim pattern `gate/DESIGN.md` established for `gate.sh`:
source `conf.sh` for its side effects (env, PATH), then `exec -a "$0" rule --home
"$(dirname "$0")/spira" "$@"`. Bead: sp-g9mhe (rewrite wave 5f).

## Intent

`rule` is the one command that writes a statute: enact writes it to the statute book (the
Spira beads database — `bd remember`/`bd recall`/`bd forget`/`bd memories`, the ONE store,
no second copy) and regenerates the wiki page that is its only git-backed copy, in that
order, because a rule that depends on remembering the second step is a resolution, not a
mechanism. `list` and `show` read it back.

**FAIL CLOSED.** A missing `.beads` at `$SPIRA_DB` refuses rather than guesses. A synthesis
hook that is missing, non-executable, or exits non-zero is reported as "the database write
succeeded, the wiki page was NOT regenerated" — never silently dropped (the defect rule.sh
itself exists to fix, sp-p0xyt: `law-synth.sh` failed with "Argument list too long" and the
bash still printed the success banner).

## Contract (unchanged from the bash)

```
rule.sh                                                # every caller, unchanged — concierge.sh, the suites
rule [--home <spira-dir>] enact <slug> "<text>" (--tool <cmd> | --mechanism <bead> | --tool none --reason <why>) --part <n> [--dry-run]
rule code [--check] [--texts <dir>]   # regenerate or check the law/ tree (law/toc.toml + statute texts); enact appends to a Part and stages it
rule [--home <spira-dir>] audit [--file --for <persona> [--repo <name>] --max N]
rule [--home <spira-dir>] retire <slug> --reason <why> [--replaced-by <slug> | --merged-into <slug>]
rule [--home <spira-dir>] rename-vocabulary [--dry-run]
rule [--home <spira-dir>] list
rule [--home <spira-dir>] show <slug> | <N.M> | <§N.M>
rule [--home <spira-dir>] render-memories [prefix-csv] [char-budget] [core-csv]
rule system-prompt-split <sysfile> <taskfile> <statutes> <prompt>
```

`enact` finds the home repository through the repo map, never the caller's directory. It refuses text the `inventory` fence of spira-lint would refuse, then writes the statute to the store, regenerates the wiki page and files no bead of its own. It then lands the whole statute book: `code::catch_up` appends every store slug the landing-ref toc and `law/pointers.toml` do not name (`code::behind`) under Part 1 in store key order, the enacted slug under its `--part`, and `law/` is committed and submitted under one bead exactly as `retire` does it (`retire` catches up the same way). `commit_law` cuts from the newest unlanded `law/*` or `spira/*` branch whose change against the landing ref stays within `law/` and `spira/statutes/`, so two writes made before either lands do not conflict: the second fast-forwards. `rule code --land` is the same landing with no store write, for catch-up and retry, and says `law/: book is up to date with the store.` when nothing is behind or drifted. A landing failure appends one line to `<SPIRA_RUN>/law-book.log` (a `law-book` row in `spira/watchers`, so it reaches the Concierge inbox) and exits 1; the statute stays in force. `rule code --check` reports the gap as `law/: behind the store by N: <slugs>` and exits 1. `rule.sh code` writes the seed texts under `spira/statutes/` along with law/, and removes any seed that is not a numbered statute (a runbook copy or a retired slug). The book holds `law-` keys only: a store key of another family is neither counted behind nor carried from an in-flight toc, and a dated `(per <Name>, YYYY-MM-DD…)` provenance mark is dropped wherever it sits in a text, a trailing one keeping its full stop.

`<slug>` is written without the `law-` prefix; it is added for you (stripped once if
already present, so `rule enact law-foo ...` and `rule enact foo ...` enact the same key).

**`render-memories`/`system-prompt-split`** (wave 4.35, sp-kelr2, wave4-decomposition.md row
Q) are `spira/lib.sh`'s `render_memories`/`system_prompt_split` shim doors, not statute
writes, and do NOT share the `enact`/`retire`/`list`/`show` `$SPIRA_DB/.beads` gate above —
`render-memories` can run entirely off the `SPIRA_MEMORIES_CMD` test seam with no database
at all (the retired render-memories suite's own shape), and `system-prompt-split` touches no database
ever. `render-memories` additionally reads `SPIRA_STATUTE_CORE` (when `core-csv` is omitted
or empty), `SPIRA_REPO` (the index tier's `rule.sh show` hint — `<harness>` when unset),
`SPIRA_MEMORIES_CACHE`/`SPIRA_MEMORIES_CACHE_AGE` (the read-through cache), and
`SPIRA_MEMORIES_CMD` (the test seam — unset, it reads `bdq memories --json` for real). On a
cache miss it shells to `bdq`, not `bd` directly, so bdq's own fences and retry loop still
apply to this read — unchanged from the bash's `bdjson memories`.

Env read directly (the same contract `rule.sh` had — nothing sources `conf.sh` a second
time inside this binary; the shim already did, so every var below is already exported by
the time `rule` runs):

- `SPIRA_DB` — the statute book. Refused (exit 1) when `$SPIRA_DB/.beads` is not a
  directory — never falls through to `bd`'s own auto-discovery of some other store.
- `SPIRA_WIKI_HOOK` — the synthesis hook, default `<home>/law-synth.sh`.
- `SPIRA_WIKI`, plus `<SPIRA_WIKI>/.git` — when present, `wiki-commit.sh` commits the
  regenerated page.
- `SPIRA_MEMORIES_CACHE` — cleared (best-effort `rm -f`) after every write.

`bd` is invoked by literal name (never `$SPIRA_BD` — `rule.sh` never read that variable
either; preserved as-is rather than "fixed" here, since widening it is a separate, untested
behaviour change outside this bead's scope).

**`conf.sh` derives `SPIRA_MEMORIES_CACHE`'s default as a plain shell variable, never
exported** — harmless for the bash `rule.sh`, which ran in the same process as `conf.sh`,
but invisible to a binary the shim `exec`s into. The shim re-exports it (and `SPIRA_DB`,
`SPIRA_WIKI`, `SPIRA_WIKI_HOOK`, already in `conf.sh`'s own export list but re-exported
defensively) immediately after sourcing `conf.sh` — see `bead/DESIGN.md`'s "Parity" section
for the sibling defect this same shape caused in `bead.sh`, caught by a suite that relies on
a derived default rather than setting the variable explicitly.

## What ported vs what stayed bash

**Ported (this crate's own logic, now typed and unit-tested):**
- Argument parsing for `enact` (one quoted text argument, `--dry-run` the only flag,
  anything else refused by name) and the ≤130-word statute-length refusal.
- `slugify` (`law-` prefix, stripped once then re-added).
- The `list` line format and sort order; JSON parsing (`serde_json`, not the bash's own
  `python3 -c` one-liners — same fields, same defaults, one fewer process per call).
- The "database unreachable" vs "statute book genuinely empty" distinction (`memories_json`)
  — `bd`'s own exit status is checked before anything is parsed, exactly as the bash fixed
  it for sp-n93br.

**Ported since (wave 4.35, sp-kelr2, row Q):** `render_memories`'s tiering/cache logic
(`rule::memories::render` plus `main.rs`'s cache read/write and `SPIRA_MEMORIES_CMD`/`bdq`
seam — same contract, one fewer `python3` process per call) and `system_prompt_split`'s
split (`rule::memories::split`). `lib.sh`'s own two functions are now one-line shims onto
`render-memories`/`system-prompt-split` above.

**Stayed bash, invoked as a subprocess:**
- `law-synth.sh` (or `$SPIRA_WIKI_HOOK`) — the wiki regeneration hook itself.
- `wiki-commit.sh` — the wiki commit.
- `bd`/`bdq` — the statute book's and the memories read's only writer/reader.
- `conf.sh` itself — parsing `spira.conf`/`spira.toml` is its own, separate rewrite
  (wave4-decomposition.md beads 4–10), not this crate's.

## Parity

See the sibling bead report: old (`rule.sh`) vs new (`rule` through the `rule.sh` shim) run
side by side against the same fixture database for `enact`/`retire`/`list`/`show`, including
the dry-run path, the overwrite-warning path, the word-limit refusal, an unreachable
database, and a missing/non-executable synth hook. Exit codes and stdout/stderr text match
byte for byte except one named, intentional simplification: `memories_json`'s error text
concatenates stdout then stderr in call order rather than byte-interleaving them the way a
shell's `2>&1` would under real concurrent output — `bd` writes only one of the two on any
call this binary makes, so the two are never observably different.

## Decisions

- **The shim still sources `conf.sh`**, exactly as `gate.sh` does, rather than teaching this
  binary to parse `spira.conf`/`spira.toml` itself — that parsing is conf.sh's own, deferred
  rewrite (group 4).
- **`bd` is called by literal name**, not `$SPIRA_BD`, because that is what `rule.sh` did;
  widening it is a separate, untested change and out of scope for a parity port.

## Retired vocabulary is refused before anything is written

`enact` (which is also the amend, and includes `--dry-run`) exits 1 when the slug or the text holds a
term spira-lint's `ffx-vocabulary` would flag, printing `"term" → "replacement"` per term. The check
uses that rule's own table (`spira_lint::rules::ffx_vocabulary`), so an accepted statute projects
`law/` and `spira/statutes/` files that pass the lint. A statute that must quote a retired term
carries `ffx-ok` in its first sentence: only that sentence reaches each Part's `index.md`, where the
lint fires. `retire` writes no text and is not checked.

`rename-vocabulary` migrates the statutes that predate that refusal, using the same table
(`ffx_vocabulary::rewrite`). A statute whose text alone holds a term is amended in place; one whose
slug holds a term is enacted under the renamed slug and the old key forgotten. Every rename leaves a
`[[retired]]` pointer to the new slug: an old slug not yet numbered is numbered first
(`vocab::record_renames`), so `check_pointers` keeps holding. The rewrite is grammatical: a
singular determiner before "summons" or "chamber" gives "call" or "rune", and the article fix <!-- ffx-ok -->
reaches slugs. A bare "summons" (no determiner, no count before it) is unreadable as singular or <!-- ffx-ok -->
plural and is listed as unresolved. A rename onto a key in force, and a term with no replacement,
are listed and left, exit 1; the rest is applied and `law/` landed once. `--dry-run` prints each
statute and, under it, each changed sentence as a `-` old and `+` new line (a rename prints
`old → new`), and writes nothing.

## Pointers: a change to the book leaves one, never a gap

A statute's slug is its identity; its `§Part.Section` is presentation, derived from
`law/toc.toml`, and may be renumbered by editing the toc and running `rule code`. Whatever
moved, merged or retired leaves a record in `law/pointers.toml`, an input like `toc.toml`
(`write_tree` never deletes it; `drift` validates that every slug it names is in the toc).
It carries `version = 1` and refuses unknown fields. Three entry kinds:

- `[[moved]]` `section`, `slug`, `on` — appended by `rule code` for every slug whose section
  differs from the generated file names on disk. A toc that drops a slug that held a number
  is refused by name: it stays in the toc until retired.
- `[[merged]]` `from`, `into`, `reason`, `on` — written by `retire --merged-into`.
- `[[retired]]` `slug`, `reason`, optional `replaced_by`, `on` — written by `retire`. The slug
  keeps its number and renders `[Retired <on>] <reason>` (plus `Replaced by §N.M <slug>.`).
  `[Reserved]` remains only for a slug with neither record.

`code::resolve` takes `law-x`, `x`, `N.M` or `§N.M` (a current section wins over a moved
entry), follows moved → merged → retired-with-replacement for as many hops as there are, one
printed line per hop, and refuses a cycle by naming it. It misses only for what no record ever
held. `rule show` and `work law` both call it, so a lookup never answers "not found" for
anything that once existed.
