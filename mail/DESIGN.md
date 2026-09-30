# mail — DESIGN

The operator/concierge mail channel, as a binary (sp-ooh1k; `spira/mail.sh` until now).
Rewrite wave 5a.

## 1. Intent

**Every message that reaches the operator or a persona passes through one gate that
enforces transport discipline, and every send is recorded on disk exactly once, atomically,
in a format any Maildir reader (or a bare `cat`) can read.** Mail is not a log: a refusal
names the rule it enforces, a repeat within a window is suppressed and counted, and a reply
closes the tracking bead it answers so "answered" is never re-derived by re-scanning beads
(`law-answers-arrive-as-mail`, brain `feedback_read_the_mailbox_first`).

Three properties this rewrite must not weaken, because other tooling depends on them
exactly as they are today:

- **On-disk Maildir format, headers and exit codes are parity-mandatory.** `spira-mail-deliver.sh`
  polls `new/` for wake triggers, `mail.sh sendmail` is `mail.sh`'s own reply-intake contract
  ("Ryan's answers arrive as mail" — brain CLAUDE.md), and a dozen suites grep specific
  header lines and exit codes.
- **Mail stays muted in production** (`SPIRA_MAIL_MUTE=1/true`, sp-9hwim): delivery still
  lands (in `cur/`, pre-flagged Seen) but wakes nobody. This rewrite must never send real
  mail to the operator while under test — every suite and this design's own tests set
  `SPIRA_MAIL_MUTE` or point `SPIRA_MAIL`/`SPIRA_DB` at fixtures.
- **A refusal names the rule.** Lint, the repeat guard and tidy's bead-store guards all
  fail closed with a message that says which check refused and why, never a bare non-zero
  exit.

## 2. Contract

```
mail send <mailbox> --from "<s>" --subject "<s>" [--kind K] [--urgent]
                     [--default D] [--bead ID] [--digest] < body
mail template <kind>              print the kind's body skeleton
mail list <mailbox> [--unread]
mail read <mailbox> [<message>]   prints, moves new -> cur
mail count <mailbox>              number of unread messages
mail unread-age <mailbox>         seconds since oldest unread; empty if none
mail done <mailbox> <msgid> [note...]
mail sendmail                     RFC 5322 on stdin; closes tracking bead on reply
mail tidy <mailbox> [--dry-run]
mail ensure <mailbox>             create the maildir if absent; no-op if present
mail sweep-dismissed [mailbox]    close asks whose mail was deleted (default operator)
```

Identical surface to mail.sh's, on purpose: every caller is a mechanical repoint (bare name
`mail.sh` → bare name `mail`; no flag, no output shape, no exit code changed), which is what
makes one-change parity across ~80 call sites tractable.

