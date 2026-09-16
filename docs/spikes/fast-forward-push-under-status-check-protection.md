# Fast-forward push under status-check protection

Spike for sp-pdo13, part of the merge-queue epic. Run 2026-09-16 against this repository's
GitHub remote, on throwaway branches, with the operator's own `gh` OAuth token (scopes
`repo, read:org, gist`; repository permission `admin`) and SSH pushes. No test database was
used; nothing here touches one.

Evidence is preserved in `fast-forward-push-under-status-check-protection-evidence/`:

- `commands-and-responses.md`: every command and GitHub's full response, in order, by
  section number. Cited below as **§n**. The organisation, repository and account are
  redacted to `OWNER/REPO`, `USER` and `NAME`; nothing else is edited.
- `prot-*.json`, `ruleset.json`: the exact protection payloads sent.
- `spike-gate.yml`, `spike-gate-needs-fixed.yml`: the stand-in workflows that produced the
  `gate` check runs.
- `lib.sh`, `shas.env`: the helper that built the commits and logged each call, and the name of
  every SHA.
- `github-docs-*.md`: the GitHub documentation pages cited, fetched as markdown with a
  provenance header.

The bead asked for this note at `docs/notes/ff-push-under-protection.md`. A spike branch may
only change `docs/spikes/`, so it is here instead.

## The question

