use super::*;

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

#[test]
fn a_changed_page_is_named_by_its_url_and_assets_fall_back_to_the_index() {
    assert_eq!(pages_for("README.md\nfrontend/cards/index.html\nfrontend/about.html\nfrontend/index.html\nfrontend/a.css\n"), ["/", "/about.html", "/cards/"]);
    assert_eq!(pages_for("frontend/a.css\nsrc/x.rs\n"), ["/"]);
}

#[test]
fn only_a_top_level_frontend_change_is_a_site_change() {
    assert!(touches_frontend("README.md\nfrontend/a.ts\n"));
    assert!(!touches_frontend("docs/frontend/x.md\nsrc/a.rs\n"));
}

#[test]
fn a_repository_without_shots_is_a_clean_no_op() {
    let tmp = dir("noop");
    let repo = tmp.path().join("r");
    std::fs::create_dir_all(&repo).unwrap();
    let g = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(a).status().unwrap().success());
    g(&["init", "-q"]);
    g(&["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "x"]);
    let (rc, msg) = batch(&repo, tmp.path(), "HEAD", "HEAD", None, None);
    assert_eq!(rc, 0, "{msg}");
    assert!(msg.contains("declares no shots capture"), "{msg}");
    assert!(!tmp.path().join("shots").exists());
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
