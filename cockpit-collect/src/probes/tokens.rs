//! The pane's token spend, counted in this process. `tokens.sh report` is the human's full
//! read; this reads only the bytes appended to a transcript since the last pass.
//!
//! Buckets, usage fields and the once-per-message-id rule are `tokens.sh`'s: a turn is
//! billed to the highest-precedence input that carries it (dwarf log, worktree transcript,
//! archivist transcript, session transcript).

use super::{push, Kv};
use crate::quoting::parse_iso8601;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

pub const KEYS: &[&str] = &[
    "SP_TOK_WINDOW_H", "SP_TOK_DWARF_WIN", "SP_TOK_ARC_WIN", "SP_TOK_SESS_WIN", "SP_TOK_WIN",
    "SP_TOK_DWARF_TURNS", "SP_TOK_ARC_TURNS", "SP_TOK_SESS_TURNS", "SP_TOK_DWARF_CTX",
    "SP_TOK_ARC_CTX", "SP_TOK_SESS_CTX", "SP_TOK_DWARF_OUT", "SP_TOK_ARC_OUT", "SP_TOK_SESS_OUT",
];

const USAGE_FIELDS: [&str; 4] = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"];

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub run: PathBuf,
    pub projects: PathBuf,
    pub window_h: f64,
}

impl Source {
    pub fn from_config() -> Result<Source, String> {
        use spira_config::process::cfg;
        let window = cfg("SPIRA_TOKEN_WINDOW_H")?;
        let window_h: f64 = window.trim().parse().map_err(|e| format!("SPIRA_TOKEN_WINDOW_H = {window:?} does not parse: {e}"))?;
        if !window_h.is_finite() || window_h < 0.0 {
            return Err(format!("SPIRA_TOKEN_WINDOW_H = {window:?} is not a window"));
        }
        Ok(Source { run: PathBuf::from(cfg("SPIRA_RUN")?), projects: PathBuf::from(cfg("SPIRA_TOKEN_PROJECTS")?), window_h })
    }
}

/// Where a turn was found, best first: the lowest rank wins a message id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    DwarfLog,
    Worktree,
    Archivist,
    Session,
}

struct Turn {
    rank: Rank,
    at: i64,
    usage: [i64; 4],
}

struct FileState {
    dev: u64,
    ino: u64,
    offset: u64,
}

#[derive(Default)]
pub struct Reader {
    source: Option<Source>,
    files: HashMap<PathBuf, FileState>,
    turns: HashMap<String, Turn>,
    /// Bytes read from transcripts over this reader's life; what a test counts.
    pub bytes_read: u64,
}

#[derive(Default, Clone, Copy)]
struct Sum {
    turns: i64,
    usage: [i64; 4],
}

impl Sum {
    fn add(&mut self, t: &Turn) {
        self.turns += 1;
        for (s, u) in self.usage.iter_mut().zip(t.usage) {
            *s += u;
        }
    }
    fn billed(&self) -> i64 {
        self.usage.iter().sum()
    }
    fn ctx(&self) -> i64 {
        if self.turns == 0 { 0 } else { self.usage[2] / self.turns }
    }
    fn out(&self) -> i64 {
        self.usage[3]
    }
}

fn encoded(p: &Path) -> String {
    let mut norm = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                norm.pop();
            }
            c => norm.push(c),
        }
    }
    norm.to_string_lossy().replace(['/', '.'], "-")
}

fn sorted_dir(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    v.sort();
    Ok(v)
}

impl Reader {
    pub fn new() -> Reader {
        Reader::default()
    }

