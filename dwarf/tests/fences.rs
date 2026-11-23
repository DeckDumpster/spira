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
#[test_id(tlm1fyi)]
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
#[test_id(ttx1b0r)]
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
#[test_id(tv7imkf)]
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
#[test_id(t2i8bgr)]
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
#[test_id(tj74jws)]
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
#[test_id(tihsyda)]
#[test]
fn a_dwarf_may_not_run_bd_through_a_wrapper_or_a_subshell() {
    store_rows_refused(9..17);
}

// covers: UC-safety-fences-03
#[test_id(tm7bagq)]
#[test]
fn a_dwarf_may_not_run_bd_in_a_loop_or_set_a_beads_dir() {
    store_rows_refused(17..BD_REFUSED.len());
}

// covers: UC-safety-fences-03
#[test_id(t7jim02)]
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
#[test_id(tdmx4v9)]
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
#[test_id(toolbfi)]
#[test]
fn a_dwarf_may_not_write_prod_or_config_by_tool() {
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
}

// covers: UC-safety-fences-02
#[test_id(tykhqmp)]
#[test]
fn a_dwarf_may_not_write_config_by_shell_or_run_in_background() {
    let f = Fence::new();
    let config = format!("{}/notes.conf", f.config());
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
#[test_id(taqcz63)]
#[test]
fn a_dwarf_may_not_kill_by_pattern_by_name() {
    kill_rows_refused(0..8);
}

// covers: UC-safety-fences-02
#[test_id(tdnywzr)]
#[test]
fn a_dwarf_may_not_kill_by_pattern_through_a_pipe_or_a_wrapper() {
    kill_rows_refused(8..KILL_REFUSED.len());
}

// covers: UC-safety-fences-02
#[test_id(tjuf7a7)]
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

// ---------------------------------------------------------------------------------------
// git hooks and branch-guard.sh: the tracked scripts in a real repository.
// ---------------------------------------------------------------------------------------

/// A worktree on `spira/<bead>` of `repo`, armed the way a dwarf's is: by `worktree-hooks.sh install`.
fn armed_worktree(rig: &Rig, repo: &Path, bead: &str) -> PathBuf {
    git(repo, &["config", "extensions.worktreeConfig", "true"]);
    let wt = rig.dir.join(format!("wt-{bead}"));
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("spira/{bead}"),
            &wt.display().to_string(),
        ],
    );
    let mut c = rig.cmd("bash");
    c.arg(spira().join("worktree-hooks.sh"))
        .arg("install")
        .arg(&wt)
        .env("SPIRA_HOME", spira());
    must(c);
    wt
}

fn gitdir_of(dir: &Path) -> PathBuf {
    PathBuf::from(git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ))
}

fn dirty_worktree(rig: &Rig) -> PathBuf {
    let wt = armed_worktree(rig, &repo(rig, "r"), "sp-test");
    std::fs::create_dir_all(wt.join("wiki/notes")).unwrap();
    std::fs::write(wt.join("wiki/notes/archivist.md"), "draft\n").unwrap();
    std::fs::write(
        gitdir_of(&wt).join("spira-dirty-before"),
        "wiki/notes/archivist.md\n",
    )
    .unwrap();
    wt
}

// covers: UC-safety-fences-17
#[test_id(ta9arzt)]
#[test]
fn a_commit_staging_a_path_dirty_before_the_session_is_refused_naming_it_and_an_own_path_commits() {
    let rig = Rig::new(&[]);
    let wt = dirty_worktree(&rig);
    std::fs::write(wt.join("own.txt"), "own\n").unwrap();
    let o = commit_all(&rig, &wt, "op", "sp-test: add -A");
    assert!(
        !o.status.success() && text(&o).contains("wiki/notes/archivist.md"),
        "git add -A is refused, naming the offender: {}",
        text(&o)
    );
    git(&wt, &["reset", "-q"]);
    must(rig.git(&wt, "op", &["add", "--", "own.txt"]));
    must(rig.git(&wt, "op", &["commit", "-q", "-m", "sp-test: own path only"]));
    assert_eq!(
        git(&wt, &["show", "--name-only", "--format=", "HEAD"]),
        "own.txt",
        "the dirty path is not in the commit"
    );
}

// covers: UC-safety-fences-17
#[test_id(tzl135r)]
#[test]
fn a_dirty_path_staged_alone_is_refused_until_the_override_is_set() {
    let rig = Rig::new(&[]);
    let wt = dirty_worktree(&rig);
    must(rig.git(&wt, "op", &["add", "--", "wiki/notes/archivist.md"]));
    let o = rig
        .git(&wt, "op", &["commit", "-q", "-m", "sp-test: dirty alone"])
        .output()
        .unwrap();
    assert!(
        !o.status.success(),
        "staging the dirty path alone is refused too"
    );
    let o = rig
        .git(&wt, "op", &["commit", "-q", "-m", "sp-test: override"])
        .env("SPIRA_ALLOW_DIRTY_STAGE", "1")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "SPIRA_ALLOW_DIRTY_STAGE=1 yields: {}",
        text(&o)
    );
}