| exit | meaning |
|---|---|
| 0 | the command did what it says (including a `read`/`unread-age` that correctly found nothing to report, where the shape says so) |
| 1 | refused — lint, repeat guard, stdin deadline, unknown mailbox/kind/command, a bead-store guard, or an I/O failure |
| 2 | `ensure` only: mailbox name itself invalid (mail.sh's own `exit 2` for this one path) |

## 3. Structure

- `env.rs` — every environment variable read, with conf.sh-matching defaults (conf.sh has
  already exported these into the process tree by the time a bare-named binary runs, the
  same contract every other crate here holds).
- `maildir.rs` — the Maildir mechanics: validate/exists/ensure/deliver (mute-aware), msgid
  minting, the unread/flag rules, sorted directory listing (matches a bash glob's order).
- `kinds.rs` — `spira/mail/kinds/<kind>.md` parsing: `requires:` frontmatter, `## Section`
  names, and the raw template `mail template` prints.
- `lint.rs` — `_lint_check`, a pure function of its inputs plus the kind files. Every rule
  from mail.sh ported unchanged, with the exact refusal text (test-mail.sh's own `want`
  greps on stderr substrings for this text, so the wording is load-bearing).
- `repeat.rs` — the operator repeat guard: fingerprint (caller/subject/mailbox), a per-
  fingerprint flock held from check to stamp, and (new) a bounded wait instead of a bare
  blocking `flock` (§5, sp-y59a6).
- `bead.rs` — the one seam to `bd`: rendering a cited bead's block, creating/closing the
  tracking bead, the suit-verdict word mapping, and the two store queries `tidy` and
  `sweep-dismissed` make. `Bd` is a trait so every one of these is a unit test against a
  fake, never a real store.
- `message.rs` — reading headers back out of a delivered file (two distinct scans; see §5).
- `sendmail.rs`, `tidy.rs` — the two biggest commands, each with its own module and tests.
- `cmds.rs` — everything else (`send`, `template`, `list`, `read`, `count`, `unread-age`,
  `done`, `ensure`, `sweep-dismissed`).
- `rfc822.rs` — the `Date:` header and `done`'s note-footer timestamp, with no calendar
  dependency (Howard Hinnant's `civil_from_days`; this workspace carries no `chrono`).
- `main.rs` — argv parsing and dispatch only; every behaviour above is reachable without a
  process.

## 4. What was ported, what was dropped

**Nothing was dropped.** Every subcommand, every flag, every lint rule and every kind-file
behaviour mail.sh had is live: `git grep` across the workspace found a real caller for
`send`, `template`, `list`, `read`, `count`, `unread-age`, `done`, `sendmail`, `tidy`,
`ensure` and `sweep-dismissed` alike (operator surface tools rewrite when touched, per the
inventory — this is the first time mail.sh is touched, so this is the first time its whole
surface gets an accounting, and none of it turned out to be dead).

**Implementation details dropped, behaviour kept identical:**

- `python3 -c '...'` JSON parsing (bead rendering, dep-list, history) → `serde_json`. Same
  fields read, same fallback to `"unresolved: <id>"` on anything the JSON doesn't have.
- `awk`/`sed` header scanning → `message.rs`, faithfully split into the *two* scans mail.sh
  actually used (see below) rather than collapsing them into one and quietly changing
  behaviour.
- `sha256sum` (a subprocess) → the `sha2` crate already vendored in this workspace
  (`sentinel::check5` uses the same crate for a similar dedup-fingerprint).

## 5. Decisions

- **Two header scans, not one.** mail.sh used `sed` (whole-file, case-sensitive) for
  `list`/`tidy`'s header reads, and a stricter `awk` (stops at the first blank line,
  case-insensitive) for `sendmail`'s reply-header reads. Collapsing these into a single
  "scan headers" helper would be simpler but is not what mail.sh did, and `sendmail`'s
  choice is deliberate: a body line that happens to start with `Subject:` must not be
  mistaken for a real header. `message.rs` keeps both, named for which caller they serve.

- **The bounded-wait fix for the repeat-guard lock (sp-y59a6) is in scope for this rewrite,
  not deferred.** mail.sh's `_repeat_check` held a bare blocking `flock` with no timeout; a
  stale lock from a crashed invocation hung every later send for that fingerprint forever
  (observed: 30+ minutes). Every check this rewrite ports is required to fail closed when it
  cannot check (wave-brief); rewriting this function from scratch in Rust made "refuse past a
  bounded wait instead of hang" the natural shape, not a bolt-on. Default 30s
  (`SPIRA_MAIL_LOCK_TIMEOUT_MS`); no suite or fixture in this workspace holds the lock
  anywhere near that long (the G-11 race test's hold is ~0.3s), so this is strictly
  additive — real contention behaves exactly as before, only a truly stuck lock now reports
  instead of hanging. `bd show sp-y59a6` describes the same defect;
  this bead does not close it (not mine to close), but the fix is real and the Concierge
  should cite it there.

- **The bead-id fallback's known mis-citation bug (sp-gctip) is preserved as-is, not
  fixed.** When `--bead` is omitted, mail.sh scans the subject and body for the first
  bead-id-shaped string and renders that bead's block — which can render the wrong bead
  when a subject's own prose happens to name one first (sp-gctip's whole finding).
  `test-mail-bead-render.sh` case 3 tests this exact fallback and expects it to keep working
  for the single-id case, and sp-gctip is a separate, already-owned, in-flight bug bead
  (owner: aeon-ixion, branch `spira/sp-gctip`, mid-rebase against `lib.sh`) proposing a
  specific fix (pass `--bead` from all 8 `lib.sh` helpers; fail closed with no render rather
  than guess). Changing this rewrite's behaviour here would either fight that branch's merge
  or quietly ship a fix nobody reviewed as part of an unrelated bead. Ported byte-for-byte;
  sp-gctip's fix lands on top of this exactly as it would have landed on the bash.

- **`bd` stays a subprocess, not a library call.** No Rust `bd` client exists in this
  workspace; every other crate (`aeon`, `queue`, `spira-lc`) shells out the same way. The
  `Bd` trait is the seam that keeps this testable without one.

- **Binary name `mail`, bare, matching `sending`/`queue`/`testenv`.** No `mail`/`mailx` is
  present on this host's system directories, so there is no name-clash risk under the
  release's own shadow-check (`release::clashes`); the workspace convention (`sending.sh` →
  `sending`, not `spira-sending`) is unambiguous either way.

- **`mail.sh` ships as a compatibility symlink, deliberately, not just deleted.** Every
  caller this tree can see was repointed to bare `mail`, but three cannot be git-grepped
  from here: the Concierge persona text builds `$SPIRA_HOME/mail.sh send operator ...`,
  brain's `escalation-hook.sh` pattern-matches `mail.sh ... --kind question|suit` verbatim
  to enforce the statute-check gate, and an operator's own `aerc` config (outside version
  control) may still say `outgoing = <release>/spira/mail.sh`. `build-tarball.sh` symlinks
  both `bin/mail.sh → mail` and `spira/mail.sh → ../bin/mail` (the latter specifically
  because those callers name `$SPIRA_HOME/mail.sh`, not just whatever `bin/` resolves on
  PATH) — the same fix sp-6onps used for `world.sh`/`slay.sh`/`aeons.sh`/`ctrl.sh`.
  **Retire this once the Concierge persona, `escalation-hook.sh` and the operator's own
  `aerc` config are repointed to bare `mail`** — it is a compatibility name, not a second
  permanent spelling.