    /// One pass: read what grew, drop what left the window, return the `SP_TOK_*` values.
    pub fn pass(&mut self, src: &Source, now: i64) -> Result<Kv, String> {
        if self.source.as_ref() != Some(src) {
            *self = Reader { source: Some(src.clone()), ..Reader::default() };
        }
        let cut = now - (src.window_h * 3600.0) as i64;
        let inputs = self.inputs(src)?;
        let live: std::collections::HashSet<&PathBuf> = inputs.iter().map(|(p, _)| p).collect();
        self.files.retain(|p, _| live.contains(p));
        for (path, rank) in &inputs {
            self.read_new(path, *rank, cut);
        }
        self.turns.retain(|_, t| t.at >= cut);

        let (mut dwarf, mut arc, mut sess) = (Sum::default(), Sum::default(), Sum::default());
        for t in self.turns.values() {
            match t.rank {
                Rank::DwarfLog | Rank::Worktree => dwarf.add(t),
                Rank::Archivist => arc.add(t),
                Rank::Session => sess.add(t),
            }
        }
        let mut out = Kv::new();
        push(&mut out, "SP_TOK_WINDOW_H", (src.window_h as i64).to_string());
        for (name, s) in [("DWARF", dwarf), ("ARC", arc), ("SESS", sess)] {
            push(&mut out, &format!("SP_TOK_{name}_WIN"), s.billed().to_string());
            push(&mut out, &format!("SP_TOK_{name}_TURNS"), s.turns.to_string());
            push(&mut out, &format!("SP_TOK_{name}_CTX"), s.ctx().to_string());
            push(&mut out, &format!("SP_TOK_{name}_OUT"), s.out().to_string());
        }
        push(&mut out, "SP_TOK_WIN", (dwarf.billed() + arc.billed() + sess.billed()).to_string());
        Ok(out)
    }

    fn inputs(&self, src: &Source) -> Result<Vec<(PathBuf, Rank)>, String> {
        let wt_prefix = format!("{}-", encoded(&src.run.join("worktree")));
        let arc_name = encoded(&src.run.join("archivist").join("cwd"));
        let mut v = Vec::new();
        for p in sorted_dir(&src.run).map_err(|e| format!("{}: {e}", src.run.display()))? {
            if p.extension().is_some_and(|e| e == "log") && p.is_file() {
                v.push((p, Rank::DwarfLog));
            }
        }
        let mut transcripts: Vec<(PathBuf, Rank)> = Vec::new();
        for dir in sorted_dir(&src.projects).map_err(|e| format!("{}: {e}", src.projects.display()))? {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let rank = if name.starts_with(&wt_prefix) {
                Rank::Worktree
            } else if name == arc_name {
                Rank::Archivist
            } else {
                Rank::Session
            };
            let Ok(files) = sorted_dir(&dir) else { continue };
            transcripts.extend(files.into_iter().filter(|f| f.extension().is_some_and(|e| e == "jsonl")).map(|f| (f, rank)));
        }
        transcripts.sort_by_key(|(_, r)| *r);
        v.extend(transcripts);
        Ok(v)
    }

    fn read_new(&mut self, path: &Path, rank: Rank, cut: i64) {
        let Ok(meta) = std::fs::metadata(path) else { return };
        let mtime = meta.mtime();
        if mtime < cut {
            return;
        }
        let state = self.files.entry(path.to_path_buf()).or_insert(FileState { dev: meta.dev(), ino: meta.ino(), offset: 0 });
        if (state.dev, state.ino) != (meta.dev(), meta.ino()) || meta.len() < state.offset {
            *state = FileState { dev: meta.dev(), ino: meta.ino(), offset: 0 };
        }
        if meta.len() == state.offset {
            return;
        }
        let Ok(mut f) = std::fs::File::open(path) else { return };
        if f.seek(SeekFrom::Start(state.offset)).is_err() {
            return;
        }
        let assistant_only = rank == Rank::DwarfLog;
        let mut reader = std::io::BufReader::new(f.take(meta.len() - state.offset));
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            self.bytes_read += n as u64;
            if line.last() != Some(&b'\n') {
                break;
            }
            state.offset += n as u64;
            if let Some((id, turn)) = parse_turn(&line, rank, assistant_only) {
                if turn.at >= cut && self.turns.get(&id).is_none_or(|t| turn.rank < t.rank) {
                    self.turns.insert(id, turn);
                }
            }
        }
    }
}

