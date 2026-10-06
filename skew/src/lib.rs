//! `skew` — is the ACTIVATED release the latest published one? Replaces `spira/skew.sh`
//! (DESIGN.md is the contract; this was written from the script's intent and its callers,
//! not ported line by line, though the decision logic below tracks the original closely
//! because that logic — not its bash expression — was already correct and load-bearing).
//!
//! Subcommands: `check [--escalate]`, `units`, `refresh [repo]`, `gap [repo]`, `copies`,
//! `foreign <repo> <base> <ref>`. See DESIGN.md §3 for the exact contract each one keeps
//! (exit codes, what goes to stdout vs stderr — callers such as `deploy.sh` capture only
//! stdout from `check`, so that split is load-bearing, not cosmetic).

pub mod ports;
pub mod real;
#[cfg(test)]
mod tests;

use ports::World;
use std::path::{Path, PathBuf};

pub const EXIT_OK: i32 = 0;
pub const EXIT_FINDING: i32 = 1;
pub const EXIT_CANNOT_CHECK: i32 = 3;

// =============================================================================================
// harness_in / harness_in_ref — thin wrappers; the real filtering is exclude.sh's (unchanged,
// still bash, out of this bead's scope). Kept as functions here only so `foreign`/`copies`
// read as they did in bash.
// =============================================================================================

fn harness_in(w: &dyn World, repo: &Path) -> Vec<String> {
    w.harness_in(repo)
}
fn harness_in_ref(w: &dyn World, repo: &Path, ref_: &str) -> Vec<String> {
    w.harness_in_ref(repo, ref_)
}

// =============================================================================================
// foreign — the landing gate's fence. See spira/skew.sh's original comment block (DESIGN.md
// §5) for the invariants this preserves: fails closed including on its own confusion; scoped
// to the vendored COPY, not the whole repository.
// =============================================================================================

pub fn foreign(w: &dyn World, repo: &str, base: &str, ref_: &str) -> i32 {
    if repo.is_empty() || base.is_empty() || ref_.is_empty() {
        w.err("skew: foreign needs <repo> <base> <ref>");
        return 1;
    }
    let repo_path = PathBuf::from(repo);

    // Fails closed on its own confusion too: a resolution failure is refused, never read
    // as the override being unset.
    match w.env("SPIRA_ALLOW_FOREIGN_HARNESS") {
        Ok(v) => {
            if v.map(|v| !v.is_empty()).unwrap_or(false) {
                return 0;
            }
        }
        Err(e) => {
            w.err(&format!("skew: {e}"));
            return 1;
        }
    }

    let spira_repo = match w.env("SPIRA_REPO") {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => {
            w.err(&format!("skew: {e}"));
            return 1;
        }
    };
    if w.same_repo(&repo_path, Path::new(&spira_repo)) {
        return 0;
    }
    let home = w.home_repo();
    if let Some(home_root) = w.repo_root(&home) {
        if w.same_repo(&repo_path, &home_root) {
            return 0;
        }
    }
    if let Some(home_field_path) = w.repo_field(&home, "path") {
        if !home_field_path.is_empty() && w.same_repo(&repo_path, Path::new(&home_field_path)) {
            return 0;
        }
    }

    let dirs = harness_in_ref(w, &repo_path, ref_);
    if dirs.is_empty() {
        return 0; // no copy in that ref; nothing this fence judges
    }

    let changed = w.changed_files(&repo_path, base, ref_);
    let mut hit: Vec<String> = Vec::new();
    for f in &changed {
        if f.is_empty() {
            continue;
        }
        for d in &dirs {
            if d.is_empty() {
                continue;
            }
            let inside = if d == "." {
                true
            } else {
                f.starts_with(&format!("{d}/"))
            };
            if inside {
                hit.push(f.clone());
                break;
            }
        }
    }
    if hit.is_empty() {
        return 0;
    }
    hit.sort();
    hit.dedup();
    for h in &hit {
        w.out(h);
    }

    let home_name = w.home_repo();
    let mut msg = String::new();
    msg.push('\n');
    msg.push_str("REFUSED by skew — this branch changes a COPY of the harness.\n\n");
    for h in &hit {
        msg.push_str("    ");
        msg.push_str(h);
        msg.push('\n');
    }
    msg.push('\n');
    msg.push_str(&format!(
        "Those paths are inside a vendored copy of the harness, in a repository that is\n\
         not the harness's own. The harness in force is {spira_repo}, so work landing\n\
         here would pass its gate, close its bead, and never run. Nothing downstream can\n\
         tell the difference: the tree that was edited is self-consistent.\n\n\
         Move the change to repo:{home_name} and re-cut the branch there. If the vendored copy\n\
         is what you actually meant to change, say so:  SPIRA_ALLOW_FOREIGN_HARNESS=1"
    ));
    w.err(&msg);
    1
}

