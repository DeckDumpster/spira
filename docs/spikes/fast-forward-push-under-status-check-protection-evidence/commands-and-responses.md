# Commands and GitHub's responses — fast-forward push under status-check protection

Captured: 2026-09-16, from a terminal session in a worktree of this repository, against its
GitHub remote. Every block is the command as run and its combined stdout/stderr, verbatim,
with exactly these substitutions: the organisation and repository are `OWNER/REPO`, the
account login is `USER`, its display name `NAME`, its node id `USER_NODE_ID`, and the
worktree path `$W`/`WORKTREE`. `$W` in a command is the worktree root. SHAs, messages,
HTTP codes and ruleset ids are unedited.

Commit graph (all built with `git commit-tree`, tree = `.github/workflows/spike-gate.yml` +
a `verdict` file the workflow reads): see `shas.env` for every SHA's name.

## 0. Create and protect the scratch branch

```console
$ git -C $W push origin 47e66f8599f23071ef2ff7b8aadf86dc55e1f6d0:refs/heads/spira/spike-protect-20260916200933
remote: 
remote: Create a pull request for 'spira/spike-protect-20260916200933' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-protect-20260916200933        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      47e66f8599f23071ef2ff7b8aadf86dc55e1f6d0 -> spira/spike-protect-20260916200933
[exit 0]
```

```console
$ cat $W/.runtime/spike/prot-pinned.json
{"required_status_checks":{"strict":false,"checks":[{"context":"gate","app_id":15368}]},
 "enforce_admins":true,"required_pull_request_reviews":null,"restrictions":null,
 "allow_force_pushes":false,"allow_deletions":false}
[exit 0]
```

```console
$ gh api -X PUT repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection --input $W/.runtime/spike/prot-pinned.json --jq '{url:.url,required_status_checks:.required_status_checks,enforce_admins:.enforce_admins.enabled,allow_force_pushes:.allow_force_pushes.enabled,allow_deletions:.allow_deletions.enabled,required_pull_request_reviews:.required_pull_request_reviews}'
{"allow_deletions":false,"allow_force_pushes":false,"enforce_admins":true,"required_pull_request_reviews":null,"required_status_checks":{"checks":[{"app_id":15368,"context":"gate"}],"contexts":["gate"],"contexts_url":"https://api.github.com/repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection/required_status_checks/contexts","strict":false,"url":"https://api.github.com/repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection/required_status_checks"},"url":"https://api.github.com/repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection"}
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/$(git -C $W rev-parse origin/main~1)/check-runs --jq '.check_runs[]|{name,conclusion,app:.app.slug,app_id:.app.id}'
{"app":"github-actions","app_id":15368,"conclusion":null,"name":"gate"}
{"app":"github-actions","app_id":15368,"conclusion":"success","name":"publish"}
{"app":"github-actions","app_id":15368,"conclusion":"success","name":"provision"}
[exit 0]
```

## 1. Push a SHA that has no gate check at all (expect refused)

```console
$ gh api repos/OWNER/REPO/commits/37f31dd0f7619523a298161294895d84f63f271f/check-runs --jq .total_count
{"message":"No commit found for SHA: 37f31dd0f7619523a298161294895d84f63f271f","documentation_url":"https://docs.github.com/rest/checks/runs#list-check-runs-for-a-git-reference","status":"422"}gh: No commit found for SHA: 37f31dd0f7619523a298161294895d84f63f271f (HTTP 422)
[exit 1]
```

```console
$ git -C $W push origin 37f31dd0f7619523a298161294895d84f63f271f:refs/heads/spira/spike-protect-20260916200933
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-protect-20260916200933.        
remote: 
remote: - Required status check "gate" is expected.        
To github.com:OWNER/REPO.git
 ! [remote rejected] 37f31dd0f7619523a298161294895d84f63f271f -> spira/spike-protect-20260916200933 (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

## 2. Start gate runs on source branches (green, red, pending)

```console
$ git -C $W push origin 1ed92904c28329bd3295bf7ce3ebe904282b94b5:refs/heads/spira/spike-src-20260916200933-green
remote: 
remote: Create a pull request for 'spira/spike-src-20260916200933-green' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-src-20260916200933-green        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      1ed92904c28329bd3295bf7ce3ebe904282b94b5 -> spira/spike-src-20260916200933-green
[exit 0]
```

```console
$ git -C $W push origin 8339474b811dd040c228b07df2ceb6df43d0ad73:refs/heads/spira/spike-src-20260916200933-red
remote: 
remote: Create a pull request for 'spira/spike-src-20260916200933-red' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-src-20260916200933-red        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      8339474b811dd040c228b07df2ceb6df43d0ad73 -> spira/spike-src-20260916200933-red
[exit 0]
```

```console
$ git -C $W push origin 42f0bdd82f2a9dc1913d3ddea77276a020997643:refs/heads/spira/spike-src-20260916200933-pending
remote: 
remote: Create a pull request for 'spira/spike-src-20260916200933-pending' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-src-20260916200933-pending        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      42f0bdd82f2a9dc1913d3ddea77276a020997643 -> spira/spike-src-20260916200933-pending
[exit 0]
```

## 3. Pending gate check (expect refused)

```console
$ gh api repos/OWNER/REPO/commits/42f0bdd82f2a9dc1913d3ddea77276a020997643/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id,head_sha}'
{"app_id":15368,"conclusion":null,"head_sha":"42f0bdd82f2a9dc1913d3ddea77276a020997643","name":"gate","status":"in_progress"}
[exit 0]
```

```console
$ git -C $W push origin 42f0bdd82f2a9dc1913d3ddea77276a020997643:refs/heads/spira/spike-protect-20260916200933
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-protect-20260916200933.        
remote: 
remote: - Required status check "gate" is in progress.        
To github.com:OWNER/REPO.git
 ! [remote rejected] 42f0bdd82f2a9dc1913d3ddea77276a020997643 -> spira/spike-protect-20260916200933 (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

## 4. Red gate check (expect refused)

```console
$ gh api repos/OWNER/REPO/commits/8339474b811dd040c228b07df2ceb6df43d0ad73/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id,head_sha}'
{"app_id":15368,"conclusion":"failure","head_sha":"8339474b811dd040c228b07df2ceb6df43d0ad73","name":"gate","status":"completed"}
[exit 0]
```

```console
$ git -C $W push origin 8339474b811dd040c228b07df2ceb6df43d0ad73:refs/heads/spira/spike-protect-20260916200933
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-protect-20260916200933.        
remote: 
remote: - Required status check "gate" is failing.        
To github.com:OWNER/REPO.git
 ! [remote rejected] 8339474b811dd040c228b07df2ceb6df43d0ad73 -> spira/spike-protect-20260916200933 (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

## 5. Green gate check, fast-forward (expect admitted)

```console
$ gh api repos/OWNER/REPO/commits/1ed92904c28329bd3295bf7ce3ebe904282b94b5/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id,head_sha}'
{"app_id":15368,"conclusion":"success","head_sha":"1ed92904c28329bd3295bf7ce3ebe904282b94b5","name":"gate","status":"completed"}
[exit 0]
```

```console
$ git -C $W push origin 1ed92904c28329bd3295bf7ce3ebe904282b94b5:refs/heads/spira/spike-protect-20260916200933
To github.com:OWNER/REPO.git
   47e66f8..1ed9290  1ed92904c28329bd3295bf7ce3ebe904282b94b5 -> spira/spike-protect-20260916200933
