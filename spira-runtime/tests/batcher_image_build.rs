//! The `batcher.local` image build at its boundary: a real git checkout, the scripted unit
//! manager. Its own binary because it edits `SPIRA_TOML`.

use spira_runtime::tasks::a::batcher_local::{image_unit_key, Leg, NAME};

#[macro_use]
extern crate spira_test_id;

fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test_id(t170zlw)]
#[test]
fn sp_4pne6q_the_build_unit_runs_testenv_on_a_checkout_of_the_tip_which_goes_when_it_ends() {
    use runtime::units::{ScriptedUnits, UnitHandle};
    use spira_runtime::tasks::a::batcher_local::edge;
    let dir = testkit::TempDir::new("image-build");
    let _env = testkit::env(&[("SPIRA_TOML", Some("unread-config-spec"))]);
    let (src, run) = (dir.path().join("src"), dir.path().join("run"));
    std::fs::create_dir_all(&src).unwrap();
    git_in(&src, &["init", "-q", "-b", "main"]);
    std::fs::write(src.join("f"), "x").unwrap();
    git_in(&src, &["add", "f"]);
    git_in(&src, &["commit", "-q", "-m", "x"]);
    let repo = batcher_cut::io::Repo { name: "spira".into(), path: src.clone(), base: "main".into(), forge: src.clone(), land: batcher_cut::io::Land::Local };
    let tip = git_in(&src, &["rev-parse", "HEAD"]);
    let tree = edge::image_tree(&run, "spira", "tagz");
    assert_eq!(tree, run.join("worktree/.image-spira-tagz"));
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("leftover"), "old").unwrap();
    let units = std::sync::Arc::new(ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0))));
    units.script(NAME, 60_000, runtime::units::Exit::Code(0));
    let e = spira_runtime::Effects { units: Some(units.clone()), ..Default::default() };
    let leg = Leg { unit: image_unit_key("spira", "tagz"), image: "tagz".into(), head: tip.clone(), ..Leg::on("spira") };
    edge::start_image_build(&e, &repo, &run, "bin/testenv".into(), vec![], &leg).unwrap();
    assert_eq!(git_in(&tree, &["rev-parse", "HEAD"]), tip);
    assert!(!tree.join("leftover").exists() && tree.join("f").exists());
    let started = units.scopes();
    assert_eq!(started.len(), 1);
    assert_eq!((started[0].unit.task.as_str(), started[0].unit.key.as_str()), (NAME, "spira.image-tagz"));
    assert_eq!(started[0].unit.argv, ["bin/testenv", "container", "image", "--tree", tree.to_str().unwrap()]);

    std::fs::write(tree.join("in-progress"), "").unwrap();
    edge::start_image_build(&e, &repo, &run, "bin/testenv".into(), vec![], &leg).unwrap();
    edge::remove_image_tree(&e, &repo, &run, &leg).unwrap();
    assert!(tree.join("in-progress").exists() && units.scopes().len() == 1, "a running build is left alone");

    let blind = spira_runtime::Effects::default();
    assert!(edge::remove_image_tree(&blind, &repo, &run, &leg).is_err());
    assert!(edge::start_image_build(&blind, &repo, &run, "bin/testenv".into(), vec![], &leg).is_err());
    assert!(tree.join("in-progress").exists(), "a checkout whose build cannot be read is left alone");

    runtime::units::Units::stop(&*units, &UnitHandle::of(NAME, &leg.unit).unwrap()).unwrap();
    edge::remove_image_tree(&e, &repo, &run, &leg).unwrap();
    assert!(!tree.exists());
    assert!(!git_in(&src, &["worktree", "list"]).contains(".image-"));
    edge::remove_image_tree(&e, &repo, &run, &leg).expect("nothing there is not an error");
}

#[test_id(turif59)]
#[test]
fn sp_4pne6q_the_store_is_asked_for_the_tag_by_image_exists() {
    use spira_runtime::tasks::a::batcher_local::edge::host_holds_image;
    let dir = testkit::TempDir::new("image-exists");
    testkit::write_exe(dir.path().join("podman"), "#!/bin/sh\n[ \"$*\" = 'image exists localhost/spira-testenv:tagz' ]\n");
    let path = format!("{}:{}", dir.path().display(), std::env::var("PATH").unwrap());
    let _env = testkit::env(&[("PATH", Some(path.as_str()))]);
    assert!(host_holds_image("tagz"));
    assert!(!host_holds_image("tagy"));
}
