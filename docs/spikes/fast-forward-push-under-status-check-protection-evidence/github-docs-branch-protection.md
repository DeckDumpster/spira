<!--
source: https://docs.github.com/en/rest/branches/branch-protection
fetched-via: https://docs.github.com/api/article/body?pathname=/en/rest/branches/branch-protection
title: REST API endpoints for protected branches
retrieved: 2026-09-16
-->

# REST API endpoints for protected branches

Use the REST API to manage protected branches.

> [!NOTE]
> Most endpoints use `Authorization: Bearer <YOUR-TOKEN>` and `Accept: application/vnd.github+json` headers, plus `X-GitHub-Api-Version: 2026-03-10`. Curl examples below omit these standard headers for brevity.

## Get branch protection

```
GET /repos/{owner}/{repo}/branches/{branch}/protection
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection
```

**Response schema (Status: 200):**

* `url`: string
* `enabled`: boolean
* `required_status_checks`: `Protected Branch Required Status Check`:
  * `url`: string
  * `enforcement_level`: string
  * `contexts`: required, array of string
  * `checks`: required, array of objects:
    * `context`: required, string
    * `app_id`: required, integer or null
  * `contexts_url`: string
  * `strict`: boolean
* `enforce_admins`: `Protected Branch Admin Enforced`:
  * `url`: required, string, format: uri
  * `enabled`: required, boolean
* `required_pull_request_reviews`: `Protected Branch Pull Request Review`:
  * `url`: string, format: uri
  * `dismissal_restrictions`: object:
    * `users`: array of `Simple User`:
      * `name`: string or null
      * `email`: string or null
      * `login`: required, string
      * `id`: required, integer, format: int64
      * `node_id`: required, string
      * `avatar_url`: required, string, format: uri
      * `gravatar_id`: required, string or null
      * `url`: required, string, format: uri
      * `html_url`: required, string, format: uri
      * `followers_url`: required, string, format: uri
      * `following_url`: required, string
      * `gists_url`: required, string
      * `starred_url`: required, string
      * `subscriptions_url`: required, string, format: uri
      * `organizations_url`: required, string, format: uri
      * `repos_url`: required, string, format: uri
      * `events_url`: required, string
      * `received_events_url`: required, string, format: uri
      * `type`: required, string
      * `site_admin`: required, boolean
      * `starred_at`: string
      * `user_view_type`: string
    * `teams`: array of `Team`:
      * `id`: required, integer
      * `node_id`: required, string
      * `name`: required, string
      * `slug`: required, string
      * `description`: required, string or null
      * `privacy`: string
      * `notification_setting`: string
      * `permission`: required, string
      * `permissions`: object:
        * `pull`: required, boolean
        * `triage`: required, boolean
        * `push`: required, boolean
        * `maintain`: required, boolean
        * `admin`: required, boolean
      * `url`: required, string, format: uri
      * `html_url`: required, string, format: uri
      * `members_url`: required, string
      * `repositories_url`: required, string, format: uri
      * `type`: required, string, enum: `enterprise`, `organization`
      * `access_source`: string, enum: `direct`, `organization`, `enterprise`
      * `organization_id`: integer
      * `enterprise_id`: integer
      * `parent`: required, any of:
        * **null**
        * **Team Simple**
          * `id`: required, integer
          * `node_id`: required, string
          * `url`: required, string, format: uri
          * `members_url`: required, string
          * `name`: required, string
          * `description`: required, string or null
          * `permission`: required, string
          * `privacy`: string
          * `notification_setting`: string
          * `html_url`: required, string, format: uri
          * `repositories_url`: required, string, format: uri
          * `slug`: required, string
          * `ldap_dn`: string
          * `type`: required, string, enum: `enterprise`, `organization`
          * `organization_id`: integer
          * `enterprise_id`: integer
    * `apps`: array of `GitHub app`:
      * `id`: required, integer
      * `slug`: string
      * `node_id`: required, string
      * `client_id`: string
      * `owner`: required, one of:
        * **Simple User** (see above)
        * **Enterprise**
          * `description`: string or null
          * `html_url`: required, string, format: uri
          * `website_url`: string or null, format: uri
          * `id`: required, integer
          * `node_id`: required, string
          * `name`: required, string
          * `slug`: required, string
          * `created_at`: required, string or null, format: date-time
          * `updated_at`: required, string or null, format: date-time
          * `avatar_url`: required, string, format: uri
      * `name`: required, string
      * `description`: required, string or null
      * `external_url`: required, string, format: uri
      * `html_url`: required, string, format: uri
      * `created_at`: required, string, format: date-time
      * `updated_at`: required, string, format: date-time
      * `permissions`: required, object, additional properties: string:
        * `issues`: string
        * `checks`: string
        * `metadata`: string
        * `contents`: string
        * `deployments`: string
      * `events`: required, array of string
      * `installations_count`: integer
    * `url`: string
    * `users_url`: string
    * `teams_url`: string
  * `bypass_pull_request_allowances`: object:
    * `users`: array of `Simple User` (see above)
    * `teams`: array of `Team` (see above)
    * `apps`: array of `GitHub app` (see above)
  * `dismiss_stale_reviews`: required, boolean
  * `require_code_owner_reviews`: required, boolean
  * `required_approving_review_count`: integer, minimum: 0, maximum: 6
  * `require_last_push_approval`: boolean, default: `false`