[exit 0]
```

## 6. PR whose head is fast-forwarded onto the protected branch (expect admitted, PR merged)

H1 (9d74d3d7df87d362f7538001c8084a75a20390e8) has no check of its own; H2 (c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a) is the PR head.

```console
$ git -C $W push origin c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a:refs/heads/spira/spike-head-20260916200933
remote: 
remote: Create a pull request for 'spira/spike-head-20260916200933' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-head-20260916200933        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a -> spira/spike-head-20260916200933
[exit 0]
```

```console
$ gh pr create -R OWNER/REPO --base spira/spike-protect-20260916200933 --head spira/spike-head-20260916200933 --title 'spike: ff push under protection (throwaway, do not merge)' --body 'Throwaway PR for a branch-protection spike. Closed or merged by push within the hour.'
Warning: 639 uncommitted changes
https://github.com/OWNER/REPO/pull/38
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id,head_sha}'
{"app_id":15368,"conclusion":"success","head_sha":"c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a","name":"gate","status":"completed"}
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/9d74d3d7df87d362f7538001c8084a75a20390e8/check-runs --jq .total_count
0
[exit 0]
```

```console
$ gh run list -R OWNER/REPO --commit c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a --json event,headSha,headBranch,conclusion,workflowName
[{"conclusion":"success","event":"pull_request","headBranch":"spira/spike-head-20260916200933","headSha":"c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a","workflowName":"Spike gate"}]
[exit 0]
```

```console
$ gh pr view 38 -R OWNER/REPO --json state,mergeable,mergeStateStatus,headRefOid,baseRefName
{"baseRefName":"spira/spike-protect-20260916200933","headRefOid":"c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a","mergeStateStatus":"CLEAN","mergeable":"MERGEABLE","state":"OPEN"}
[exit 0]
```

```console
$ git -C $W push origin c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a:refs/heads/spira/spike-protect-20260916200933
To github.com:OWNER/REPO.git
   1ed9290..c20715c  c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a -> spira/spike-protect-20260916200933
[exit 0]
```

```console
$ gh pr view 38 -R OWNER/REPO --json state,merged,mergedAt,mergedBy,mergeCommit,headRefOid
Unknown JSON field: "merged"
Available fields:
  additions
  assignees
  author
  autoMergeRequest
  baseRefName
  baseRefOid
  body
  changedFiles
  closed
  closedAt
  comments
  commits
  createdAt
  deletions
  files
  fullDatabaseId
  headRefName
  headRefOid
  headRepository
  headRepositoryOwner
  id
  isCrossRepository
  isDraft
  labels
  latestReviews
  maintainerCanModify
  mergeCommit
  mergeStateStatus
  mergeable
  mergedAt
  mergedBy
  milestone
  number
  potentialMergeCommit
  projectCards
  projectItems
  reactionGroups
  reviewDecision
  reviewRequests
  reviews
  state
  statusCheckRollup
  title
  updatedAt
  url