// =============================================================================================
// copies — every mapped repository that carries a harness, and whether it is ours.
// =============================================================================================

pub fn copies(w: &dyn World) -> i32 {
    let mut found = false;
    for n in w.repo_names() {
        let Some(p) = w.repo_root(&n) else { continue };
        if !w.exists(&p) {
            continue;
        }
        for d in harness_in(w, &p) {
            if d.is_empty() {
                continue;
            }
            found = true;
            let spira_repo = match w.env("SPIRA_REPO") {
                Ok(v) => v.unwrap_or_default(),
                Err(e) => {
                    w.err(&format!("skew: {e}"));
                    return 1;
                }
            };
            let kind = if w.same_repo(&p, Path::new(&spira_repo)) { "self" } else { "second" };
            w.out(&format!("{n} {} {d} {kind}", p.display()));
        }
    }
    if found {
        0
    } else {
        1
    }
}

// =============================================================================================
// check / check_local — see spira/skew.sh's original comments (preserved in DESIGN.md §2)
// for NOT-LATEST / MANIFEST-MISMATCH / LOCAL-BEHIND / LOCAL-TAMPERED / CANNOT-VERIFY.
// =============================================================================================

pub fn check(w: &dyn World, escalate_flag: bool) -> i32 {
    let releases = match w.env("SPIRA_RELEASES") {
        Ok(v) => v.filter(|v| !v.is_empty()),
        Err(e) => {
            w.err(&format!("skew: {e}"));
            return EXIT_CANNOT_CHECK;
        }
    };
    let Some(releases) = releases else {
        w.err("skew: SPIRA_RELEASES is not set — cannot check release currency");
        return EXIT_CANNOT_CHECK;
    };
    let releases_dir = PathBuf::from(&releases);
    let current_link = releases_dir.join("current");

    if !w.is_symlink(&current_link) {
        // CHECKOUT MODE.
        let spira_repo = match w.env("SPIRA_REPO") {
            Ok(v) => v.unwrap_or_default(),
            Err(e) => {
                w.err(&format!("skew: {e}"));
                return EXIT_CANNOT_CHECK;
            }
        };
        let repo_path = PathBuf::from(&spira_repo);
        if w.is_git_repo(&repo_path) {
            let (rc, out) = gap_line(w, &repo_path);
            w.out(&out);
            if rc == EXIT_FINDING && escalate_flag {
                escalate(w, "v2:BEHIND=1", &format!("BEHIND {out}"));
            }
            return rc;
        }
        w.err(&format!("skew: no release is activated at {} — cannot determine skew", current_link.display()));
        return EXIT_CANNOT_CHECK;
    }

    let Some(activated_name) = w.readlink(&current_link) else {
        w.err(&format!("skew: cannot read symlink {}", current_link.display()));
        return EXIT_CANNOT_CHECK;
    };

    let manifest = releases_dir.join(&activated_name).join("MANIFEST");
    let Some(manifest_text) = w.read_to_string(&manifest) else {
        w.err(&format!("skew: MANIFEST missing at {}", manifest.display()));
        return EXIT_CANNOT_CHECK;
    };
    let manifest_commit = manifest_text
        .lines()
        .find_map(|l| l.strip_prefix("commit "))
        .map(str::trim)
        .unwrap_or("");
    if manifest_commit.is_empty() || !is_sha40(manifest_commit) {
        w.err(&format!("skew: MANIFEST at {} has no valid commit SHA", manifest.display()));
        return EXIT_CANNOT_CHECK;
    }

    // LOCAL RELEASE MODE (queue.local).
    if w.repo_land(&w.home_repo()) == "queue.local" {
        return check_local(w, &activated_name, manifest_commit, escalate_flag);
    }

    let spira_repo = match w.env("SPIRA_REPO") {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => {
            w.err(&format!("skew: {e}"));
            return EXIT_CANNOT_CHECK;
        }
    };
    let repo_path = PathBuf::from(&spira_repo);

    let tag_sidecar = releases_dir.join(".tags").join(&activated_name);
    let mut release_tag = w.read_to_string(&tag_sidecar).map(|s| s.trim().to_string()).unwrap_or_default();

    // NOT APPLICABLE, NOT CANNOT-VERIFY — but only here. The sidecar names the release tag
    // with no git at all (artifact mode judges NOT-LATEST from it); a checkout is needed only
    // to match tags to the MANIFEST commit when the sidecar is empty. A release-only install
    // with no sidecar and no checkout cannot be asked the question at all: exit 0, named
    // (sp-wecsq's case). Returning earlier, whenever SPIRA_REPO had no .git, silenced
    // artifact-mode verdicts too (test-skew-check-release, the regression this fixes).
    if release_tag.is_empty() && !w.is_git_repo(&repo_path) {
        w.out(&format!(
            "skew: not applicable — no release tag recorded for {activated_name} and no harness checkout at SPIRA_REPO ({}) to match tags against",
            repo_path.display()
        ));
        return EXIT_OK;
    }

    let all_tags = match resolve_all_tags(w, &repo_path) {
        Ok(t) => t,
        Err(code) => return code,
    };

    let latest_tag = all_tags.last().cloned().unwrap_or_default();

    if release_tag.is_empty() {
        for t in &all_tags {
            if let Some(tc) = w.rev_parse(&repo_path, &format!("{t}^{{commit}}"), false) {
                if tc == manifest_commit {
                    release_tag = t.clone();
                    break;
                }
            }
        }
    }

    let mut findings = String::new();
    let mut hard = false;
    let mut cond_not_latest = 0;
    let mut cond_mismatch = 0;

    if !release_tag.is_empty() && release_tag != latest_tag {
        hard = true;
        cond_not_latest = 1;
        findings.push_str(&format!(
            "NOT-LATEST activated {activated_name} is not the latest published release {latest_tag}\n"
        ));
    }

    if !release_tag.is_empty() {
        let tag_commit = w.rev_parse(&repo_path, &format!("{release_tag}^{{commit}}"), false).unwrap_or_default();
        if !tag_commit.is_empty() && manifest_commit != tag_commit {
            hard = true;
            cond_mismatch = 1;
            findings.push_str(&format!(
                "MANIFEST-MISMATCH MANIFEST records {manifest_commit} but release tag {release_tag} points at {tag_commit}\n"
            ));
        }
    } else {
        findings.push_str(&format!(
            "CANNOT-VERIFY no release tag found for {activated_name}; MANIFEST commit {manifest_commit} unverified\n"
        ));
    }

    if findings.is_empty() {
        w.out(&format!("skew: in effect — {activated_name} is the latest published release; MANIFEST matches tag"));
        return EXIT_OK;
    }

    w.out(&findings);

    if !hard {
        w.err("skew: the check could not complete — this is not a clean verdict");
        return EXIT_CANNOT_CHECK;
    }

    if escalate_flag {
        let key = format!("v2:NOT-LATEST={cond_not_latest} MANIFEST-MISMATCH={cond_mismatch}");
        escalate(w, &key, &findings);
    }
    EXIT_FINDING
}