fn parse_turn(line: &[u8], rank: Rank, assistant_only: bool) -> Option<(String, Turn)> {
    if !line.windows(7).any(|w| w == b"\"usage\"") {
        return None;
    }
    let o: Value = serde_json::from_slice(line.trim_ascii()).ok()?;
    if assistant_only && o.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let m = o.get("message")?.as_object()?;
    let u = m.get("usage")?.as_object().filter(|u| !u.is_empty())?;
    let id = m.get("id")?.as_str().filter(|s| !s.is_empty())?;
    let at = parse_iso8601(o.get("timestamp")?.as_str()?)?;
    let mut usage = [0i64; 4];
    for (slot, k) in usage.iter_mut().zip(USAGE_FIELDS) {
        *slot = u.get(k).and_then(Value::as_i64).unwrap_or(0);
    }
    Some((id.to_string(), Turn { rank, at, usage }))
}

static HELD: Mutex<Option<Reader>> = Mutex::new(None);

/// This process's reader, advanced one pass; `?` for every key when the source cannot be read.
pub fn section(out: &mut Kv, source: Result<Source, String>, now: i64) {
    section_in(&HELD, out, source, now);
}

fn section_in(held: &Mutex<Option<Reader>>, out: &mut Kv, source: Result<Source, String>, now: i64) {
    let kv = source.and_then(|src| held.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Reader::new).pass(&src, now));
    match kv {
        Ok(kv) => out.extend(kv),
        Err(_) => {
            for k in KEYS {
                push(out, k, "?");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NOW: i64 = 1_800_000_000; // 2027-01-15T08:00:00Z
    const IN_WINDOW: &str = "2027-01-15T06:00:00Z";
    const IN_WINDOW_LATER: &str = "2027-01-15T07:00:00Z";
    const OUTSIDE: &str = "2027-01-14T20:00:00Z";

    fn turn(kind: &str, id: &str, ts: &str, u: [i64; 4]) -> String {
        format!(
            "{{\"type\":\"{kind}\",\"timestamp\":\"{ts}\",\"message\":{{\"id\":\"{id}\",\"usage\":{{\"input_tokens\":{},\"cache_creation_input_tokens\":{},\"cache_read_input_tokens\":{},\"output_tokens\":{}}}}}}}\n",
            u[0], u[1], u[2], u[3]
        )
    }

    fn touch(p: &Path) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW as u64 - 60)).unwrap();
    }

    fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
        touch(p);
    }

    fn append(p: &Path, text: &str) {
        std::fs::File::options().append(true).open(p).unwrap().write_all(text.as_bytes()).unwrap();
        touch(p);
    }

    struct World {
        _dir: testkit::TempDir,
        src: Source,
        log: PathBuf,
        wt: PathBuf,
        arc: PathBuf,
        sess: PathBuf,
    }

    fn world() -> World {
        let dir = testkit::TempDir::new("cockpit-tokens");
        let run = dir.path().join("run");
        let projects = dir.path().join("projects");
        let enc = |p: &Path| p.to_str().unwrap().replace(['/', '.'], "-");
        let log = run.join("a.log");
        let wt = projects.join(format!("{}-x", enc(&run.join("worktree")))).join("s1.jsonl");
        let arc = projects.join(enc(&run.join("archivist").join("cwd"))).join("a.jsonl");
        let sess = projects.join("-home-me").join("s.jsonl");
        write(
            &log,
            &(turn("assistant", "m1", IN_WINDOW, [10, 20, 30, 40])
                + &turn("user", "m-user", IN_WINDOW, [9, 9, 9, 9])
                + &turn("assistant", "m-old", OUTSIDE, [7, 7, 7, 7])
                + &turn("assistant", "m-shared", IN_WINDOW, [100, 0, 0, 0])),
        );
        write(&wt, &(turn("assistant", "m-shared", IN_WINDOW, [1, 1, 1, 1]) + &turn("user", "m2", IN_WINDOW, [1, 2, 3, 4])));
        write(&arc, &turn("assistant", "m3", IN_WINDOW, [5, 5, 5, 5]));
        write(&sess, &(turn("assistant", "m4", IN_WINDOW, [100, 0, 0, 0]) + &turn("assistant", "m5", IN_WINDOW, [0, 0, 200, 0])));
        std::fs::create_dir_all(&run).unwrap();
        World { src: Source { run, projects, window_h: 5.0 }, _dir: dir, log, wt, arc, sess }
    }

    fn get(kv: &Kv, k: &str) -> String {
        kv.iter().find(|(n, _)| n == k).unwrap_or_else(|| panic!("{k} missing")).1.clone()
    }

    #[test_id(ttb89oh)]
    #[test]
    fn a_turn_is_billed_once_to_the_best_input_and_buckets_sum_as_tokens_sh_did() {
        let w = world();
        let kv = Reader::new().pass(&w.src, NOW).unwrap();
        let want = [
            ("SP_TOK_WINDOW_H", "5"),
            ("SP_TOK_DWARF_TURNS", "3"), ("SP_TOK_DWARF_WIN", "210"), ("SP_TOK_DWARF_CTX", "11"), ("SP_TOK_DWARF_OUT", "44"),
            ("SP_TOK_ARC_TURNS", "1"), ("SP_TOK_ARC_WIN", "20"), ("SP_TOK_ARC_CTX", "5"), ("SP_TOK_ARC_OUT", "5"),
            ("SP_TOK_SESS_TURNS", "2"), ("SP_TOK_SESS_WIN", "300"), ("SP_TOK_SESS_CTX", "100"), ("SP_TOK_SESS_OUT", "0"),
            ("SP_TOK_WIN", "530"),
        ];
        for (k, v) in want {
            assert_eq!(get(&kv, k), v, "{k}");
        }
        assert_eq!(kv.len(), KEYS.len());
    }

    #[test_id(tswmirp)]
    #[test]
    fn a_second_pass_reads_only_the_appended_bytes_and_nothing_when_nothing_grew() {
        let w = world();
        let mut r = Reader::new();
        r.pass(&w.src, NOW).unwrap();
        let first = r.bytes_read;
        assert!(first > 0);
        r.pass(&w.src, NOW).unwrap();
        assert_eq!(r.bytes_read, first, "nothing grew, nothing read");

        let added = turn("assistant", "m6", IN_WINDOW_LATER, [0, 0, 0, 1000]);
        append(&w.sess, &added);
        let kv = r.pass(&w.src, NOW).unwrap();
        assert_eq!(r.bytes_read, first + added.len() as u64);
        assert_eq!(get(&kv, "SP_TOK_SESS_TURNS"), "3");
        assert_eq!(get(&kv, "SP_TOK_SESS_OUT"), "1000");
    }

    #[test_id(t7j0cdd)]
    #[test]
    fn a_shorter_or_replaced_file_is_forgotten_and_read_again() {
        let w = world();
        let mut r = Reader::new();
        r.pass(&w.src, NOW).unwrap();
        let kept = turn("assistant", "m4", IN_WINDOW, [100, 0, 0, 0]);
        let shorter = std::fs::File::options().write(true).open(&w.sess).unwrap();
        shorter.set_len(kept.len() as u64).unwrap();
        touch(&w.sess);
        let before = r.bytes_read;
        let kv = r.pass(&w.src, NOW).unwrap();
        assert_eq!(r.bytes_read, before + kept.len() as u64, "a shorter file is read from its start");
        assert_eq!(get(&kv, "SP_TOK_SESS_TURNS"), "2", "m5 stays counted: ids are the turns' identity, not the file's");
        let before = r.bytes_read;
        let longer = turn("assistant", "m7", IN_WINDOW, [3, 0, 0, 0]) + &turn("assistant", "m8", IN_WINDOW, [4, 0, 0, 0]) + &turn("assistant", "m4", IN_WINDOW, [100, 0, 0, 0]);
        let tmp = w.sess.with_extension("new");
        write(&tmp, &longer);
        std::fs::rename(&tmp, &w.sess).unwrap();
        let kv = r.pass(&w.src, NOW).unwrap();
        assert_eq!(r.bytes_read, before + longer.len() as u64, "a replaced file is read from its start");
        assert_eq!(get(&kv, "SP_TOK_SESS_TURNS"), "4");
    }

    #[test_id(tpf8py9)]
    #[test]
    fn a_half_written_last_line_waits_for_its_newline() {
        let w = world();
        let mut r = Reader::new();
        r.pass(&w.src, NOW).unwrap();
        let full = turn("assistant", "m9", IN_WINDOW_LATER, [0, 0, 0, 77]);
        let (head, tail) = full.split_at(full.len() / 2);
        append(&w.sess, head);
        let kv = r.pass(&w.src, NOW).unwrap();
        assert_eq!(get(&kv, "SP_TOK_SESS_TURNS"), "2");
        append(&w.sess, tail);
        let kv = r.pass(&w.src, NOW).unwrap();
        assert_eq!(get(&kv, "SP_TOK_SESS_TURNS"), "3");
        assert_eq!(get(&kv, "SP_TOK_SESS_OUT"), "77");
    }

    #[test_id(twqx3hg)]
    #[test]
    fn a_file_untouched_since_the_cutoff_is_not_opened() {
        let w = world();
        let stale = w.src.projects.join("-home-me").join("old.jsonl");
        write(&stale, &turn("assistant", "m-stale", IN_WINDOW, [1, 1, 1, 1]));
        let f = std::fs::File::options().write(true).open(&stale).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW as u64 - 6 * 3600)).unwrap();
        let mut r = Reader::new();
        r.pass(&w.src, NOW).unwrap();
        let fresh_only = Reader::new().pass(&w.src, NOW).unwrap();
        let mut with_stale_counted = Reader::new();
        with_stale_counted.pass(&w.src, NOW - 3 * 3600).unwrap();
        assert_eq!(get(&fresh_only, "SP_TOK_SESS_TURNS"), "2", "m-stale is in a file the window cannot touch");
        assert!(with_stale_counted.bytes_read > r.bytes_read, "a cutoff before the file's mtime reads it");
    }

    #[test_id(tlcbx91)]
    #[test]
    fn turns_older_than_the_window_drop_and_an_empty_window_has_zero_context() {
        let w = world();
        let mut r = Reader::new();
        r.pass(&w.src, NOW).unwrap();
        let kv = r.pass(&w.src, NOW + 6 * 3600).unwrap();
        assert_eq!(get(&kv, "SP_TOK_WIN"), "0");
        assert_eq!(get(&kv, "SP_TOK_DWARF_CTX"), "0");
        assert_eq!(get(&kv, "SP_TOK_SESS_TURNS"), "0");
        let _ = (&w.log, &w.wt, &w.arc);
    }

    #[test_id(tfzm39b)]
    #[test]
    fn an_unreadable_directory_or_key_answers_question_marks_for_every_key() {
        let w = world();
        let held = Mutex::new(None);
        for source in [
            Ok(Source { projects: w.src.projects.join("absent"), ..w.src.clone() }),
            Ok(Source { run: w.src.run.join("absent"), ..w.src.clone() }),
            Err("SPIRA_TOKEN_WINDOW_H is not a registered config key".to_string()),
        ] {
            let mut out = Kv::new();
            section_in(&held, &mut out, source, NOW);
            assert_eq!(out.len(), KEYS.len());
            assert!(out.iter().all(|(k, v)| v == "?" && KEYS.contains(&k.as_str())), "{out:?}");
        }
    }

    #[test_id(talgd9t)]
    #[test]
    fn the_section_starts_no_tokens_sh_however_many_passes_run() {
        let w = world();
        let bin = w._dir.path().join("bin");
        let marker = w._dir.path().join("started");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join("tokens.sh");
        testkit::write_exe(&script, &format!("#!/bin/sh\necho started >> {}\nexit 1\n", marker.display()));
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let _env = testkit::env(&[("PATH", Some(&path))]);
        let held = Mutex::new(None);
        for _ in 0..2 {
            let mut out = Kv::new();
            section_in(&held, &mut out, Ok(w.src.clone()), NOW);
            assert_eq!(get(&out, "SP_TOK_WIN"), "530");
        }
        assert!(!marker.exists(), "tokens.sh was started");
    }
}
