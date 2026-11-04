//! Children the runner starts in a systemd scope of their own, so one signal reaches round-vm
//! and everything it started, whatever process group each moved itself into (coreutils
//! `timeout` takes one of its own), and gone means the scope's cgroup is empty. Without a scope
//! the child gets a process group of its own. Also round-vm's progress marks.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};
use serde_json::Value;

use crate::ports::{fields, CertifyEnd, Child, Mark, Signal, Verb, VerbRefusal};

/// round-vm's live progress file in its run directory.
pub const PROGRESS_FILE: &str = "round-progress.json";
/// The most of a red's output the Judge is handed, from its end.
pub const OUTPUT_MAX_BYTES: usize = 64 * 1024;
const KILL_DEADLINE: Duration = Duration::from_secs(10); // batch-job: one kill(1) call

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnSpec {
    pub verb: Option<Verb>,
    pub prog: String,
    pub args: Vec<String>,
    /// Where its stderr goes; captured when `None`.
    pub stderr_file: Option<PathBuf>,
    /// round-vm's run directory, where its progress file is.
    pub progress: Option<PathBuf>,
    /// The transient scope the child runs in, `<unit>.scope`, and its properties.
    pub scope: Option<String>,
    pub scope_props: Vec<String>,
}

pub trait Spawn: Send + Sync {
    fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn Child>, String>;
}

/// Spawns through the OS, signalling through `kill(1)` on [`Proc`].
pub struct SysSpawn {
    pub proc_: Arc<dyn Proc>,
}

impl Spawn for SysSpawn {
    fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn Child>, String> {
        let mut cmd = match &spec.scope {
            Some(unit) => {
                // batch-job: a round pass runs as long as its VM does; the runner's deadlines end it
                // FORK-EDGE: needs a Units scope start (a property-carrying scope whose process stays this one's child, piped stdout)
                let mut c = Command::new("systemd-run");
                c.args(["--user", "--scope", "--quiet", "--unit", unit]);
                for prop in &spec.scope_props {
                    c.args(["-p", prop]);
                }
                c.arg(&spec.prog);
                c
            }
            // batch-job: a round pass runs as long as its VM does; the runner's deadlines end it
            // FORK-EDGE: machine leg — the program the pass names (round-vm, testenv)
            None => Command::new(&spec.prog),
        };
        cmd.args(&spec.args)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        match &spec.stderr_file {
            Some(f) => {
                let file = std::fs::File::create(f).map_err(|e| format!("{}: {e}", f.display()))?;
                cmd.stderr(Stdio::from(file));
            }
            None => {
                cmd.stderr(Stdio::piped());
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", spec.prog))?;
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let mut readers = Vec::new();
        if let Some(s) = child.stdout.take() {
            readers.push(drain(s, out.clone()));
        }
        if let Some(s) = child.stderr.take() {
            readers.push(drain(s, err.clone()));
        }
        Ok(Box::new(SysChild {
            pgid: child.id(),
            verb: spec.verb.unwrap_or(Verb::Certify),
            child: Mutex::new(child),
            out,
            err,
            readers: Mutex::new(readers),
            answer: Mutex::new(None),
            progress: spec.progress.clone(),
            scope: spec.scope.as_ref().map(|u| format!("{u}.scope")),
            proc_: self.proc_.clone(),
        }))
    }
}

fn drain(mut r: impl Read + Send + 'static, to: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        crate::lock(&to).extend(buf);
    })
}

struct SysChild {
    pgid: u32,
    verb: Verb,
    child: Mutex<std::process::Child>,
    out: Arc<Mutex<Vec<u8>>>,
    err: Arc<Mutex<Vec<u8>>>,
    readers: Mutex<Vec<JoinHandle<()>>>,
    answer: Mutex<Option<Result<CertifyEnd, VerbRefusal>>>,
    progress: Option<PathBuf>,
    scope: Option<String>,
    proc_: Arc<dyn Proc>,
}

impl SysChild {
    fn call(&self, prog: &str, args: Vec<String>) -> Result<runtime::ports::ProcOutput, String> {
        self.proc_.run(&ProcSpec {
            prog: prog.into(),
            args,
            stdin: vec![],
            deadline: KILL_DEADLINE,
        })
    }

    /// `kill <sig> -- -<pgid>`: true when it reached a process.
    fn kill(&self, sig: &str) -> Result<bool, String> {
        let args = vec![sig.into(), "--".into(), format!("-{}", self.pgid)];
        Ok(self.call("kill", args)?.code == Some(0))
    }

    /// Every process of the scope has exited: systemd removes an emptied transient scope.
    fn scope_empty(&self, unit: &str) -> bool {
        let args = vec!["--user".into(), "is-active".into(), unit.into()];
        self.call("systemctl", args).is_ok_and(|o| {
            matches!(
                crate::death::UnitState::from_is_active(&String::from_utf8_lossy(&o.stdout)),
                crate::death::UnitState::Stopped(_)
            )
        })
    }
}

impl Child for SysChild {
    fn poll(&self) -> Option<Result<CertifyEnd, VerbRefusal>> {
        let mut answer = crate::lock(&self.answer);
        if answer.is_none() {
            let status = match crate::lock(&self.child).try_wait() {
                Ok(Some(st)) => st,
                Ok(None) => return None,
                Err(e) => {
                    *answer = Some(Err(VerbRefusal {
                        verb: self.verb,
                        code: None,
                        text: format!("waiting on the child: {e}"),
                    }));
                    return answer.clone();
                }
            };
            for r in crate::lock(&self.readers).drain(..) {
                let _ = r.join();
            }
            let stdout = String::from_utf8_lossy(&crate::lock(&self.out)).into_owned();
            let stderr = String::from_utf8_lossy(&crate::lock(&self.err)).into_owned();
            *answer = Some(ended(self.verb, status.code(), &stdout, &stderr));
        }
        answer.clone()
    }

