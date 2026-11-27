# gh-intake — GitHub issue triage gate

Replaces `spira/gh-intake.sh` (385 lines, deleted, sp-8fsql). One binary, `gh-intake`, the
same argv shape (`gh-intake [--dry-run]`) and the same environment variables. Written from
the script's intent and its one real caller (`systemd/spira-gh-intake.service`), not ported
line by line (law-rust-rewrites-start-from-intent).

Bidirectional since sp-j3fim ("wave 4.31"): §§1-6 below are the one-way ingest gate as
originally written; §7 is the write-back half — `gh-intake closeout` and
`closeout-pending`.

## 1. Intent

A public GitHub tracker feeds Spira's work queue without ever giving Spira write access to
GitHub or letting an untrusted stranger's issue text execute as an instruction. Every open
issue is triaged by GitHub's own access control — `author_association` — into a work bead
(trusted) or an untrusted record plus a digest (everyone else), with one escape hatch:
a trusted collaborator can apply the `spira:accept` label, checked again at promotion time
rather than trusted as a label alone (law-a-pattern-match-is-not-an-identity-check).

Re-running never double-files: the external ref (`github:<repo>#<n>` /
`github-untrusted:<repo>#<n>`) is the join key, exactly as gh-intake.sh keyed it.

## 2. What is deliberately NOT touched

- **No caller-chosen credential.** `Http::get` (`ports.rs`) has no parameter through which a
  token could travel; `real.rs`'s `RealHttp` attaches the forge GitHub App installation token
  (`broker token`, minted once per run, never logged) to every request, and never reads
  `GITHUB_TOKEN` or any personal token. Unauthenticated calls share GitHub's 60/h per-IP
  budget with everything on the host. `spira/test-gh-intake.sh` case 3 checks it
  behaviourally: a stub HTTP server records every header a real run sends.
