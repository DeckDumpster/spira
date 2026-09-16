<!--
source: https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets
fetched-via: https://docs.github.com/api/article/body?pathname=/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets
title: Available rules for rulesets
retrieved: 2026-09-16
-->

# Available rules for rulesets

Learn which rules you can add to a ruleset to protect specific branches and tags in a repository.

You can create branch or tag rulesets to control how users can interact with selected branches and tags in a repository. You can also create push rulesets to block pushes to a private or internal repository and that repository's entire fork network.

When you create a ruleset, you can allow certain users to bypass the rules in the ruleset. This can be users with certain roles, specific teams, or GitHub Apps.

For push rulesets, bypass permissions apply to a repository and the repository's entire fork network. This means that the only users who can bypass this ruleset for any repository in this repository's entire fork network are the users who can bypass this ruleset in the root repository.

For more information on creating rulesets and bypass permissions, see [Creating rulesets for a repository](/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/creating-rulesets-for-a-repository).

## Restrict creations

If selected, only users with bypass permissions can create branches or tags whose name matches the pattern you specify.

## Restrict updates

If selected, only users with bypass permissions can push to branches or tags whose name matches the pattern you specify.

## Restrict deletions

If selected, only users with bypass permissions can delete branches or tags whose name matches the pattern you specify. This rule is selected by default.

## Require linear history

Enforcing a linear commit history prevents collaborators from pushing merge commits to the targeted branches or tags. This means that any pull requests merged into the branch or tag must use a squash merge or a rebase merge. A strictly linear commit history can help teams revert changes more easily. For more information about merge methods, see [Pull request merges](/en/pull-requests/reference/pull-request-merges).

Before you can require a linear commit history, your repository must allow squash merging or rebase merging. For more information, see [Configuring pull request merges](/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges).

## Require deployments to succeed before merging

You can require that changes are successfully deployed to specific environments before a branch can be merged. For example, you can use this rule to ensure that changes are successfully deployed to a staging environment before the changes merge to your default branch.

## Require signed commits

When you enable required commit signing on a branch, contributors and bots can only push commits that have been signed and verified to the branch. For more information, see [About commit signature verification](/en/authentication/managing-commit-signature-verification/about-commit-signature-verification).

Branch protection rules and rulesets behave differently when you create a branch: with rulesets, we check only the commits that aren't accessible from other branches, whereas with branch protection rules, we do not verify signed commits unless you restrict pushes that create matching branches. With both, when you update a branch, we still check all the commits in the specified range, even if a commit is reachable from other branches.

With both methods, we use the `verified_signature?` to confirm if a commit has a valid signature. If not, the update is not accepted.

> \[!NOTE]
>
> * If you have enabled vigilant mode in your account settings, which indicates that your commits will always be signed, any commits that GitHub identifies as "Partially verified" are permitted on branches that require signed commits. For more information about vigilant mode, see [Displaying verification statuses for all of your commits](/en/authentication/managing-commit-signature-verification/displaying-verification-statuses-for-all-of-your-commits).
> * If a collaborator pushes an unsigned commit to a branch that requires commit signatures, the collaborator will need to rebase the commit to include a verified signature, then force push the rewritten commit to the branch.

You can push local commits to the branch if the commits are signed and verified.

You can also merge signed and verified commits into the branch using a pull request. When GitHub evaluates whether a pull request can be merged, it creates a test merge commit whose parents are the latest commit on the base branch and the pull request's head commit. GitHub checks the commits introduced by this test merge, including commits from the head branch. As a result, unsigned commits on the head branch can block a squash merge, even though GitHub would sign the final squash commit. This restriction can also apply to the author of the pull request.

To merge a blocked pull request, rewrite and sign the unsigned commits on the head branch, or ask someone with permission to bypass the applicable protections to merge the pull request.

You can squash and merge pull requests locally, but you must sign the resulting commit before pushing it to the branch. If another protection requires changes to be made through a pull request, you may also need bypass permissions to push the locally merged commit. See [Checking out pull requests locally](/en/pull-requests/how-tos/review-pull-requests/checking-out-pull-requests-locally) and [Signing commits](/en/authentication/managing-commit-signature-verification/signing-commits).

For more information about merge methods, see [About merge methods on GitHub](/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/about-merge-methods-on-github).

## Require a pull request before merging

You can require that all changes to the target branch be associated with a pull request. The pull request doesn't necessarily have to be approved, but it must be opened.

