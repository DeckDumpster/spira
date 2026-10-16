use super::*;
use std::os::unix::fs::PermissionsExt;

const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
const DECL: &str = "image = \"i\"\n[shots]\nargv = [\"serve\"]\n[tier.t]\nargv = [\"x\"]\n";
const PNG: &[u8] = b"\x89PNG not compared";

fn dir(n: &str) -> testkit::TempDir {
    testkit::TempDir::new(&format!("shots-{n}"))
}

fn pages() -> Vec<String> {
    vec!["/".into(), "/cards/1".into()]
}

fn writes_all(out: &Path) -> (i32, String) {
    for p in pages() {
        for w in [390, 1280] {
            std::fs::write(out.join(format!("{}-{w}.png", slug(&p))), PNG).unwrap();
        }
    }
    (0, String::new())
}

#[test]
fn a_capture_leaves_one_png_per_page_and_width() {
    let tmp = dir("ok");
    let (rc, msg) = capture(tmp.path(), "sp-b", TIP, &pages(), &[390, 1280], &writes_all);
    assert_eq!(rc, 0, "{msg}");
    let d = shot_dir(tmp.path(), "sp-b", TIP);
    for f in ["index-390.png", "index-1280.png", "cards_1-390.png", "cards_1-1280.png"] {
        assert!(d.join(f).is_file(), "{f}");
    }
}

#[test]
fn a_page_that_fails_to_capture_is_named_and_leaves_nothing() {
    let tmp = dir("fail");
    let half = |out: &Path| {
        std::fs::write(out.join("index-390.png"), PNG).unwrap();
        std::fs::write(out.join("index-1280.png"), PNG).unwrap();
        (1, "Protocol error (Page.captureScreenshot)".to_string())
    };
    let (rc, msg) = capture(tmp.path(), "sp-b", TIP, &pages(), &[390, 1280], &half);
    assert_eq!(rc, 1);
    assert!(msg.contains("/cards/1 at 390px") && msg.contains("Protocol error"), "{msg}");
    assert!(!shot_dir(tmp.path(), "sp-b", TIP).exists());
}

#[test]
fn an_empty_file_is_no_capture() {
    let tmp = dir("empty");
    let empty = |out: &Path| {
        for p in pages() {
            for w in [390, 1280] {
                std::fs::write(out.join(format!("{}-{w}.png", slug(&p))), b"").unwrap();
            }
        }
        (0, String::new())
    };
    assert_eq!(capture(tmp.path(), "sp-b", TIP, &pages(), &[390, 1280], &empty).0, 1);
}

#[test]
fn a_prior_image_is_cleared_and_never_stands_in_for_a_capture() {
    let tmp = dir("stale");
    let d = shot_dir(tmp.path(), "sp-b", TIP);
    std::fs::create_dir_all(&d).unwrap();
    for f in ["index-390.png", "index-1280.png"] {
        std::fs::write(d.join(f), PNG).unwrap();
    }
    let nothing = |out: &Path| {
        assert_eq!(std::fs::read_dir(out).unwrap().count(), 0, "the directory is cleared before the capture");
        (0, String::new())
    };
    assert_eq!(capture(tmp.path(), "sp-b", TIP, &["/".to_string()], &[390, 1280], &nothing).0, 1);
}

fn verify_with(run: &Path, decl: Option<&str>, changed: Option<&str>, waived: Option<&str>) -> (i32, String) {
    verify(run, "sp-b", TIP, decl, changed, waived)
}

#[test]
fn a_frontend_change_without_images_is_refused_naming_both_exits() {
    let tmp = dir("refuse");
    let (rc, msg) = verify_with(tmp.path(), Some(DECL), Some("README.md\nfrontend/src/a.ts\n"), None);
    assert_eq!(rc, 1);
    assert!(msg.contains("testenv shots") && msg.contains("SPIRA_SHOTS_WAIVED=<reason>") && msg.contains("[390, 1280]"), "{msg}");
}