Design §7 protects the base branch with a required `gate` status check, no force pushes and
no deletion, and deliberately **no** pull-request requirement, "because that rule refuses the
fast-forward push in §3". §3 lands a green batch with `git push <remote>
<batch-head>:<base-branch>` and expects GitHub to mark the batch PR merged. The bead asks
whether that works:

> On a scratch protected branch: a push of a SHA with a green required check is admitted, one
> without is refused, and a PR whose head is pushed is marked merged.

An answer is the three observations, plus whatever else the experiment shows the protection
step (sp-sk134) and the verdict (sp-6ijen) need to know.

## The answer in one paragraph

**Yes, all three hold.** A fast-forward push of a SHA with a green `gate` check run is
admitted. The same push with no check, a pending check or a red check is refused. Pushing an
open PR's green head onto its base marks the PR `MERGED`, with the head SHA as its merge
commit. The experiment also found three things the design does not account for:

1. **The design's reason for not requiring a PR is wrong.** With "require a pull request" on,
   pushing the head of an open, green PR was admitted, and a green SHA with no PR was refused.
2. **A `skipped` gate admits the push.** The real `gate.yml` produces exactly that when
   provisioning fails. Filed as sp-w411b, now blocking sp-sk134.
3. **The check must be pinned to the GitHub Actions app, and admins must be bound.**
   Otherwise, anyone with write access can post a hand-made `gate` status and push, and an
   admin token pushes past everything.

## What was found

### The acceptance criteria

All on a classic-protected scratch branch with payload `prot-pinned.json`: the `gate` check
pinned to `app_id` 15368, `enforce_admins` true, `strict` false, force pushes and deletion
off, no PR requirement. The PUT echoed those settings back (§0). App id 15368 is
`github-actions`: it is the app on the real `gate` check runs on `main` (end of §0, the
check-runs query).

| § | pushed SHA's `gate` check | fast-forward? | GitHub's answer |
|---|---|---|---|
| 1 | none | yes | refused: `GH006 … Required status check "gate" is expected.` |
| 3 | `in_progress` | yes | refused: `Required status check "gate" is in progress.` |
| 4 | `failure` | yes | refused: `Required status check "gate" is failing.` |
| 5 | `success` (push event) | yes | **admitted** `47e66f8..1ed9290` |
| 6 | `success` (pull_request event, PR head) | yes | **admitted** `1ed9290..c20715c`; PR `state: MERGED`, `merge_commit_sha` = head SHA, `merged_by` = the pusher |

About §6:

- **Only the tip is checked.** The PR head's parent `H1` had zero check runs of its own (§6,
  `total_count` 0) and the push was still admitted. A batch whose member commits were never
  individually gated can land, as the design needs.
- **A `pull_request` run attaches its check to the head SHA.** The workflow run lists
  `event: pull_request, headSha: <head>`. That run checked out GitHub's test merge of head
  into base. Because the push must be a fast-forward (§10), the base cannot have moved, so
  the tested tree and the landed tree are the same.
- **The PR is marked merged within seconds.** `gh pr view` a few seconds after the push
  showed `mergedAt` set (end of §6).

### The other shapes the protection step must choose between

| § | setting | pushed | GitHub's answer |
|---|---|---|---|
| 7 | pinned to Actions app | a commit **status** `gate=success` posted with the user token (`creator: USER`) | refused: `Required status check "gate" was not set by the expected GitHub app.` |
| 8 | `app_id: -1` (any source) | the same SHA | **admitted** |
| 9 | `enforce_admins: false` | no check at all, as an admin | **admitted**, with `Bypassed rule violations … Required status check "gate" is expected.` |
| 10 | force pushes off | non-fast-forward of a SHA with a **green** check | refused: `Cannot force-push to this branch` |
| 12 | `strict: true` | fast-forward, green | admitted: "up to date" does not get in the way of a fast-forward |
| 13 | **require PR (0 approvals)** + checks | head of an open PR, green | **admitted**; PR `MERGED` |
| 14 | require PR + checks | green SHA, **no PR** | refused: `Changes must be made through a pull request.` |
| 16 | pinned, `gate needs: provision` | provision `failure`, so `gate` concluded **`skipped`** | **admitted** |
| 17 | same, `gate` under `if: !cancelled()`, exit 75 when provision did not succeed | `gate` `failure` | refused: `Required status check "gate" is failing.` |

In §8 the PUT with `app_id: -1` echoed `app_id: null`, so "any source" reads back as null.
The REST reference says of `app_id`: "Omit this field to automatically select the GitHub App
that has recently provided this check … Pass -1 to explicitly allow any app to set the
status" (`github-docs-branch-protection.md`, line 321). **So a protect step that omits
`app_id` has a source that depends on history.** It has to pass 15368.

§16 matches the documentation: "Required status checks must have a `successful`, `skipped`,
or `neutral` status before collaborators can make changes to a protected branch"
(`github-docs-about-protected-branches.md`, line 81). In this repository's `gate.yml`, the
`gate` job has `needs: provision` and no `if:` (lines 123–125). **A provisioning failure
therefore yields exactly the `skipped` gate that §16 admitted.** The §17 fix does not carry
over verbatim: the real job's `runs-on` is `needs.provision.outputs.label`, which is empty
when provision failed. That is written into sp-w411b.

§13 contradicts the design's premise. GitHub's own docs are vaguer: "If another protection
requires changes to be made through a pull request, you may also need bypass permissions to
push the locally merged commit" (`github-docs-about-protected-branches.md`, line 118). That
sentence is about a locally made squash or merge commit. §13 pushed the PR's own head, which
is what §3 pushes.

### Repository rulesets instead of classic protection

`ruleset.json` has the same three rules: `deletion`, `non_fast_forward`, and
`required_status_checks` with `integration_id` 15368. It has no bypass actors, and GitHub
reported `current_user_can_bypass: never` (§11).

- Green was admitted, red was refused, the forged status was refused, and the PR head was
  admitted (§11j). Same as classic.
- **One push with no check was admitted: §11a.** The ruleset's own audit records it:
  rule-suite 4103563189, `required_status_checks: pass`, pushed within 2 s of the ruleset's
  creation (`created_at` 20:13:57.775, `pushed_at` 20:13:59), while its branch showed zero check runs and combined status `pending` (§11e).
- **Seven further no-check pushes were all refused** (§11e–§11i):
  - a SHA GitHub had never seen;
  - the same SHA pushed again;
  - a push 8 ms after creating a fresh ruleset;
  - a SHA first refused on a classic-protected branch and then pushed to the ruleset branch,
    which was 11a's exact history;
  - three fresh rulesets, each pushed 1.5 s after creation.

  **The admission is not explained and was not reproduced: 1 wrong admission in 8.**
- 11a's push also left 11b–11d as client-side non-fast-forward rejections, so they test
  nothing. 11j repeats them on a clean branch.

Classic protection wrongly admitted nothing: **0 in 9** pushes that should have been refused
(§1, §3, §4, §7, §10 twice, the classic refusal in §11h, §14, §17). The one
admission classic protection lets through that the design must still close is §16's
`skipped`, and GitHub documents that as behaviour, not a fault.

### Adjacent facts the other beads need

- Today a `pull_request` gate runs **only the suites the diff selects** (`gate.yml`, Suites
  step). Under protection, a green check from a narrowed PR run admits a push. **"Every
  commit on main passed gate" means "passed the full corpus" only after sp-sx2t9 lands.**
  sp-sk134 should not be armed on `main` before sp-sx2t9 and sp-w411b.
- `gate.yml` also runs on `workflow_dispatch`, with the full corpus, on any ref. A dispatch
  green on an arbitrary branch SHA is pushable under option A below, and not under option B.
- Pushes to the protected scratch branches triggered no workflow, because this repository's
  workflows only fire on `main` and tags. On `main` itself, `gate.yml`'s `push` trigger
  would run the whole corpus again after every landing. Design item 4 says that stops;
  sp-sx2t9 owns it.
- `enforce_admins: true` does not stop an admin removing protection through the API. It
  binds pushes, not configuration. Cleanup did exactly that, with `204 No Content` (§15).

## Options

Costs are counted from the payloads and calls in the evidence. Latency is not listed as a
cost because protection adds no CI run. The verdict pushes as soon as the PR's check is
green in all three.

### A. Classic protection as designed, with the check pinned and admins bound

`PUT /repos/{o}/{r}/branches/{base}/protection` with `prot-pinned.json`.

- **Cost:** one request to set it, 223 bytes, 5 top-level fields. One `GET` per doctor run to
  read it. Two configuration keys (check name, app id) and no more.
- **Measured:**
  - wrongly admitted pushes: 0 in 9;
  - wanted pushes admitted: all (§5, §6, §12);
  - PR marked merged: yes.
- **Risk:**
  - A `skipped` or `neutral` `gate` admits the push (§16). Closed only by sp-w411b.
  - A person can push any SHA that holds a green `gate` check from any Actions run, including
    a `workflow_dispatch` on a side branch, without a PR. The invariant "passed gate" still
    holds. The audit trail "every landing is a PR" does not.
  - Classic protection has no per-push decision log. The rule-suites API used to diagnose
    §11a is a ruleset feature. An admission can only be detected after the fact, by reading
    the check runs of what landed.

### B. A, plus "require a pull request" with 0 required approvals

`prot-pr.json`: the same request, one more field.

- **Cost:**
  - the same one request, 256 bytes;
  - no extra requests per landing, because §3 already opens the PR;
  - approvals must stay at 0: the queue authors its own PRs and the author cannot approve
    them.
- **Measured:** PR head admitted and marked merged (§13). Green SHA with no PR refused (§14).
  **One observation of each.**
- **Risk:**
  - It rests on n=1 against a GitHub doc sentence that hints the opposite.
  - If GitHub changes this, every landing is refused. That fails closed and loudly, and one
    request reverses it.
  - It also forbids hand-landing a fix without a PR. The hand-land statute would have to go
    through a PR, or through removing protection.

### C. A repository ruleset with the same rules

`POST /repos/{o}/{r}/rulesets` with `ruleset.json`.

- **Cost:** 466 bytes. Two requests to keep it idempotent: list rulesets to find ours by
  name, then `POST` or `PUT`. A doctor check has to match a ruleset by name and conditions
  rather than read one fixed URL.
- **Measured:** 1 wrong admission in 8 no-check pushes, unexplained (§11a). Otherwise the same
  as A.
- **Risk:**
  - That admission.
  - Gain: a per-push decision log (`rulesets/rule-suites`) and bypass lists, which is what
    made §11a diagnosable at all.

## Recommendation

**A: classic branch protection, as the design specifies, with two additions the design leaves
unstated.** The `checks` entry names `app_id` 15368 (the Actions app), never omitted and never
-1. `enforce_admins` is true.

The protection step should not be armed on `main` until sp-w411b (a skipped gate is
admitted) and sp-sx2t9 (PR gates run the full corpus) have landed. Before those, the check it
requires does not mean what the invariant says.

Design §7's stated reason for leaving out the PR requirement is false (§13). B is a strict
tightening with no per-landing cost. I do not recommend it now: it rests on one observation
against ambiguous documentation, and its failure mode stops every landing. It is a one-field
change worth making **after** the queue has landed a batch under A, once the §3 push is known
to work on `main`. That gives a second, real observation first. This finding is recorded on
sp-sk134.

Doctor should also meter the invariant rather than only the setting. For the newest N
first-parent commits on the base, `gate` must be `success` from app 15368. That is one
check-runs request per commit, and it is the only way to see an admission the protection
should have refused.

**Load-bearing assumption:** classic protection's required-check evaluation on `git push` is
deterministic, so the 0-in-9 observed here holds on `main`, including in the seconds after
protection is set.

## Falsifier

The recommendation is wrong if **any commit reaches the protected base while its `gate`
check, from app 15368, is anything other than `success`**. `skipped` and `neutral` count as
violations once sp-w411b has landed.

This is checkable. For each first-parent commit on the base since protection was set:

    gh api repos/{o}/{r}/commits/<sha>/check-runs \
      --jq '[.check_runs[]|select(.name=="gate" and .app.id==15368 and .conclusion=="success")]|length'

It must be ≥ 1. A zero on a commit pushed while protection was on means classic protection
has the same intermittent admission as §11a, and neither A nor C holds the invariant alone.

Two narrower falsifiers:

- **Against A over B:** a PR-less push that did harm, such as a `workflow_dispatch`-green
  side-branch SHA pushed to `main` by hand.
- **Against B being safe:** a `git push` of an open green PR's head refused with
  `Changes must be made through a pull request` while §13's settings are in force.

## What this did not establish

- **Why §11a admitted a push.** Not reproduced in 7 attempts. Only the ruleset's own
  rule-suite record exists for it.
- **Whether classic protection could do the same.** Zero in 9 is an observation, not a
  bound.
- **`cancelled` and `neutral` conclusions.** `neutral` is documented as passing; neither was
  pushed. A cancelled PR run matters because `gate.yml` cancels superseded PR runs.
- **Other credentials:**
  - a GitHub App installation token or a fine-grained PAT (only a user OAuth token was used);
  - HTTPS pushes (only SSH);
  - GitHub Enterprise Server, where the Actions app id differs, which is why the app id
    should be a configuration key with 15368 as its default.
- **B with approvals required, or with "require approval of the most recent push".** The
  docs say manual pushes fail under the latter unless contents match GitHub's merge
  (`github-docs-available-rules-for-rulesets.md`, line 71).
- **Protection latency on `main`**, and anything at `main`'s scale: history size, a real
  batch, the real self-hosted gate.
- **`merge_group`.** Unused by the design.

## Cleanup

Everything below is recorded in §15 and at the end of §17:

- protection removed from all three classic scratch branches: `204`;
- all five rulesets deleted: `204`, repository ruleset count 0;
- all 18 throwaway branches deleted: `ls-remote` count 0;
- all nine stand-in workflow runs deleted, so the repository lists only its three real
  workflows.

Two things could not be removed. The two throwaway PRs remain as `MERGED` into branches that
no longer exist, because GitHub has no API to delete a pull request. The hand-posted `gate`
commit status stays on its now-unreachable commit, because statuses cannot be deleted.
Protection on `main` was never touched: it read `Branch not protected` before and after.