### Additional settings

> \[!NOTE] If you select **Dismiss stale pull request approvals when new commits are pushed** and/or **Require approval of the most recent reviewable push**, manually creating the merge commit for a pull request and pushing it directly to a protected branch will fail, unless the contents of the merge exactly match the merge generated by GitHub for the pull request.
>
> In addition, with these settings, approving reviews will be dismissed as stale if the merge base introduces new changes after the review was submitted. The merge base is the commit that is the last common ancestor between the topic branch and the base branch. If the merge base changes, the pull request cannot be merged until someone approves the work again.

Repository administrators or custom roles with the "edit repository rules" permission can require that all pull requests receive a specific number of approving reviews before someone merges the pull request into a protected branch. You can require approving reviews from people with write permissions in the repository or from a designated code owner.

If you enable required reviews, collaborators can only push changes to a branch via a pull request that is approved by the required number of reviewers with write permissions.

If someone chooses the **Request changes** option in a review, then that person must approve the pull request before the pull request can be merged. If a reviewer who requests changes on a pull request isn't available, anyone with write permissions for the repository can dismiss the blocking review.

Even after all required reviewers have approved a pull request, collaborators cannot merge the pull request if there are other open pull requests that have a head branch pointing to the same commit with pending or rejected reviews. Someone with write permissions must approve or dismiss the blocking review on the other pull requests first.

Optionally, you can choose to dismiss stale pull request approvals when commits are pushed that affect the diff in the pull request. GitHub records the state of the diff at the point when a pull request is approved. This state represents the set of changes that the reviewer approved. If the diff changes from this state (for example, because a contributor pushes new changes to the pull request branch or clicks **Update branch**, or because a related pull request is merged into the target branch), the approving review is dismissed as stale, and the pull request cannot be merged until someone approves the work again. For information about the target branch, see [Pull requests](/en/pull-requests/reference/pull-requests).

Optionally, you can choose to require reviews from code owners. If you do, any pull request that modifies content with a code owner must be approved by that code owner before the pull request can be merged into the protected branch. Note that if code has multiple owners, an approval from *any* of the code owners will be sufficient to meet this requirement. For more information, see [About code owners](/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-code-owners).

Optionally, you can restrict who can dismiss pull request reviews. If you enable this setting, select the users, teams, or GitHub Apps that can dismiss reviews on branches targeted by the ruleset. You can configure this setting in the UI or through the REST API or GraphQL API. For more information, see [Dismissing a pull request review](/en/pull-requests/how-tos/review-pull-requests/dismissing-a-pull-request-review).

Optionally, you can require an approval from someone other than the last person to push to a branch before a pull request can be merged. This means at least one other authorized reviewer has approved any changes. For example, the "last reviewer" can check that the latest set of changes incorporates feedback from other reviews, and does not add new, unreviewed content.

For complex pull requests that require many reviews, requiring an approval from someone other than the last person to push can be a compromise that avoids the need to dismiss all stale reviews: with this option, "stale" reviews are not dismissed, and the pull request remains approved as long as someone other than the person who made the most recent changes approves it. Users who have already reviewed a pull request can reapprove after the most recent push to meet this requirement. If you are concerned about pull requests being "hijacked" (where unapproved content is added to approved pull requests), it is safer to dismiss stale reviews.

Optionally, you can require all comments on the pull request to be resolved before it can be merged to a branch. This ensures that all comments are addressed or acknowledged before merge.

Optionally, you can require a merge type of merge, squash, or rebase. This means the targeted branches may only be merged based on the allowed type. Additionally if the repository has disabled a merge method and the ruleset required a different method, the merge will be blocked. See [About merge methods on GitHub](/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/about-merge-methods-on-github).

#### Additional approval for unattributed Copilot pull requests

> \[!NOTE]
> This feature is in public preview and subject to change.

**Require an additional approval for unattributed Copilot pull requests** is enabled by default, for both new and existing rulesets. When Copilot opens a pull request that isn't attributed to a person, the ruleset requires one more approval than the number you configured. For example, a ruleset that requires one approval requires two approvals from people with write access.

Requiring one approval usually means two people are involved in a change: the person who wrote it and the person who approved it. That assumption doesn't hold when Copilot opens a pull request under its own app identity instead of on behalf of a person, for example when you prompt it from a shared context such as a group thread or channel. See [Integrating Copilot cloud agent with Slack](/en/copilot/how-tos/copilot-integrations/integrate-cloud-agent-with-slack) and [Integrating Copilot cloud agent with Teams](/en/copilot/how-tos/copilot-integrations/integrate-cloud-agent-with-teams).

