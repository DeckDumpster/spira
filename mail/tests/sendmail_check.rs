//! `mail sendmail --check`, the binary run for real: it fails on each broken piece of the send
//! path, passes on a healthy one, and delivers nothing either way.

use spira_otel::metrics::fixture::bare;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

struct Fx {
    tmp: testkit::TempDir,
    mail: PathBuf,
}

fn healthy() -> Fx {
    let tmp = testkit::TempDir::new("mail-check");
    let mail = tmp.path().join("mail");
    for sub in ["tmp", "new", "cur"] {
        fs::create_dir_all(mail.join("concierge").join(sub)).unwrap();
    }
    Fx { tmp, mail }
}

fn check(fx: &Fx, toml: Option<&Path>) -> (bool, String) {
    let declared = fx.mail.display().to_string();
    let run = fx.tmp.path().display().to_string();
    let made = spira_config::process::fixture_toml(fx.tmp.path(), &[("SPIRA_MAIL", &declared), ("SPIRA_RUN", &run)]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let mut c = bare(Path::new(env!("CARGO_BIN_EXE_mail")), toml.unwrap_or(&made), &home);
    let out = c.args(["sendmail", "--check"]).stdin(Stdio::null()).output().expect("mail runs");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn count(fx: &Fx) -> usize {
    ["tmp", "new", "cur"].iter().map(|s| fs::read_dir(fx.mail.join("concierge").join(s)).map(|d| d.count()).unwrap_or(0)).sum()
}

#[test]
fn a_healthy_send_path_passes_and_delivers_nothing() {
    let fx = healthy();
    let (ok, err) = check(&fx, None);
    assert!(ok, "{err}");
    assert_eq!(count(&fx), 0);
}

#[test]
fn an_unresolvable_config_fails_naming_config() {
    let fx = healthy();
    let (ok, err) = check(&fx, Some(&fx.tmp.path().join("absent.toml")));
    assert!(!ok);
    assert!(err.contains("config"), "{err}");
}

#[test]
fn a_missing_mail_root_fails_naming_mailbox() {
    let fx = healthy();
    fs::remove_dir_all(&fx.mail).unwrap();
    let (ok, err) = check(&fx, None);
    assert!(!ok);
    assert!(err.contains("mailbox"), "{err}");
}

#[test]
fn a_mailbox_without_its_subdirectories_fails_naming_mailbox() {
    let fx = healthy();
    fs::remove_dir_all(fx.mail.join("concierge").join("new")).unwrap();
    let (ok, err) = check(&fx, None);
    assert!(!ok);
    assert!(err.contains("mailbox"), "{err}");
}

fn chmod(mode: u32, p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn an_unwritable_tmp_fails_naming_mailbox() {
    let fx = healthy();
    let tmp = fx.mail.join("concierge").join("tmp");
    chmod(0o555, &tmp);
    let writable_anyway = fs::write(tmp.join(".probe"), "").is_ok();
    let (ok, err) = check(&fx, None);
    chmod(0o755, &tmp);
    if writable_anyway {
        return;
    }
    assert!(!ok);
    assert!(err.contains("mailbox"), "{err}");
}