#[test]
fn images_at_every_width_admit_and_one_width_does_not() {
    let tmp = dir("admit");
    let changed = Some("frontend/a.css\n");
    let d = shot_dir(tmp.path(), "sp-b", TIP);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("index-390.png"), PNG).unwrap();
    let (rc, msg) = verify_with(tmp.path(), Some(DECL), changed, None);
    assert_eq!(rc, 1);
    assert!(msg.contains("[1280]") && !msg.contains("[390"), "{msg}");
    std::fs::write(d.join("index-1280.png"), PNG).unwrap();
    assert_eq!(verify_with(tmp.path(), Some(DECL), changed, None).0, 0);
}

#[test]
fn images_of_another_tip_do_not_admit() {
    let tmp = dir("tip");
    let other = shot_dir(tmp.path(), "sp-b", &"f".repeat(40));
    std::fs::create_dir_all(&other).unwrap();
    for w in [390, 1280] {
        std::fs::write(other.join(format!("index-{w}.png")), PNG).unwrap();
    }
    assert_eq!(verify_with(tmp.path(), Some(DECL), Some("frontend/a\n"), None).0, 1);
}

#[test]
fn a_waiver_with_a_reason_admits_and_one_without_does_not() {
    let tmp = dir("waive");
    let changed = Some("frontend/a\n");
    let (rc, msg) = verify_with(tmp.path(), Some(DECL), changed, Some("comment-only change"));
    assert_eq!(rc, 0);
    assert!(msg.contains("comment-only change"), "{msg}");
    assert_eq!(verify_with(tmp.path(), Some(DECL), changed, Some("  ")).0, 1);
}

#[test]
fn a_change_outside_frontend_or_in_a_repo_without_shots_is_not_asked_for_images() {
    let tmp = dir("scope");
    assert_eq!(verify_with(tmp.path(), Some(DECL), Some("docs/frontend/x.md\nsrc/a.rs\n"), None).0, 0);
    assert_eq!(verify_with(tmp.path(), Some("image = \"i\"\n[tier.t]\nargv = [\"x\"]\n"), Some("frontend/a\n"), None).0, 0);
    assert_eq!(verify_with(tmp.path(), None, Some("frontend/a\n"), None).0, 0);
}

#[test]
fn a_diff_git_cannot_produce_is_a_fault_not_an_admission() {
    let tmp = dir("fault");
    assert_eq!(verify_with(tmp.path(), Some(DECL), None, None).0, 2);
}

#[test]
fn the_guard_admits_images_it_cannot_open() {
    let tmp = dir("unread");
    let d = shot_dir(tmp.path(), "sp-b", TIP);
    std::fs::create_dir_all(&d).unwrap();
    for w in [390, 1280] {
        let f = d.join(format!("index-{w}.png"));
        std::fs::write(&f, PNG).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0)).unwrap();
    }
    assert_eq!(verify_with(tmp.path(), Some(DECL), Some("frontend/a\n"), None).0, 0);
}

#[test]
fn nothing_in_the_tool_opens_an_image() {
    let src = include_str!("../shots.rs");
    let code = &src[..src.find("#[cfg(test)]").unwrap()];
    let reads: Vec<&str> = code.lines().filter(|l| l.contains("fs::read") && !l.contains("read_dir") && !l.contains("read_to_string(log)") && !l.contains(".join(foreign::DECL)")).collect();
    assert!(reads.is_empty(), "{reads:?}");
    for banned in ["File::open", "image::", "baseline", "golden"] {
        assert!(!code.contains(banned), "{banned}");
    }
}

#[test]
fn slugs_are_plain_filenames() {
    assert_eq!(slug("/"), "index");
    assert_eq!(slug("/a/b?x=1"), "a_b_x_1");
    assert_eq!(slug("/../../etc"), "etc");
}