[exit 1]
```

```console
$ gh api repos/OWNER/REPO/pulls/38 --jq '{state,merged,merge_commit_sha,merged_by:.merged_by.login}'
{"merge_commit_sha":"c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a","merged":true,"merged_by":"USER","state":"closed"}
[exit 0]
```

```console
$ gh pr view 38 -R OWNER/REPO --json state,mergedAt,mergedBy,mergeCommit,headRefOid
{"headRefOid":"c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a","mergeCommit":{"oid":"c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a"},"mergedAt":"2026-09-16T20:11:05Z","mergedBy":{"id":"USER_NODE_ID","is_bot":false,"login":"USER","name":"NAME"},"state":"MERGED"}
[exit 0]
```

## 7. A commit status named gate posted with a user token, check pinned to the Actions app (expect refused)

```console
$ git -C $W push origin d6684fc431d7f8ace67a67cf43f391ed8e2d5749:refs/heads/spira/spike-forge-20260916200933
remote: 
remote: Create a pull request for 'spira/spike-forge-20260916200933' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-forge-20260916200933        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      d6684fc431d7f8ace67a67cf43f391ed8e2d5749 -> spira/spike-forge-20260916200933
[exit 0]
```

```console
$ gh api -X POST repos/OWNER/REPO/statuses/d6684fc431d7f8ace67a67cf43f391ed8e2d5749 -f state=success -f context=gate -f description='posted by hand' --jq '{context,state,creator:.creator.login}'
{"context":"gate","creator":"USER","state":"success"}
[exit 0]
```

```console
$ git -C $W push origin d6684fc431d7f8ace67a67cf43f391ed8e2d5749:refs/heads/spira/spike-protect-20260916200933
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-protect-20260916200933.        
remote: 
remote: - Required status check "gate" was not set by the expected GitHub app.        
To github.com:OWNER/REPO.git
 ! [remote rejected] d6684fc431d7f8ace67a67cf43f391ed8e2d5749 -> spira/spike-protect-20260916200933 (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

## 8. Same commit, protection re-set with app_id -1 (any source) (expect admitted)

```console
$ gh api -X PUT repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection --input $W/.runtime/spike/prot-any.json --jq .required_status_checks.checks
[{"app_id":null,"context":"gate"}]
[exit 0]
```

```console
$ git -C $W push origin d6684fc431d7f8ace67a67cf43f391ed8e2d5749:refs/heads/spira/spike-protect-20260916200933
To github.com:OWNER/REPO.git
   c20715c..d6684fc  d6684fc431d7f8ace67a67cf43f391ed8e2d5749 -> spira/spike-protect-20260916200933
[exit 0]
```

## 9. enforce_admins false, admin pushes a SHA with no check (expect admitted as a bypass)

```console
$ gh api -X PUT repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection --input $W/.runtime/spike/prot-noadmin.json --jq '{enforce_admins:.enforce_admins.enabled,checks:.required_status_checks.checks}'
{"checks":[{"app_id":15368,"context":"gate"}],"enforce_admins":false}
[exit 0]
```

```console
$ git -C $W push origin 85ec7036a983c31962b327ea7a79c7c844755d39:refs/heads/spira/spike-protect-20260916200933
remote: Bypassed rule violations for refs/heads/spira/spike-protect-20260916200933:        
remote: 
remote: - Required status check "gate" is expected.        
remote: 
To github.com:OWNER/REPO.git
   d6684fc..85ec703  85ec7036a983c31962b327ea7a79c7c844755d39 -> spira/spike-protect-20260916200933
[exit 0]
```

## 10. enforce_admins true again; non-fast-forward push of a SHA whose gate check is green (expect refused)

```console
$ gh api -X PUT repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection --input $W/.runtime/spike/prot-pinned.json --jq '{enforce_admins:.enforce_admins.enabled,checks:.required_status_checks.checks,allow_force_pushes:.allow_force_pushes.enabled}'
{"allow_force_pushes":false,"checks":[{"app_id":15368,"context":"gate"}],"enforce_admins":true}
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/42f0bdd82f2a9dc1913d3ddea77276a020997643/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id}'
{"app_id":15368,"conclusion":null,"name":"gate","status":"in_progress"}
[exit 0]
```

```console
$ git -C $W merge-base --is-ancestor 85ec7036a983c31962b327ea7a79c7c844755d39 42f0bdd82f2a9dc1913d3ddea77276a020997643; echo is-ancestor=$?
is-ancestor=1
[exit 0]
```

```console
$ git -C $W push --force origin 42f0bdd82f2a9dc1913d3ddea77276a020997643:refs/heads/spira/spike-protect-20260916200933
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-protect-20260916200933.        
remote: 
remote: - Required status check "gate" is in progress.        
remote: 
remote: - Cannot force-push to this branch        
To github.com:OWNER/REPO.git
 ! [remote rejected] 42f0bdd82f2a9dc1913d3ddea77276a020997643 -> spira/spike-protect-20260916200933 (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

## 11. Repository ruleset instead of classic protection, on a second scratch branch

```console
$ git -C $W push origin 47e66f8599f23071ef2ff7b8aadf86dc55e1f6d0:refs/heads/spira/spike-rules-20260916200933
remote: 
remote: Create a pull request for 'spira/spike-rules-20260916200933' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-rules-20260916200933        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      47e66f8599f23071ef2ff7b8aadf86dc55e1f6d0 -> spira/spike-rules-20260916200933
[exit 0]
```

```console
$ cat $W/.runtime/spike/ruleset.json
{"name":"spike-protect-20260916200933","target":"branch","enforcement":"active",
 "conditions":{"ref_name":{"include":["refs/heads/spira/spike-rules-20260916200933"],"exclude":[]}},
 "bypass_actors":[],
 "rules":[{"type":"deletion"},{"type":"non_fast_forward"},
  {"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":false,"do_not_enforce_on_create":false,
   "required_status_checks":[{"context":"gate","integration_id":15368}]}}]}
[exit 0]
```

```console
$ gh api -X POST repos/OWNER/REPO/rulesets --input $W/.runtime/spike/ruleset.json --jq '{id,name,enforcement,rules:[.rules[].type],current_user_can_bypass}'
{"current_user_can_bypass":"never","enforcement":"active","id":23563414,"name":"spike-protect-20260916200933","rules":["deletion","non_fast_forward","required_status_checks"]}
[exit 0]
```

### 11a. no check (expect refused)

```console
$ git -C $W push origin 37f31dd0f7619523a298161294895d84f63f271f:refs/heads/spira/spike-rules-20260916200933
To github.com:OWNER/REPO.git
   47e66f8..37f31dd  37f31dd0f7619523a298161294895d84f63f271f -> spira/spike-rules-20260916200933