## 6. Test strategy

- **Unit** (`cargo test -p mail`): every lint rule (the table test-mail.sh used to run
  in-process against sourced bash — now impossible against a compiled binary, so it moved
  here verbatim, case for case); kind-file parsing and section-emptiness; the repeat
  fingerprint/normalisation and its bounded-wait timeout; bead-block rendering (including
  truncation and the notes/close-reason fallback) and the suit-verdict mapping, both against
  a fake `Bd`; `tidy`'s three keep rules, dedup and fail-closed guards; `sendmail`'s reply
  routing (sender mailbox / chamber persona / no-route, all → the right destination) and
  tracking-bead close; Maildir mechanics (deliver, mute, flags, msgid uniqueness); the RFC
  822 date formatter against a known timestamp.
- **Integration** (existing bash suites, repointed to call `mail` by bare name instead of
  `mail.sh`; run only through `testenv`, never on the host): `test-mail.sh`'s T2 section
  (Maildir send/read/list/unread-age/done, the repeat guard including its G-11 concurrency
  and atomicity cases, the stdin deadline, mail-mute, UC-17 reply routing, the archivist
  digest guard end-to-end) still runs against the real binary; its T1 lint table is removed
  (moved to `lint.rs`'s unit tests, since it can no longer source `mail.sh` as bash) and the
  suite's `covers:` line updated accordingly. `test-mail-tidy.sh`, `test-mail-bead-render.sh`,
  `test-mail-dismiss-sweep.sh`, `test-mail-health.sh`, `test-mail-pane.sh`,
  `test-mail-mailbox-arg.sh`, `test-mail-real-senders.sh`, `test-mail-aeon.sh`,
  `test-mail-aeon-hook.sh`, `test-brief-mail-options.sh`, `test-mail-tidy-units.sh` and
  `test-cockpit-layout-mail.sh` are repointed, not retired — every one of them exercises real
  subprocess/filesystem behaviour (Maildir side effects, systemd unit rendering, a live
  `bd` fixture) that a unit test does not reach, and none of their use cases has a crate-test
  substitute the coverage map can currently recognise (`sp-pype5` is not landed).
- **Parity**: see the delivery report for the concrete before/after runs. The contract
  (argv, stdout/stderr shape, exit codes, Maildir layout) is unchanged by construction —
  every subcommand's Rust implementation was written by re-reading the corresponding bash
  function line by line, not by re-deriving behaviour from tests alone.