fn is_sha40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Resolves the release-tag source: the harness's own git tags, a local directory of
/// tarballs with `.tag` sidecars, or `gh release list` against a forge repo. Errors are
/// already printed to stderr (matching bash) before returning the "cannot check" code.
fn resolve_all_tags(w: &dyn World, repo: &Path) -> Result<Vec<String>, i32> {
    let mut all_tags = w.tags_matching(repo, "spira-release-*");
    all_tags.sort();

    let release_repo = match w.env("SPIRA_RELEASE_REPO") {
        Ok(v) => v.filter(|v| !v.is_empty()),
        Err(e) => {
            w.err(&format!("skew: {e}"));
            return Err(EXIT_CANNOT_CHECK);
        }
    };
    let rel_repo = match release_repo {
        Some(v) => Some(v),
        None => match w.env("SPIRA_GH_INTAKE_REPO") {
            Ok(v) => v.filter(|v| !v.is_empty()),
            Err(e) => {
                w.err(&format!("skew: {e}"));
                return Err(EXIT_CANNOT_CHECK);
            }
        },
    };

    let rel_dir: Option<String> = rel_repo.as_deref().and_then(|r| {
        if let Some(rest) = r.strip_prefix("file://") {
            Some(rest.to_string())
        } else if r.starts_with('/') {
            Some(r.to_string())
        } else {
            None
        }
    });

    if all_tags.is_empty() {
        if let Some(dir) = &rel_dir {
            let dir_path = PathBuf::from(dir);
            if !w.exists(&dir_path) {
                w.err(&format!(
                    "skew: SPIRA_RELEASE_REPO names the local release directory {dir}, which does not exist"
                ));
                return Err(EXIT_CANNOT_CHECK);
            }
            all_tags = w.local_tag_sidecars(&dir_path);
        } else if let Some(slug) = &rel_repo {
            match w.gh_release_list(slug) {
                Ok(json) => {
                    all_tags = parse_gh_release_tags(&json);
                    all_tags.sort();
                }
                Err(e) => {
                    // USER is not a registered config key — this read of the raw
                    // environment cannot fail, but `env()` is fallible in general.
                    let who = match w.env("USER") {
                        Ok(v) => v.unwrap_or_else(|| "the operator".into()),
                        Err(env_err) => {
                            w.err(&format!("skew: {env_err}"));
                            return Err(EXIT_CANNOT_CHECK);
                        }
                    };
                    w.err(&format!(
                        "skew: could not list the releases of {slug} with gh ({e}) — make a gh credential visible to user units (the systemd user manager's environment, or gh auth as {who}), or set SPIRA_RELEASE_REPO to a local directory of release tarballs"
                    ));
                    return Err(EXIT_CANNOT_CHECK);
                }
            }
        } else {
            w.err("skew: no release source — set SPIRA_RELEASE_REPO to the forge repository that publishes this install's releases (owner/repo) or to a local directory of release tarballs");
            return Err(EXIT_CANNOT_CHECK);
        }
    }

    if all_tags.is_empty() {
        let named = rel_dir.clone().or(rel_repo.clone()).unwrap_or_else(|| "the checkout".into());
        w.err(&format!("skew: no release tags found in {named} — cannot determine release currency"));
        return Err(EXIT_CANNOT_CHECK);
    }
    Ok(all_tags)
}