* `restrictions`: `Branch Restriction Policy`:
  * `url`: required, string, format: uri
  * `users_url`: required, string, format: uri
  * `teams_url`: required, string, format: uri
  * `apps_url`: required, string, format: uri
  * `users`: required, array of objects:
    * `login`: string
    * `id`: integer, format: int64
    * `node_id`: string
    * `avatar_url`: string
    * `gravatar_id`: string
    * `url`: string
    * `html_url`: string
    * `followers_url`: string
    * `following_url`: string
    * `gists_url`: string
    * `starred_url`: string
    * `subscriptions_url`: string
    * `organizations_url`: string
    * `repos_url`: string
    * `events_url`: string
    * `received_events_url`: string
    * `type`: string
    * `site_admin`: boolean
    * `user_view_type`: string
  * `teams`: required, array of `Team` (see above)
  * `apps`: required, array of objects:
    * `id`: integer
    * `slug`: string
    * `node_id`: string
    * `owner`: object:
      * `login`: string
      * `id`: integer
      * `node_id`: string
      * `url`: string
      * `repos_url`: string
      * `events_url`: string
      * `hooks_url`: string
      * `issues_url`: string
      * `members_url`: string
      * `public_members_url`: string
      * `avatar_url`: string
      * `description`: string
      * `gravatar_id`: string
      * `html_url`: string
      * `followers_url`: string
      * `following_url`: string
      * `gists_url`: string
      * `starred_url`: string
      * `subscriptions_url`: string
      * `organizations_url`: string
      * `received_events_url`: string
      * `type`: string
      * `site_admin`: boolean
      * `user_view_type`: string
    * `name`: string
    * `client_id`: string
    * `description`: string
    * `external_url`: string
    * `html_url`: string
    * `created_at`: string
    * `updated_at`: string
    * `permissions`: object:
      * `metadata`: string
      * `contents`: string
      * `issues`: string
      * `single_file`: string
    * `events`: array of string
* `required_linear_history`: object:
  * `enabled`: boolean
* `allow_force_pushes`: object:
  * `enabled`: boolean
* `allow_deletions`: object:
  * `enabled`: boolean
* `block_creations`: object:
  * `enabled`: boolean
* `required_conversation_resolution`: object:
  * `enabled`: boolean
* `name`: string
* `protection_url`: string
* `required_signatures`: object:
  * `url`: required, string, format: uri
  * `enabled`: required, boolean
* `lock_branch`: object:
  * `enabled`: boolean, default: `false`
* `allow_fork_syncing`: object:
  * `enabled`: boolean, default: `false`

## Update branch protection

```
PUT /repos/{owner}/{repo}/branches/{branch}/protection
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Protecting a branch requires admin or owner permissions to the repository.
Note

Passing new arrays of users and teams replaces their previous values.

Note

The list of users, apps, and teams in total is limited to 100 items.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`required_status_checks`** (object or null) (required)
  Require status checks to pass before merging. Set to null to disable.
  - **`strict`** (boolean) (required)
    Require branches to be up to date before merging.
  - **`contexts`** (array of strings) (required)
    Closing down notice: The list of status checks to require in order to merge into this branch. If any of these checks have recently been set by a particular GitHub App, they will be required to come from that app in future for the branch to merge. Use checks instead of contexts for more fine-grained control.
  - **`checks`** (array of objects)
    The list of status checks to require in order to merge into this branch.
    - **`context`** (string) (required)
      The name of the required check
    - **`app_id`** (integer)
      The ID of the GitHub App that must provide this check. Omit this field to automatically select the GitHub App that has recently provided this check, or any app if it was not set by a GitHub App. Pass -1 to explicitly allow any app to set the status.

- **`enforce_admins`** (boolean or null) (required)
  Enforce all configured restrictions for administrators. Set to true to enforce required status checks for repository administrators. Set to null to disable.

- **`required_pull_request_reviews`** (object or null) (required)
  Require at least one approving review on a pull request, before merging. Set to null to disable.
  - **`dismissal_restrictions`** (object)
    Specify which users, teams, and apps can dismiss pull request reviews. Pass an empty dismissal_restrictions object to disable. User and team dismissal_restrictions are only available for organization-owned repositories. Omit this parameter for personal repositories.
    - **`users`** (array of strings)
      The list of user logins with dismissal access
    - **`teams`** (array of strings)
      The list of team slugs with dismissal access
    - **`apps`** (array of strings)
      The list of app slugs with dismissal access
  - **`dismiss_stale_reviews`** (boolean)
    Set to true if you want to automatically dismiss approving reviews when someone pushes a new commit.
  - **`require_code_owner_reviews`** (boolean)
    Blocks merging pull requests until code owners review them.
  - **`required_approving_review_count`** (integer)
    Specify the number of reviewers required to approve pull requests. Use a number between 1 and 6 or 0 to not require reviewers.
  - **`require_last_push_approval`** (boolean)
    Whether the most recent push must be approved by someone other than the person who pushed it. Default: false.
    Default: `false`
  - **`bypass_pull_request_allowances`** (object)
    Allow specific users, teams, or apps to bypass pull request requirements.
    - **`users`** (array of strings)
      The list of user logins allowed to bypass pull request requirements.
    - **`teams`** (array of strings)
      The list of team slugs allowed to bypass pull request requirements.
    - **`apps`** (array of strings)
      The list of app slugs allowed to bypass pull request requirements.

- **`restrictions`** (object or null) (required)
  Restrict who can push to the protected branch. User, app, and team restrictions are only available for organization-owned repositories. Set to null to disable.
  - **`users`** (array of strings) (required)
    The list of user logins with push access
  - **`teams`** (array of strings) (required)
    The list of team slugs with push access
  - **`apps`** (array of strings)
    The list of app slugs with push access

- **`required_linear_history`** (boolean)
  Enforces a linear commit Git history, which prevents anyone from pushing merge commits to a branch. Set to true to enforce a linear commit history. Set to false to disable a linear commit Git history. Your repository must allow squash merging or rebase merging before you can enable a linear commit history. Default: false. For more information, see "Requiring a linear commit history" in the GitHub Help documentation.

- **`allow_force_pushes`** (boolean or null)
  Permits force pushes to the protected branch by anyone with write access to the repository. Set to true to allow force pushes. Set to false or null to block force pushes. Default: false. For more information, see "Enabling force pushes to a protected branch" in the GitHub Help documentation."

- **`allow_deletions`** (boolean)
  Allows deletion of the protected branch by anyone with write access to the repository. Set to false to prevent deletion of the protected branch. Default: false. For more information, see "Enabling force pushes to a protected branch" in the GitHub Help documentation.

- **`block_creations`** (boolean)
  If set to true, the restrictions branch protection settings which limits who can push will also block pushes which create new branches, unless the push is initiated by a user, team, or app which has the ability to push. Set to true to restrict new branch creation. Default: false.

- **`required_conversation_resolution`** (boolean)
  Requires all conversations on code to be resolved before a pull request can be merged into a branch that matches this rule. Set to false to disable. Default: false.

- **`lock_branch`** (boolean)
  Whether to set the branch as read-only. If this is true, users will not be able to push to the branch. Default: false.
  Default: `false`

- **`allow_fork_syncing`** (boolean)
  Whether users can pull changes from upstream when the branch is locked. Set to true to allow fork syncing. Set to false to prevent fork syncing. Default: false.
  Default: `false`

### HTTP response status codes

- **200** - OK

- **403** - Forbidden

- **404** - Resource not found

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X PUT \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection \
  -d '{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "continuous-integration/travis-ci"
    ]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "dismissal_restrictions": {
      "users": [
        "octocat"
      ],
      "teams": [
        "justice-league"
      ]
    },
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true,
    "required_approving_review_count": 2,
    "require_last_push_approval": true,
    "bypass_pull_request_allowances": {
      "users": [
        "octocat"
      ],
      "teams": [
        "justice-league"
      ]
    }
  },
  "restrictions": {
    "users": [
      "octocat"
    ],
    "teams": [
      "justice-league"
    ],
    "apps": [
      "super-ci"
    ]
  },
  "required_linear_history": true,
  "allow_force_pushes": true,
  "allow_deletions": true,
  "block_creations": true,
  "required_conversation_resolution": true,
  "lock_branch": true,
  "allow_fork_syncing": true
}'
```