[exit 0]
```

### 11b. forged status only (expect refused)

```console
$ git -C $W push origin d6684fc431d7f8ace67a67cf43f391ed8e2d5749:refs/heads/spira/spike-rules-20260916200933
To github.com:OWNER/REPO.git
 ! [rejected]        d6684fc431d7f8ace67a67cf43f391ed8e2d5749 -> spira/spike-rules-20260916200933 (non-fast-forward)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
hint: Updates were rejected because a pushed branch tip is behind its remote
hint: counterpart. If you want to integrate the remote changes, use 'git pull'
hint: before pushing again.
hint: See the 'Note about fast-forwards' in 'git push --help' for details.
[exit 1]
```

### 11c. green Actions check, fast-forward (expect admitted)

```console
$ git -C $W push origin 1ed92904c28329bd3295bf7ce3ebe904282b94b5:refs/heads/spira/spike-rules-20260916200933
To github.com:OWNER/REPO.git
 ! [rejected]        1ed92904c28329bd3295bf7ce3ebe904282b94b5 -> spira/spike-rules-20260916200933 (non-fast-forward)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
hint: Updates were rejected because a pushed branch tip is behind its remote
hint: counterpart. If you want to integrate the remote changes, use 'git pull'
hint: before pushing again.
hint: See the 'Note about fast-forwards' in 'git push --help' for details.
[exit 1]
```

### 11d. red Actions check (expect refused)

```console
$ git -C $W push origin 8339474b811dd040c228b07df2ceb6df43d0ad73:refs/heads/spira/spike-rules-20260916200933
To github.com:OWNER/REPO.git
 ! [rejected]        8339474b811dd040c228b07df2ceb6df43d0ad73 -> spira/spike-rules-20260916200933 (non-fast-forward)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
hint: Updates were rejected because a pushed branch tip is behind its remote
hint: counterpart. If you want to integrate the remote changes, use 'git pull'
hint: before pushing again.
hint: See the 'Note about fast-forwards' in 'git push --help' for details.
[exit 1]
```

```console
$ gh api repos/OWNER/REPO/rules/branches/spira/spike-rules-20260916200933 --jq '[.[]|{type,ruleset_id,parameters}]'
[{"parameters":null,"ruleset_id":23563414,"type":"deletion"},{"parameters":null,"ruleset_id":23563414,"type":"non_fast_forward"},{"parameters":{"do_not_enforce_on_create":false,"required_status_checks":[{"context":"gate","integration_id":15368}],"strict_required_status_checks_policy":false},"ruleset_id":23563414,"type":"required_status_checks"}]
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/rulesets/23563414 --jq '{enforcement,conditions,bypass_actors,current_user_can_bypass,created_at,updated_at}'
{"bypass_actors":[],"conditions":{"ref_name":{"exclude":[],"include":["refs/heads/spira/spike-rules-20260916200933"]}},"created_at":"2026-09-16T20:13:57.775Z","current_user_can_bypass":"never","enforcement":"active","updated_at":"2026-09-16T20:13:57.851Z"}
[exit 0]
```

```console
$ gh api 'repos/OWNER/REPO/rulesets/rule-suites?ref=refs/heads/spira/spike-rules-20260916200933' --jq '[.[]|{id,pushed_at,before_sha,after_sha,result,evaluation_result}]'
[{"after_sha":"37f31dd0f7619523a298161294895d84f63f271f","before_sha":"47e66f8599f23071ef2ff7b8aadf86dc55e1f6d0","evaluation_result":null,"id":4103563189,"pushed_at":"2026-09-16T20:13:59Z","result":"pass"}]
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/rulesets/rule-suites/4103563189 --jq '{result,evaluation_result,rule_evaluations}'
{"evaluation_result":null,"result":"pass","rule_evaluations":[{"enforcement":"active","result":"pass","rule_source":{"type":"secret_scanning"},"rule_type":"secret_scanning"},{"enforcement":"active","result":"pass","rule_source":{"id":23563414,"name":"spike-protect-20260916200933","type":"ruleset"},"rule_type":"required_status_checks"},{"enforcement":"active","result":"pass","rule_source":{"id":23563414,"name":"spike-protect-20260916200933","type":"ruleset"},"rule_type":"non_fast_forward"},{"enforcement":"active","result":"pass","rule_source":{"id":23563414,"name":"spike-protect-20260916200933","type":"ruleset"},"rule_type":"deletion"}]}
[exit 0]
```

### 11e. Retry 11a with a SHA GitHub has never seen, 2 minutes after the ruleset was created (expect refused)

```console
$ gh api repos/OWNER/REPO/commits/37f31dd0f7619523a298161294895d84f63f271f/check-runs --jq .total_count; gh api repos/OWNER/REPO/commits/37f31dd0f7619523a298161294895d84f63f271f/status --jq '{state,total_count}'
0
{"state":"pending","total_count":0}
[exit 0]
```

```console
$ git -C $W push origin 8e6349793b1f4daeecfbf1dc782275edd3aa643f:refs/heads/spira/spike-rules-20260916200933
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules-20260916200933        
remote: 
remote: - Required status check "gate" is expected.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] 8e6349793b1f4daeecfbf1dc782275edd3aa643f -> spira/spike-rules-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

### 11f. Push the SHA refused in 11e again (tests whether a previously-seen SHA passes)