fn guard(rig: &Rig, root: &Path, email: &str, work: Option<&Path>) -> (i32, String) {
    let mut c = rig.cmd("bash");
    c.arg(spira().join("branch-guard.sh"))
        .arg("staged")
        .arg(root)
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", email)
        .env("STUB_ROOT", root);
    if let Some(w) = work {
        c.env("SPIRA_WORK", w);
    }
    let o = c.output().unwrap();
    (o.status.code().unwrap(), text(&o))
}

// covers: UC-safety-fences-18
#[test_id(ttbnax9)]
#[test]
fn a_dwarf_may_not_commit_to_the_base_branch_or_outside_its_assigned_worktree() {
    let rig = Rig::new(&[]);
    let r = repo(&rig, "r");
    let dwarf = "dwarf-andvari@spira.local";
    let (rc, out) = guard(&rig, &r, dwarf, None);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains(dwarf) && out.contains("main") && out.contains("--no-verify"),
        "the refusal names the committer, the branch and the override: {out}"
    );
    assert_eq!(
        guard(&rig, &r, "op@example.com", None).0,
        0,
        "operator identity passes on the base branch"
    );

    let wt = rig.dir.join("wt");
    git(
        &r,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "spira/sp-wt",
            &wt.display().to_string(),
        ],
    );
    assert_eq!(
        guard(&rig, &wt, dwarf, None).0,
        0,
        "a dwarf on a non-base branch passes"
    );
    assert_eq!(
        guard(&rig, &wt, dwarf, Some(&wt)).0,
        0,
        "a dwarf in its assigned worktree passes"
    );

    let (rc, out) = guard(&rig, &r, dwarf, Some(&wt));
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains(&r.display().to_string())
            && out.contains(&wt.display().to_string())
            && out.contains("branch-guard.sh"),
        "the refusal names both paths: {out}"
    );
    assert_eq!(
        guard(&rig, &r, "op@example.com", Some(&wt)).0,
        0,
        "an operator may commit in the main checkout whatever SPIRA_WORK says"
    );
    git(&r, &["checkout", "-q", "-b", "spira/sp-sweep"]);
    assert_eq!(
        guard(&rig, &r, dwarf, None).0,
        0,
        "a sweep with no SPIRA_WORK on its own branch passes"
    );
}

// covers: UC-safety-fences-18
#[test_id(tglnkx3)]
#[test]
fn a_dwarf_in_a_worktree_may_not_commit_to_a_local_landing_ref() {
    let rig = Rig::new(&[]);
    let r = rig.dir.join("local");
    std::fs::create_dir(&r).unwrap();
    git(&r, &["init", "-q", "-b", "local/main"]);
    git(&r, &["commit", "-q", "--allow-empty", "-m", "initial"]);
    git(&r, &["checkout", "-q", "-b", "elsewhere"]);
    let wt = rig.dir.join("wt");
    git(
        &r,
        &[
            "worktree",
            "add",
            "-q",
            &wt.display().to_string(),
            "local/main",
        ],
    );
    let guard_on_local = |email: &str| {
        let mut c = rig.cmd("bash");
        c.arg(spira().join("branch-guard.sh"))
            .arg("staged")
            .arg(&wt)
            .env("STUB_LANDREF", "local/main")
            .env("GIT_COMMITTER_EMAIL", email);
        let o = c.output().unwrap();
        (o.status.code().unwrap(), text(&o))
    };
    let (rc, out) = guard_on_local("dwarf-andvari@spira.local");
    assert!(rc == 1 && out.contains("local/main"), "{out}");
    assert_eq!(guard_on_local("op@example.com").0, 0);
}