**Response schema (Status: 200):**

* `url`: required, string, format: uri
* `required_status_checks`: `Status Check Policy`:
  * `url`: required, string, format: uri
  * `strict`: required, boolean
  * `contexts`: required, array of string
  * `checks`: required, array of objects:
    * `context`: required, string
    * `app_id`: required, integer or null
  * `contexts_url`: required, string, format: uri
* `required_pull_request_reviews`: object:
  * `url`: required, string, format: uri
  * `dismiss_stale_reviews`: boolean
  * `require_code_owner_reviews`: boolean
  * `required_approving_review_count`: integer
  * `require_last_push_approval`: boolean, default: `false`
  * `dismissal_restrictions`: object:
    * `url`: required, string, format: uri
    * `users_url`: required, string, format: uri
    * `teams_url`: required, string, format: uri
    * `users`: required, array of `Simple User`:
      * `name`: string or null
      * `email`: string or null
      * `login`: required, string
      * `id`: required, integer, format: int64
      * `node_id`: required, string
      * `avatar_url`: required, string, format: uri
      * `gravatar_id`: required, string or null
      * `url`: required, string, format: uri
      * `html_url`: required, string, format: uri
      * `followers_url`: required, string, format: uri
      * `following_url`: required, string
      * `gists_url`: required, string
      * `starred_url`: required, string
      * `subscriptions_url`: required, string, format: uri
      * `organizations_url`: required, string, format: uri
      * `repos_url`: required, string, format: uri
      * `events_url`: required, string
      * `received_events_url`: required, string, format: uri
      * `type`: required, string
      * `site_admin`: required, boolean
      * `starred_at`: string
      * `user_view_type`: string
    * `teams`: required, array of `Team`:
      * `id`: required, integer
      * `node_id`: required, string
      * `name`: required, string
      * `slug`: required, string
      * `description`: required, string or null
      * `privacy`: string
      * `notification_setting`: string
      * `permission`: required, string
      * `permissions`: object:
        * `pull`: required, boolean
        * `triage`: required, boolean
        * `push`: required, boolean
        * `maintain`: required, boolean
        * `admin`: required, boolean
      * `url`: required, string, format: uri
      * `html_url`: required, string, format: uri
      * `members_url`: required, string
      * `repositories_url`: required, string, format: uri
      * `type`: required, string, enum: `enterprise`, `organization`
      * `access_source`: string, enum: `direct`, `organization`, `enterprise`
      * `organization_id`: integer
      * `enterprise_id`: integer
      * `parent`: required, any of:
        * **null**
        * **Team Simple**
          * `id`: required, integer
          * `node_id`: required, string
          * `url`: required, string, format: uri
          * `members_url`: required, string
          * `name`: required, string
          * `description`: required, string or null
          * `permission`: required, string
          * `privacy`: string
          * `notification_setting`: string
          * `html_url`: required, string, format: uri
          * `repositories_url`: required, string, format: uri
          * `slug`: required, string
          * `ldap_dn`: string
          * `type`: required, string, enum: `enterprise`, `organization`
          * `organization_id`: integer
          * `enterprise_id`: integer
    * `apps`: array of `GitHub app`:
      * `id`: required, integer
      * `slug`: string
      * `node_id`: required, string
      * `client_id`: string
      * `owner`: required, one of:
        * **Simple User** (see above)
        * **Enterprise**
          * `description`: string or null
          * `html_url`: required, string, format: uri
          * `website_url`: string or null, format: uri
          * `id`: required, integer
          * `node_id`: required, string
          * `name`: required, string
          * `slug`: required, string
          * `created_at`: required, string or null, format: date-time
          * `updated_at`: required, string or null, format: date-time
          * `avatar_url`: required, string, format: uri
      * `name`: required, string
      * `description`: required, string or null
      * `external_url`: required, string, format: uri
      * `html_url`: required, string, format: uri
      * `created_at`: required, string, format: date-time
      * `updated_at`: required, string, format: date-time
      * `permissions`: required, object, additional properties: string:
        * `issues`: string
        * `checks`: string
        * `metadata`: string
        * `contents`: string
        * `deployments`: string
      * `events`: required, array of string
      * `installations_count`: integer
  * `bypass_pull_request_allowances`: object:
    * `users`: required, array of `Simple User` (see above)
    * `teams`: required, array of `Team` (see above)
    * `apps`: array of `GitHub app` (see above)