```console
$ git -C $W push origin 8e6349793b1f4daeecfbf1dc782275edd3aa643f:refs/heads/spira/spike-rules-20260916200933
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules-20260916200933        
remote: 
remote: - Required status check "gate" is expected.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] 8e6349793b1f4daeecfbf1dc782275edd3aa643f -> spira/spike-rules-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

### 11g. Second ruleset on a third branch; push a never-seen no-check SHA within seconds of creating it (tests propagation delay)

```console
$ gh api -X POST repos/OWNER/REPO/rulesets --input $W/.runtime/spike/ruleset2.json --jq '{id,name,conditions}' && date -u +%T.%N
{"conditions":{"ref_name":{"exclude":[],"include":["refs/heads/spira/spike-rules2-20260916200933"]}},"id":23563447,"name":"spike-protect2-20260916200933"}
20:14:40.385498045
[exit 0]
```

```console
$ date -u +%T.%N; git -C $W push origin 2f5ef2957515264728607fceb2ec5f87d353b2cf:refs/heads/spira/spike-rules2-20260916200933
20:14:40.393418097
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules2-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules2-20260916200933        
remote: 
remote: - Required status check "gate" is expected.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] 2f5ef2957515264728607fceb2ec5f87d353b2cf -> spira/spike-rules2-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

### 11h. Reproduce 11a's history: refuse a no-check SHA on a classic-protected branch, then push it to the ruleset branch

```console
$ git -C $W push origin 47e66f8599f23071ef2ff7b8aadf86dc55e1f6d0:refs/heads/spira/spike-classic2-20260916200933
remote: 
remote: Create a pull request for 'spira/spike-classic2-20260916200933' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-classic2-20260916200933        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      47e66f8599f23071ef2ff7b8aadf86dc55e1f6d0 -> spira/spike-classic2-20260916200933
[exit 0]
```

```console
$ gh api -X PUT repos/OWNER/REPO/branches/spira/spike-classic2-20260916200933/protection --input $W/.runtime/spike/prot-pinned.json --jq .required_status_checks.checks
[{"app_id":15368,"context":"gate"}]
[exit 0]
```

```console
$ git -C $W push origin 6a3cbb70c87c65c498ebe7b66c2d08990e32c56e:refs/heads/spira/spike-classic2-20260916200933
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-classic2-20260916200933.        
remote: 
remote: - Required status check "gate" is expected.        
To github.com:OWNER/REPO.git
 ! [remote rejected] 6a3cbb70c87c65c498ebe7b66c2d08990e32c56e -> spira/spike-classic2-20260916200933 (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

```console
$ gh api repos/OWNER/REPO/commits/6a3cbb70c87c65c498ebe7b66c2d08990e32c56e/check-runs --jq .total_count
0
[exit 0]
```

```console
$ git -C $W push origin 6a3cbb70c87c65c498ebe7b66c2d08990e32c56e:refs/heads/spira/spike-rules2-20260916200933
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules2-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules2-20260916200933        
remote: 
remote: - Required status check "gate" is expected.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] 6a3cbb70c87c65c498ebe7b66c2d08990e32c56e -> spira/spike-rules2-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

### 11i. Repeat 11a's timing three times: fresh branch, fresh ruleset, never-seen no-check SHA pushed about 1.5 s after creation

```console
$ gh api -X POST repos/OWNER/REPO/rulesets --input $W/.runtime/spike/ruleset3.json --jq '{id,created_at}'
{"created_at":"2026-09-16T20:15:10.799Z","id":23563478}
[exit 0]
```

```console
$ git -C $W push origin 48a9a4a0d7ffc0a99bc43c12c29b2ddb1bbaea5c:refs/heads/spira/spike-rules3-20260916200933
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules3-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules3-20260916200933        
remote: 
remote: - Required status check "gate" is expected.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] 48a9a4a0d7ffc0a99bc43c12c29b2ddb1bbaea5c -> spira/spike-rules3-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

```console
$ gh api -X POST repos/OWNER/REPO/rulesets --input $W/.runtime/spike/ruleset4.json --jq '{id,created_at}'
{"created_at":"2026-09-16T20:15:16.049Z","id":23563488}
[exit 0]
```

```console
$ git -C $W push origin f125b55e82b1cf110e18355f98676feec2f3cffb:refs/heads/spira/spike-rules4-20260916200933
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules4-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules4-20260916200933        
remote: 
remote: - Required status check "gate" is expected.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] f125b55e82b1cf110e18355f98676feec2f3cffb -> spira/spike-rules4-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

```console
$ gh api -X POST repos/OWNER/REPO/rulesets --input $W/.runtime/spike/ruleset5.json --jq '{id,created_at}'
{"created_at":"2026-09-16T20:15:21.393Z","id":23563493}
[exit 0]
```

```console
$ git -C $W push origin 82b26b306db793d47704bba93ef9c8a923d0ea91:refs/heads/spira/spike-rules5-20260916200933
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules5-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules5-20260916200933        
remote: 
remote: - Required status check "gate" is expected.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] 82b26b306db793d47704bba93ef9c8a923d0ea91 -> spira/spike-rules5-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

### 11j. Ruleset branch: green, red, forged, PR-green (expect admitted, refused, refused, admitted)

```console
$ git -C $W push origin 1ed92904c28329bd3295bf7ce3ebe904282b94b5:refs/heads/spira/spike-rules2-20260916200933
To github.com:OWNER/REPO.git
   47e66f8..1ed9290  1ed92904c28329bd3295bf7ce3ebe904282b94b5 -> spira/spike-rules2-20260916200933
[exit 0]
```

```console
$ git -C $W push origin 8339474b811dd040c228b07df2ceb6df43d0ad73:refs/heads/spira/spike-rules2-20260916200933
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules2-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules2-20260916200933        
remote: 
remote: - Required status check "gate" is failing.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] 8339474b811dd040c228b07df2ceb6df43d0ad73 -> spira/spike-rules2-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

