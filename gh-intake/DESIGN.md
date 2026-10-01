# gh-intake — GitHub issue triage gate

Replaces `spira/gh-intake.sh` (385 lines, deleted, sp-8fsql). One binary, `gh-intake`, the
same argv shape (`gh-intake [--dry-run]`) and the same environment variables. Written from
the script's intent and its one real caller (`systemd/spira-gh-intake.service`), not ported
line by line (law-rust-rewrites-start-from-intent).

Bidirectional since sp-j3fim ("wave 4.31"): §§1-6 below are the one-way ingest gate as
originally written; §7 is the write-back half — `gh-intake closeout`/`unlanded-scan`/
`backfill` — ported from `spira/lib.sh`'s GitHub-closeout family (AB,
wave4-decomposition.md row AB) rather than from a bash script of its own.

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

- **No credential, ever.** `Http::get` (`ports.rs`) has no parameter through which a token
  could travel, and `real.rs`'s implementation never reads `GITHUB_TOKEN` or sets an
  `Authorization:` header. `spira-lint`'s `gh-intake-lint` rule used to police exactly this
  by scanning `spira/gh-intake.sh` for a mutating curl flag or a credential reference; it is
  retired in this same change (spira-lint/DESIGN.md, "Rule `gh-intake-lint` — RETIRED") because
  the property is now structural rather than scanned — there is no bash text left to carry
  the regression the rule was watching for. `spira/test-gh-intake.sh`'s case 3 ("no credential
  ever reaches the wire") checks it behaviourally instead: a stub HTTP server records every
  header a real run sends and a canary `GITHUB_TOKEN` value that must never appear on the wire.
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
| `SPIRA_BD` | `bd` | the `bd` binary |
| `SPIRA_MAIL_BIN` | `mail` | the digest mailer |
| `SPIRA_HOME` | `.` | where `lib.sh` lives, for `repo_root` |
| `SPIRA_GH_INTAKE_REPO` | *(empty)* | `owner/repo` to ingest; empty is a real no-op, exit 0 |
| `SPIRA_GH_INTAKE_BEAD_REPO` | `repo`'s basename | the `repo:` label and repo-map lookup |
| `SPIRA_SCOPE_LABEL` | `spira` | the label a fayth's predicate reads |
| `SPIRA_PLAN_LABEL` | `plan` | the lane label |
| `SPIRA_GH_INTAKE_PRIORITY` | `1` | must be `0`-`4`, checked before anything else runs |
| `SPIRA_GH_INTAKE_API` | `https://api.github.com` | the API base, for a fixture server |

### 3.3 Flow

1. Empty `SPIRA_GH_INTAKE_REPO` → log and exit 0. Nothing else runs.
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
   no fayth can claim is treated as not having been filed correctly at all.

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
  `sentinel`/`queue`/`aeon`/... already use). `gh-intake` is a workspace `[[bin]]` target,
  so `build-tarball.sh --workspace`/`--bin-dir` ships it with no new flag.
- `spira/lib-test-install.sh`'s `INSTALL_FIXTURE_UNIT_BINS` gained `gh-intake` so the
  install-fixture suites stage a stub at `bin/gh-intake` the same way they already do for
  `sentinel`/`queue`/`aeon`.
- `spira/test-install-hooks-artifact.sh` no longer stubs a no-op `gh-intake.sh` script (the
  unit no longer names one).
- `spira/test-gh-intake.sh` is rewritten as a thin T1 wiring smoke test of the real binary
  (§5 lists what it and the cargo suite each cover) rather than sourcing the deleted
  script's functions under `GH_INTAKE_LIB=1`.
- `spira/config-fence-allow`'s `spira/gh-intake.sh` entry is removed along with the file it
  named; `spira/test-gh-intake.sh`'s entry stays — that file still exists.
- `spira-lint`'s `gh-intake-lint` rule is retired (deleted from `all_rules()`, its module and
  source file removed) along with every caller that named it by string: `gate.steps`'s
  comment, `.github/workflows/gate.yml`'s `spira-lint --only gh-intake-lint` Lints step,
  and `spira/repo-map.example`'s illustrative gate-string comment.

## 7. The closeout half (sp-j3fim, "wave 4.31")

The write-back complement to §§1-6's one-way ingest: `gh_issue_closeout`,
`_gh_close_ask_unblock`, `gh_issue_ask_unlanded`, `_gh_resolve_stale_asks` and
`_gh_unlanded_scan` ported natively from `spira/lib.sh` (wave4-decomposition.md row AB)
into `closeout.rs`, plus `ask_already_open`/`ask_closed_subject` — family C's dedupe
primitives sp-31hjr ("wave 4.30") left in lib.sh because this family was the last bash
caller naming them directly. Intake still holds no credential; the closeout half runs
only from the credentialed landing path, exactly as the lib.sh comment on
`gh_issue_closeout` said, and sends nothing to GitHub but a commit sha/subject (already
public) or a fixed sentence (law-beads-is-never-public).

### 7.1 Invocation

```
gh-intake closeout <bead-id> <sha> <repo-path>   gh_issue_closeout alone
gh-intake unlanded-scan                          _gh_unlanded_scan (one full pass)
gh-intake backfill [--dry-run]                   replaces spira/gh-issue-backfill.sh
```

