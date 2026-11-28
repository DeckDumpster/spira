//! The input hashes over what git really lists: the real `input_trees` on a throwaway
//! repository, which is the subject.

#[macro_use]
extern crate spira_test_id;

use gate::ports::World;
use gate::real::Real;
use spira_config::input_hash::{input_hashes, Member};
use testkit::{git, TempDir};

#[test_id(t0xxrl0)]
#[test]
fn a_nested_member_and_a_root_input_reach_the_hash_from_a_real_tree() {
    let d = TempDir::new("gate-input-trees");
    let repo = d.path();
    let write = |path: &str, text: &str| {
        let p = repo.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    git(repo, &["init", "-q"]);
    let members = [
        Member { name: "nested".into(), dir: "libs/nested".into(), ..Default::default() },
        Member { name: "other".into(), dir: "other".into(), ..Default::default() },
    ];
    let hashes = || {
        git(repo, &["add", "-A"]);
        let tree = git(repo, &["write-tree"]);
        input_hashes(&members, &Real::new(repo.to_path_buf()).input_trees(repo, tree.trim()).unwrap())
    };
    write("libs/nested/src/lib.rs", "one");
    write("other/lib.rs", "one");
    write("Cargo.lock", "one");
    let first = hashes();
    write("libs/nested/src/lib.rs", "two");
    let nested = hashes();
    assert_ne!(nested["nested"], first["nested"]);
    assert_eq!(nested["other"], first["other"]);
    write("Cargo.lock", "two");
    let locked = hashes();
    assert_ne!(locked["nested"], nested["nested"]);
    assert_ne!(locked["other"], nested["other"]);
}