// covers: UC-safety-fences-19
#[test_id(th9bu9t)]
#[test]
fn the_audit_reports_a_dwarf_tip_and_a_checkout_ahead_then_clears_after_an_operator_commit() {
    let rig = Rig::new(&[]);
    let r = repo(&rig, "r");
    std::fs::write(r.join("f.txt"), "dwarf direct commit\n").unwrap();
    must(rig.git(
        &r,
        "dwarf-andvari",
        &["commit", "-q", "-a", "-m", "sp-test: planted on main"],
    ));
    let check = |rig: &Rig| {
        let mut c = rig.cmd("bash");
        c.arg(spira().join("branch-guard.sh"))
            .arg("check")
            .env("STUB_ROOT", &r);
        let o = c.output().unwrap();
        (o.status.code().unwrap(), text(&o))
    };
    let (rc, out) = check(&rig);
    assert!(
        rc == 1 && out.contains("DWARF COMMIT ON") && out.contains("SHARED CHECKOUT AHEAD"),
        "{out}"
    );

    git(&r, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let (rc, out) = check(&rig);
    assert!(
        rc == 1 && out.contains("DWARF COMMIT ON") && !out.contains("SHARED CHECKOUT AHEAD"),
        "{out}"
    );

    std::fs::write(r.join("f.txt"), "operator restores order\n").unwrap();
    must(rig.git(
        &r,
        "op",
        &["commit", "-q", "-a", "-m", "chore: operator commit"],
    ));
    git(&r, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let (rc, out) = check(&rig);
    assert!(rc == 0 && out.contains("clean"), "{out}");
}

/// A harness-shaped clone (the signature `exclude.sh` scopes by) armed by `exclude.sh install`
/// with the tracked `hooks/` reached through a link, so the hook finds its siblings.
fn armed_clone(rig: &Rig) -> PathBuf {
    let r = rig.dir.join("clone");
    std::fs::create_dir(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    for f in ["boundary", "gate.sh", "lib.sh"] {
        std::fs::write(r.join(f), "").unwrap();
    }
    std::os::unix::fs::symlink(spira().join("hooks"), r.join("hooks")).unwrap();
    git(&r, &["add", "boundary", "gate.sh", "lib.sh"]);
    git(&r, &["commit", "-q", "-m", "seed: harness-shaped clone"]);
    git(&r, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let mut c = rig.cmd("bash");
    c.arg(spira().join("exclude.sh")).arg("install").arg(&r);
    assert!(must(c).contains("hooks"));
    assert_eq!(git(&r, &["config", "core.hooksPath"]), "hooks");
    r
}

fn stage(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
    git(dir, &["add", "-f", name]);
}

fn unstage(dir: &Path, name: &str) {
    git(dir, &["rm", "-q", "-f", "--cached", name]);
    std::fs::remove_file(dir.join(name)).unwrap();
}

fn through_the_hook(rig: &Rig, r: &Path, who: &str, red: &str) -> (bool, String) {
    let o = rig
        .git(r, who, &["commit", "-q", "-m", "sp-test: through the hook"])
        .env("STUB_LINT_RED", red)
        .output()
        .unwrap();
    (o.status.success(), text(&o))
}

// covers: UC-safety-fences-22
#[test_id(tkacn00)]
#[test]
fn the_tracked_pre_commit_hook_refuses_beads_data_and_a_red_scratch_fence() {
    let rig = Rig::new(&[]);
    let r = armed_clone(&rig);
    stage(&r, "export.jsonl", "{}\n");
    let (ok, out) = through_the_hook(&rig, &r, "op", "");
    assert!(
        !ok && out.contains("exclude.sh") && out.contains("export.jsonl"),
        "exclude.sh refuses beads data through the hook: {out}"
    );
    unstage(&r, "export.jsonl");

    stage(&r, "sp-xxxx-notes.md", "notes\n");
    let (ok, out) = through_the_hook(&rig, &r, "op", "scratch-fence");
    assert!(
        !ok && out.contains("scratch-fence"),
        "a red scratch-fence refuses the commit: {out}"
    );
    assert!(
        rig.lint_log()
            .lines()
            .any(|l| l.starts_with("scratch-fence ") && l.contains("sp-xxxx-notes.md")),
        "the rule ran over the staged tree: {}",
        rig.lint_log()
    );
}

// covers: UC-safety-fences-22
#[test_id(t9vd7yd)]
#[test]
fn the_tracked_pre_commit_hook_refuses_a_dwarf_on_base_and_passes_an_operator() {
    let rig = Rig::new(&[]);
    let r = armed_clone(&rig);
    stage(&r, "lib.sh", "more\n");
    let (ok, out) = through_the_hook(&rig, &r, "dwarf-andvari", "");
    assert!(
        !ok && out.contains("branch-guard.sh"),
        "branch-guard.sh refuses a dwarf on the base branch: {out}"
    );
    let (ok, out) = through_the_hook(&rig, &r, "op", "");
    assert!(
        ok,
        "an ordinary operator commit passes the same hook: {out}"
    );
}

// covers: UC-safety-fences-22
#[test_id(tg6ua2c)]
#[test]
fn the_tracked_pre_commit_hook_refuses_by_name_when_the_linter_is_missing() {
    let rig = Rig::new(&[]);
    let r = armed_clone(&rig);
    stage(&r, "g.txt", "g\n");
    let o = rig
        .git(&r, "op", &["commit", "-q", "-m", "sp-test: no linter"])
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        !o.status.success() && text(&o).contains("spira-lint is not on PATH"),
        "{}",
        text(&o)
    );
}

fn alive(dir: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "show-ref",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .unwrap()
        .success()
}

/// `repo`'s `core.hooksPath` pointed at a directory holding only the tracked `reference-transaction`.
fn arm_reference_hook(rig: &Rig, repo: &Path) {
    let hooks = rig.dir.join(format!(
        "ref-hooks-{}",
        repo.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir(&hooks).unwrap();
    testkit::write_exe(
        hooks.join("reference-transaction"),
        &format!(
            "#!/bin/bash\nexec \"{}\" \"$@\"\n",
            spira().join("hooks/reference-transaction").display()
        ),
    );
    git(
        repo,
        &["config", "core.hooksPath", &hooks.display().to_string()],
    );
}

// covers: UC-safety-fences-20
#[test_id(tc9g870)]
#[test]
fn a_bead_branch_cannot_be_deleted_outside_the_chokepoint_and_nothing_else_is_refused() {
    let rig = Rig::new(&[]);
    let r = repo(&rig, "r");
    for b in [
        "spira/sp-victim",
        "spira/queue/20260918T000000Z",
        "feature/unrelated",
    ] {
        git(&r, &["branch", b]);
    }
    arm_reference_hook(&rig, &r);
    let delete = |b: &str, sanctioned: bool| {
        let mut c = rig.git(&r, "op", &["branch", "-D", b]);
        if sanctioned {
            c.env("SPIRA_REF_SANCTIONED", "1");
        }
        c.output().unwrap()
    };
    let o = delete("spira/sp-victim", false);
    assert!(
        !o.status.success() && alive(&r, "spira/sp-victim"),
        "the ref survives an unsanctioned delete: {}",
        text(&o)
    );
    assert!(
        delete("spira/sp-victim", true).status.success() && !alive(&r, "spira/sp-victim"),
        "the sanctioned path still deletes"
    );
    assert!(
        !delete("spira/queue/20260918T000000Z", false)
            .status
            .success()
            && alive(&r, "spira/queue/20260918T000000Z"),
        "queue branches are held too"
    );
    assert!(
        delete("feature/unrelated", false).status.success(),
        "a ref outside refs/heads/spira/ is untouched"
    );

    must(rig.git(&r, "op", &["checkout", "-q", "-b", "spira/sp-mine"]));
    std::fs::write(r.join("work.txt"), "w\n").unwrap();
    assert!(
        commit_all(&rig, &r, "op", "ordinary work").status.success() && alive(&r, "spira/sp-mine"),
        "creating a branch and committing are not refused"
    );

    let hook = spira().join("hooks/reference-transaction");
    let zero = "0".repeat(40);
    let mut c = rig.cmd("bash");
    c.arg(&hook)
        .arg("committed")
        .stdin(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        let line = format!("{} {zero} refs/heads/spira/sp-x\n", "a".repeat(40));
        for _ in 0..700 {
            stdin
                .write_all(line.as_bytes())
                .expect("the hook drains its stdin without a broken pipe");
        }
    }
    assert!(
        child.wait().unwrap().success(),
        "the committed phase exits 0"
    );

    git(&r, &["branch", "spira/sp-packme"]);
    let head = git(&r, &["rev-parse", "spira/sp-packme"]);
    must(rig.git(&r, "op", &["pack-refs", "--all", "--prune"]));
    assert_eq!(
        git(&r, &["rev-parse", "spira/sp-packme"]),
        head,
        "packing is a representation change, not a deletion"
    );
    assert!(
        !delete("spira/sp-packme", false).status.success() && alive(&r, "spira/sp-packme"),
        "a packed bead branch is still held"
    );
}

// covers: UC-safety-fences-20
// covers: UC-safety-fences-21
#[test_id(tyqcatu)]
#[test]
fn a_dwarf_worktree_is_armed_with_every_canonical_hook_and_cannot_delete_a_bead_branch() {
    let rig = Rig::new(&[]);
    let r = repo(&rig, "r");
    let wt = armed_worktree(&rig, &r, "sp-wt");
    let armed = gitdir_of(&wt).join("hooks");
    let mut canonical = Vec::new();
    for e in std::fs::read_dir(spira().join("hooks")).unwrap() {
        let n = e.unwrap().file_name().to_string_lossy().to_string();
        if !n.ends_with(".sh") {
            canonical.push(n);
        }
    }
    assert!(
        canonical.len() >= 3,
        "the canonical set is read from the directory: {canonical:?}"
    );
    for n in &canonical {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(armed.join(n))
            .unwrap_or_else(|_| panic!("{n} is armed in the worktree"))
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "{n} is executable");
    }
    assert!(
        !armed.join("dwarf-fence.sh").exists(),
        "helpers beside the hooks are not armed as hooks"
    );

    git(&wt, &["branch", "spira/sp-wt-victim"]);
    let o = rig
        .git(&wt, "op", &["branch", "-D", "spira/sp-wt-victim"])
        .output()
        .unwrap();
    assert!(
        !o.status.success() && alive(&r, "spira/sp-wt-victim"),
        "an unsanctioned delete from inside a worktree is refused: {}",
        text(&o)
    );
    let o = rig
        .git(&wt, "op", &["branch", "-D", "spira/sp-wt-victim"])
        .env("SPIRA_REF_SANCTIONED", "1")
        .output()
        .unwrap();
    assert!(
        o.status.success() && !alive(&r, "spira/sp-wt-victim"),
        "the sanctioned delete from inside a worktree succeeds: {}",
        text(&o)
    );
}

fn in_wt(rig: &Rig, wt: &Path, who: &str, msg: &str, env: &[(&str, &str)]) -> (bool, String) {
    let o = rig
        .git(wt, who, &["commit", "-q", "-m", msg])
        .envs(env.iter().copied())
        .output()
        .unwrap();
    (o.status.success(), text(&o))
}

fn signed_worktree(rig: &Rig) -> (PathBuf, PathBuf) {
    let r = repo(rig, "r");
    let wt = armed_worktree(rig, &r, "sp-comp");
    for f in ["boundary", "gate.sh", "lib.sh"] {
        std::fs::write(wt.join(f), "").unwrap();
    }
    git(&wt, &["add", "-A"]);
    must(rig.git(
        &wt,
        "op",
        &[
            "commit",
            "-q",
            "--no-verify",
            "-m",
            "test: seed the harness signature",
        ],
    ));
    (r, wt)
}

// covers: UC-safety-fences-21
#[test_id(tuj25na)]
#[test]
fn the_composed_worktree_pre_commit_runs_exclude_and_branch_guard() {
    let rig = Rig::new(&[]);
    let (r, wt) = signed_worktree(&rig);
    stage(&wt, "export.jsonl", "{}\n");
    let (ok, out) = in_wt(&rig, &wt, "op", "sp-test: stage beads export", &[]);
    assert!(
        !ok && out.contains("exclude.sh") && out.contains("export.jsonl"),
        "exclude.sh refuses through the worktree hook: {out}"
    );
    unstage(&wt, "export.jsonl");

    stage(&wt, "seed.txt", "more\n");
    let elsewhere = rig.dir.join("somewhere-else");
    let env = [
        ("SPIRA_WORK", elsewhere.to_str().unwrap()),
        ("STUB_ROOT", r.to_str().unwrap()),
    ];
    let (ok, out) = in_wt(&rig, &wt, "dwarf-t", "sp-test: wrong worktree", &env);
    assert!(
        !ok && out.contains("branch-guard.sh"),
        "branch-guard.sh refuses a SPIRA_WORK mismatch: {out}"
    );
}

// covers: UC-safety-fences-21
#[test_id(tzsm7tt)]
#[test]
fn the_composed_worktree_pre_commit_runs_the_canonical_fences_before_the_dirty_path_guard() {
    let rig = Rig::new(&[]);
    let (_, wt) = signed_worktree(&rig);
    std::fs::write(
        gitdir_of(&wt).join("spira-dirty-before"),
        "sp-both-guards.md\n",
    )
    .unwrap();
    stage(&wt, "sp-both-guards.md", "planted\n");
    let (ok, out) = in_wt(
        &rig,
        &wt,
        "op",
        "sp-test: offends both",
        &[("STUB_LINT_RED", "scratch-fence")],
    );
    assert!(
        !ok && out.contains("scratch-fence") && !out.contains("staged paths were dirty"),
        "the canonical fences stop it first: {out}"
    );
    let (ok, out) = in_wt(&rig, &wt, "op", "sp-test: dirty only", &[]);
    assert!(
        !ok && out.contains("staged paths were dirty") && out.contains("sp-both-guards.md"),
        "with the fences silent the dirty-path guard still runs: {out}"
    );
}

/// What git does when `HEAD` is re-pointed: before 2.46 the reference-transaction hook is not asked.
fn git_reports_head_moves(rig: &Rig) -> bool {
    let p = rig.dir.join("headprobe");
    std::fs::create_dir(&p).unwrap();
    git(&p, &["init", "-q", "-b", "main"]);
    git(&p, &["commit", "-q", "--allow-empty", "-m", "p"]);
    git(&p, &["branch", "probe-b"]);
    let hooks = rig.dir.join("probe-hooks");
    std::fs::create_dir(&hooks).unwrap();
    let marker = rig.dir.join("saw-head");
    testkit::write_exe(hooks.join("reference-transaction"), &format!("#!/bin/bash\n[ \"$1\" = prepared ] && cat | grep -q ' HEAD$' && touch \"{}\"\nexit 0\n", marker.display()));
    git(
        &p,
        &["config", "core.hooksPath", &hooks.display().to_string()],
    );
    git(&p, &["checkout", "-q", "probe-b"]);
    marker.exists()
}

struct Landing {
    lr: PathBuf,
    wt: PathBuf,
    old: String,
    new: String,
}

impl Landing {
    fn new(rig: &Rig) -> Landing {
        let lr = rig.dir.join("landrepo");
        std::fs::create_dir(&lr).unwrap();
        git(&lr, &["init", "-q", "-b", "main"]);
        git(&lr, &["commit", "-q", "--allow-empty", "-m", "seed"]);
        git(&lr, &["branch", "local/main"]);
        git(&lr, &["commit", "-q", "--allow-empty", "-m", "second"]);
        let (old, new) = (
            git(&lr, &["rev-parse", "local/main"]),
            git(&lr, &["rev-parse", "HEAD"]),
        );
        arm_reference_hook(rig, &lr);
        let wt = armed_worktree(rig, &lr, "sp-mine");
        Landing { lr, wt, old, new }
    }

    fn run(&self, rig: &Rig, env: &[(&str, &str)], dir: &Path, args: &[&str]) -> (bool, String) {
        let o = rig
            .git(dir, "op", args)
            .env("STUB_LANDREF", "local/main")
            .env("STUB_ROOT", &self.lr)
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (o.status.success(), text(&o))
    }

    fn landref(&self) -> String {
        git(&self.lr, &["rev-parse", "refs/heads/local/main"])
    }
}

const DWARF_SESSION: [(&str, &str); 2] = [("SPIRA_DWARF", "fixture"), ("BEAD_ID", "sp-mine")];

// covers: UC-safety-fences-35
#[test_id(t53blol)]
#[test]
fn a_dwarf_may_not_check_out_the_landing_ref_from_its_worktree_or_the_main_checkout() {
    let rig = Rig::new(&[]);
    if !git_reports_head_moves(&rig) {
        return;
    }
    let l = Landing::new(&rig);
    let (ok, out) = l.run(&rig, &DWARF_SESSION, &l.wt, &["checkout", "local/main"]);
    assert!(
        !ok,
        "a dwarf's checkout of the landing ref is refused: {out}"
    );
    assert_eq!(
        git(&l.wt, &["symbolic-ref", "--short", "HEAD"]),
        "spira/sp-mine"
    );
    for want in [
        "local/main",
        "git merge local/main",
        "SPIRA_DWARF",
        "git switch spira/sp-mine",
    ] {
        assert!(out.contains(want), "the refusal names {want}: {out}");
    }
    assert!(
        !out.contains("SPIRA_REF_SANCTIONED"),
        "the refusal does not advertise the override: {out}"
    );
    let (ok, out) = l.run(&rig, &DWARF_SESSION, &l.lr, &["checkout", "local/main"]);
    assert!(
        !ok && out.contains("git merge local/main"),
        "git -C at the armed main checkout is refused too: {out}"
    );
    assert_eq!(git(&l.lr, &["symbolic-ref", "--short", "HEAD"]), "main");
}

// covers: UC-safety-fences-35
#[test_id(t2olj64)]
#[test]
fn a_dwarf_may_not_write_the_landing_ref_and_an_unresolvable_one_fails_closed() {
    let rig = Rig::new(&[]);
    let l = Landing::new(&rig);
    let (ok, out) = l.run(
        &rig,
        &DWARF_SESSION,
        &l.wt,
        &["update-ref", "refs/heads/local/main", &l.new, &l.old],
    );
    assert!(
        !ok && out.contains("git merge local/main"),
        "update-ref on the landing ref is refused: {out}"
    );
    l.run(
        &rig,
        &DWARF_SESSION,
        &l.wt,
        &["branch", "-f", "local/main", "HEAD"],
    );
    assert_eq!(l.landref(), l.old, "the landing ref did not move");

    let env = [DWARF_SESSION[0], DWARF_SESSION[1], ("STUB_LANDREF", "")];
    let o = rig
        .git(
            &l.wt,
            "op",
            &["update-ref", "refs/heads/local/main", &l.new, &l.old],
        )
        .envs(env)
        .env("STUB_ROOT", &l.lr)
        .output()
        .unwrap();
    let out = text(&o);
    assert!(
        !o.status.success()
            && out.contains(&l.lr.display().to_string())
            && out.contains("spira-config repo landref"),
        "an unresolvable landing ref fails closed, naming the root: {out}"
    );
}

// covers: UC-safety-fences-35
#[test_id(tr0iaqj)]
#[test]
fn an_operator_and_the_sanctioned_writer_may_move_the_landing_ref_with_a_dwarf_session_set() {
    let rig = Rig::new(&[]);
    let l = Landing::new(&rig);
    let mv = |env: &[(&str, &str)], from: &str, to: &str| {
        l.run(
            &rig,
            env,
            &l.lr,
            &["update-ref", "refs/heads/local/main", to, from],
        )
        .0
    };
    assert!(
        mv(&[], &l.old, &l.new) && l.landref() == l.new,
        "an operator without SPIRA_DWARF may move it"
    );
    assert!(
        mv(
            &[DWARF_SESSION[0], ("SPIRA_REF_SANCTIONED", "1")],
            &l.new,
            &l.old
        ) && l.landref() == l.old,
        "the sanctioned writer may move it"
    );
    assert!(
        !mv(&DWARF_SESSION, &l.old, &l.new) && l.landref() == l.old,
        "a dwarf may not"
    );
}

// covers: UC-safety-fences-35
#[test_id(t3k5mwn)]
#[test]
fn a_dwarf_may_commit_on_its_bead_branch_and_merge_the_landing_ref_into_it() {
    let rig = Rig::new(&[]);
    let l = Landing::new(&rig);
    std::fs::write(l.wt.join("mine.rs"), "fn main() {}\n").unwrap();
    git(&l.wt, &["add", "mine.rs"]);
    let (ok, out) = l.run(
        &rig,
        &DWARF_SESSION,
        &l.wt,
        &["commit", "-q", "-m", "sp-test: own work"],
    );
    assert!(ok, "a commit on the bead branch passes: {out}");
    let (ok, out) = l.run(&rig, &DWARF_SESSION, &l.wt, &["merge", "-q", "local/main"]);
    assert!(
        ok,
        "merging the landing ref into the bead branch passes: {out}"
    );
}

fn exclude(rig: &Rig, args: &[&str], stdin: &str) -> (i32, String) {
    use std::io::Write;
    let mut c = rig.cmd("bash");
    c.arg(spira().join("exclude.sh"))
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    (o.status.code().unwrap(), text(&o))
}

// covers: UC-safety-fences-23
#[test_id(titx67o)]
#[test]
fn beads_data_is_filtered_by_shape_inside_the_harness_scope_and_refused_when_staged() {
    let rig = Rig::new(&[]);
    let signature = "spira/boundary\nspira/gate.sh\nspira/lib.sh\n";
    let nested = "vendor/spira/boundary\nvendor/spira/gate.sh\nvendor/spira/lib.sh\n";

    let (rc, out) = exclude(&rig, &["filter"], "a/b.txt\nc/d.jsonl\n");
    assert!(
        rc == 3 && out.contains("nothing to guard"),
        "no harness signature: refuses to guard nothing: {out}"
    );

    let offenders = [
        "spira/.beads/config.yaml",
        "spira/.dolt/noms/manifest",
        "spira/embeddeddolt/x",
        "spira/proxieddb/y",
        "spira/export.jsonl",
        "spira/state.db",
        "spira/state.db-wal",
        "spira/state.sqlite3",
        "spira/.beads-credential-key",
        "spira/sub/dir/.beads/metadata.json",
    ];
    let (rc, out) = exclude(
        &rig,
        &["filter"],
        &format!("{signature}{}", offenders.join("\n")),
    );
    assert_eq!(rc, 0, "{out}");
    for p in offenders {
        assert!(out.lines().any(|l| l == p), "flags {p}: {out}");
    }
    let clean =
        "spira/dwarf.sh\nspira/docs/readme.md\nspira/sub/dir/notes.md\nspira/a.jsonline.txt";
    assert_eq!(
        exclude(&rig, &["filter"], &format!("{signature}{clean}")),
        (1, String::new()),
        "clean paths are never flagged"
    );

    let (rc, out) = exclude(
        &rig,
        &["filter"],
        &format!("{nested}outside/.beads/config.yaml"),
    );
    assert!(
        rc == 1 && !out.contains("outside/.beads"),
        "a path outside a nested harness directory is not this fence's business: {out}"
    );
    let (rc, out) = exclude(
        &rig,
        &["filter"],
        &format!("{nested}vendor/spira/.beads/config.yaml"),
    );
    assert!(
        rc == 0 && out.contains("vendor/spira/.beads/config.yaml"),
        "{out}"
    );

    let r = rig.dir.join("harness");
    std::fs::create_dir_all(r.join("spira")).unwrap();
    for f in ["boundary", "gate.sh", "lib.sh"] {
        std::fs::write(r.join("spira").join(f), "").unwrap();
    }
    git(&r, &["init", "-q", "-b", "main"]);
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "init"]);
    std::fs::create_dir(r.join(".beads")).unwrap();
    std::fs::write(r.join(".beads/config.yaml"), "x\n").unwrap();
    git(&r, &["add", "-A"]);
    let root = r.display().to_string();
    let (rc, out) = exclude(&rig, &["staged", &root], "");
    assert!(
        rc == 1 && out.contains(".beads/config.yaml") && out.contains("git restore --staged"),
        "a staged root-level .beads/ is refused, naming the path and the way out: {out}"
    );
    let listed = git(&r, &["ls-files"]);
    let (rc, out) = exclude(&rig, &["filter"], &listed);
    assert!(
        rc == 0 && out.contains(".beads/config.yaml"),
        "filter widens one-level harness scope to the root as staged does: {out}"
    );
    assert_eq!(exclude(&rig, &["check", &root], "").0, 1, "check agrees");
    git(&r, &["reset", "-q", "--hard"]);
    git(&r, &["clean", "-qfdx"]);
    std::fs::write(r.join("spira/lib.sh"), "more\n").unwrap();
    git(&r, &["add", "-A"]);
    assert_eq!(
        exclude(&rig, &["staged", &root], "").0,
        0,
        "a clean stage passes"
    );
}

fn spike_rig() -> (Rig, PathBuf) {
    let rig = Rig::new(&[
        ("SPIRA_SPIKE_LABEL", "research"),
        ("SPIRA_SPIKE_DIR", "notes/spikes"),
        ("SPIRA_SPIKE_PATHS", "notes/spikes sources"),
    ]);
    let r = rig.dir.join("repo");
    std::fs::create_dir_all(r.join("src")).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    std::fs::write(r.join("src/lib.rs"), "base\n").unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "base"]);
    (rig, r)
}

