# rule — design

Replaces the logic of `rule.sh` (root of the harness, ~220 lines) with one Rust binary,
`rule`. `rule.sh` stays as the one entry point every caller already names — `concierge.sh`,
`cockpit/db.sh`'s comment, every runes brief that tells an agent `rule.sh enact <slug>
"<text>"`, and the suites — the same shim pattern `gate/DESIGN.md` established for `gate.sh`:
source `conf.sh` for its side effects (env, PATH), then `exec -a "$0" rule --home
"$(dirname "$0")/spira" "$@"`. Bead: sp-g9mhe (rewrite wave 5f).

## Intent

`rule` is the one command that writes a statute: enact writes it to the statute book (the
ONE store, no second copy, reached only through `spira-lc content`) and regenerates the wiki page that is its only git-backed copy, in that
order, because a rule that depends on remembering the second step is a resolution, not a
mechanism. `list` and `show` read it back.

**FAIL CLOSED.** A book that does not answer, or answers with something unreadable, is an
error and never an empty book. A synthesis
hook that is missing, non-executable, or exits non-zero is reported as "the database write
succeeded, the wiki page was NOT regenerated" — never silently dropped (the defect rule.sh
itself exists to fix, sp-p0xyt: `law-synth.sh` failed with "Argument list too long" and the
bash still printed the success banner).

## Contract (unchanged from the bash)

```
rule.sh                                                # every caller, unchanged — concierge.sh, the suites
rule [--home <spira-dir>] enact <slug> "<text>" (--tool <cmd> | --mechanism <bead> | --tool none --reason <why>) --part <n> [--dry-run]
rule code [--check] [--texts <dir>]   # regenerate or check the book in the wiki checkout (toc.toml + statute texts); enact appends to a Part and commits it there
rule [--home <spira-dir>] audit [--file --for <persona> [--repo <name>] --max N]
rule [--home <spira-dir>] retire <slug> --reason <why> [--replaced-by <slug> | --merged-into <slug>]
rule [--home <spira-dir>] rename-vocabulary [--dry-run]
rule [--home <spira-dir>] list
rule [--home <spira-dir>] show <slug> | <N.M> | <§N.M>
rule [--home <spira-dir>] render-memories [prefix-csv] [char-budget] [core-csv]
rule system-prompt-split <sysfile> <taskfile> <statutes> <prompt>
```

`enact` refuses text the `inventory` fence of spira-lint would refuse, then writes the statute to the store, regenerates the wiki page and files no bead of its own. It then writes the whole statute book into the brain checkout: the `SPIRA_LAW_BOOK` directory (default `wiki/law`) of `SPIRA_WIKI`, resolved by `code::book_dir` (an empty key, or a `SPIRA_WIKI` with no `.git`, is refused by name). `code::catch_up` first drops every toc slug the store lacks (`code::without_missing`; `code::in_force` is the same step plus the moves, for `rule code`), then appends every store slug the book's toc and `pointers.toml` do not name (`code::behind`) under Part 1 in store key order, the enacted slug under its `--part`; `land_book` renders `toc.toml`, `pointers.toml`, `index.md` and the Part trees, takes the paths git reports changed under the book, and commits and pushes only those through `wiki-commit.sh` under `law: <verb> <key>` (`retire` catches up the same way, after leaving a `retired-law-*` record in the store). No branch, worktree, bead or submit is made and the harness landing ref does not move. Each write regenerates the whole book from the store, so concurrent writers converge and `rule code --land` is the retry: it says `law: book is up to date with the store.` when nothing is behind or drifted. A write failure appends one line to `<SPIRA_RUN>/law-book.log` (a `law-book` row in `spira/watchers`, so it reaches the Concierge inbox) and exits 1; the statute stays in force. `rule code --check` reports the gap as `behind the store by N: <slugs>` and exits 1; `rule code` with no `--check` writes the book without committing it. The seed texts under `spira/statutes/` are `spira/seed.sh`'s shipped set and `rule` never writes them. The book holds `law-` keys only: a store key of another family is not counted behind, and a dated `(per <Name>, YYYY-MM-DD…)` provenance mark is dropped wherever it sits in a text, a trailing one keeping its full stop.

`<slug>` is written without the `law-` prefix; it is added for you (stripped once if
already present, so `rule enact law-foo ...` and `rule enact foo ...` enact the same key).

**`render-memories`/`system-prompt-split`** (wave 4.35, sp-kelr2, wave4-decomposition.md row
Q) are `spira/lib.sh`'s `render_memories`/`system_prompt_split` shim doors, not statute
writes. `render-memories` can run entirely off the `SPIRA_MEMORIES_CMD` test seam with no database
at all (the retired render-memories suite's own shape), and `system-prompt-split` touches no database
ever. `render-memories` additionally reads `SPIRA_STATUTE_CORE` (when `core-csv` is omitted
or empty), `SPIRA_REPO` (the index tier's `rule.sh show` hint — `<harness>` when unset),
`SPIRA_MEMORIES_CACHE`/`SPIRA_MEMORIES_CACHE_AGE` (the read-through cache), and
`SPIRA_MEMORIES_CMD` (the test seam — unset, it reads the content door). On a
cache miss it sends one `content memories --json` request.

## The content port

`rule::book::Book` is every call to the store, over `spira_store::lc_call::LcClient`; statute
text travels as the request body. The service names the store, so `rule` reads neither
`SPIRA_DB` nor a `.beads` directory.

| operation | requests |
|---|---|
| `enact` | `content recall K`, `content memories --json`, `content remember --key K` |
| `retire` | `content memories --json`, `content remember --key retired-K` (not for a merge), `content forget K` |
| `rename-vocabulary` | per entry `content remember --key K` then `content memories --json`; a rename adds the record's `remember` and `content forget OLD` |
| `list`, `show`, `code` | `content memories --json`, only for a book never mirrored |
| `audit --file` | `content list --brief --json --limit 0` (the beads not closed); unreadable files nothing |

A `remember` that reports an error is judged by reading the book back: in force, not written,
or unknown.

Env read directly (the same contract `rule.sh` had — nothing sources `conf.sh` a second
time inside this binary; the shim already did, so every var below is already exported by
the time `rule` runs):

- `SPIRA_WIKI_HOOK` — the synthesis hook, default `<home>/law-synth.sh`.
- `SPIRA_WIKI`, plus `<SPIRA_WIKI>/.git` — when present, `wiki-commit.sh` commits the
  regenerated page.
- `SPIRA_MEMORIES_CACHE` — cleared (best-effort `rm -f`) after every write.

**`conf.sh` derives `SPIRA_MEMORIES_CACHE`'s default as a plain shell variable, never
exported** — harmless for the bash `rule.sh`, which ran in the same process as `conf.sh`,
but invisible to a binary the shim `exec`s into, so the binary resolves it itself
(`memories_cache_path`).

## What ported vs what stayed bash

**Ported (this crate's own logic, now typed and unit-tested):**
- Argument parsing for `enact` (one quoted text argument, `--dry-run` the only flag,
  anything else refused by name) and the ≤130-word statute-length refusal.
- `slugify` (`law-` prefix, stripped once then re-added).
- The `list` line format and sort order; JSON parsing (`serde_json`, not the bash's own
  `python3 -c` one-liners — same fields, same defaults, one fewer process per call).
- The "book unreachable" vs "statute book genuinely empty" distinction (`Book::memories`)
  — the reply's exit status is checked before anything is parsed.

**Ported since (wave 4.35, sp-kelr2, row Q):** `render_memories`'s tiering/cache logic
(`rule::memories::render` plus `main.rs`'s cache read/write and `SPIRA_MEMORIES_CMD`
seam — same contract, one fewer `python3` process per call) and `system_prompt_split`'s
split (`rule::memories::split`). `lib.sh`'s own two functions are now one-line shims onto
`render-memories`/`system-prompt-split` above.

**Stayed bash, invoked as a subprocess:**
- `law-synth.sh` (or `$SPIRA_WIKI_HOOK`) — the wiki regeneration hook itself.
- `wiki-commit.sh` — the wiki commit.
- `conf.sh` itself — parsing `spira.conf`/`spira.toml` is its own, separate rewrite
  (wave4-decomposition.md beads 4–10), not this crate's.

## Decisions

- **The shim still sources `conf.sh`**, exactly as `gate.sh` does, rather than teaching this
  binary to parse `spira.conf`/`spira.toml` itself — that parsing is conf.sh's own, deferred
  rewrite (group 4).

## Retired vocabulary is refused before anything is written

`enact` (which is also the amend, and includes `--dry-run`) exits 1 when the slug or the text holds a
term spira-lint's `ffx-vocabulary` would flag, printing `"term" → "replacement"` per term. The check
uses that rule's own table (`spira_lint::rules::ffx_vocabulary`), so an accepted statute projects
book files that pass the lint. A statute that must quote a retired term
carries `ffx-ok` in its first sentence: only that sentence reaches each Part's `index.md`, where the
lint fires. `retire` writes no text and is not checked.

`rename-vocabulary` migrates the statutes and `sop-*` runbooks that predate that refusal, using the same table
(`ffx_vocabulary::rewrite`). A statute whose text alone holds a term is amended in place; one whose
slug holds a term is enacted under the renamed slug and the old key forgotten. Every rename leaves a
`[[retired]]` pointer to the new slug: a numbered old slug's section passes to the new one
(`vocab::record_renames`) and the rename leaves a `retired-law-*` record in the store. The rewrite is grammatical: a
singular determiner before "summons" or "chamber" gives "call" or "rune", and the article fix <!-- ffx-ok -->
reaches slugs. A bare "summons" (no determiner, no count before it) is unreadable as singular or <!-- ffx-ok -->
plural and is listed as unresolved. A rename onto a key in force, and a term with no replacement,
are listed and left, exit 1; the rest is applied and the book committed once. `--dry-run` prints each
statute and, under it, each changed sentence as a `-` old and `+` new line (a rename prints
`old → new`), and writes nothing; its tail line counts statutes and runbooks apart.

A `sop-*` runbook is rewritten by the same plan and replacements. One whose text alone holds a term is
amended; one whose name holds a term is written under the new name, then a `retired-<old>` record
(`sop::shelf::Retired`, `replaced_by` = the new key, `text` = the old text) is remembered, then the old
key forgotten, and the record forgotten again if that fails. `sop show <old>` follows `replaced_by`:
it prints `<old> was renamed to <new>` and the new text. A runbook rename is neither numbered nor
pointed at in the book, and a target that is a key or has a `retired-` record is a collision. The book is
committed only when a statute was written; otherwise `law: no statute rewritten; nothing to commit.`

## Pointers: a change to the book leaves one, never a gap

A statute's slug is its identity; its `§Part.Section` is presentation, derived from
the book's `toc.toml`, and may be renumbered by editing the toc and running `rule code`. Whatever
moved, merged or retired leaves a record in the book's `pointers.toml`, an input like `toc.toml`
(`write_tree` never deletes it). It carries `version = 1` and refuses unknown fields. The book is
exactly the statutes in force: a slug the store no longer holds leaves `toc.toml` and its section
file, and its pointer stays so the old name still resolves. Three entry kinds:

- `[[moved]]` `section`, `slug`, `on` — appended by `rule code` for every landed slug whose
  section changed. A toc that drops a slug the store still holds is refused by name.
- `[[merged]]` `from`, `into`, `reason`, `on` — written by `retire --merged-into`.
- `[[retired]]` `slug`, `reason`, optional `replaced_by`, `on` — written by `retire` and
  `rename-vocabulary`, and read back from the store's `retired-law-*` records
  (`code::store_pointers`), so a rename or retirement survives the loss of the branch that
  carried it. A successor the store holds and the toc does not yet list takes the old slug's
  section (`code::in_force`); otherwise the section is gone and the later ones renumber.
  `check_pointers` requires what a pointer leads to (`replaced_by`, `into`, `moved.slug`) to be
  in the toc, not the slug it leaves.

`code::resolve` takes `law-x`, `x`, `N.M` or `§N.M` (a current section wins over a moved
entry), follows moved → merged → retired-with-replacement for as many hops as there are, one
printed line per hop, and refuses a cycle by naming it. It misses only for what no record ever
held. `rule show` and `work law` both call it, so a lookup never answers "not found" for
anything that once existed.