* `required_signatures`: object:
  * `url`: required, string, format: uri
  * `enabled`: required, boolean
* `enforce_admins`: object:
  * `url`: required, string, format: uri
  * `enabled`: required, boolean
* `required_linear_history`: object:
  * `enabled`: required, boolean
* `allow_force_pushes`: object:
  * `enabled`: required, boolean
* `allow_deletions`: object:
  * `enabled`: required, boolean
* `restrictions`: `Branch Restriction Policy`:
  * `url`: required, string, format: uri
  * `users_url`: required, string, format: uri
  * `teams_url`: required, string, format: uri
  * `apps_url`: required, string, format: uri
  * `users`: required, array of objects:
    * `login`: string
    * `id`: integer, format: int64
    * `node_id`: string
    * `avatar_url`: string
    * `gravatar_id`: string
    * `url`: string
    * `html_url`: string
    * `followers_url`: string
    * `following_url`: string
    * `gists_url`: string
    * `starred_url`: string
    * `subscriptions_url`: string
    * `organizations_url`: string
    * `repos_url`: string
    * `events_url`: string
    * `received_events_url`: string
    * `type`: string
    * `site_admin`: boolean
    * `user_view_type`: string
  * `teams`: required, array of `Team` (see above)
  * `apps`: required, array of objects:
    * `id`: integer
    * `slug`: string
    * `node_id`: string
    * `owner`: object:
      * `login`: string
      * `id`: integer
      * `node_id`: string
      * `url`: string
      * `repos_url`: string
      * `events_url`: string
      * `hooks_url`: string
      * `issues_url`: string
      * `members_url`: string
      * `public_members_url`: string
      * `avatar_url`: string
      * `description`: string
      * `gravatar_id`: string
      * `html_url`: string
      * `followers_url`: string
      * `following_url`: string
      * `gists_url`: string
      * `starred_url`: string
      * `subscriptions_url`: string
      * `organizations_url`: string
      * `received_events_url`: string
      * `type`: string
      * `site_admin`: boolean
      * `user_view_type`: string
    * `name`: string
    * `client_id`: string
    * `description`: string
    * `external_url`: string
    * `html_url`: string
    * `created_at`: string
    * `updated_at`: string
    * `permissions`: object:
      * `metadata`: string
      * `contents`: string
      * `issues`: string
      * `single_file`: string
    * `events`: array of string
* `required_conversation_resolution`: object:
  * `enabled`: boolean
* `block_creations`: object:
  * `enabled`: required, boolean
* `lock_branch`: object:
  * `enabled`: boolean, default: `false`
* `allow_fork_syncing`: object:
  * `enabled`: boolean, default: `false`

## Delete branch protection

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **204** - No Content

- **403** - Forbidden

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection
```

**Response schema (Status: 204):**

## Get admin branch protection

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/enforce_admins
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/enforce_admins
```

**Response schema (Status: 200):**

* `url`: required, string, format: uri
* `enabled`: required, boolean

## Set admin branch protection

```
POST /repos/{owner}/{repo}/branches/{branch}/protection/enforce_admins
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Adding admin enforcement requires admin or owner permissions to the repository and branch protection to be enabled.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X POST \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/enforce_admins
```

**Response schema (Status: 200):**