fn spike_branch(r: &Path, id: &str, files: &[&str]) {
    git(r, &["checkout", "-q", "-B", &format!("spira/{id}"), "main"]);
    for f in files {
        std::fs::create_dir_all(r.join(f).parent().unwrap()).unwrap();
        std::fs::write(r.join(f), id).unwrap();
    }
    git(r, &["add", "-A"]);
    git(r, &["commit", "-q", "-m", id]);
}

fn confine(rig: &Rig, r: &Path, id: &str, labels: &str) -> (i32, String) {
    let mut c = rig.cmd("bash");
    c.env("SPIRA_SPIKE_LABEL", "research")
        .arg(spira().join("confine.sh"))
        .args([
            id,
            &format!("spira/{id}"),
            &r.display().to_string(),
            "main",
            labels,
        ]);
    let o = c.output().unwrap();
    (o.status.code().unwrap(), text(&o))
}

// covers: UC-safety-fences-32
#[test_id(tgrywng)]
#[test]
fn a_spike_branch_touching_code_or_an_adjacent_directory_is_refused_naming_the_path() {
    let (rig, r) = spike_rig();
    spike_branch(&r, "sp-poc", &["notes/spikes/question.md", "src/poc.rs"]);
    let (rc, out) = confine(&rig, &r, "sp-poc", "spira research");
    assert_eq!(rc, 1, "{out}");
    let outside = out.split("outside:").nth(1).unwrap_or_default();
    assert!(
        outside.contains("src/poc.rs") && !outside.contains("question.md"),
        "the refusal names the offending path and not the document: {out}"
    );
    assert!(
        out.contains("notes/spikes") && out.contains("branch of their own"),
        "it says what may land and the remedy: {out}"
    );

    spike_branch(&r, "sp-adj", &["notes/spikes-scratch/poc.rs"]);
    let (rc, out) = confine(&rig, &r, "sp-adj", "spira research");
    assert!(
        rc == 1 && out.contains("notes/spikes-scratch/poc.rs"),
        "an adjacent directory is a string prefix, not a path prefix: {out}"
    );
}