- **`repo_root`**, unported: `spira/lib.sh` is last in the rewrite order (the inventory,
  group 4 — "leave lib.sh alone" is still Ryan's standing instruction). `RealRepo` shells
  out to `bash -c '. lib.sh; repo_root "$1"'`, the identical boundary gate-check's Rust port
  already draws around this same function.
- **`mail`**, a subprocess call (operator surface). Called by bare name, stdin piped, the
  same boundary the bash script used against `mail.sh` before it was rewritten and retired
  by sp-ooh1k (a `mail.sh` compat symlink remains for callers outside this tree, but this
  crate's default already points at the real binary).

## 3. Contract

### 3.1 Invocation

`gh-intake [--dry-run]`. `-h`/`--help` prints usage and exits 0. Any other argument is a
refusal, exit 2 — unchanged from the bash.

### 3.2 Environment

| var | default | meaning |
|---|---|---|
| `SPIRA_DB` | *(required)* | the beads store; missing is a refusal, exit 1 |
| `SPIRA_MAIL_BIN` | `mail` | the digest mailer |
| `SPIRA_HOME` | `.` | where `lib.sh` lives, for `repo_root` |
| `SPIRA_GH_INTAKE_REPO` | *(empty)* | `owner/repo` to ingest in addition to every registered repo with a GitHub remote; empty with none registered is a real no-op, exit 0 |
| `SPIRA_GH_INTAKE_BEAD_REPO` | `repo`'s basename | the `repo:` label and repo-map lookup |
| `SPIRA_SCOPE_LABEL` | `spira` | the label a rune's predicate reads |
| `SPIRA_PLAN_LABEL` | `plan` | the lane label |
| `SPIRA_GH_INTAKE_PRIORITY` | `1` | must be `0`-`4`, checked before anything else runs |
| `SPIRA_GH_INTAKE_API` | `https://api.github.com` | the API base, for a fixture server |

### 3.3 Flow

1. Targets are `SPIRA_GH_INTAKE_REPO` (filed against `SPIRA_GH_INTAKE_BEAD_REPO`) plus every
   repo-map entry whose checkout has a GitHub remote (`origin`, else the first), filed against
   that entry. No target → log and exit 0. Steps 3–7 run per target; a refusal on one does
   not skip the rest, a rate-limit deferral ends the pass. A repo is read for the first time
   when it has no `Seen` file and no bead carries its ref prefix: its open issues are written
   to `$SPIRA_RUN/gh-intake.seen/` and nothing is filed; later passes skip those numbers.
2. `repo_root(bead_repo)` must resolve AND hold a `.git` — otherwise refuse (exit 1) with the
   same remediation text the bash gave (add to the repo-map, or set the bead-repo override).
3. `bd list --all --json`: total beads must be `> 0` (a positive control — a store that
   reports zero is refused as *broken*, never read as *empty*,
   law-absence-needs-a-positive-control) and the `github`-prefixed subset splits into
   known work (has the scope label) and known untrusted records.
4. Page `GET /repos/:r/issues?state=open&per_page=100` up to 20 pages (2000 issues); a page
   that parses as a JSON object is GitHub's own error shape and is a refusal carrying that
   message; a page that is not JSON at all is a refusal with a fixed message. Pull requests
   (any object carrying a `pull_request` key) are dropped.
5. Each issue, in ascending number order:
   - Already a known work bead (by ref) → note the re-ingest on the existing bead (or, under
     `--dry-run`, print what would be noted) and move on. No second bead, ever.
   - `author_association` in `OWNER`/`MEMBER`/`COLLABORATOR` → trusted outright.
   - Otherwise an author whose current access is admin/maintain/write (`actor_has_access`) is
     trusted too: a private org member reads as `CONTRIBUTOR` even to the App.
   - Otherwise, carrying the exact label `spira:accept` → resolve the actor who most
     recently applied it (the issue's events timeline) and check that actor's CURRENT access
     (org membership, then collaborator permission — `admin`/`maintain`/`write` only) before
     trusting it. The label's presence is never sufficient by itself.
   - Trusted → close any existing untrusted record for this issue (best-effort), then file a
     work bead: `--external-ref github:<repo>#<n> --labels <scope>,<lane>,repo:<bead_repo>
     -t bug -p <priority>`, body = the issue text quoted between `[UNTRUSTED TEXT]` markers
     with its author and a SHA-256 of the body recorded above the quote.
   - Not trusted, and not already recorded → file an untrusted record: label
     `gh-untrusted` alone, priority 4, same quoting convention. Collected for the digest.
6. A non-empty batch of NEW untrusted records (real run only) → one digest mail to
   `operator`, `--kind note`, listing each with a 3-line body preview.
7. **The post-check** (real run only): re-read the store; any `github:`-prefixed bead now
   missing the scope label is a refusal (exit 1) naming every offending id — filed work that
   no rune can claim is treated as not having been filed correctly at all.

### 3.4 Ports (`ports.rs`)

`Http`, `Bd`, `Repo`, `Mail` — four traits, no credential anywhere in any of their
signatures. `logic::run()` takes `&dyn` references to all four plus a `Config`, and is
otherwise pure: every test in `tests.rs` drives it through fakes, replacing the bash's
`GH_INTAKE_LIB=1` source-and-call-directly seam (dispatch.md, "GitHub triage/dedup") with
ordinary dependency injection — the actual boundary law-prefer-the-real-dependency asks for,
rather than an env var that turns half the script into a library.

## 4. Parity — named differences from the bash

- **A page whose body is not a JSON array or object** (extremely unlikely from GitHub, never
  observed) is refused here; the bash's python would call `len()` on whatever it decoded
  (a string's character count, say) and could misjudge whether to keep paging. Fail-closed
  is the documented replacement.
- **An issue object missing a `number` field** is skipped, not fatal; the bash's python
  would raise an uncaught `KeyError` inside the page's `for` loop, losing every issue on that
  page silently (mapfile then sees truncated or no output). GitHub has never been observed
  to omit `number`; this only matters against a hostile or badly broken fixture server.
- **`bd list --all --json` failing to invoke at all** is reported as "could not read the
  store" here, distinct from "the store reports zero beads." The bash conflated both into
  the zero-beads message (its python printed `0` on any parse exception). Both are still
  refusals with exit 1; only the wording differs, and the more specific wording is the
  positive-control discipline this codebase already asks of every other fail-closed check.

No other behavioural difference is intended. Everything else — argv, every environment
variable and its default, the triage decision tree, the external-ref join keys, the label
sets, the priority bands, the digest shape, the post-check — is load-bearing and preserved.

## 5. Tests

`cargo test -p gh-intake`: 18 tests over `logic::run` and the pure `model.rs` predicates —
trusted creation, idempotent re-run (note not a duplicate), untrusted recording plus digest,
PR exclusion, `spira:accept` promotion both ways (access granted / denied), `--dry-run`
makes no store writes, pagination across a full page, an unresolvable repo-map entry, a
zero-bead store, an API refusal surfaced rather than swallowed, and the post-check catching
a store-side labelling defect. No network, no real `bd`, no real `mail.sh`. §7.7 below
covers the closeout half's own 20.

## 6. Callers repointed

- `systemd/spira-gh-intake.service`: `ExecStart=@SPIRA_PROD@/gh-intake.sh` →
  `ExecStart=@SPIRA_PROD_ROOT@/bin/gh-intake` (sp-gypjk's own convention — the same one
  `sentinel`/`queue`/`dwarf`/... already use). `gh-intake` is a workspace `[[bin]]` target,
  so `build-tarball.sh --workspace`/`--bin-dir` ships it with no new flag.
- `spira/test-gh-intake.sh` is rewritten as a thin T1 wiring smoke test of the real binary
  (§5 lists what it and the cargo suite each cover) rather than sourcing the deleted
  script's functions under `GH_INTAKE_LIB=1`.
- `spira/config-fence-allow`'s `spira/gh-intake.sh` entry is removed along with the file it
  named; `spira/test-gh-intake.sh`'s entry stays — that file still exists.
- `spira-lint`'s `gh-intake-lint` rule is retired (deleted from `all_rules()`, its module and
  source file removed) along with every caller that named it by string: `gate.steps`'s
  comment, `.github/workflows/gate.yml`'s `spira-lint --only gh-intake-lint` Lints step,
  and `spira/repo-map.example`'s illustrative gate-string comment.

## 7. The closeout half

The write-back complement to §§1-6's one-way ingest. Intake holds no credential; this half
runs only from the credentialed side, and sends GitHub nothing but a commit sha/subject
(already public) or a fixed sentence (law-beads-is-never-public).

The lifecycle store records a pending closeout (`gh-closeout/<bead>`) in the transaction that
moves a bead whose external reference starts with `github:` to LANDED, SUPERSEDED or DROPPED.
Nothing here finds work by listing beads: the drain reads the pending rows, then each bead by
id.

### 7.1 Invocation

```
gh-intake closeout <bead-id> <sha> <repo-path>   one issue, by hand
gh-intake closeout-pending                       one drain of the pending rows
```

The `intake.gh_closeout` runtime task runs the drain when `spira-lc gh-closeout pending` is
not empty.

### 7.2 The drain (`closeout::drain`)

Per row: LANDED comments on and closes the issue with the delivery's sha. SUPERSEDED and
DROPPED follow `provenance::trace` over point reads: a settled non-landing outcome is
reported on the issue; a landed successor closes it with the successor's sha; a successor
still in flight leaves the row untouched; anything else asks the operator once, and the ask's
id is recorded on the row (`gh-closeout asked`), which is what stops a second ask. A row with
an ask is done once its issue is closed on GitHub (the ask is closed by id) or the ask is
answered. A failed GitHub call leaves the row and bumps `attempts` (`gh-closeout failed`); the
next drain retries it. The comment is posted once however often the close is retried
(`$SPIRA_RUN/gh-reported/<id>`); `gh-closed/<id>` marks a settled issue.

### 7.3 Ports

`Closeouts` (pending, done, asked, failed), `Lifecycle` (`bead` by id only), `Bd`
(`show_json` by id, `close`, `dep_remove`/`dep_relate`), `Gh`, `Git`, `Repo`, `Mail`.
`Mail::send_question` returns the ask bead's id. `$SPIRA_RUN`'s markers are plain `std::fs`.

### 7.4 Tests

`closeout::tests`: a drain over a LANDED, a won't-fix DROPPED and a SUPERSEDED-then-LANDED row
records only `pending`, `bead`, `show_json` and `done`, comments once per issue, and a second
drain does nothing; a failed close keeps the row with `attempts` bumped and the next drain
closes once; a successor in flight leaves the row; an unnamed drop asks once; an ask whose
issue closed on GitHub is closed by id. All against fakes.