Same response schema as [Get admin branch protection](#get-admin-branch-protection).

## Delete admin branch protection

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/enforce_admins
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Removing admin enforcement requires admin or owner permissions to the repository and branch protection to be enabled.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **204** - No Content

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/enforce_admins
```

**Response schema (Status: 204):**

## Get pull request review protection

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/required_pull_request_reviews
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_pull_request_reviews
```

**Response schema (Status: 200):**

* `url`: string, format: uri
* `dismissal_restrictions`: object:
  * `users`: array of `Simple User`:
    * `name`: string or null
    * `email`: string or null
    * `login`: required, string
    * `id`: required, integer, format: int64
    * `node_id`: required, string
    * `avatar_url`: required, string, format: uri
    * `gravatar_id`: required, string or null
    * `url`: required, string, format: uri
    * `html_url`: required, string, format: uri
    * `followers_url`: required, string, format: uri
    * `following_url`: required, string
    * `gists_url`: required, string
    * `starred_url`: required, string
    * `subscriptions_url`: required, string, format: uri
    * `organizations_url`: required, string, format: uri
    * `repos_url`: required, string, format: uri
    * `events_url`: required, string
    * `received_events_url`: required, string, format: uri
    * `type`: required, string
    * `site_admin`: required, boolean
    * `starred_at`: string
    * `user_view_type`: string
  * `teams`: array of `Team`:
    * `id`: required, integer
    * `node_id`: required, string
    * `name`: required, string
    * `slug`: required, string
    * `description`: required, string or null
    * `privacy`: string
    * `notification_setting`: string
    * `permission`: required, string
    * `permissions`: object:
      * `pull`: required, boolean
      * `triage`: required, boolean
      * `push`: required, boolean
      * `maintain`: required, boolean
      * `admin`: required, boolean
    * `url`: required, string, format: uri
    * `html_url`: required, string, format: uri
    * `members_url`: required, string
    * `repositories_url`: required, string, format: uri
    * `type`: required, string, enum: `enterprise`, `organization`
    * `access_source`: string, enum: `direct`, `organization`, `enterprise`
    * `organization_id`: integer
    * `enterprise_id`: integer
    * `parent`: required, any of:
      * **null**
      * **Team Simple**
        * `id`: required, integer
        * `node_id`: required, string
        * `url`: required, string, format: uri
        * `members_url`: required, string
        * `name`: required, string
        * `description`: required, string or null
        * `permission`: required, string
        * `privacy`: string
        * `notification_setting`: string
        * `html_url`: required, string, format: uri
        * `repositories_url`: required, string, format: uri
        * `slug`: required, string
        * `ldap_dn`: string
        * `type`: required, string, enum: `enterprise`, `organization`
        * `organization_id`: integer
        * `enterprise_id`: integer
  * `apps`: array of `GitHub app`:
    * `id`: required, integer
    * `slug`: string
    * `node_id`: required, string
    * `client_id`: string
    * `owner`: required, one of:
      * **Simple User** (see above)
      * **Enterprise**
        * `description`: string or null
        * `html_url`: required, string, format: uri
        * `website_url`: string or null, format: uri
        * `id`: required, integer
        * `node_id`: required, string
        * `name`: required, string
        * `slug`: required, string
        * `created_at`: required, string or null, format: date-time
        * `updated_at`: required, string or null, format: date-time
        * `avatar_url`: required, string, format: uri
    * `name`: required, string
    * `description`: required, string or null
    * `external_url`: required, string, format: uri
    * `html_url`: required, string, format: uri
    * `created_at`: required, string, format: date-time
    * `updated_at`: required, string, format: date-time
    * `permissions`: required, object, additional properties: string:
      * `issues`: string
      * `checks`: string
      * `metadata`: string
      * `contents`: string
      * `deployments`: string
    * `events`: required, array of string
    * `installations_count`: integer
  * `url`: string
  * `users_url`: string
  * `teams_url`: string
* `bypass_pull_request_allowances`: object:
  * `users`: array of `Simple User` (see above)
  * `teams`: array of `Team` (see above)
  * `apps`: array of `GitHub app` (see above)
* `dismiss_stale_reviews`: required, boolean
* `require_code_owner_reviews`: required, boolean
* `required_approving_review_count`: integer, minimum: 0, maximum: 6
* `require_last_push_approval`: boolean, default: `false`

## Update pull request review protection

```
PATCH /repos/{owner}/{repo}/branches/{branch}/protection/required_pull_request_reviews
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Updating pull request review enforcement requires admin or owner permissions to the repository and branch protection to be enabled.
Note

Passing new arrays of users and teams replaces their previous values.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`dismissal_restrictions`** (object)
  Specify which users, teams, and apps can dismiss pull request reviews. Pass an empty dismissal_restrictions object to disable. User and team dismissal_restrictions are only available for organization-owned repositories. Omit this parameter for personal repositories.
  - **`users`** (array of strings)
    The list of user logins with dismissal access
  - **`teams`** (array of strings)
    The list of team slugs with dismissal access
  - **`apps`** (array of strings)
    The list of app slugs with dismissal access

- **`dismiss_stale_reviews`** (boolean)
  Set to true if you want to automatically dismiss approving reviews when someone pushes a new commit.

- **`require_code_owner_reviews`** (boolean)
  Blocks merging pull requests until code owners have reviewed.

- **`required_approving_review_count`** (integer)
  Specifies the number of reviewers required to approve pull requests. Use a number between 1 and 6 or 0 to not require reviewers.

- **`require_last_push_approval`** (boolean)
  Whether the most recent push must be approved by someone other than the person who pushed it. Default: false
  Default: `false`

- **`bypass_pull_request_allowances`** (object)
  Allow specific users, teams, or apps to bypass pull request requirements.
  - **`users`** (array of strings)
    The list of user logins allowed to bypass pull request requirements.
  - **`teams`** (array of strings)
    The list of team slugs allowed to bypass pull request requirements.
  - **`apps`** (array of strings)
    The list of app slugs allowed to bypass pull request requirements.

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X PATCH \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_pull_request_reviews \
  -d '{
  "dismissal_restrictions": {
    "users": [
      "octocat"
    ],
    "teams": [
      "justice-league"
    ],
    "apps": [
      "octoapp"
    ]
  },
  "bypass_pull_request_allowances": {
    "users": [
      "octocat"
    ],
    "teams": [
      "justice-league"
    ],
    "apps": [
      "octoapp"
    ]
  },
  "dismiss_stale_reviews": true,
  "require_code_owner_reviews": true,
  "required_approving_review_count": 2,
  "require_last_push_approval": true
}'
```

**Response schema (Status: 200):**

Same response schema as [Get pull request review protection](#get-pull-request-review-protection).

## Delete pull request review protection

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/required_pull_request_reviews
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **204** - No Content

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_pull_request_reviews
```

**Response schema (Status: 204):**

## Get commit signature protection

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/required_signatures
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
When authenticated with admin or owner permissions to the repository, you can use this endpoint to check whether a branch requires signed commits. An enabled status of true indicates you must sign commits on this branch. For more information, see Signing commits with GPG in GitHub Help.
Note

You must enable branch protection to require signed commits.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_signatures
```

**Response schema (Status: 200):**

Same response schema as [Get admin branch protection](#get-admin-branch-protection).

## Create commit signature protection

```
POST /repos/{owner}/{repo}/branches/{branch}/protection/required_signatures
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
When authenticated with admin or owner permissions to the repository, you can use this endpoint to require signed commits on a branch. You must enable branch protection to require signed commits.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X POST \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_signatures
```

**Response schema (Status: 200):**

Same response schema as [Get admin branch protection](#get-admin-branch-protection).

## Delete commit signature protection

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/required_signatures
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
When authenticated with admin or owner permissions to the repository, you can use this endpoint to disable required signed commits on a branch. You must enable branch protection to require signed commits.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **204** - No Content

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_signatures
```

**Response schema (Status: 204):**

## Get status checks protection

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/required_status_checks
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_status_checks
```

**Response schema (Status: 200):**

* `url`: required, string, format: uri
* `strict`: required, boolean
* `contexts`: required, array of string
* `checks`: required, array of objects:
  * `context`: required, string
  * `app_id`: required, integer or null
* `contexts_url`: required, string, format: uri

## Update status check protection

```
PATCH /repos/{owner}/{repo}/branches/{branch}/protection/required_status_checks
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Updating required status checks requires admin or owner permissions to the repository and branch protection to be enabled.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`strict`** (boolean)
  Require branches to be up to date before merging.

- **`contexts`** (array of strings)
  Closing down notice: The list of status checks to require in order to merge into this branch. If any of these checks have recently been set by a particular GitHub App, they will be required to come from that app in future for the branch to merge. Use checks instead of contexts for more fine-grained control.

- **`checks`** (array of objects)
  The list of status checks to require in order to merge into this branch.
  - **`context`** (string) (required)
    The name of the required check
  - **`app_id`** (integer)
    The ID of the GitHub App that must provide this check. Omit this field to automatically select the GitHub App that has recently provided this check, or any app if it was not set by a GitHub App. Pass -1 to explicitly allow any app to set the status.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X PATCH \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_status_checks \
  -d '{
  "strict": true,
  "contexts": [
    "continuous-integration/travis-ci"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get status checks protection](#get-status-checks-protection).

## Remove status check protection

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/required_status_checks
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **204** - No Content

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_status_checks
```

**Response schema (Status: 204):**

## Get all status check contexts

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/required_status_checks/contexts
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_status_checks/contexts
```

**Response schema (Status: 200):**

Array of string

## Add status check contexts

```
POST /repos/{owner}/{repo}/branches/{branch}/protection/required_status_checks/contexts
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`contexts`** (array of strings) (required)
  The name of the status checks

### HTTP response status codes

- **200** - OK

- **403** - Forbidden

- **404** - Resource not found

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example adding status checks to a branch protection rule

**Request:**

```curl
curl -L \
  -X POST \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_status_checks/contexts \
  -d '{
  "contexts": [
    "continuous-integration/travis-ci",
    "continuous-integration/jenkins"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get all status check contexts](#get-all-status-check-contexts).

## Set status check contexts

```
PUT /repos/{owner}/{repo}/branches/{branch}/protection/required_status_checks/contexts
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`contexts`** (array of strings) (required)
  The name of the status checks

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example updating status checks for a branch protection rule

**Request:**

```curl
curl -L \
  -X PUT \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_status_checks/contexts \
  -d '{
  "contexts": [
    "continuous-integration/travis-ci"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get all status check contexts](#get-all-status-check-contexts).

## Remove status check contexts

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/required_status_checks/contexts
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`contexts`** (array of strings) (required)
  The name of the status checks

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example removing status checks from a branch protection rule

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/required_status_checks/contexts \
  -d '{
  "contexts": [
    "continuous-integration/jenkins"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get all status check contexts](#get-all-status-check-contexts).

## Get access restrictions

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/restrictions
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Lists who has access to this protected branch.
Note

Users, apps, and teams restrictions are only available for organization-owned repositories.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions
```

**Response schema (Status: 200):**

* `url`: required, string, format: uri
* `users_url`: required, string, format: uri
* `teams_url`: required, string, format: uri
* `apps_url`: required, string, format: uri
* `users`: required, array of objects:
  * `login`: string
  * `id`: integer, format: int64
  * `node_id`: string
  * `avatar_url`: string
  * `gravatar_id`: string
  * `url`: string
  * `html_url`: string
  * `followers_url`: string
  * `following_url`: string
  * `gists_url`: string
  * `starred_url`: string
  * `subscriptions_url`: string
  * `organizations_url`: string
  * `repos_url`: string
  * `events_url`: string
  * `received_events_url`: string
  * `type`: string
  * `site_admin`: boolean
  * `user_view_type`: string
* `teams`: required, array of `Team`:
  * `id`: required, integer
  * `node_id`: required, string
  * `name`: required, string
  * `slug`: required, string
  * `description`: required, string or null
  * `privacy`: string
  * `notification_setting`: string
  * `permission`: required, string
  * `permissions`: object:
    * `pull`: required, boolean
    * `triage`: required, boolean
    * `push`: required, boolean
    * `maintain`: required, boolean
    * `admin`: required, boolean
  * `url`: required, string, format: uri
  * `html_url`: required, string, format: uri
  * `members_url`: required, string
  * `repositories_url`: required, string, format: uri
  * `type`: required, string, enum: `enterprise`, `organization`
  * `access_source`: string, enum: `direct`, `organization`, `enterprise`
  * `organization_id`: integer
  * `enterprise_id`: integer
  * `parent`: required, any of:
    * **null**
    * **Team Simple**
      * `id`: required, integer
      * `node_id`: required, string
      * `url`: required, string, format: uri
      * `members_url`: required, string
      * `name`: required, string
      * `description`: required, string or null
      * `permission`: required, string
      * `privacy`: string
      * `notification_setting`: string
      * `html_url`: required, string, format: uri
      * `repositories_url`: required, string, format: uri
      * `slug`: required, string
      * `ldap_dn`: string
      * `type`: required, string, enum: `enterprise`, `organization`
      * `organization_id`: integer
      * `enterprise_id`: integer
* `apps`: required, array of objects:
  * `id`: integer
  * `slug`: string
  * `node_id`: string
  * `owner`: object:
    * `login`: string
    * `id`: integer
    * `node_id`: string
    * `url`: string
    * `repos_url`: string
    * `events_url`: string
    * `hooks_url`: string
    * `issues_url`: string
    * `members_url`: string
    * `public_members_url`: string
    * `avatar_url`: string
    * `description`: string
    * `gravatar_id`: string
    * `html_url`: string
    * `followers_url`: string
    * `following_url`: string
    * `gists_url`: string
    * `starred_url`: string
    * `subscriptions_url`: string
    * `organizations_url`: string
    * `received_events_url`: string
    * `type`: string
    * `site_admin`: boolean
    * `user_view_type`: string
  * `name`: string
  * `client_id`: string
  * `description`: string
  * `external_url`: string
  * `html_url`: string
  * `created_at`: string
  * `updated_at`: string
  * `permissions`: object:
    * `metadata`: string
    * `contents`: string
    * `issues`: string
    * `single_file`: string
  * `events`: array of string

## Delete access restrictions

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/restrictions
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Disables the ability to restrict who can push to this branch.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **204** - No Content

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions
```

**Response schema (Status: 204):**

## Get apps with access to the protected branch

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/apps
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Lists the GitHub Apps that have push access to this branch. Only GitHub Apps that are installed on the repository and that have been granted write access to the repository contents can be added as authorized actors on a protected branch.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/apps
```

**Response schema (Status: 200):**

Array of `GitHub app`:
  * `id`: required, integer
  * `slug`: string
  * `node_id`: required, string
  * `client_id`: string
  * `owner`: required, one of:
    * **Simple User**
      * `name`: string or null
      * `email`: string or null
      * `login`: required, string
      * `id`: required, integer, format: int64
      * `node_id`: required, string
      * `avatar_url`: required, string, format: uri
      * `gravatar_id`: required, string or null
      * `url`: required, string, format: uri
      * `html_url`: required, string, format: uri
      * `followers_url`: required, string, format: uri
      * `following_url`: required, string
      * `gists_url`: required, string
      * `starred_url`: required, string
      * `subscriptions_url`: required, string, format: uri
      * `organizations_url`: required, string, format: uri
      * `repos_url`: required, string, format: uri
      * `events_url`: required, string
      * `received_events_url`: required, string, format: uri
      * `type`: required, string
      * `site_admin`: required, boolean
      * `starred_at`: string
      * `user_view_type`: string
    * **Enterprise**
      * `description`: string or null
      * `html_url`: required, string, format: uri
      * `website_url`: string or null, format: uri
      * `id`: required, integer
      * `node_id`: required, string
      * `name`: required, string
      * `slug`: required, string
      * `created_at`: required, string or null, format: date-time
      * `updated_at`: required, string or null, format: date-time
      * `avatar_url`: required, string, format: uri
  * `name`: required, string
  * `description`: required, string or null
  * `external_url`: required, string, format: uri
  * `html_url`: required, string, format: uri
  * `created_at`: required, string, format: date-time
  * `updated_at`: required, string, format: date-time
  * `permissions`: required, object, additional properties: string:
    * `issues`: string
    * `checks`: string
    * `metadata`: string
    * `contents`: string
    * `deployments`: string
  * `events`: required, array of string
  * `installations_count`: integer

## Add app access restrictions

```
POST /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/apps
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Grants the specified apps push access for this branch. Only GitHub Apps that are installed on the repository and that have been granted write access to the repository contents can be added as authorized actors on a protected branch.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`apps`** (array of strings) (required)
  The GitHub Apps that have push access to this branch. Use the slugified version of the app name. Note: The list of users, apps, and teams in total is limited to 100 items.

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X POST \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/apps \
  -d '{
  "apps": [
    "octoapp"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get apps with access to the protected branch](#get-apps-with-access-to-the-protected-branch).

## Set app access restrictions

```
PUT /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/apps
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Replaces the list of apps that have push access to this branch. This removes all apps that previously had push access and grants push access to the new list of apps. Only GitHub Apps that are installed on the repository and that have been granted write access to the repository contents can be added as authorized actors on a protected branch.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`apps`** (array of strings) (required)
  The GitHub Apps that have push access to this branch. Use the slugified version of the app name. Note: The list of users, apps, and teams in total is limited to 100 items.

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X PUT \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/apps \
  -d '{
  "apps": [
    "octoapp"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get apps with access to the protected branch](#get-apps-with-access-to-the-protected-branch).

## Remove app access restrictions

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/apps
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Removes the ability of an app to push to this branch. Only GitHub Apps that are installed on the repository and that have been granted write access to the repository contents can be added as authorized actors on a protected branch.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`apps`** (array of strings) (required)
  The GitHub Apps that have push access to this branch. Use the slugified version of the app name. Note: The list of users, apps, and teams in total is limited to 100 items.

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/apps \
  -d '{
  "apps": [
    "my-app"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get apps with access to the protected branch](#get-apps-with-access-to-the-protected-branch).

## Get teams with access to the protected branch

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/teams
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Lists the teams who have push access to this branch. The list includes child teams.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/teams
```

**Response schema (Status: 200):**

Array of `Team`:
  * `id`: required, integer
  * `node_id`: required, string
  * `name`: required, string
  * `slug`: required, string
  * `description`: required, string or null
  * `privacy`: string
  * `notification_setting`: string
  * `permission`: required, string
  * `permissions`: object:
    * `pull`: required, boolean
    * `triage`: required, boolean
    * `push`: required, boolean
    * `maintain`: required, boolean
    * `admin`: required, boolean
  * `url`: required, string, format: uri
  * `html_url`: required, string, format: uri
  * `members_url`: required, string
  * `repositories_url`: required, string, format: uri
  * `type`: required, string, enum: `enterprise`, `organization`
  * `access_source`: string, enum: `direct`, `organization`, `enterprise`
  * `organization_id`: integer
  * `enterprise_id`: integer
  * `parent`: required, any of:
    * **null**
    * **Team Simple**
      * `id`: required, integer
      * `node_id`: required, string
      * `url`: required, string, format: uri
      * `members_url`: required, string
      * `name`: required, string
      * `description`: required, string or null
      * `permission`: required, string
      * `privacy`: string
      * `notification_setting`: string
      * `html_url`: required, string, format: uri
      * `repositories_url`: required, string, format: uri
      * `slug`: required, string
      * `ldap_dn`: string
      * `type`: required, string, enum: `enterprise`, `organization`
      * `organization_id`: integer
      * `enterprise_id`: integer

## Add team access restrictions

```
POST /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/teams
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Grants the specified teams push access for this branch. You can also give push access to child teams.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`teams`** (array of strings) (required)
  The slug values for teams

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example adding a team in a branch protection rule

**Request:**

```curl
curl -L \
  -X POST \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/teams \
  -d '{
  "teams": [
    "justice-league"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get teams with access to the protected branch](#get-teams-with-access-to-the-protected-branch).

## Set team access restrictions

```
PUT /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/teams
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Replaces the list of teams that have push access to this branch. This removes all teams that previously had push access and grants push access to the new list of teams. Team restrictions include child teams.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`teams`** (array of strings) (required)
  The slug values for teams

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example replacing a team in a branch protection rule

**Request:**

```curl
curl -L \
  -X PUT \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/teams \
  -d '{
  "teams": [
    "justice-league"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get teams with access to the protected branch](#get-teams-with-access-to-the-protected-branch).

## Remove team access restrictions

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/teams
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Removes the ability of a team to push to this branch. You can also remove push access for child teams.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`teams`** (array of strings) (required)
  The slug values for teams

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example removing a team in a branch protection rule

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/teams \
  -d '{
  "teams": [
    "octocats"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get teams with access to the protected branch](#get-teams-with-access-to-the-protected-branch).

## Get users with access to the protected branch

```
GET /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/users
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Lists the people who have push access to this branch.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

### HTTP response status codes

- **200** - OK

- **404** - Resource not found

### Code examples

#### Example

**Request:**

```curl
curl -L \
  -X GET \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/users
```

**Response schema (Status: 200):**

Array of `Simple User`:
  * `name`: string or null
  * `email`: string or null
  * `login`: required, string
  * `id`: required, integer, format: int64
  * `node_id`: required, string
  * `avatar_url`: required, string, format: uri
  * `gravatar_id`: required, string or null
  * `url`: required, string, format: uri
  * `html_url`: required, string, format: uri
  * `followers_url`: required, string, format: uri
  * `following_url`: required, string
  * `gists_url`: required, string
  * `starred_url`: required, string
  * `subscriptions_url`: required, string, format: uri
  * `organizations_url`: required, string, format: uri
  * `repos_url`: required, string, format: uri
  * `events_url`: required, string
  * `received_events_url`: required, string, format: uri
  * `type`: required, string
  * `site_admin`: required, boolean
  * `starred_at`: string
  * `user_view_type`: string

## Add user access restrictions

```
POST /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/users
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Grants the specified people push access for this branch.

TypeDescriptionarrayUsernames for people who can have push access. Note: The list of users, apps, and teams in total is limited to 100 items.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`users`** (array of strings) (required)
  The username for users

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example adding a user in a branch protection rule

**Request:**

```curl
curl -L \
  -X POST \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/users \
  -d '{
  "users": [
    "octocat"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get users with access to the protected branch](#get-users-with-access-to-the-protected-branch).

## Set user access restrictions

```
PUT /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/users
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Replaces the list of people that have push access to this branch. This removes all people that previously had push access and grants push access to the new list of people.

TypeDescriptionarrayUsernames for people who can have push access. Note: The list of users, apps, and teams in total is limited to 100 items.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`users`** (array of strings) (required)
  The username for users

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example replacing a user in a branch protection rule

**Request:**

```curl
curl -L \
  -X PUT \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/users \
  -d '{
  "users": [
    "octocat"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get users with access to the protected branch](#get-users-with-access-to-the-protected-branch).

## Remove user access restrictions

```
DELETE /repos/{owner}/{repo}/branches/{branch}/protection/restrictions/users
```

Protected branches are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team, GitHub Enterprise Cloud, and GitHub Enterprise Server. For more information, see GitHub's products in the GitHub Help documentation.
Removes the ability of a user to push to this branch.

TypeDescriptionarrayUsernames of the people who should no longer have push access. Note: The list of users, apps, and teams in total is limited to 100 items.

### Parameters

#### Headers

- **`accept`** (string)
  Setting to `application/vnd.github+json` is recommended.

#### Path and query parameters

- **`owner`** (string) (required)
  The account owner of the repository. The name is not case sensitive.

- **`repo`** (string) (required)
  The name of the repository without the .git extension. The name is not case sensitive.

- **`branch`** (string) (required)
  The name of the branch. Cannot contain wildcard characters. To use wildcard characters in branch names, use the GraphQL API.

#### Body parameters

- **`users`** (array of strings) (required)
  The username for users

### HTTP response status codes

- **200** - OK

- **422** - Validation failed, or the endpoint has been spammed.

### Code examples

#### Example removing a user in a branch protection rule

**Request:**

```curl
curl -L \
  -X DELETE \
  https://api.github.com/repos/OWNER/REPO/branches/BRANCH/protection/restrictions/users \
  -d '{
  "users": [
    "octocat"
  ]
}'
```

**Response schema (Status: 200):**

Same response schema as [Get users with access to the protected branch](#get-users-with-access-to-the-protected-branch).