This setting has no effect if the ruleset requires zero approvals, so repositories that use pull requests as a record of changes rather than to gate on approvals are unaffected.

If you clear this setting, these pull requests require only the number of approvals you configured. If you also require an approval from someone other than the last person to push, at least one approval must cover the last push and come from someone other than Copilot.

#### Required reviewers

Optionally, you can require review or approval from specific teams when a pull request changes certain files or directories. You can specify up to 15 different teams, and for each team you can require a certain number of approvals from team members. For an approval from a team member to count, the team must have write permissions (or higher) for the repository.

The **Reviewer** dropdown allows you to select any team which is in scope where the rule is being defined.

* **Organization-wide rules**: The team must belong to the organization.
* **Repository-level rules**: The team must belong to the organization that owns the repository.

This rule is not available on user-owned repositories as they do not contain teams.

Required approvals can be set from 0 (zero) to 10. Requiring zero approvals means that the team will be added for visibility, but the team does not need to approve the request.

For each team, you can specify a list of file patterns which determines what files the setting applies to. The format of this file list is the same as a standard [`.gitignore`](/en/get-started/git-basics/ignoring-files) file:

* A pattern starting with an exclamation mark (`!`) is a negation. This will cause paths matching earlier patterns to *not* require approvals.
* Patterns are matched in order, so negated patterns can "unmatch" files which matched previous rules.

## Require status checks to pass before merging

Required status checks ensure that all required CI tests are passing before collaborators can make changes to a branch or tag targeted by your ruleset. Required status checks can be checks or statuses. For more information, see [Status checks](/en/pull-requests/reference/status-checks).

You can use the commit status API to allow external services to mark commits with an appropriate status. For more information, see [REST API endpoints for commit statuses](/en/rest/commits/statuses).

After enabling required status checks, all required status checks must pass before collaborators can merge changes into the branch or tag.

Any person or integration with write permissions to a repository can set the state of any status check in the repository, but in some cases you may only want to accept a status check from a specific GitHub App. When you add a required status check rule, you can select an app as the expected source of status updates. The app must be installed in the repository with the `statuses:write` permission, must have recently submitted a check run, and must be associated with a pre-existing required status check in the ruleset. If the status is set by any other person or integration, merging won't be allowed. If you select "any source," you can still manually verify the author of each status, listed in the merge box.

