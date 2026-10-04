# skew — is the activated release the latest published, and the landing gate's fence

Replaces `spira/skew.sh` (805 lines). One binary, `skew`, the same six subcommands
(`check`, `units`, `refresh`, `gap`, `copies`, `foreign`). Written from the script's intent
and its callers (bd sp-yyk47), not ported line by line — though the decision logic below
tracks the original bash closely, because that logic (NOT-LATEST / MANIFEST-MISMATCH /
LOCAL-BEHIND / LOCAL-TAMPERED / CANNOT-VERIFY, the fail-closed exit-3 discipline, the
foreign-harness exemptions) was already correct and load-bearing, not an accreted accident.

## 1. Intent

Two unrelated jobs share this binary because skew.sh already bundled them and every caller
already resolves one name:

- **Release currency.** The installed Spira is read-only; the only way to change it is to
  activate a new release tarball (or, under `queue.local`, land a round). A drift check
  against a tree that cannot drift reports clean forever unless it asks a real question:
  is the activated release the most recently published tag (`check`), and does the running
  tree still match its own MANIFEST (`check_local`'s LOCAL-TAMPERED)? `units` and `gap` are
  the two cheaper checkout-mode questions (are installed units stale, how far behind is
  HEAD) that don't need a release tag at all. `refresh` is the one subcommand that acts
  rather than reports — see §8.
- **The landing gate's foreign-harness fence (`foreign`).** A branch in a repository that is
  not the harness's own may not change files inside a vendored copy of the harness: work
  landing there passes its own gate and closes its bead naming a real commit, then never
  runs, because the tree that was edited is self-consistent and nothing else compares the
  two. `gate` (the crate) calls `skew foreign <repo> <base> <ref>` as a subprocess, exactly
  as it called `bash skew.sh foreign ...` before this bead.

## 2. What is deliberately NOT touched

- **`lib.sh` stays the one authority on the repository map.** `repo_names`, `repo_root`,
  `repo_field`, `spira_same_repo`, `spira_home_repo`, `spira_landref`, `repo_land`,
  `ref_remote`, `ref_branch` are not re-derived in Rust; `real.rs`'s `seam()` sources
  `lib.sh` and calls the named function, capturing stdout — the same one-shot context call
  `gate-check` and `queue` already use for the same reason (law-leave-lib.sh-alone during
  the cutover; re-deriving repo-map resolution a second time is exactly the duplication that
  rule exists to prevent).
- **`exclude.sh harness-in`** (which directories inside a repository are a vendored harness
  copy) is unchanged, still bash, out of this bead's scope (group 1, gate/fences). `skew`
  pipes `git ls-files` / `git ls-tree` through it as a subprocess, exactly as bash did.
- **`release`, `mail`, `overrides.sh`, `units-install --diff` (was `install.sh --diff`,
  sp-31dm0), `gh`** are called exactly as skew.sh called them (same argv shapes, same env
  vars: `SPIRA_GH`, `GH_TIMEOUT`, and now `SPIRA_INSTALL_SH` to pin the installer). Nothing
  about the release binary's own contract, the mail format, or the override mechanism moves.

## 3. Contract

### 3.1 Invocation and exit codes

`skew <check|units|refresh|gap|copies|foreign> [args...]`, the same case dispatch as
skew.sh, default subcommand `check` (bare `skew` with no args). Exit codes, preserved
exactly:

| code | meaning |
|---|---|
| 0 | checked, and the answer is clean |
| 1 | checked, and there is a finding — on stdout |
| 3 | could not check — said out loud on stderr, never a silent pass (law-absence-needs-a-positive-control) |

### 3.2 stdout vs stderr is load-bearing

`deploy.sh`'s health check captures only `check`'s stdout (`2>/dev/null`). Every finding
line (`NOT-LATEST ...`, `MANIFEST-MISMATCH ...`, `LOCAL-BEHIND ...`, `LOCAL-TAMPERED ...`,
`CANNOT-VERIFY ...`, the "in effect" success line, `gap`'s count line, `units`'s diff
output) goes to **stdout**. Only "could not check at all" messages (`SPIRA_RELEASES is not
set`, `MANIFEST missing`, `no release tags found`, …) and `foreign`'s REFUSED explanation go
to **stderr**. This split is preserved subcommand-by-subcommand and line-by-line from
skew.sh; `real.rs`'s `World::out`/`World::err` are the only two functions that write to the
process's actual streams, so the split is visible at every call site in `lib.rs`.

### 3.3 Subcommands

| subcommand | args | prints | exit |
|---|---|---|---|
| `check` | `[--escalate]` | findings or "in effect" | 0/1/3 |
| `units` | — | a diff, or "installed units match" | 0/1/3 |
| `refresh` | `[repo]` | what it did (or declined) | 0/1 |
| `gap` | `[repo]` | how many commits behind | 0/1/3 |
| `copies` | — | `<repo> <path> <dir> self\|second` per harness copy found | 0/1 |
| `foreign` | `<repo> <base> <ref>` | offending paths, then a REFUSED explanation on stderr | 0/1 |

## 4. Schema

```rust
trait World {
    // lib.sh seam — never re-derived (§2)
    fn repo_names(&self) -> Vec<String>;
    fn repo_root(&self, name: &str) -> Option<PathBuf>;
    fn repo_field(&self, name: &str, field: &str) -> Option<String>;
    fn same_repo(&self, a: &Path, b: &Path) -> bool;
    fn home_repo(&self) -> String;
    fn landref(&self, repo: &Path) -> Option<String>;
    fn repo_land(&self, name: &str) -> String;
    fn ref_remote(&self, base: &str, repo: &Path) -> Option<String>;
    fn ref_branch(&self, base: &str) -> String;

    // git, exclude.sh, release/mail/overrides.sh/units-install/gh — one method per
    // semantic operation skew.sh needed, not one per raw flag.
    ...

    // output — the only two places that touch the real stdout/stderr (§3.2)
    fn out(&self, s: &str);
    fn err(&self, s: &str);
}
```

`real.rs` implements it against the host (subprocess `git`/`release`/`gh`/`mail.sh`/
`overrides.sh`/`bash <installer> --diff`, plus the `lib.sh` seam). `tests.rs`'s `Fake`
implements it as `RefCell`-backed maps keyed by the same arguments bash's own functions
took (e.g. `rev_parse` keyed by `"<repo>|<rev>"`), recording every call so a test can assert
on it (`mail_calls`, `overrides_applied`, `written`, `removed`) — the same technique
`forge`'s `FakeGh` and `gate`'s fakes use.

## 5. Decisions

- **`skew` locates `lib.sh` via an explicit `$SPIRA_HOME` first, release-relatively
  otherwise** (`main.rs`'s `resolve_home`) — the inverse of skew.sh's own rule (which found
  lib.sh beside its own script location, `$(dirname "$0")`, and never read `$SPIRA_HOME` to
  find itself). `_activated_release_cmd` (deploy.sh, sp-r15cf) and `spira-skew.service`
  (which deliberately pins `@SPIRA_HOME@`, not `@SPIRA_PROD@`, in split-checkout mode, so
  `SPIRA_REPO` derives to the dev checkout rather than the read-only release) both set
  `$SPIRA_HOME` explicitly and must be honored, not second-guessed by a self-location
  scheme that would otherwise always win once `skew` runs from an installed release (a
  release always has a `../spira/lib.sh` sibling, so release-relative resolution never gets
  a chance to lose to a deliberate override unless the explicit value is checked first).
  Release-relative self-location is the fallback, for a bare invocation with nothing set.
- **`check`'s artifact-mode tag source (local directory of `.tag` sidecars, or `gh release
  list`) is parsed by hand**, not with a JSON crate: the shape is one fixed object list with
  two known keys (`tagName`, `isDraft`), and `forge`'s own precedent (keeping its artifact-
  zip Python rather than adding a zip crate) already established that a tiny, fully-
  specified parse job doesn't need a dependency. `parse_gh_release_tags` is unit-tested
  directly against canned JSON.
- **The escalation dedup stamp's filename hash moved from POSIX `cksum` to FNV-1a.** The
  bash tool computing a byte-identical `cksum` no longer exists, and the digest is only ever
  compared against a stamp `skew` itself wrote in the same run directory — never against a
  stamp a still-running bash `skew.sh` wrote concurrently, since the two binaries cannot
  both be installed. A mismatched digest across the cutover costs exactly one
  re-escalation of a standing condition, the same documented, accepted cost a versioned
  condition key already pays on any scheme change (skew.sh's own comment on `v2:`).
- **`refresh`'s stage-and-swap writes are "write-then-rename" always**, not conditionally
  atomic only when the target exists (bash wrote new files directly for `A` rows and only
  renamed for `M` rows). Rename-into-place is strictly safer for both and costs nothing
  extra; `law-replace-running-scripts-atomically` applies to every write here, not only the
  ones bash happened to already protect.
- **`foreign`'s exact wording changes from "REFUSED by skew.sh" to "REFUSED by skew"** —
  the file it names no longer exists. No caller matched that exact string (audited: `gate`'s
  own tests use a fake and never inspect `skew`'s literal stderr); the dedicated bash suite
  that did (`test-skew-foreign.sh`) is retired by this bead (§6).

## 6. Parity and what moved

Parity was proved against the ported decision logic directly: every branch in `check`,
`check_local`, `foreign`, `copies`, `gap`, `refresh`, `units` and `escalate` has a unit test
over a `Fake` constructed to hit that branch, mirroring the scenarios skew.sh's own
dedicated suites built with real fixtures (temp git repos, stub `release`/`mail.sh`). Six
suites are retired, their coverage moved here:

| retired suite | coverage now lives in |
|---|---|
| `test-skew-check-release.sh` | `check_not_latest_and_manifest_mismatch_both_fire`, `check_clean_when_tag_matches_and_is_latest`, `check_cannot_verify_alone_is_exit_3_not_1` |
| `test-skew-local-release.sh` | `check_local_tampered_and_behind_both_fire`, `check_local_hotfix_is_clean`, `check_local_unresolvable_divergence_is_cannot_verify` |
| `test-skew-foreign.sh` | `foreign_*` (7 tests) |
| `test-skew-copies.sh` | `copies_found_reports_self_and_second`, `copies_none_found_is_a_finding` |
| `test-skew-refresh.sh` | `refresh_*` (7 tests) |
| `test-doctor-gate-compile-check.sh` (skew's own `gap`/`units` paths only — the doctor half stays with the `doctor` crate) | n/a — not this table; see `doctor/DESIGN.md` |

`test-skew-escalate.sh` is **kept** (repointed to call `skew` by bare name): it carries
`UC-operator-channel-25` as its only covering suite in `docs/test-plan/operator-channel.toml`,
and it is a genuine integration check (mail.sh's real output format) that a fake-backed unit
test does not replace. `test-unit-drift.sh` (the only carrier of
`UC-instance-lifecycle-34`) is likewise kept, repointed.

Both `gate` (`which("skew.sh")` → `which("skew")`, `skew_foreign`'s subprocess no longer
wrapped in `bash`, `harness_hash` reads the resolved `skew` binary's bytes instead of a
script's) and `landing-pass` (`command("skew.sh")` → `command("skew")`) are repointed in
this same change; see the bead's report for the full caller list.

## 7. Fail-closed

Every check that cannot answer refuses with exit 3 and says why, on stderr, never a silent
pass (law-absence-needs-a-positive-control): missing `SPIRA_RELEASES`, an unreadable
MANIFEST, no release tag source, an unresolvable base ref, a `gh`/local-directory lookup
that finds nothing. `check`'s `CANNOT-VERIFY`-alone case is exit 3, not 1 — a finding that
cannot be proven either way is not a clean verdict in either direction. `foreign` fails
closed on its own confusion: an init failure is the caller's job to distinguish (the trap
skew.sh itself documented is gate's own responsibility now, at the `gate` crate's call
site, not `skew`'s — `skew`'s own exit code is faithful either way).

**NOT APPLICABLE is exit 0, not exit 3** (sp-wecsq). `check`'s forge-tag branch (release
mode, not `queue.local`) answers MANIFEST-MISMATCH by resolving a release tag to the commit
it points at, which needs a real git checkout of the harness at `SPIRA_REPO` — a
release-only install (no `repo:spira` row; the installed Spira is read-only by design, §1)
never has one. That is not this box's version of "could not check this time" (CANNOT-VERIFY,
a real tag lookup that came up empty); the facts the question needs cannot exist on this
kind of install at all, so `check` says so on stdout and exits 0, before ever calling
`resolve_all_tags`. Release acceptance phase B's fresh install has exactly this shape; before
this fix `spira-skew-prod.service` exited 3 on every run there, forever, which `check_oneshots`
(release crate) correctly refused to accept as the unit "starting clean."

## 8. Test strategy

Unit tests (33, `cargo test -p skew`) cover every subcommand's branches against a `Fake`.
Not covered here, and not fixable from this side without a live database, a real `gh`
credential, or a real release tarball: `check`'s `gh release list` path against a live
forge, `refresh`'s release-mode `release build/verify/activate` against a real release
directory, and `check_local`'s `release verify` against a real MANIFEST. Those remain the
round's integration coverage through `test-skew-escalate.sh`, `test-unit-drift.sh`, and the
suites that exercise `deploy.sh`/`release`/`queue` end to end (`test-deploy.sh`,
`test-land-local-release.sh`, `test-landing-rebase.sh`).
