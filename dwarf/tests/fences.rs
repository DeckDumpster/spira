//! The tracked fence scripts, run for real under `bash` in a `testkit::git` repository. The
//! scripts are the subject; the programs beside them that other crates own (`spira-config`,
//! `spira-lint`, `spira-lc`) are stubs, because an integration target of this crate is not
//! built with those binaries. A stub answers only what the script asks of it and logs what
//! it was asked, so a script that stopped calling it fails here.

#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use testkit::{git, TempDir};

fn spira() -> PathBuf {
    testkit::manifest_dir()
        .join("../spira")
        .canonicalize()
        .unwrap()
}

const SPIRA_CONFIG: &str = r#"#!/bin/bash
case "$1" in
resolve) cat "$STUB_RESOLVED" ;;
env-bootstrap|check-bd) ;;
repo) case "$2" in
    landref) printf '%s\n' "$STUB_LANDREF" ;;
    ref-remote) r="${3%%/*}"; [ "$r" != "$3" ] && [ "$r" = "$STUB_REMOTE" ] && echo "$r" || exit 1 ;;
    ref-branch) echo "${3#*/}" ;;
    all) echo fixture ;;
    root) printf '%s\n' "$STUB_ROOT" ;;
    *) exit 1 ;;
    esac ;;
*) exit 1 ;;
esac
"#;

const SPIRA_LC: &str = "#!/bin/bash\ncat >/dev/null\n";

