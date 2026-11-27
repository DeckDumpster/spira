//! `RealGit` against real git: when the push-mode landing tree needs no checkout.

use landing_pass::ports::Git;
use landing_pass::real::RealGit;

#[macro_use]
extern crate spira_test_id;

const BASE: &str = "refs/remotes/origin/main";

#[test_id(tt74ger)]
#[test]
fn a_landing_tree_is_on_landing_only_with_that_branch_checked_out_at_the_base() {
    let t = testkit::TempDir::new("landing-pass-on-landing");
    let (repo, tree) = (t.path().join("repo"), t.path().join("land"));
    std::fs::create_dir_all(&repo).unwrap();
    testkit::git(&repo, &["init", "-q", "-b", "main"]);
    testkit::git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    testkit::git(&repo, &["update-ref", BASE, "HEAD"]);
    let on = || RealGit.tree_on_landing(&tree, BASE);

    RealGit.tree_add_detached(&repo, &tree, BASE);
    assert!(!on(), "detached at the base is not on `landing`");

    assert!(RealGit.tree_checkout_landing(&tree, BASE));
    assert!(on());
    testkit::git(&repo, &["pack-refs", "--all"]);
    assert!(on(), "packed refs answer too");

    testkit::git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
    testkit::git(&repo, &["update-ref", BASE, "HEAD"]);
    assert!(!on(), "the base moved");

    assert!(RealGit.tree_checkout_landing(&tree, BASE));
    assert!(on());
    testkit::git(&tree, &["commit", "-q", "--allow-empty", "-m", "unpushed"]);
    assert!(!on(), "`landing` is ahead of the base");

    assert!(!RealGit.tree_on_landing(&tree, "refs/remotes/origin/absent"), "an unreadable base is not a match");
}