To troubleshoot issues with configuring status checks in rulesets, see [Troubleshooting rules](/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/troubleshooting-rules#troubleshooting-required-status-checks).

You can think of required status checks as being either "loose" or "strict." The type of required status check you choose determines whether your branch is required to be up to date with the base branch before merging.

| Type of required status check | Setting                                                                               | Merge requirements                                                                 | Considerations                                                                                                                                                                                                                                     |
| ----------------------------- | ------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Strict**                    | The **Require branches to be up to date before merging** checkbox is checked.         | The topic branch **must** be up to date with the base branch before merging.       | This is the default behavior for required status checks. More builds may be required, as you'll need to bring the head branch up to date after other collaborators update the target branch.                                                       |
| **Loose**                     | The **Require branches to be up to date before merging** checkbox is **not** checked. | The branch **does not** have to be up to date with the base branch before merging. | You'll have fewer required builds, as you won't need to bring the head branch up to date after other collaborators merge pull requests. Status checks may fail after you merge your branch if there are incompatible changes with the base branch. |
| **Disabled**                  | The **Require status checks to pass before merging** checkbox is **not** checked.     | The branch has no merge restrictions.                                              | If required status checks aren't enabled, collaborators can merge the branch at any time, regardless of whether it is up to date with the base branch. This increases the possibility of incompatible changes.                                     |

For status check troubleshooting information, see [Troubleshooting required status checks](/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

## Block force pushes

You can prevent users from force pushing to the targeted branches or tags. This rule is enabled by default.

If someone force pushes to a branch or tag, commits that other collaborators have based their work on may be removed from the history of the branch or tag. This may lead to merge conflicts or corrupted pull requests. Force pushing can also be used to delete branches or point a branch to commits that were not approved in a pull request.

> \[!NOTE] If force pushes are blocked, organization owners or repository administrators will be unable to change or rename the default branch unless they are authorized to bypass the ruleset.

Enabling force pushes will not override any other rules. For example, if a branch requires a linear commit history, you cannot force push merge commits to that branch.

## Require secret scanning alerts are resolved

> \[!NOTE]
> The rule to require secret scanning alerts to be resolved before merging is in public preview and subject to change.

If your repositories use secret scanning, you can prevent a pull request from merging when either of these conditions applies:

* A secret scanning scan has not completed for the head commit of the pull request.
* A commit in the pull request introduced an open secret scanning alert that matches a secret type selected in the ruleset.

You can configure the rule for provider, custom, and generic patterns. AI-detected secrets are not supported.

For more information, see [Blocking pull request merges that contain secrets](/en/code-security/how-tos/secure-your-secrets/prevent-future-leaks/block-merges-with-secrets).

## Require code scanning results

If your repositories are configured with code scanning, you can use rulesets to prevent pull requests from being merged when one of the following conditions is met:

* A required tool finds a code scanning alert of a severity that is defined in the ruleset.
* A required tool's analysis is still in progress.
* A required tool is not configured for the repository.

For more information, see [Set code scanning merge protection](/en/code-security/how-tos/find-and-fix-code-vulnerabilities/manage-your-configuration/set-merge-protection). For more general information about code scanning, see [Code scanning](/en/code-security/concepts/code-scanning/code-scanning).

## Require code quality results

If your repositories are configured with GitHub Code Quality, you can use rulesets to prevent pull requests from being merged when one of the following conditions is met:

* Analysis is still in progress.
* Analysis fails for any reason, for example: you have exhausted your budget for actions minutes.
* Code Quality found a result of a severity of the level defined in the ruleset, or a higher severity.

For more information, see [GitHub Code Quality](/en/code-security/concepts/code-quality/code-quality) and [Setting code quality thresholds for pull requests](/en/code-security/how-tos/maintain-quality-code/set-pr-thresholds).

## Restrict code coverage

> \[!NOTE]
> This feature is in public preview and subject to change.

If your repository has GitHub Code Quality enabled and code coverage data is being uploaded, you can use rulesets to prevent pull requests from being merged based on code coverage thresholds. For more information about uploading coverage data, see [Setting up code coverage for your repository](/en/code-security/how-tos/maintain-quality-code/set-up-code-coverage).

This rule blocks a pull request from being merged when either of two code coverage thresholds is not met:

* **Minimum line coverage percentage**: the aggregated line coverage for the pull request branch is below the configured percentage.
* **Maximum line coverage drop**: line coverage drops by more than the configured number of percentage points relative to the default branch.

For how to configure the thresholds, the prerequisite for uploading coverage data, and how to roll the rule out safely, see [Setting code coverage thresholds for pull requests](/en/code-security/how-tos/maintain-quality-code/restrict-code-coverage).

## Restrict file paths

Prevent commits that include changes in specified file paths from being pushed to the repository. Limit is 200 entries and up to 200 characters in each entry.

You can use `fnmatch` syntax for this. For example, a restriction targeting `test/demo/**/*` prevents any pushes to files or folders in the `test/demo/` directory. A restriction targeting `test/docs/pushrules.md` prevents pushes specifically to the `pushrules.md` file in the `test/docs/` directory. For more information, see [Creating rulesets for a repository](/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/creating-rulesets-for-a-repository#using-fnmatch-syntax).

You can also add allowed exceptions to this rule. A file matching an allowed exception can be pushed even when it also matches a restricted path.

Allowed exceptions use `fnmatch` syntax and are validated when you save the ruleset. For example, `**/gradle/wrapper/*.jar` allows the Gradle wrapper JAR file in any directory.

## Restrict file path length

Prevent commits that include file paths that exceed a specified character limit from being pushed to the repository.

## Restrict file extensions

Prevent commits that include files with specified file extensions from being pushed to the repository. Limit is 200 entries and up to 200 characters in each entry.

If you need to allow an exception for a single file of an otherwise blocked file type, use "Restrict file paths" with a pattern such as `**/*.jar`. Only the "Restrict file paths" and "Restrict file size" rules support allowed exceptions.

## Restrict file size

Prevent commits that exceed a specified file size limit from being pushed to the repository.

You can also add allowed exceptions to this rule. A file matching an allowed exception can exceed the file size limit.

Allowed exceptions use `fnmatch` syntax and are validated when you save the ruleset. For example, `**/gradle/wrapper/*.jar` allows the Gradle wrapper JAR file in any directory.