### 7.2 Environment (closeout-side verbs only)

| var | default | meaning |
|---|---|---|
| `SPIRA_DB` / `SPIRA_BD` | *(required)* / `bd` | the beads store |
| `SPIRA_HOME` | `.` | the repo registry's root for `spira_config::repos` |
| `SPIRA_RUN` | *(required)* | `gh-closed/`, `landstate/` and `gh-wait-log/` all live here |
| `SPIRA_ASK_LABEL` | *(empty)* | the label an operator ask carries |
| `SPIRA_GH_ASK_GRACE_SECS` | `3600` | how long after `closed_at` before an unlanded bead is asked about |
| `SPIRA_MAIL_BIN` | `mail` | the operator-ask sender |

### 7.3 Ports added (`ports.rs`)

`Gh` (shells to `ghq` — bead::bdq's own `__ghq`, `timeout "${GH_TIMEOUT:-120}"
"${SPIRA_GH:-gh}" "$@"` — never the `gh` binary directly) and `Git` (plain `git -C`,
read-only). `Bd` gained `show_json`/`list_by_label`/`dep_remove`/`dep_relate`; `Mail`
gained `send_question`. `$SPIRA_RUN`'s own state (`gh-closed/<id>` markers, the
`landstate/<id>` read, the `gh-wait-log/<id>` throttle) is plain `std::fs` inside
`closeout.rs`, not behind a port — the same choice `landing-pass/src/landstate.rs` made,
single-process file I/O with nothing to fake.

### 7.4 Two readers of "is this landed", deliberately not unified

`gh_unlanded_scan`'s `landed_sha` (subject-shape anchored: `spira: land <id>` or
`<id>:...`, `-F` fixed-string `--grep`, both the base ref and its local counterpart) and
`backfill`'s own `grep_ancestor` (loose: `git log --grep=<id>` on the base ref alone, no
subject-shape check, first hit wins) are two separate `Git` methods, not one shared
implementation — `spira/test-land-commit-contract.sh` ("gap G4") proved only that they
*agree* on the one fixture that matters (a real `spira: land <id>` commit), not that they
are the same algorithm. Unifying them would be a behaviour change this bead does not make.

### 7.5 `ask_already_open` is gh-intake's own copy, not a shared one

sp-31hjr gave sentinel (`pass.rs`) and landing-pass (`real.rs`'s `RealBeads::ask_open`)
each their own native copy rather than a shared library dependency between binaries; this
crate's `closeout.rs` copy is the third, following the same precedent (see also
`sending`'s/`cockpit-collect`'s/`sentinel`'s own separate copies of the subject-grep
"landed" shape). lib.sh's bash copy retires outright with this bead — no caller is left
anywhere in the tree.

### 7.6 Callers repointed

- `landing-pass/src/real.rs`: `Lib::closeout`/`Lib::gh_unlanded_scan` shell to `gh-intake
  closeout`/`gh-intake unlanded-scan` by bare name now (stdout relayed through this
  pass's own `Reporter::raw`); `Op::Closeout`/`Op::GhUnlandedScan` are gone from
  `seam.rs` — no lib.sh snippet backs either op any more.
- `queue/src/real.rs`: `Lib::gh_issue_closeout` shells to `gh-intake closeout` the same
  way `land_mark` already shells to `landing-pass mark`; `Op::GhCloseout` is gone from
  `seam.rs`.
- `spira/gh-issue-backfill.sh` is deleted; `gh-intake backfill` replaces it.
  `spira/test-land-commit-contract.sh`'s "other reader" section calls the new verb.
- `spira/test-gh-issue-closeout.sh` is rewritten as a thin T1 wiring smoke test of the
  real binary (closeout, backfill, unlanded-scan — both directions of the ask dedupe),
  the same role `spira/test-gh-intake.sh` plays for the ingest half; the exhaustive logic
  moved to `cargo test -p gh-intake`'s own `closeout` module. Its `config-fence-allow`
  entry is removed (the rewrite names no `spira.toml`/repo-map literal).

### 7.7 Tests

`closeout::tests` (20, inside `closeout.rs`, `cargo test -p gh-intake`): every pure
parser/formatter against lib.sh's exact text; `gh_issue_closeout` comments+closes, is
idempotent, no-ops on a non-github bead, marks-without-closing an already-forge-closed
issue; `gh_issue_ask_unlanded` dedupes BOTH directions (files a missed ask, suppresses a
duplicate) and separately proves an answered ask writes the durable marker instead of
re-asking (the Defect-2 regression the original bash suite carried); mail refusal and the
empty-stderr probe-fault case; `_gh_close_ask_unblock` converts a blocking dep and leaves
a non-blocking one alone; `_gh_resolve_stale_asks` closes a stale ask once its issue is
closed on the forge; `gh_unlanded_scan` distinguishes a landed-by-commit bead from a
truly unlanded one in the same pass, waits silently (and throttles) on an in-flight
landstate, and respects the grace period; `backfill`'s dry-run/real pass pair, via its
own looser ancestry search. All against fakes — no network, no real `bd`/`ghq`/`mail`/
git. `spira/test-gh-issue-closeout.sh` (§7.6) is the real-binary complement.