/// `gh release list --json tagName,isDraft` -> published (non-draft) `spira-release-*` tags.
/// A hand-written JSON scan (no serde dependency for one small, fixed shape) — see DESIGN.md
/// §6 for why this stays intentionally simple rather than pulling in a JSON crate for one
/// caller.
fn parse_gh_release_tags(json: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Each release object: {"isDraft":false,"tagName":"spira-release-..."} in any key order.
    for obj in split_json_objects(json) {
        let is_draft = obj.contains("\"isDraft\":true") || obj.contains("\"isDraft\": true");
        if is_draft {
            continue;
        }
        if let Some(tag) = extract_json_string(&obj, "tagName") {
            if tag.starts_with("spira-release-") {
                out.push(tag);
            }
        }
    }
    out
}

fn split_json_objects(json: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (i, c) in json.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start.take() {
                        out.push(json[s..=i].to_string());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn extract_json_string(obj: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let idx = obj.find(&pat)?;
    let rest = &obj[idx + pat.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

pub fn check_local(w: &dyn World, activated_name: &str, manifest_commit: &str, escalate_flag: bool) -> i32 {
    let home = w.home_repo();
    let Some(repo) = w.repo_root(&home) else {
        w.err(&format!("skew: cannot check — repo:{home} has no resolvable git checkout"));
        return EXIT_CANNOT_CHECK;
    };
    if !w.is_git_repo(&repo) {
        w.err(&format!("skew: cannot check — repo:{home} has no resolvable git checkout"));
        return EXIT_CANNOT_CHECK;
    }
    // BY NAME, NOT BY PATH. `landref` given a path re-derives the name via repo_name_at,
    // which prefers SPIRA_HOME_REPO for a path matching SPIRA_REPO — `home` above is
    // already the one true name for this checkout; re-deriving it a second, weaker way
    // here would only be able to get it wrong (skew.sh's own `spira_landref "$home"`, not
    // `spira_landref "$repo"`).
    let Some(base) = w.landref(Path::new(&home)) else {
        w.err(&format!("skew: cannot check — cannot resolve the ref repo:{home} lands on"));
        return EXIT_CANNOT_CHECK;
    };
    let Some(tip) = w.rev_parse(&repo, &base, false) else {
        w.err(&format!("skew: cannot check — ref {base} does not resolve in {}", repo.display()));
        return EXIT_CANNOT_CHECK;
    };

    let mut findings = String::new();
    let mut hard = false;
    let mut cond_behind = 0;
    let mut cond_tampered = 0;
    let mut hotfix_line = String::new();

    let releases = match w.env("SPIRA_RELEASES") {
        Ok(v) => v.map(PathBuf::from),
        Err(e) => {
            w.err(&format!("skew: {e}"));
            return EXIT_CANNOT_CHECK;
        }
    };
    let (ok, verify_out) = w.release_verify_no_pre_activate(activated_name, releases.as_deref());
    if !ok {
        hard = true;
        cond_tampered = 1;
        let flat = verify_out.replace('\n', " ");
        findings.push_str(&format!("LOCAL-TAMPERED release {activated_name} fails verify: {flat}\n"));
    }

    if manifest_commit != tip {
        let status = w.release_status();
        hotfix_line = status.lines().find(|l| l.starts_with("RUNNING UNLANDED ")).unwrap_or("").to_string();
        let is_recorded = hotfix_line.starts_with(&format!("RUNNING UNLANDED {manifest_commit}:"));
        if !is_recorded {
            if w.is_ancestor(&repo, manifest_commit, &tip) {
                let behind = w.rev_list_count(&repo, &format!("{manifest_commit}..{tip}"));
                hard = true;
                cond_behind = 1;
                let behind_s = behind.map(|n| n.to_string()).unwrap_or_else(|| "an unresolved number of".into());
                findings.push_str(&format!(
                    "LOCAL-BEHIND activated {activated_name} ({manifest_commit}) is {behind_s} commit(s) behind {base} ({tip})\n"
                ));
            } else {
                findings.push_str(&format!(
                    "CANNOT-VERIFY activated {activated_name} ({manifest_commit}) is neither {base}'s tip ({tip}) nor a recorded standing hotfix\n"
                ));
            }
        }
    }

    if findings.is_empty() {
        if manifest_commit == tip {
            w.out(&format!("skew: in effect — {activated_name} matches {base} ({tip}); MANIFEST verifies"));
        } else {
            w.out(&format!(
                "skew: in effect — {activated_name} is a recorded standing hotfix ({hotfix_line}); {base} is at {tip}; MANIFEST verifies"
            ));
        }
        return EXIT_OK;
    }

    w.out(&findings);

    if !hard {
        w.err("skew: the check could not complete — this is not a clean verdict");
        return EXIT_CANNOT_CHECK;
    }

    if escalate_flag {
        let key = format!("v2:LOCAL-BEHIND={cond_behind} LOCAL-TAMPERED={cond_tampered}");
        escalate(w, &key, &findings);
    }
    EXIT_FINDING
}

// =============================================================================================
// escalate — once per distinct CONDITION, not once per pass. See DESIGN.md §7.
// =============================================================================================

pub fn escalate(w: &dyn World, condition_key: &str, findings: &str) {
    let stamp_key = format!("skew.escalated-{}", cksum(condition_key));
    if let Some(prior) = w.stamp_read(&stamp_key) {
        let shown = if prior.is_empty() { condition_key.to_string() } else { prior };
        w.out(&format!("skew: condition already reported — {shown}"));
        return;
    }

    let default_action: &str = if condition_key.contains("MANIFEST-MISMATCH=1") {
        "the release artifact and its git tag disagree — verify the release was built from the correct commit; rebuild and re-activate if not"
    } else if condition_key.contains("LOCAL-TAMPERED=1") {
        "the activated release no longer matches its own MANIFEST — release build the commit again and release activate the rebuilt release"
    } else if condition_key.contains("LOCAL-BEHIND=1") {
        "the activated release is behind local/main's tip — land it forward with queue land-local, or if the tip is bad, queue rollback-local"
    } else if condition_key.contains("LOCAL-SKEW=1") {
        "what is running does not match local/main's head — land the round again with queue land-local, or undo with queue rollback-local; refresh will not act on its own"
    } else {
        "activate the latest published release — download the latest tarball and run release install-tarball with it"
    };

    let subj = "The Spira copy in force is not the code that landed";
    let body = format!(
        "## Question\n{subj}\n\n## Default\n{default_action}\n\nbeads can be closed, gated and merged while the behaviour they changed never takes effect — the tree that was edited is self-consistent, so nothing downstream reports a fault\n\n{findings}\n"
    );

    match w.mail_send_question(subj, &body, default_action) {
        Ok(()) => {
            w.stamp_write(&stamp_key, condition_key);
            w.out(&format!("skew: escalated — {condition_key}"));
        }
        Err(out) => {
            w.out(&format!("skew: escalation failed: {out}"));
        }
    }
}

/// A stable, deterministic digest of the condition key, used only as a filename suffix and
/// only ever compared against itself (never against a stamp `skew.sh` wrote — a mismatch
/// there just re-escalates a standing condition once, the same documented, acceptable cost
/// a versioned condition key already pays on any scheme change). FNV-1a, not POSIX `cksum`:
/// no caller needs bit-for-bit parity with the bash tool that no longer exists.
fn cksum(data: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

// =============================================================================================
// refresh — advance a checkout to its base ref via stage-and-swap (checkout mode), rebuild
// and activate (release mode), or check-only alarm (queue.local). See DESIGN.md §8.
// =============================================================================================

pub fn refresh(w: &dyn World, repo_arg: Option<&str>) -> i32 {
    let mut repo = match repo_arg {
        Some(r) => PathBuf::from(r),
        None => {
            let home = w.home_repo();
            match w.repo_root(&home) {
                Some(p) if w.is_git_repo(&p) => p,
                _ => match w.env("SPIRA_REPO") {
                    Ok(v) => PathBuf::from(v.unwrap_or_default()),
                    Err(e) => {
                        w.out(&format!("skew: refresh: {e}"));
                        return 1;
                    }
                },
            }
        }
    };
    if repo.as_os_str().is_empty() {
        repo = match w.env("SPIRA_REPO") {
            Ok(v) => PathBuf::from(v.unwrap_or_default()),
            Err(e) => {
                w.out(&format!("skew: refresh: {e}"));
                return 1;
            }
        };
    }

    if let Some(name) = repo_name_for_path(w, &repo) {
        if w.repo_land(&name) == "queue.local" {
            return refresh_check_only(w, &repo, &name);
        }
    }

    let releases = match w.env("SPIRA_RELEASES") {
        Ok(v) => v.filter(|v| !v.is_empty()),
        Err(e) => {
            w.out(&format!("skew: refresh: {e}"));
            return 1;
        }
    };
    let current_is_symlink = releases
        .as_ref()
        .map(|r| w.is_symlink(&PathBuf::from(r).join("current")))
        .unwrap_or(false);

    if current_is_symlink {
        if !w.is_git_repo(&repo) {
            w.out(&format!("skew: refresh: {} is not a git checkout", repo.display()));
            return 1;
        }
        let Some(base) = w.landref(&repo) else {
            w.out("skew: refresh: cannot resolve the ref $repo lands on");
            return 1;
        };
        let remote = w.ref_remote(&base, &repo);
        if let Some(r) = &remote {
            w.fetch(&repo, r);
        }
        let behind = w.rev_list_count(&repo, &format!("HEAD..{base}")).unwrap_or(0);
        if behind == 0 {
            w.out(&format!("skew: refresh: release mode — already at {base}"));
            return 0;
        }
        if !w.merge_ff_only(&repo, &base) {
            w.out(&format!("skew: refresh: cannot fast-forward to {base}"));
            return 1;
        }
        let Some(sha) = w.rev_parse(&repo, "HEAD", false) else {
            w.out("skew: refresh: cannot resolve HEAD after fast-forward");
            return 1;
        };
        let releases_dir = PathBuf::from(releases.unwrap());
        if w.release_build_verify_activate(&sha, &repo, &base, &releases_dir).is_err() {
            w.out(&format!("skew: refresh: release build/verify/activate of {sha} failed"));
            return 1;
        }
        w.out(&format!("skew: refreshed — new release installed ({behind} commit(s))"));
        w.overrides_apply(&repo);
        return 0;
    }

    if !w.is_git_repo(&repo) {
        w.out(&format!("skew: refresh: {} is not a git checkout", repo.display()));
        return 1;
    }
    let Some(base) = w.landref(&repo) else {
        w.out("skew: refresh: cannot resolve the ref $repo lands on");
        return 1;
    };
    let base_branch = w.ref_branch(&base);
    let remote = w.ref_remote(&base, &repo);
    if let Some(r) = &remote {
        w.fetch(&repo, r);
    }

    let behind = w.rev_list_count(&repo, &format!("HEAD..{base}")).unwrap_or(0);
    if behind == 0 {
        return 0;
    }

    let dirty = w.dirty_tracked(&repo);
    if !dirty.is_empty() {
        let stash_tag = format!("skew-refresh-{}", w.now_stamp());
        match w.stash_push(&repo, &stash_tag) {
            Ok(()) => {
                w.out(&format!("skew: refresh: stashed dirty tracked files (tag: {stash_tag}):"));
                w.out(&dirty);
            }
            Err(e) => {
                w.out(&format!("skew: refresh declined — tracked files are modified and stash failed: {e}"));
                return 1;
            }
        }
    }

    let current = w.current_branch(&repo);
    if current != base_branch {
        let shown = if current.is_empty() { "a detached HEAD".to_string() } else { current };
        w.out(&format!("skew: refresh declined — checkout is on {shown}, not {base_branch}"));
        return 1;
    }

    let mut err_count = 0;
    for (status, f) in w.diff_status(&repo, &format!("HEAD..{base}")) {
        if f.is_empty() {
            continue;
        }
        match status {
            'M' => match w.show_file(&repo, &base, &f) {
                Some(content) => {
                    let mode = w.file_mode(&repo, &base, &f);
                    let executable = mode.as_deref() == Some("100755");
                    if w.write_staged(&repo.join(&f), &content, executable).is_err() {
                        err_count += 1;
                    }
                }
                None => err_count += 1,
            },
            'A' => {
                w.mkdir_p(&repo.join(&f).parent().map(Path::to_path_buf).unwrap_or_default());
                match w.show_file(&repo, &base, &f) {
                    Some(content) => {
                        let mode = w.file_mode(&repo, &base, &f);
                        let executable = mode.as_deref() == Some("100755");
                        if w.write_staged(&repo.join(&f), &content, executable).is_err() {
                            err_count += 1;
                        }
                    }
                    None => err_count += 1,
                }
            }
            'D' => w.remove_file(&repo.join(&f)),
            _ => {}
        }
    }
    if err_count > 0 {
        w.out(&format!("skew: refresh: stage-and-swap failed for {err_count} file(s)"));
        return 1;
    }

    if !w.reset_mixed(&repo, &base) {
        w.out("skew: refresh: git reset --mixed failed after stage-and-swap");
        return 1;
    }

    w.out(&format!("skew: refreshed to {base} ({behind} commit(s))"));
    w.overrides_apply(&repo);
    0
}

fn repo_name_for_path(w: &dyn World, path: &Path) -> Option<String> {
    for n in w.repo_names() {
        if let Some(p) = w.repo_root(&n) {
            if w.same_repo(path, &p) {
                return Some(n);
            }
        }
    }
    None
}

fn refresh_check_only(w: &dyn World, repo: &Path, name: &str) -> i32 {
    // BY NAME, NOT BY PATH — see check_local's identical note. `name` is the one true name
    // for this path, already resolved by the caller; `landref(repo)` would re-derive a
    // weaker guess and can disagree with it (skew.sh's own `spira_landref "$name"`).
    let Some(base) = w.landref(Path::new(name)) else {
        w.out(&format!("skew: refresh: queue.local — cannot resolve the ref {} lands on", repo.display()));
        return 1;
    };
    let Some(base_sha) = w.rev_parse(repo, &base, false) else {
        w.out(&format!("skew: refresh: queue.local — ref {base} does not resolve"));
        return 1;
    };

    let releases = match w.env("SPIRA_RELEASES") {
        Ok(v) => v.filter(|v| !v.is_empty()),
        Err(e) => {
            w.out(&format!("skew: refresh: {e}"));
            return 1;
        }
    };
    let running: String = if let Some(r) = &releases {
        let current_link = PathBuf::from(r).join("current");
        if !w.is_symlink(&current_link) {
            if !w.is_git_repo(repo) {
                w.out(&format!("skew: refresh: {} is not a git checkout", repo.display()));
                return 1;
            }
            w.rev_parse(repo, "HEAD", false).unwrap_or_default()
        } else {
            let activated_name = w.readlink(&current_link).unwrap_or_default();
            let manifest = PathBuf::from(r).join(&activated_name).join("MANIFEST");
            let text = w.read_to_string(&manifest);
            let commit = text
                .as_deref()
                .and_then(|t| t.lines().find_map(|l| l.strip_prefix("commit ")))
                .map(|s| s.trim().to_string());
            match commit {
                Some(c) if !c.is_empty() => c,
                _ => {
                    w.out(&format!("skew: refresh: queue.local — MANIFEST missing or unreadable at {}", manifest.display()));
                    return 1;
                }
            }
        }
    } else {
        if !w.is_git_repo(repo) {
            w.out(&format!("skew: refresh: {} is not a git checkout", repo.display()));
            return 1;
        }
        w.rev_parse(repo, "HEAD", false).unwrap_or_default()
    };


    if running == base_sha {
        w.out(&format!("skew: refresh: queue.local — running ({running}) matches {base}; nothing to deploy"));
        return 0;
    }

    let status = w.release_status();
    let hotfix_line = status.lines().find(|l| l.starts_with("RUNNING UNLANDED ")).unwrap_or("");
    if hotfix_line.starts_with(&format!("RUNNING UNLANDED {running}:")) {
        w.out(&format!(
            "skew: refresh: queue.local — running ({running}) is a recorded standing hotfix ({hotfix_line}); refresh never resets it"
        ));
        return 0;
    }

    let finding = format!(
        "LOCAL-SKEW running {running} does not match {base} ({base_sha}) — queue.local deploys only through queue land-local; refresh never resets it"
    );
    w.out(&format!("skew: {finding}"));
    escalate(w, "v1:LOCAL-SKEW=1", &finding);
    1
}

// =============================================================================================
// gap — how far is the running checkout behind its base ref?
// =============================================================================================

/// The pure decision: (exit code, the one line gap prints — stdout on 0/1, stderr on 3).
/// Kept separate from `World::out`/`err` so `check`'s CHECKOUT MODE branch can re-print the
/// same line to ITS OWN stdout regardless of exit code, exactly as bash's
/// `_gap_out="$(gap "$SPIRA_REPO" 2>&1)"; printf '%s\n' "$_gap_out"` did.
fn gap_line(w: &dyn World, repo: &Path) -> (i32, String) {
    if !w.is_git_repo(repo) {
        return (EXIT_CANNOT_CHECK, format!("skew: gap: {} is not a git checkout", repo.display()));
    }
    let Some(base) = w.landref(repo) else {
        return (EXIT_CANNOT_CHECK, "skew: gap: cannot resolve the ref $repo lands on".to_string());
    };
    let remote = w.ref_remote(&base, repo);
    if let Some(r) = &remote {
        if !w.fetch(repo, r) {
            return (EXIT_CANNOT_CHECK, format!("skew: gap: cannot fetch from {r} — gap is unknown"));
        }
    }
    if w.rev_parse(repo, &base, false).is_none() {
        return (EXIT_CANNOT_CHECK, format!("skew: gap: ref {base} does not resolve — cannot report gap"));
    }
    let Some(behind) = w.rev_list_count(repo, &format!("HEAD..{base}")) else {
        return (EXIT_CANNOT_CHECK, "skew: gap: cannot count commits between HEAD and {base}".to_string());
    };
    let current = w.rev_parse_short(repo, "HEAD").unwrap_or_default();
    let base_short = w.rev_parse_short(repo, &base).unwrap_or_default();
    if behind == 0 {
        return (EXIT_OK, format!("skew: gap: HEAD ({current}) is at {base} — 0 commits behind"));
    }
    (EXIT_FINDING, format!("skew: gap: HEAD ({current}) is {behind} commit(s) behind {base} ({base_short})"))
}

pub fn gap(w: &dyn World, repo_arg: Option<&str>) -> i32 {
    let spira_repo = match w.env("SPIRA_REPO") {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => {
            w.err(&format!("skew: {e}"));
            return EXIT_CANNOT_CHECK;
        }
    };
    let repo = repo_arg.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&spira_repo));
    let (rc, line) = gap_line(w, &repo);
    if rc == EXIT_CANNOT_CHECK {
        w.err(&line);
    } else {
        w.out(&line);
    }
    rc
}

// =============================================================================================
// units — are the installed units what the templates render?
// =============================================================================================

pub fn units(w: &dyn World) -> i32 {
    // units-install is a compiled binary now (sp-31dm0, on top of sp-yyk47's own skew):
    // install.sh --diff retired to `units-install --diff`, resolved via SPIRA_INSTALL_SH
    // first (an explicit pin, matching skew.sh's own fix) or PATH otherwise — never a
    // path relative to SPIRA_HOME/../systemd, which only made sense for a bash script
    // living beside the old systemd/install.sh. EMPTY OR NOT EXECUTABLE both refuse — an
    // explicit SPIRA_INSTALL_SH pin pointing at nothing is exactly as unanswerable as no
    // pin and no PATH hit (skew.sh's own `[ -z "$installer" ] || [ ! -x "$installer" ]`).
    let installer = match w.env("SPIRA_INSTALL_SH") {
        Ok(v) => v.filter(|v| !v.is_empty()).or_else(|| w.which("units-install")),
        Err(e) => {
            w.err(&format!("skew: {e}"));
            return EXIT_CANNOT_CHECK;
        }
    };
    let Some(installer) = installer.filter(|p| w.is_executable(Path::new(p))) else {
        w.err("skew: units-install is missing (not on PATH) — unit staleness has no answer");
        return EXIT_CANNOT_CHECK;
    };
    let (rc, out) = w.install_diff(Path::new(&installer));
    if rc == 0 {
        w.out("skew: units — installed units match what this box renders");
        return EXIT_OK;
    }
    w.out(&out);
    EXIT_FINDING
}
