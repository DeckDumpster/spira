//! `concierge.sh` and the SessionStart hook are the subject, so they run under `bash`: the
//! script is copied beside a stub `spira/lib.sh` (the config and statute-book seam), so the
//! real brief composition, resume, holder detection and session recording run with no config
//! layers, no tmux server and no systemd. No shell suite may grow.

#[macro_use]
extern crate spira_test_id;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LIB_STUB: &str = r#"spira_require() { :; }
rune_get() { local v="STUB_$2"; printf '%s' "${!v:-$3}"; }
render_memories() { printf '%s' "$STUB_STATUTES"; }
persona_model() { :; }
"#;
const STATUTES: &str = "## law-rm-alpha\n\nAlpha fixture statute body.\n";

fn repo() -> PathBuf {
    testkit::manifest_dir().parent().unwrap().to_path_buf()
}

struct Fx {
    tmp: testkit::TempDir,
}

struct Out {
    code: Option<i32>,
    out: String,
    err: String,
}

impl Fx {
    fn new() -> Fx {
        let fx = Fx { tmp: testkit::TempDir::new("concierge-launcher") };
        let d = fx.path();
        for sub in ["home/spira", "home/runes", "bin", "run", "projects/proj", "brain"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        std::fs::copy(repo().join("concierge.sh"), d.join("home/concierge.sh")).unwrap();
        std::fs::copy(repo().join("spira/runes/concierge.md"), d.join("home/runes/concierge.md")).unwrap();
        std::fs::write(d.join("home/spira/lib.sh"), LIB_STUB).unwrap();
        testkit::write_exe(d.join("bin/tmux"), "#!/bin/sh\nexit 1\n");
        testkit::write_exe(d.join("bin/release"), "#!/bin/sh\nexit 0\n");
        testkit::write_exe(d.join("bin/claude"), "#!/bin/sh\necho STUB_CLAUDE_RAN\n");
        fx
    }

    fn path(&self) -> &Path {
        self.tmp.path()
    }

    fn concierge(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let d = self.path();
        let mut cmd = Command::new("bash");
        cmd.arg(d.join("home/concierge.sh")).args(args).env_clear();
        cmd.env("PATH", format!("{}:/usr/bin:/bin", d.join("bin").display()))
            .env("SPIRA_HOME", d.join("home"))
            .env("SPIRA_REPO", d.join("brain"))
            .env("SPIRA_RUN", d.join("run"))
            .env("SPIRA_COCKPIT", "cockpit")
            .env("SPIRA_DB", "db")
            .env("SPIRA_RELEASE", d.join("home"))
            .env("SPIRA_TOKEN_PROJECTS", d.join("projects"))
            .env("CONCIERGE_SOCKET", "fixture-socket")
            .env("CONCIERGE_SESSION", "fixture-session")
            .env("STUB_STATUTES", STATUTES);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let o = cmd.stdin(Stdio::null()).output().unwrap();
        Out { code: o.status.code(), out: String::from_utf8_lossy(&o.stdout).into(), err: String::from_utf8_lossy(&o.stderr).into() }
    }

    fn brain(&self) -> String {
        self.path().join("brain").display().to_string()
    }
}

// covers: UC-operator-channel-41
#[test_id(tprrvfn)]
#[test]
fn brief_composes_the_shipped_persona_with_every_placeholder_filled() {
    let fx = Fx::new();
    let o = fx.concierge(&["brief"], &[]);
    assert_eq!(o.code, Some(0), "{}", o.err);
    let brief = std::fs::read_to_string(o.out.trim()).unwrap();
    assert!(!brief.contains("{{"), "a placeholder survived rendering");
    for needle in ["mail send operator", "bead.sh file", "# Memories in force", "## law-rm-alpha", "Alpha fixture statute body"] {
        assert!(brief.contains(needle), "brief lacks {needle:?}");
    }
    assert!(!brief.contains(".claude/bead.sh"), "a no-wiki brief named a wiki-relative tool");
    assert!(!brief.contains("brain wiki"), "a no-wiki brief carries the wiki clause");

    let wiki = fx.concierge(&["brief"], &[("SPIRA_WIKI", &fx.brain())]);
    let with_wiki = std::fs::read_to_string(wiki.out.trim()).unwrap();
    assert!(with_wiki.contains("brain wiki"), "a wiki install lost its wiki clause");
}

// covers: UC-operator-channel-41
#[test_id(t7x1rcp)]
#[test]
fn brief_refuses_a_missing_persona_a_typo_core_and_a_stray_placeholder() {
    let fx = Fx::new();
    let missing = fx.concierge(&["brief"], &[("CONCIERGE_RUNE", "no-such-persona")]);
    assert_eq!(missing.code, Some(1));
    assert!(missing.err.contains("no-such-persona.md"), "{}", missing.err);

    let typo = fx.concierge(
        &["brief"],
        &[("STUB_RUNE_STATUTE_CORE", "law-slug-that-does-not-exist"), ("STUB_STATUTES", "# Statute index\n- law-rm-alpha\n")],
    );
    assert_eq!(typo.code, Some(1));
    assert!(typo.err.contains("no statute rendered in full"), "{}", typo.err);

    let md = fx.path().join("home/runes/stray.md");
    std::fs::write(&md, "Run {{NO_SUCH_PLACEHOLDER}} now.\n").unwrap();
    let stray = fx.concierge(&["brief"], &[("CONCIERGE_RUNE", "stray")]);
    assert_eq!(stray.code, Some(1));
    assert!(stray.err.contains("{{NO_SUCH_PLACEHOLDER}}"), "{}", stray.err);

    let args = fx.concierge(&["brief", "--resume"], &[]);
    assert_eq!(args.code, Some(2));
    assert!(args.err.contains("append-system-prompt-file"), "{}", args.err);
}

// covers: UC-operator-channel-40
#[test_id(twv8rha)]
#[test]
fn resume_id_is_used_only_when_the_recorded_cwd_is_the_brain_dir() {
    let fx = Fx::new();
    let record = fx.path().join("run/concierge-session");
    let none = fx.concierge(&["_resume-id"], &[]);
    assert_eq!((none.out.as_str(), none.err.contains("no session file")), ("", true));

    std::fs::write(&record, format!("sess-1\n{}\n", fx.brain())).unwrap();
    assert_eq!(fx.concierge(&["_resume-id"], &[]).out, "sess-1");

    std::fs::write(&record, "sess-1\n/somewhere/else\n").unwrap();
    let moved = fx.concierge(&["_resume-id"], &[]);
    assert_eq!(moved.out, "");
    assert!(moved.err.contains("starting empty"), "{}", moved.err);
}

/// A process whose argv0 is `argv0` and which holds `transcript` open on fd 0; returns once
/// the exec has happened (its cmdline names `argv0`).
struct Holder(std::process::Child);

impl Holder {
    fn id(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn holder(argv0: &str, transcript: &Path, detached: bool) -> Holder {
    let mut cmd = Command::new("bash");
    cmd.args(["-c", "exec -a \"$1\" sleep 60 < \"$2\"", "bash", argv0]).arg(transcript).stdout(Stdio::null());
    if detached {
        unsafe {
            cmd.pre_exec(|| if libc::setsid() >= 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) });
        }
    }
    let child = Holder(cmd.spawn().unwrap());
    let cmdline = format!("{}/{}/cmdline", system_paths::PROC, child.id());
    while !std::fs::read(&cmdline).map(|c| c.starts_with(argv0.as_bytes())).unwrap_or(false) {
        std::thread::yield_now();
    }
    child
}

// covers: UC-operator-channel-40
#[test_id(tt8llsj)]
#[test]
fn a_session_is_held_by_the_claude_process_that_has_its_transcript_open() {
    let fx = Fx::new();
    let transcript = fx.path().join("projects/proj/live-sid.jsonl");
    std::fs::write(&transcript, "").unwrap();
    assert_eq!(fx.concierge(&["_live-pid", "live-sid"], &[]).out, "", "no holder yet");

    let claude = holder("claude", &transcript, false);
    assert_eq!(fx.concierge(&["_live-pid", "live-sid"], &[]).out, claude.id().to_string());

    let unrelated = fx.path().join("projects/proj/unrelated.jsonl");
    std::fs::write(&unrelated, "").unwrap();
    let _mention = holder("claude", &unrelated, false);
    assert_eq!(fx.concierge(&["_live-pid", "mentioned-sid"], &[]).out, "", "a claude process that never opened the session counted");
    let other = fx.path().join("projects/proj/other-sid.jsonl");
    std::fs::write(&other, "").unwrap();
    let _tail = holder("tail", &other, false);
    assert_eq!(fx.concierge(&["_live-pid", "other-sid"], &[]).out, "", "a non-claude holder counted");
}

// covers: UC-operator-channel-40
#[test_id(tadb4cv)]
#[test]
fn start_exits_3_headless_when_a_live_holder_has_no_tmux_session_or_tty() {
    let fx = Fx::new();
    let transcript = fx.path().join("projects/proj/headless-sid.jsonl");
    std::fs::write(&transcript, "").unwrap();
    std::fs::write(fx.path().join("run/concierge-session"), format!("headless-sid\n{}\n", fx.brain())).unwrap();
    let claude = holder("claude", &transcript, true);

    let o = fx.concierge(&["start"], &[]);
    assert_eq!(o.code, Some(3), "{}", o.err);
    assert!(o.err.contains("HEADLESS") && o.err.contains("headless-sid"), "{}", o.err);
    assert!(!o.out.contains("STUB_CLAUDE_RAN"));
    assert!(std::fs::metadata(format!("{}/{}", system_paths::PROC, claude.id())).is_ok(), "the holder was killed without recovery");
    drop(claude);

    let free = fx.concierge(&["start"], &[("CONCIERGE_RUNE", "no-such-persona")]);
    assert_eq!(free.code, Some(1), "{}", free.err);
    assert!(free.err.contains("no brief at"), "without a holder start must proceed to the brief: {}", free.err);
}

// covers: UC-operator-channel-40
#[test_id(twowbda)]
#[test]
fn the_session_start_hook_records_the_concierge_session_and_only_for_the_prod_harness() {
    let fx = Fx::new();
    let d = fx.path();
    std::fs::create_dir_all(d.join("hooks")).unwrap();
    std::fs::copy(repo().join("spira/hooks/session.sh"), d.join("hooks/session.sh")).unwrap();
    std::fs::write(d.join("conf.sh"), "").unwrap();
    std::fs::write(d.join("inbox"), "").unwrap();
    testkit::write_exe(d.join("bin/watchd"), "#!/bin/sh\necho 'manifest unreadable' >&2\nexit 1\n");
    let run = |concierge: Option<&str>, prod: &Path, payload: &str| {
        let mut cmd = Command::new("bash");
        cmd.arg(d.join("hooks/session.sh")).env_clear().stdin(Stdio::piped()).stdout(Stdio::piped());
        cmd.env("PATH", format!("{}:/usr/bin:/bin", d.join("bin").display()))
            .env("SPIRA_HOME", d)
            .env("SPIRA_PROD", prod)
            .env("SPIRA_RUN", d.join("run"))
            .env("SPIRA_CONCIERGE_INBOX", d.join("inbox"));
        if let Some(c) = concierge {
            cmd.env("SPIRA_CONCIERGE", c);
        }
        let mut child = cmd.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), payload.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        assert_eq!(o.status.code(), Some(0));
    };
    let record = d.join("run/concierge-session");
    let read = || std::fs::read_to_string(&record).unwrap_or_default();

    run(None, d, r#"{"session_id":"s1","cwd":"/brain"}"#);
    assert_eq!(read(), "", "recorded outside the Concierge");

    run(Some("1"), d, r#"{"session_id":"s1","cwd":"/brain"}"#);
    assert_eq!(read(), "s1\n/brain\n");
    run(Some("1"), d, r#"{"session_id":"s2","cwd":"/brain"}"#);
    assert_eq!(read(), "s2\n/brain\nprevious:s1\n");

    let elsewhere = d.join("other-harness");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let link = d.join("prod-link");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    run(Some("1"), &link, r#"{"session_id":"s3","cwd":"/brain"}"#);
    assert_eq!(read(), "s2\n/brain\nprevious:s1\n", "a harness that is not in force recorded");
}