// covers: UC-safety-fences-32
#[test_id(t0xmdda)]
#[test]
fn a_spike_branch_inside_the_allowed_trees_passes_and_a_plan_branch_is_not_confined() {
    let (rig, r) = spike_rig();
    for (id, files) in [
        ("sp-doc", &["notes/spikes/question.md"][..]),
        ("sp-src", &["notes/spikes/q.md", "sources/fetched.md"][..]),
        ("sp-deep", &["notes/spikes/sources/2026/a.jsonl"][..]),
    ] {
        spike_branch(&r, id, files);
        assert_eq!(
            confine(&rig, &r, id, "spira research").0,
            0,
            "{id} stays inside the allowed trees"
        );
    }
    spike_branch(&r, "sp-build", &["src/feature.rs"]);
    assert_eq!(
        confine(&rig, &r, "sp-build", "spira plan"),
        (0, String::new()),
        "the fence binds the spike persona, not the path"
    );

    spike_branch(&r, "sp-behind", &["notes/spikes/behind.md"]);
    git(&r, &["checkout", "-q", "main"]);
    std::fs::write(r.join("src/unrelated.rs"), "moved\n").unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "main moves on"]);
    let (rc, out) = confine(&rig, &r, "sp-behind", "spira research");
    assert!(
        rc == 0 && !out.contains("unrelated.rs"),
        "a branch behind its base is judged on its own work: {out}"
    );
}