    fn mark(&self) -> Option<Mark> {
        let run = self.progress.as_ref()?;
        mark_of(&std::fs::read_to_string(run.join(PROGRESS_FILE)).ok()?)
    }

    fn signal(&self, sig: Signal) -> Result<(), String> {
        let (word, name) = match sig {
            Signal::Term => ("-TERM", "SIGTERM"),
            Signal::Kill => ("-KILL", "SIGKILL"),
        };
        match &self.scope {
            Some(unit) => {
                let args = vec![
                    "--user".into(),
                    "kill".into(),
                    format!("--signal={name}"),
                    unit.clone(),
                ];
                self.call("systemctl", args).map(drop)
            }
            None => self.kill(word).map(drop),
        }
    }

    fn gone(&self) -> bool {
        self.poll().is_some()
            && match &self.scope {
                Some(unit) => self.scope_empty(unit),
                None => !self.kill("-0").unwrap_or(true),
            }
    }
}

/// A child's answer from its exit. Every exit is an answer, because certify's exit is its
/// verdict's channel; a child killed by a signal ran to no exit and answers nothing.
pub fn ended(
    verb: Verb,
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<CertifyEnd, VerbRefusal> {
    let Some(exit) = code else {
        return Err(VerbRefusal {
            verb,
            code: None,
            text: format!("killed by a signal before it exited\n{stderr}"),
        });
    };
    let fields = fields(stdout);
    let output = fields
        .get("red-output")
        .map(|p| tail_of(Path::new(p)))
        .unwrap_or_default();
    Ok(CertifyEnd {
        exit,
        fields,
        output,
    })
}

fn tail_of(p: &Path) -> String {
    let Ok(bytes) = std::fs::read(p) else {
        return String::new();
    };
    let from = bytes.len().saturating_sub(OUTPUT_MAX_BYTES);
    String::from_utf8_lossy(&bytes[from..]).into_owned()
}

/// round-vm's progress body: the commit it names its pass by, its phase, every phase it entered,
/// when it last changed, and the VM it names on entering `build` (`VM <handle>`).
pub fn mark_of(text: &str) -> Option<Mark> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let round = v.get("round")?.as_str()?.to_string();
    let phase = v.get("phase")?.as_str()?.to_string();
    let phases = v
        .get("phases")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| Some(p.get("phase")?.as_str()?.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let at_ms = v.get("updated_at")?.as_u64()?.saturating_mul(1000);
    let vm = v
        .get("detail")
        .and_then(Value::as_str)
        .and_then(|d| d.strip_prefix("VM "))
        .and_then(|h| h.split_whitespace().next())
        .map(str::to_string);
    Some(Mark {
        round,
        phase,
        phases,
        at_ms,
        vm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_exit_is_an_answer_with_its_fields_and_a_signal_is_none() {
        let e = ended(
            Verb::Certify,
            Some(4),
            "fault=over-cap unreached=3\nprose\n",
            "",
        )
        .unwrap();
        assert_eq!(
            (e.exit, e.fields["fault"].as_str()),
            (4, "over-cap unreached=3")
        );
        assert_eq!(e.output, "");
        let r = ended(Verb::Certify, None, "red=a\n", "boom").unwrap_err();
        assert_eq!(r.code, None);
        assert!(r.text.contains("boom"), "{}", r.text);
    }

    #[test]
    fn a_red_output_file_is_read_from_its_end_and_a_missing_one_is_empty() {
        let d = testkit::TempDir::new("rc-child-output");
        let f = d.path().join("red.out");
        let mut body = "x".repeat(OUTPUT_MAX_BYTES);
        body.push_str("at spira/lib.sh:12");
        std::fs::write(&f, &body).unwrap();
        let e = ended(
            Verb::Certify,
            Some(1),
            &format!("red=a\nred-output={}\n", f.display()),
            "",
        )
        .unwrap();
        assert_eq!(e.output.len(), OUTPUT_MAX_BYTES);
        assert!(e.output.ends_with("spira/lib.sh:12"));
        let gone = ended(Verb::Certify, Some(1), "red-output=/nonexistent/x\n", "").unwrap();
        assert_eq!(gone.output, "");
    }

    #[test]
    fn a_progress_mark_names_its_pass_and_the_vm_it_entered_build_on() {
        let body = r#"{"round":"b1","phase":"suites","phases":[{"phase":"vm","started":10},{"phase":"build","started":20},{"phase":"suites","started":30}],"detail":"VM 103 warm","updated_at":42}"#;
        let m = mark_of(body).unwrap();
        assert_eq!(
            m,
            Mark {
                round: "b1".into(),
                phase: "suites".into(),
                phases: vec!["vm".into(), "build".into(), "suites".into()],
                at_ms: 42_000,
                vm: Some("103".into()),
            }
        );
        assert_eq!(mark_of("not json"), None);
        assert_eq!(
            mark_of(r#"{"phase":"vm","updated_at":1}"#),
            None,
            "a mark names its pass"
        );
        let no_vm = body.replace("VM 103 warm", "leasing a VM");
        assert_eq!(mark_of(&no_vm).unwrap().vm, None);
    }
}