```console
$ git -C $W push origin d6684fc431d7f8ace67a67cf43f391ed8e2d5749:refs/heads/spira/spike-rules2-20260916200933
remote: error: GH013: Repository rule violations found for refs/heads/spira/spike-rules2-20260916200933.        
remote: Review all repository rules at https://github.com/OWNER/REPO/rules?ref=refs%2Fheads%2Fspira%2Fspike-rules2-20260916200933        
remote: 
remote: - Required status check "gate" was not set by the expected GitHub app.        
remote: 
To github.com:OWNER/REPO.git
 ! [remote rejected] d6684fc431d7f8ace67a67cf43f391ed8e2d5749 -> spira/spike-rules2-20260916200933 (push declined due to repository rule violations)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

```console
$ git -C $W push origin c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a:refs/heads/spira/spike-rules2-20260916200933
To github.com:OWNER/REPO.git
   1ed9290..c20715c  c20715ceaf196ea2d9132fbb6c3b0bf6a5b1366a -> spira/spike-rules2-20260916200933
[exit 0]
```

## 10 (repeated). Non-fast-forward push of a SHA whose gate check is now green (expect refused for force push only)

```console
$ gh api repos/OWNER/REPO/commits/42f0bdd82f2a9dc1913d3ddea77276a020997643/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id}'
{"app_id":15368,"conclusion":"success","name":"gate","status":"completed"}
[exit 0]
```

```console
$ git -C $W push --force origin 42f0bdd82f2a9dc1913d3ddea77276a020997643:refs/heads/spira/spike-protect-20260916200933
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-protect-20260916200933.        
remote: 
remote: - Cannot force-push to this branch        
To github.com:OWNER/REPO.git
 ! [remote rejected] 42f0bdd82f2a9dc1913d3ddea77276a020997643 -> spira/spike-protect-20260916200933 (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

## 12. strict (require branch up to date) on; fast-forward push of a green SHA (expect admitted)

```console
$ gh api -X PUT repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection --input $W/.runtime/spike/prot-strict.json --jq '{strict:.required_status_checks.strict,checks:.required_status_checks.checks,enforce_admins:.enforce_admins.enabled}'
{"checks":[{"app_id":15368,"context":"gate"}],"enforce_admins":true,"strict":true}
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/9a76a94097f756678c23807ddf9bdc0b1ca51498/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id}'
{"app_id":15368,"conclusion":"success","name":"gate","status":"completed"}
[exit 0]
```

```console
$ git -C $W push origin 9a76a94097f756678c23807ddf9bdc0b1ca51498:refs/heads/spira/spike-protect-20260916200933
To github.com:OWNER/REPO.git
   85ec703..9a76a94  9a76a94097f756678c23807ddf9bdc0b1ca51498 -> spira/spike-protect-20260916200933
[exit 0]
```

## 13. Require a pull request (0 approvals) as well; open PR with a green head; push the head (tests design §7's reason for not requiring a PR)

```console
$ gh api -X PUT repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection --input $W/.runtime/spike/prot-pr.json --jq '{checks:.required_status_checks.checks,enforce_admins:.enforce_admins.enabled,required_pull_request_reviews}'
{"checks":[{"app_id":15368,"context":"gate"}],"enforce_admins":true,"required_pull_request_reviews":{"dismiss_stale_reviews":false,"require_code_owner_reviews":false,"require_last_push_approval":false,"required_approving_review_count":0,"url":"https://api.github.com/repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection/required_pull_request_reviews"}}
[exit 0]
```

```console
$ git -C $W push origin 691f4ea0fccf198a6655222f1ce275ae11f4bc93:refs/heads/spira/spike-head2-20260916200933
remote: 
remote: Create a pull request for 'spira/spike-head2-20260916200933' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-head2-20260916200933        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      691f4ea0fccf198a6655222f1ce275ae11f4bc93 -> spira/spike-head2-20260916200933
[exit 0]
```

```console
$ gh pr create -R OWNER/REPO --base spira/spike-protect-20260916200933 --head spira/spike-head2-20260916200933 --title 'spike: ff push under require-PR protection (throwaway, do not merge)' --body 'Throwaway PR for a branch-protection spike.'
Warning: 639 uncommitted changes
https://github.com/OWNER/REPO/pull/39
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/691f4ea0fccf198a6655222f1ce275ae11f4bc93/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id}'
{"app_id":15368,"conclusion":"success","name":"gate","status":"completed"}
[exit 0]
```

```console
$ git -C $W push origin 691f4ea0fccf198a6655222f1ce275ae11f4bc93:refs/heads/spira/spike-protect-20260916200933
To github.com:OWNER/REPO.git
   9a76a94..691f4ea  691f4ea0fccf198a6655222f1ce275ae11f4bc93 -> spira/spike-protect-20260916200933
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/pulls/39 --jq '{state,merged,merge_commit_sha,merged_by:.merged_by.login}'
{"merge_commit_sha":"691f4ea0fccf198a6655222f1ce275ae11f4bc93","merged":true,"merged_by":"USER","state":"closed"}
[exit 0]
```

## 14. Require-PR still on; push a SHA with a green gate check but no pull request (expect refused)

```console
$ git -C $W push origin 9a5d6509fa754e6e05bd378dc92eebe5b97559f0:refs/heads/spira/spike-src-20260916200933-nopr
remote: 
remote: Create a pull request for 'spira/spike-src-20260916200933-nopr' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-src-20260916200933-nopr        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      9a5d6509fa754e6e05bd378dc92eebe5b97559f0 -> spira/spike-src-20260916200933-nopr
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/9a5d6509fa754e6e05bd378dc92eebe5b97559f0/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id}'
{"app_id":15368,"conclusion":"success","name":"gate","status":"completed"}
[exit 0]
```

```console
$ gh pr list -R OWNER/REPO --search 9a5d6509fa754e6e05bd378dc92eebe5b97559f0 --state all --json number,state
[]
[exit 0]
```

```console
$ git -C $W push origin 9a5d6509fa754e6e05bd378dc92eebe5b97559f0:refs/heads/spira/spike-protect-20260916200933
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-protect-20260916200933.        
remote: 
remote: - Changes must be made through a pull request.        
To github.com:OWNER/REPO.git
 ! [remote rejected] 9a5d6509fa754e6e05bd378dc92eebe5b97559f0 -> spira/spike-protect-20260916200933 (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

## 15. Cleanup

```console
$ gh api -X DELETE repos/OWNER/REPO/branches/spira/spike-protect-20260916200933/protection -i | head -1
HTTP/2.0 204 No Content
[exit 0]
```

```console
$ gh api -X DELETE repos/OWNER/REPO/branches/spira/spike-classic2-20260916200933/protection -i | head -1
HTTP/2.0 204 No Content
[exit 0]
```

```console
$ gh api -X DELETE repos/OWNER/REPO/rulesets/23563414 -i | head -1
HTTP/2.0 204 No Content
[exit 0]
```

```console
$ gh api -X DELETE repos/OWNER/REPO/rulesets/23563447 -i | head -1
HTTP/2.0 204 No Content
[exit 0]
```

```console
$ gh api -X DELETE repos/OWNER/REPO/rulesets/23563478 -i | head -1
HTTP/2.0 204 No Content
[exit 0]
```

```console
$ gh api -X DELETE repos/OWNER/REPO/rulesets/23563488 -i | head -1
HTTP/2.0 204 No Content
[exit 0]
```

```console
$ gh api -X DELETE repos/OWNER/REPO/rulesets/23563493 -i | head -1
HTTP/2.0 204 No Content
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/rulesets --jq length
0
[exit 0]
```

```console
$ git -C $W ls-remote origin 'refs/heads/spira/spike-*' | awk '{print $2}'
refs/heads/spira/spike-classic2-20260916200933
refs/heads/spira/spike-forge-20260916200933
refs/heads/spira/spike-head-20260916200933
refs/heads/spira/spike-head2-20260916200933
refs/heads/spira/spike-protect-20260916200933
refs/heads/spira/spike-rules-20260916200933
refs/heads/spira/spike-rules2-20260916200933
refs/heads/spira/spike-rules3-20260916200933
refs/heads/spira/spike-rules4-20260916200933
refs/heads/spira/spike-rules5-20260916200933
refs/heads/spira/spike-src-20260916200933-green
refs/heads/spira/spike-src-20260916200933-nopr
refs/heads/spira/spike-src-20260916200933-pending
refs/heads/spira/spike-src-20260916200933-red
refs/heads/spira/spike-src-20260916200933-strict
[exit 0]
```

```console
$ git -C $W push origin :refs/heads/spira/spike-classic2-20260916200933 :refs/heads/spira/spike-forge-20260916200933 :refs/heads/spira/spike-head-20260916200933 :refs/heads/spira/spike-head2-20260916200933 :refs/heads/spira/spike-protect-20260916200933 :refs/heads/spira/spike-rules-20260916200933 :refs/heads/spira/spike-rules2-20260916200933 :refs/heads/spira/spike-rules3-20260916200933 :refs/heads/spira/spike-rules4-20260916200933 :refs/heads/spira/spike-rules5-20260916200933 :refs/heads/spira/spike-src-20260916200933-green :refs/heads/spira/spike-src-20260916200933-nopr :refs/heads/spira/spike-src-20260916200933-pending :refs/heads/spira/spike-src-20260916200933-red :refs/heads/spira/spike-src-20260916200933-strict
To github.com:OWNER/REPO.git
 - [deleted]         spira/spike-classic2-20260916200933
 - [deleted]         spira/spike-forge-20260916200933
 - [deleted]         spira/spike-head-20260916200933
 - [deleted]         spira/spike-head2-20260916200933
 - [deleted]         spira/spike-protect-20260916200933
 - [deleted]         spira/spike-rules-20260916200933
 - [deleted]         spira/spike-rules2-20260916200933
 - [deleted]         spira/spike-rules3-20260916200933
 - [deleted]         spira/spike-rules4-20260916200933
 - [deleted]         spira/spike-rules5-20260916200933
 - [deleted]         spira/spike-src-20260916200933-green
 - [deleted]         spira/spike-src-20260916200933-nopr
 - [deleted]         spira/spike-src-20260916200933-pending
 - [deleted]         spira/spike-src-20260916200933-red
 - [deleted]         spira/spike-src-20260916200933-strict
[exit 0]
```

```console
$ git -C $W ls-remote origin 'refs/heads/spira/spike-*' | wc -l
0
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/branches/main/protection 2>&1 | head -1
{"message":"Branch not protected","documentation_url":"https://docs.github.com/rest/branches/branch-protection#get-branch-protection","status":"404"}gh: Branch not protected (HTTP 404)
[exit 0]
```

```console
$ gh run list -R OWNER/REPO --workflow 'Spike gate' --json databaseId --jq length
could not find any workflows named Spike gate
[exit 1]
```

```console
$ gh pr list -R OWNER/REPO --state all --search 'spike: ff push' --json number,state,title
[{"number":38,"state":"MERGED","title":"spike: ff push under protection (throwaway, do not merge)"},{"number":39,"state":"MERGED","title":"spike: ff push under require-PR protection (throwaway, do not merge)"}]
[exit 0]
```

## 16. gate needs provision; provision fails so gate is skipped (does a skipped required check admit the push?)

```console
$ cat $W/.runtime/spike/spike-gate.yml
name: Spike gate needs
on:
  push:
    branches: ['spira/spike-src-20260916200933b-*']
jobs:
  provision:
    runs-on: ubuntu-latest
    steps:
      - run: exit 1
  gate:
    needs: provision
    runs-on: ubuntu-latest
    steps:
      - run: exit 0
[exit 0]
```

```console
$ git -C $W push origin 84b340fa228d082541daaf65551be4590b0bcc6c:refs/heads/spira/spike-protect-20260916200933b
remote: 
remote: Create a pull request for 'spira/spike-protect-20260916200933b' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-protect-20260916200933b        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      84b340fa228d082541daaf65551be4590b0bcc6c -> spira/spike-protect-20260916200933b
[exit 0]
```

```console
$ gh api -X PUT repos/OWNER/REPO/branches/spira/spike-protect-20260916200933b/protection --input $W/.runtime/spike/prot-pinned.json --jq '{checks:.required_status_checks.checks,enforce_admins:.enforce_admins.enabled}'
{"checks":[{"app_id":15368,"context":"gate"}],"enforce_admins":true}
[exit 0]
```

```console
$ git -C $W push origin 53c99b9eab3f9eaccd6268637ab572ed7b5ad5b5:refs/heads/spira/spike-src-20260916200933b-skip
remote: 
remote: Create a pull request for 'spira/spike-src-20260916200933b-skip' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-src-20260916200933b-skip        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      53c99b9eab3f9eaccd6268637ab572ed7b5ad5b5 -> spira/spike-src-20260916200933b-skip
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/53c99b9eab3f9eaccd6268637ab572ed7b5ad5b5/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id}'
{"app_id":15368,"conclusion":"skipped","name":"gate","status":"completed"}
{"app_id":15368,"conclusion":"failure","name":"provision","status":"completed"}
[exit 0]
```

```console
$ git -C $W push origin 53c99b9eab3f9eaccd6268637ab572ed7b5ad5b5:refs/heads/spira/spike-protect-20260916200933b
To github.com:OWNER/REPO.git
   84b340f..53c99b9  53c99b9eab3f9eaccd6268637ab572ed7b5ad5b5 -> spira/spike-protect-20260916200933b
