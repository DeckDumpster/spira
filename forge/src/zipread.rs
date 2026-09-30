//! Reads a GitHub Actions run's `batch-results` artifact. Ported byte-for-byte from
//! `forge.sh`'s `_forge_artifact_zip` / `_forge_red_suites_artifact` /
//! `_forge_fail_lines_artifact` — same `gh api` calls, same embedded Python zip readers, now
//! launched by Rust instead of bash (DESIGN.md §2).

use crate::ports::{Gh, Proc};
use std::path::{Path, PathBuf};

/// Downloads the run's `batch-results-*` artifact zip to a temp dir; None if there is none
/// (no artifact, or the download failed). Caller removes the parent dir.
fn artifact_zip(gh: &dyn Gh, proc: &dyn Proc, repo: &Path, run_id: &str) -> Option<PathBuf> {
    let list = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}/artifacts")]);
    if !list.ok() {
        return None;
    }
    const FIND_ID: &str = r#"
import json, sys
try:
    for a in json.load(sys.stdin).get('artifacts', []):
        if (a.get('name') or '').startswith('batch-results-'):
            print(a['id']); break
except: pass
"#;
    let (rc, out) = proc.run("python3", &["-c", FIND_ID], Some(&list.stdout));
    if rc != 0 {
        return None;
    }
    let art_id = String::from_utf8_lossy(&out).trim().to_string();
    if art_id.is_empty() {
        return None;
    }
    let zip_bytes = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/actions/artifacts/{art_id}/zip")]);
    if !zip_bytes.ok() || zip_bytes.stdout.is_empty() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("forge-artifact-{}-{}", std::process::id(), unique()));
    std::fs::create_dir_all(&dir).ok()?;
    let zpath = dir.join("b.zip");
    std::fs::write(&zpath, &zip_bytes.stdout).ok()?;
    Some(zpath)
}

fn unique() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

const RED_SUITES_PY: &str = r#"
import zipfile, json, sys
try:
    with zipfile.ZipFile(sys.argv[1]) as z:
        names = z.namelist()
        rj = next((n for n in names if n.endswith('results.jsonl')), None)
        if rj is not None:
            red, flaky = [], []
            for line in z.read(rj).decode('utf-8', 'replace').splitlines():
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except Exception:
                    continue
                if row.get('case') != '(verdict)':
                    continue
                st = row.get('status')
                if st == 'red-red':
                    red.append(row.get('suite', ''))
                elif st == 'red-green':
                    flaky.append(row.get('suite', ''))
            if red or flaky:
                for s in red:
                    print('red-suite: ' + s)
                for s in flaky:
                    print('flaky: ' + s)
                sys.exit(0)
        rs = next((n for n in names if n.endswith('red-suites.json')), None)
        if rs is None:
            sys.exit(0)
        d = json.loads(z.read(rs))
        red = d.get('red', [])
        flaky = d.get('flaky', [])
        red_count = int(d.get('red_count', len(red)))
        if len(red) < red_count:
            print('artifact-truncated: ' + str(len(red)) + '/' + str(red_count))
        else:
            for s in red:
                print('red-suite: ' + s)
            for s in flaky:
                print('flaky: ' + s)
except Exception:
    pass
"#;

/// The run's full red/flaky suite list, or "" if there is no artifact to read. A
/// `artifact-truncated: n/m` line means the artifact's own count disagrees with what it
/// holds — the caller must not attribute against a partial list.
pub fn red_suites_artifact(gh: &dyn Gh, proc: &dyn Proc, repo: &Path, run_id: &str) -> String {
    let Some(zip) = artifact_zip(gh, proc, repo, run_id) else { return String::new() };
    let zip_s = zip.to_string_lossy().into_owned();
    let (_, out) = proc.run("python3", &["-c", RED_SUITES_PY, &zip_s], None);
    if let Some(parent) = zip.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
    String::from_utf8_lossy(&out).trim_end().to_string()
}

const FAIL_LINES_PY: &str = r#"
import zipfile, sys
zpath, suite = sys.argv[1], sys.argv[2]
try:
    with zipfile.ZipFile(zpath) as z:
        name = next((n for n in z.namelist()
                     if n == suite + '.out' or n.endswith('/' + suite + '.out')), None)
        if name is None:
            sys.exit(0)
        lines = z.read(name).decode('utf-8', 'replace').splitlines()
        fails = [l for l in lines if 'FAIL' in l][:20]
        if not fails:
            fails = lines[-20:]
        for l in fails:
            print('fail-line: ' + suite + ': ' + l)
except Exception:
    pass
"#;

/// Up to 20 `FAIL`-containing lines from each named suite's own `<suite>.out` in the run's
/// artifact (or its last 20 lines if none matched); `fail-line: <suite>: <text>` per line.
pub fn fail_lines_artifact(gh: &dyn Gh, proc: &dyn Proc, repo: &Path, run_id: &str, suites: &[&str]) -> Vec<String> {
    let Some(zip) = artifact_zip(gh, proc, repo, run_id) else { return Vec::new() };
    let zip_s = zip.to_string_lossy().into_owned();
    let mut lines = Vec::new();
    for s in suites {
        let (_, out) = proc.run("python3", &["-c", FAIL_LINES_PY, &zip_s, s], None);
        for l in String::from_utf8_lossy(&out).lines() {
            if !l.is_empty() {
                lines.push(l.to_string());
            }
        }
    }
    if let Some(parent) = zip.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
    lines
}

/// Fallback suite list from per-job annotations (GitHub caps 10/step) — used when the
/// artifact is absent or truncated.
pub fn red_suites_annotations(gh: &dyn Gh, proc: &dyn Proc, repo: &Path, jobs_json: &str) -> Vec<String> {
    const JOB_IDS_PY: &str = r#"
import json, sys
try:
    for j in json.load(sys.stdin).get('jobs', []):
        print(j['id'])
except Exception:
    pass
"#;
    const ANNOT_PY: &str = r#"
import json, sys
try:
    for a in json.load(sys.stdin):
        lvl = a.get('annotation_level', '')
        title = a.get('title', '')
        msg = a.get('message', '')
        path = a.get('path', '')
        if lvl == 'warning' and title == 'flaky suite':
            idx = msg.find(' was red')
            if idx > 0:
                print('flaky: ' + msg[:idx])
        elif lvl == 'error' and title == 'red-twice suite':
            print('red-suite: ' + msg)
        elif lvl == 'failure' and path.startswith('spira/test-') and path.endswith('.sh'):
            print('red-suite: ' + path.split('/')[-1])
except Exception:
    pass
"#;
    let (_, ids_out) = proc.run("python3", &["-c", JOB_IDS_PY], Some(jobs_json.as_bytes()));
    let mut lines = Vec::new();
    for jid in String::from_utf8_lossy(&ids_out).lines() {
        let jid = jid.trim();
        if jid.is_empty() {
            continue;
        }
        let annot = gh.call(Some(repo), &["api", &format!("repos/{{owner}}/{{repo}}/check-runs/{jid}/annotations")]);
        if !annot.ok() {
            continue;
        }
        let (_, out) = proc.run("python3", &["-c", ANNOT_PY], Some(&annot.stdout));
        for l in String::from_utf8_lossy(&out).lines() {
            if !l.is_empty() {
                lines.push(l.to_string());
            }
        }
    }
    lines
}