const SPIRA_LINT: &str = r#"#!/bin/bash
rule=""; root=""
while [ $# -gt 0 ]; do case "$1" in --only) rule="$2"; shift ;; --root) root="$2"; shift ;; esac; shift; done
printf '%s %s\n' "$rule" "$(ls -A "$root" | tr '\n' ' ')" >> "$STUB_LINT_LOG"
[ "$rule" != "${STUB_LINT_RED:-}" ] || { echo "spira-lint: $rule refused" >&2; exit 1; }
"#;

struct Rig {
    dir: TempDir,
    toml: PathBuf,
}

impl Rig {
    fn new(declare: &[(&str, &str)]) -> Rig {
        let dir = TempDir::new("fences");
        let toml = spira_config::process::fixture_toml(&dir, declare);
        let doc = spira_config::validate(&std::fs::read_to_string(&toml).unwrap()).unwrap();
        let home = spira();
        let repo = home.parent().unwrap().to_path_buf();
        let env: BTreeMap<String, String> =
            [("HOME".to_string(), dir.display().to_string())].into();
        let resolved = spira_config::resolve::resolve(spira_config::resolve::ResolveInput {
            env: &env,
            home: &home,
            repo: &repo,
            toml: Some(&doc),
            conf_d: &home.join("conf.d"),
        })
        .unwrap_or_else(|_| panic!("the fixture config resolves"));
        std::fs::write(dir.join("resolved.sh"), resolved.to_sh_all()).unwrap();
        std::fs::create_dir(dir.join("bin")).unwrap();
        for (name, body) in [
            ("spira-config", SPIRA_CONFIG),
            ("spira-lc", SPIRA_LC),
            ("spira-lint", SPIRA_LINT),
        ] {
            testkit::write_exe(dir.join("bin").join(name), body);
        }
        Rig { dir, toml }
    }

    fn cmd(&self, prog: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(prog);
        c.env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.join("bin").display()),
            )
            .env("HOME", &*self.dir)
            .env("SPIRA_TOML", &self.toml)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("STUB_RESOLVED", self.dir.join("resolved.sh"))
            .env("STUB_LANDREF", "origin/main")
            .env("STUB_REMOTE", "origin")
            .env("STUB_LINT_LOG", self.dir.join("lint.log"));
        c
    }

    fn lint_log(&self) -> String {
        std::fs::read_to_string(self.dir.join("lint.log")).unwrap_or_default()
    }

    fn git(&self, dir: &Path, who: &str, args: &[&str]) -> Command {
        let mut c = self.cmd("git");
        let email = format!(
            "{who}@{}",
            if who.starts_with("dwarf") {
                "spira.local"
            } else {
                "example.com"
            }
        );
        c.arg("-C").arg(dir).args(args);
        c.env("GIT_AUTHOR_NAME", who)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_COMMITTER_NAME", who)
            .env("GIT_COMMITTER_EMAIL", &email);
        c
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn must(mut c: Command) -> String {
    let o = c.output().unwrap();
    assert!(o.status.success(), "{c:?}: {}", text(&o));
    text(&o)
}

/// A repository on `main` with one commit, and `origin/main` at that commit.
fn repo(rig: &Rig, name: &str) -> PathBuf {
    let r = rig.dir.join(name);
    std::fs::create_dir(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    std::fs::write(r.join("f.txt"), "initial\n").unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "initial"]);
    git(&r, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    r
}

fn commit_all(rig: &Rig, dir: &Path, who: &str, msg: &str) -> Output {
    must(rig.git(dir, who, &["add", "-A"]));
    rig.git(dir, who, &["commit", "-q", "-m", msg])
        .output()
        .unwrap()
}

// ---------------------------------------------------------------------------------------
// dwarf-fence.sh: payload tables. Each row is run in its own process, so a table runs its
// rows on threads; a table carries refused rows and allowed rows, so a fence that blocks
// everything or nothing fails it.
// ---------------------------------------------------------------------------------------

struct Fence {
    dir: TempDir,
}

impl Fence {
    fn new() -> Fence {
        let dir = TempDir::new("dwarf-fence");
        for d in ["run", "prod", ".config/spira"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        Fence { dir }
    }
    fn prod(&self) -> String {
        self.dir.join("prod").display().to_string()
    }
    fn run_dir(&self) -> String {
        self.dir.join("run").display().to_string()
    }
    fn config(&self) -> String {
        self.dir.join(".config/spira").display().to_string()
    }

    fn feed(&self, payload: &str, env: &[(&str, &str)]) -> (i32, String) {
        use std::io::Write;
        let mut c = Command::new("bash");
        c.arg(spira().join("hooks/dwarf-fence.sh"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &*self.dir)
            .env("SPIRA_RUN", self.run_dir())
            .env("SPIRA_PROD", self.prod())
            .envs(env.iter().copied())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = c.spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
        let o = child.wait_with_output().unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
        )
    }

    fn bash(&self, cmd: &str, env: &[(&str, &str)]) -> (i32, String) {
        self.feed(
            &serde_json::json!({"tool_name": "Bash", "tool_input": {"command": cmd}}).to_string(),
            env,
        )
    }
}

const DWARF: (&str, &str) = ("SPIRA_DWARF", "test-dwarf");
const OVERRIDE: (&str, &str) = ("SPIRA_DWARF_OVERRIDE", "1");

enum Row {
    Refused(String),
    Allowed(String),
}

fn blocked(out: &str) -> bool {
    out.contains("\"decision\":\"block\"")
}

/// Runs every row on its own thread as a dwarf session and returns the rows that went the wrong way.
fn wrong_rows(fence: &Fence, rows: &[Row], env: &[(&str, &str)]) -> Vec<String> {
    std::thread::scope(|s| {
        let hs: Vec<_> = rows
            .iter()
            .map(|row| {
                s.spawn(move || {
                    let (cmd, want) = match row {
                        Row::Refused(c) => (c, true),
                        Row::Allowed(c) => (c, false),
                    };
                    let (rc, out) = fence.bash(cmd, env);
                    (rc == 0 && blocked(&out) == want)
                        .then(String::new)
                        .unwrap_or_else(|| {
                            format!(
                                "{} {cmd:?}: rc={rc} out={out}",
                                if want { "not refused:" } else { "refused:" }
                            )
                        })
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap())
            .filter(|m| !m.is_empty())
            .collect()
    })
}

fn refused(c: impl Into<String>) -> Row {
    Row::Refused(c.into())
}
fn allowed(c: impl Into<String>) -> Row {
    Row::Allowed(c.into())
}

// covers: UC-safety-fences-01
#[test_id(tws7o6h)]
#[test]
fn a_dwarf_may_not_operate_the_queue_or_the_landing_but_may_name_them_as_arguments() {
    let f = Fence::new();
    let rows = [
        refused("bash spira/landing.sh spira"),
        refused("git push origin main"),
        refused("gh pr create --title x"),
        refused("/opt/spira/bin/queue flush spira"),
        refused("queue verdict spira"),
        refused("git add spira/landing.sh && bash spira/landing.sh push"),
        allowed("/opt/spira/bin/queue stats"),
        allowed("/opt/spira/bin/testenv --suites test-verdict.sh spira/sp-x"),
        allowed("git add spira/landing.sh"),
    ];
    let wrong = wrong_rows(&f, &rows, &[DWARF]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    let (_, out) = f.bash("bash spira/landing.sh spira", &[DWARF]);
    assert!(
        out.contains("sp-kz8ob"),
        "the refusal names its reason: {out}"
    );
}

// covers: UC-safety-fences-02
#[test_id(tmk8gia)]
#[test]
fn a_dwarf_may_not_write_under_prod_or_the_queue() {
    let f = Fence::new();
    let (prod, q) = (f.prod(), format!("{}/queue", f.run_dir()));
    let rows = [
        refused(format!("printf x > {q}/x")),
        refused(format!("echo x > {prod}/f")),
        refused(format!("rm -rf {prod}/releases")),
        refused(format!("mv {prod}/dwarf.sh /tmp/x")),
        refused(format!("echo x | tee {prod}/dwarf.sh")),
        refused(format!("sed -i s/a/b/ {prod}/dwarf.sh")),
        refused(format!("git -C {prod} commit -m x")),
        refused(format!("git -C {prod} reset --hard")),
        refused(format!("rm {q}/x")),
        refused(format!("mv a {q}/b")),
        refused(format!("echo x | tee {q}/x")),
        refused(format!("sed -i s/a/b/ {q}/x")),
    ];
    let wrong = wrong_rows(&f, &rows, &[DWARF]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    let overridden = wrong_rows(&f, &[allowed(format!("rm {q}/x"))], &[DWARF, OVERRIDE]);
    assert!(
        overridden.is_empty(),
        "the override bypasses the queue-write refusal: {overridden:#?}"
    );
}

// covers: UC-safety-fences-02
#[test_id(tdxm2uq)]
#[test]
fn a_dwarf_may_read_and_run_from_prod_and_name_its_paths_in_prose() {
    let f = Fence::new();
    let (prod, run) = (f.prod(), f.run_dir());
    let heredoc = |body: String| format!("work file title --description - <<'DESC'\n{body}\nDESC");
    let rows = [
        refused(format!("echo x > {prod}/f")),
        allowed(format!("printf x > {run}/landstate/x")),
        allowed(format!("bash \"{prod}/spira/census\" --with-suppressed")),
        allowed(format!("cat {prod}/spira/census")),
        allowed(format!("bash {prod}/census --with-suppressed")),
        allowed(format!("{prod}/../bin/sop match /tmp/sp-x.payload")),
        allowed(format!("{prod}/incident.sh list")),
        allowed(format!(
            "{prod}/mail send operator --from Ops --subject q --kind question --default x"
        )),
        allowed(format!("{prod}/../bin/testenv suites status")),
        allowed(format!("{prod}/../bin/groomer deadlocked")),
        allowed(heredoc(format!(
            "bash \"{prod}/census\" and /verdict.sh are mentioned"
        ))),
        allowed(format!("arm in dwarf.sh see {prod}/dwarf.sh")),
        allowed(format!("See {prod}/dwarf.sh tee operation")),
    ];
    let wrong = wrong_rows(&f, &rows, &[DWARF]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

// covers: UC-safety-fences-02
#[test_id(tjxwgqu)]
#[test]
fn a_dwarf_may_read_the_queue_and_name_its_path_in_prose() {
    let f = Fence::new();
    let q = format!("{}/queue", f.run_dir());
    let heredoc = |body: String| format!("work file title --description - <<'DESC'\n{body}\nDESC");
    let rows = [
        refused(format!("rm {q}/x")),
        allowed(format!("cat {q}/sp-7youp")),
        allowed(format!("ls {q} | wc -l")),
        allowed(format!("grep -l '^LANDED' {q}/*")),
        allowed(heredoc(format!("see {q}/sp-7youp for evidence"))),
        allowed(format!("echo '{q}/x needs no write'")),
    ];
    let wrong = wrong_rows(&f, &rows, &[DWARF]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

const BD_REFUSED: &[&str] = &[
    "bd create x",
    "~/.local/bin/bd create x",
    "BEADS_DIR=/x/.beads ~/.local/bin/bd create x",
    "cd /x && bd list",
    "$SPIRA_RELEASE/bin/bdq create -t bug x",
    "/r/bin/bdq --help",
    "bash -c \"bd show sp-1\"",
    "env FOO=1 bd-embedded list",
    "export BEADS_DIR=/x",
    "sudo -u root bd list",
    "timeout 5 /usr/bin/bd list",
    "echo `bd create x`",
    "echo $(bd list)",
    "ls | xargs bd show",
    "find . -exec bd list {} +",
    "eval \"bd create x\"",
    "bash -lc \"BEADS_DIR=/x true\"",
    "declare -x BEADS_DIR=/x",
    "if bd list; then true; fi",
    "while bd list; do true; done",
    "nice -n 5 bd list",
    "xargs -n 1 bd show",
    "time bd list",
    "nohup bd list",
    "true\nbd list",
];

const BD_ALLOWED: &[&str] = &[
    "work file \"bd is broken\" --for builder --repo spira --body-file -",
    "grep -n bd foo.rs",
    "echo \"run bd\"",
    "work note-on sp-1 - <<EOF\nbd create x\nEOF",
    "cargo test -p bead",
    "echo BEADS_DIR=x",
];

fn store_rows_refused(range: std::ops::Range<usize>) {
    let f = Fence::new();
    let rows: Vec<Row> = BD_REFUSED[range].iter().map(|c| refused(*c)).collect();
    let wrong = wrong_rows(&f, &rows, &[DWARF, ("SPIRA_DB", "/x/db")]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

// covers: UC-safety-fences-03
#[test_id(tqgvaht)]
#[test]
fn a_dwarf_may_not_run_bd_or_bdq_by_a_path_and_is_told_the_work_verb() {
    store_rows_refused(0..9);
    let (_, out) = Fence::new().bash("bd create x", &[DWARF]);
    assert!(
        out.contains("work file"),
        "the refusal names the exit: {out}"
    );
}

// covers: UC-safety-fences-03
#[test_id(tsxdrrk)]
#[test]
fn a_dwarf_may_not_run_bd_through_a_wrapper_or_a_subshell() {
    store_rows_refused(9..17);
}

// covers: UC-safety-fences-03
#[test_id(t126kj5)]
#[test]
fn a_dwarf_may_not_run_bd_in_a_loop_or_set_a_beads_dir() {
    store_rows_refused(17..BD_REFUSED.len());
}

// covers: UC-safety-fences-03
#[test_id(trld5ui)]
#[test]
fn a_dwarf_may_name_bd_in_prose_and_the_override_and_a_non_dwarf_session_pass() {
    let f = Fence::new();
    let rows: Vec<Row> = BD_ALLOWED.iter().map(|c| allowed(*c)).collect();
    let wrong = wrong_rows(&f, &rows, &[DWARF, ("SPIRA_DB", "/x/db")]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    let rows: Vec<Row> = BD_REFUSED.iter().take(3).map(|c| allowed(*c)).collect();
    let overridden = wrong_rows(&f, &rows, &[DWARF, OVERRIDE]);
    assert!(
        overridden.is_empty(),
        "the override bypasses: {overridden:#?}"
    );
    let outside = wrong_rows(&f, &rows, &[]);
    assert!(
        outside.is_empty(),
        "the fence is silent outside a dwarf session: {outside:#?}"
    );
}

// covers: UC-safety-fences-15
#[test_id(tv6a534)]
#[test]
fn the_fence_blocks_on_stdout_with_exit_zero_and_is_inert_outside_its_scope() {
    let f = Fence::new();
    let landing = "bash spira/landing.sh spira";
    let (rc, out) = f.bash(landing, &[DWARF]);
    assert!(
        rc == 0 && blocked(&out),
        "a block is stdout with exit 0: rc={rc} {out}"
    );
    let heredoc = format!("work file title --body-file - <<'DESC'\n{landing}\nDESC");
    let rows = [allowed(format!("echo '{landing}'")), allowed(heredoc)];
    let wrong = wrong_rows(&f, &rows, &[DWARF]);
    assert!(
        wrong.is_empty(),
        "quoted and heredoc prose pass: {wrong:#?}"
    );
    assert!(!blocked(&f.bash(landing, &[]).1), "no SPIRA_DWARF: inert");
    assert!(
        !blocked(&f.bash(landing, &[DWARF, OVERRIDE]).1),
        "SPIRA_DWARF_OVERRIDE passes"
    );
    let read =
        serde_json::json!({"tool_name": "Read", "tool_input": {"file_path": "/etc/hostname"}})
            .to_string();
    assert!(
        !blocked(&f.feed(&read, &[DWARF]).1),
        "a tool the fence does not own passes"
    );
    for raw in ["not json at all", ""] {
        let (rc, out) = f.feed(raw, &[DWARF]);
        assert!(
            rc == 0 && !blocked(&out),
            "a malformed payload fails open: {raw:?} rc={rc} {out}"
        );
    }
}

// The tool-write, background and pattern-kill rows are no cell's; the suite this replaces asserted them and nothing else does.
// covers: UC-safety-fences-02
#[test_id(t3jp6sx)]
#[test]
fn a_dwarf_may_not_write_prod_or_config_by_tool_or_shell_or_run_in_background() {
    let f = Fence::new();
    let write = |tool: &str, path: String, env: &[(&str, &str)]| {
        blocked(
            &f.feed(
                &serde_json::json!({"tool_name": tool, "tool_input": {"file_path": path}})
                    .to_string(),
                env,
            )
            .1,
        )
    };
    let (prod, config) = (
        format!("{}/dwarf.sh", f.prod()),
        format!("{}/notes.conf", f.config()),
    );
    let outside = format!("{}/outside/notes.md", f.dir.display());
    assert!(write("Write", prod.clone(), &[DWARF]) && write("Edit", prod.clone(), &[DWARF]));
    assert!(write("Write", config.clone(), &[DWARF]) && write("Edit", config.clone(), &[DWARF]));
    assert!(
        !write("Write", outside, &[DWARF])
            && !write(
                "Edit",
                format!("{}/worktree/sp-x/notes.md", f.run_dir()),
                &[DWARF]
            )
    );
    assert!(
        !write("Write", prod, &[DWARF, OVERRIDE])
            && !write("Edit", config.clone(), &[DWARF, OVERRIDE])
    );

    let c = &config;
    let rows = [
        refused(format!("echo x > {c}")),
        refused(format!("printf x >> {c}")),
        refused(format!("rm {c}")),
        refused(format!("sed -i s/a/b/ {c}")),
        refused(format!("echo x | tee {c}")),
        allowed(format!("cat {c}")),
    ];
    let wrong = wrong_rows(&f, &rows, &[DWARF]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(wrong_rows(&f, &[allowed(format!("rm {c}"))], &[DWARF, OVERRIDE]).is_empty());

    let rib = |cmd: &str, bg: bool, env: &[(&str, &str)]| {
        f.feed(&serde_json::json!({"tool_name": "Bash", "tool_input": {"command": cmd, "run_in_background": bg}}).to_string(), env)
    };
    let (rc, out) = rib("echo hi", true, &[DWARF]);
    assert!(
        rc == 0 && blocked(&out) && out.contains("foreground"),
        "{out}"
    );
    assert!(
        !blocked(&rib("echo hi", false, &[DWARF]).1)
            && !blocked(&rib("echo hi", true, &[]).1)
            && !blocked(&rib("echo hi", true, &[DWARF, OVERRIDE]).1)
    );
}

const KILL_REFUSED: &[&str] = &[
    "pkill -f \"it-\"",
    "pkill cargo",
    "killall cargo",
    "pgrep -f it- | xargs kill",
    "FOO=1 sudo pkill -9 -f x",
    "cd /tmp && pkill -f it-",
    "bash -c \"pkill -f it-\"",
    "kill $(pgrep -f it-)",
    "sh -lc \"pkill x\"",
    "pgrep x | xargs -r kill",
    "sudo -u root pkill -f x",
    "env -u FOO pkill -f x",
    "kill \"$(pgrep -f it-)\"",
    "pgrep -f it- | while read p; do kill $p; done",
    "timeout 5 /usr/bin/pkill -f it-",
];

fn kill_rows_refused(range: std::ops::Range<usize>) {
    let f = Fence::new();
    let rows: Vec<Row> = KILL_REFUSED[range].iter().map(|c| refused(*c)).collect();
    let wrong = wrong_rows(&f, &rows, &[DWARF]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

// covers: UC-safety-fences-02
#[test_id(togz69m)]
#[test]
fn a_dwarf_may_not_kill_by_pattern_by_name() {
    kill_rows_refused(0..8);
}

// covers: UC-safety-fences-02
#[test_id(thr5sjd)]
#[test]
fn a_dwarf_may_not_kill_by_pattern_through_a_pipe_or_a_wrapper() {
    kill_rows_refused(8..KILL_REFUSED.len());
}

// covers: UC-safety-fences-02
#[test_id(tnioaxk)]
#[test]
fn a_dwarf_may_kill_a_pid_and_stop_a_unit_and_the_override_and_a_non_dwarf_pass() {
    let f = Fence::new();
    let rows: Vec<Row> = [
        "ps aux | grep kill",
        "ps aux | grep -v kill | wc -l",
        "kill 1234",
        "kill -9 1234 5678",
        "systemctl --user stop spira-x.service",
        "grep pkill notes.md",
        "echo \"do not pkill -f it-\"",
        "cat /proc/1234/cmdline | xargs -0 echo",
        "git commit -m \"no pkill here\"",
    ]
    .into_iter()
    .map(allowed)
    .collect();
    let wrong = wrong_rows(&f, &rows, &[DWARF]);
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(!blocked(&f.bash("pkill -f it-", &[]).1));
    assert!(!blocked(&f.bash("pkill -f it-", &[DWARF, OVERRIDE]).1));
}