[exit 0]
```

## 17. Same, but gate runs when provision failed and fails itself (expect refused)

```console
$ cat $W/.runtime/spike/spike-gate.yml
name: Spike gate needs
on:
  push:
    branches: ['spira/spike-src-20260916200933b-*']
jobs:
  provision:
    runs-on: ubuntu-latest
    steps:
      - run: exit 1
  gate:
    needs: provision
    if: ${{ !cancelled() }}
    runs-on: ubuntu-latest
    steps:
      - if: needs.provision.result != 'success'
        run: exit 75
      - run: exit 0
[exit 0]
```

```console
$ git -C $W push origin e2fd02a95b2f22d1185c19b359317e1e84d6f608:refs/heads/spira/spike-src-20260916200933b-fixed
remote: 
remote: Create a pull request for 'spira/spike-src-20260916200933b-fixed' on GitHub by visiting:        
remote:      https://github.com/OWNER/REPO/pull/new/spira/spike-src-20260916200933b-fixed        
remote: 
To github.com:OWNER/REPO.git
 * [new branch]      e2fd02a95b2f22d1185c19b359317e1e84d6f608 -> spira/spike-src-20260916200933b-fixed
[exit 0]
```

```console
$ gh api repos/OWNER/REPO/commits/e2fd02a95b2f22d1185c19b359317e1e84d6f608/check-runs --jq '.check_runs[]|{name,status,conclusion,app_id:.app.id}'
{"app_id":15368,"conclusion":"failure","name":"gate","status":"completed"}
{"app_id":15368,"conclusion":"failure","name":"provision","status":"completed"}
[exit 0]
```

```console
$ git -C $W push origin e2fd02a95b2f22d1185c19b359317e1e84d6f608:refs/heads/spira/spike-protect-20260916200933b
remote: error: GH006: Protected branch update failed for refs/heads/spira/spike-protect-20260916200933b.        
remote: 
remote: - Required status check "gate" is failing.        
To github.com:OWNER/REPO.git
 ! [remote rejected] e2fd02a95b2f22d1185c19b359317e1e84d6f608 -> spira/spike-protect-20260916200933b (protected branch hook declined)
error: failed to push some refs to 'github.com:OWNER/REPO.git'
[exit 1]
```

### cleanup of 16-17

```console
$ gh api -X DELETE repos/OWNER/REPO/branches/spira/spike-protect-20260916200933b/protection -i | head -1
HTTP/2.0 204 No Content
[exit 0]
```

```console
$ git -C $W push origin :refs/heads/spira/spike-protect-20260916200933b :refs/heads/spira/spike-src-20260916200933b-skip :refs/heads/spira/spike-src-20260916200933b-fixed
To github.com:OWNER/REPO.git
 - [deleted]         spira/spike-protect-20260916200933b
 - [deleted]         spira/spike-src-20260916200933b-fixed
 - [deleted]         spira/spike-src-20260916200933b-skip
[exit 0]
```

```console
$ git -C $W ls-remote origin 'refs/heads/spira/spike-*' | wc -l; gh api repos/OWNER/REPO/rulesets --jq length; gh run list -R OWNER/REPO --limit 20 --json workflowName --jq '[.[].workflowName]|unique'
0
0
["Gate","Spike gate","Test image"]
[exit 0]
```
