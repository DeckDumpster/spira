//! `spira-lc admin-store-gc [--db NAME]...`: an online `DOLT_GC()` of each live database, as the
//! administrator, with the store's size and the mount's free space before and after.

use std::path::{Path, PathBuf};

use crate::db::{Conn, GC_IO_TIMEOUT};
use crate::CANNOT_TELL;

pub const USAGE: &str = "usage: spira-lc admin-store-gc [--db NAME]...

Runs CALL DOLT_GC() on each live database and prints, per database, the .dolt/noms bytes and
the free bytes on the store mount before and after.

  --db NAME   collect only NAME (repeatable); default every database the server lists
              except information_schema and mysql
  -h          this text

exit 0 every database collected, 1 any failed, 2 usage or configuration";

const SYSTEM_DATABASES: [&str; 2] = ["information_schema", "mysql"];

/// What the collection needs of the server and the store's mount.
pub trait StoreGc {
    fn databases(&self) -> Result<Vec<String>, String>;
    fn noms_bytes(&self, db: &str) -> u64;
    fn free_bytes(&self) -> Result<u64, String>;
    fn gc(&self, db: &str) -> Result<(), String>;
}

fn plain_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse(args: &[String]) -> Result<Option<Vec<String>>, String> {
    let mut dbs = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--db" => match it.next() {
                Some(v) if plain_name(v) => dbs.push(v.clone()),
                Some(v) => return Err(format!("{v:?} is not a plain database name")),
                None => return Err("--db needs a database name".into()),
            },
            bare if bare.starts_with('-') => return Err(format!("unknown flag {bare}; the flag is --db")),
            bare => return Err(format!("{bare:?} is a bare word; name it: --db {bare}")),
        }
    }
    Ok(Some(dbs))
}

pub fn run(args: &[String], port: &dyn StoreGc) -> (i32, String) {
    let named = match parse(args) {
        Ok(Some(n)) => n,
        Ok(None) => return (0, USAGE.to_string()),
        Err(e) => return (CANNOT_TELL, format!("admin-store-gc: {e}\n{USAGE}")),
    };
    let dbs = if named.is_empty() {
        match port.databases() {
            Ok(all) => all.into_iter().filter(|d| !SYSTEM_DATABASES.contains(&d.as_str())).collect(),
            Err(e) => return (CANNOT_TELL, format!("admin-store-gc: listing databases: {e}")),
        }
    } else {
        named
    };
    let mut lines = Vec::new();
    let mut failed = false;
    for db in &dbs {
        let (noms0, free0) = (port.noms_bytes(db), port.free_bytes());
        match port.gc(db) {
            Ok(()) => {
                let (noms1, free1) = (port.noms_bytes(db), port.free_bytes());
                let free = |f: Result<u64, String>| f.map_or_else(|e| format!("unreadable ({e})"), |b| format!("{b} B"));
                lines.push(format!("{db}: collected, noms {noms0} B -> {noms1} B; mount free {} -> {}", free(free0), free(free1)));
            }
            Err(e) => {
                failed = true;
                lines.push(format!("{db}: collection failed: {e}"));
            }
        }
    }
    (i32::from(failed), lines.join("\n"))
}

fn tree_bytes(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => tree_bytes(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

struct Live {
    admin: Conn,
    data: PathBuf,
}

impl StoreGc for Live {
    fn databases(&self) -> Result<Vec<String>, String> {
        let rows = self.admin.query("SELECT SCHEMA_NAME AS name FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME").map_err(|e| format!("{e:?}"))?;
        Ok(rows.iter().filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect())
    }

    fn noms_bytes(&self, db: &str) -> u64 {
        tree_bytes(&self.data.join(db).join(".dolt/noms"))
    }

    fn free_bytes(&self) -> Result<u64, String> {
        let c = std::ffi::CString::new(self.data.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return Err(format!("cannot stat {}: {}", self.data.display(), std::io::Error::last_os_error()));
        }
        Ok(st.f_bavail as u64 * st.f_frsize as u64)
    }

    fn gc(&self, db: &str) -> Result<(), String> {
        if !plain_name(db) {
            return Err(format!("{db:?} is not a plain database name"));
        }
        self.admin.execute(&format!("USE `{db}`; CALL DOLT_GC()")).map_err(|e| format!("{e:?}"))
    }
}

/// The verb on the real server, from the environment `admin-drop-lifecycle` reads.
pub fn admin_store_gc(args: &[String]) -> (i32, String) {
    if matches!(parse(args), Ok(None)) {
        return (0, USAGE.to_string());
    }
    let cannot = |why: String| (CANNOT_TELL, format!("admin-store-gc: {why}"));
    let get = |k: &str| std::env::var(k).ok();
    let (user, password) = match crate::admin_drop::administrator(get, crate::admin_drop::provisioned) {
        Ok(c) => c,
        Err(e) => return cannot(e),
    };
    let setting = |k: &str| spira_config::process::cfg(k);
    let (host, port, data) = match (setting("SPIRA_BEADS_SQL_HOST"), setting("SPIRA_BEADS_SQL_PORT"), setting("SPIRA_DOLT_DATA")) {
        (Ok(h), Ok(p), Ok(d)) if !d.is_empty() => match p.trim().parse::<u16>() {
            Ok(p) => (h, p, d),
            Err(e) => return cannot(format!("SPIRA_BEADS_SQL_PORT {p:?}: {e}")),
        },
        (Ok(_), Ok(_), Ok(_)) => return cannot("SPIRA_DOLT_DATA is empty: no store is served from a data directory".into()),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return cannot(e),
    };
    let mut admin = Conn::administrator(&host, port, &user, password);
    admin.io_timeout = GC_IO_TIMEOUT;
    run(args, &Live { admin, data: PathBuf::from(data) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    struct FakeStoreGc {
        dbs: Vec<&'static str>,
        failing: BTreeMap<&'static str, &'static str>,
        collected: RefCell<Vec<String>>,
    }

    impl FakeStoreGc {
        fn new(dbs: &[&'static str]) -> Self {
            FakeStoreGc { dbs: dbs.to_vec(), failing: BTreeMap::new(), collected: RefCell::new(Vec::new()) }
        }
    }

    impl StoreGc for FakeStoreGc {
        fn databases(&self) -> Result<Vec<String>, String> {
            Ok(self.dbs.iter().map(|d| d.to_string()).collect())
        }
        fn noms_bytes(&self, db: &str) -> u64 {
            if self.collected.borrow().iter().any(|c| c == db) { 100 } else { 900 }
        }
        fn free_bytes(&self) -> Result<u64, String> {
            Ok(5_000 + 800 * self.collected.borrow().len() as u64)
        }
        fn gc(&self, db: &str) -> Result<(), String> {
            if let Some(e) = self.failing.get(db) {
                return Err(e.to_string());
            }
            self.collected.borrow_mut().push(db.to_string());
            Ok(())
        }
    }

    #[test]
    fn the_noms_size_counts_every_file_under_nested_directories() {
        let d = testkit::TempDir::new("store-gc-noms");
        let noms = d.path().join("store_a/.dolt/noms");
        std::fs::create_dir_all(noms.join("oldgen")).unwrap();
        std::fs::write(noms.join("a"), [0u8; 10]).unwrap();
        std::fs::write(noms.join("oldgen/b"), [0u8; 5]).unwrap();
        let live = Live { admin: Conn::unreachable(), data: d.path().to_path_buf() };
        assert_eq!(live.noms_bytes("store_a"), 15);
        assert_eq!(live.noms_bytes("absent"), 0);
        assert!(live.free_bytes().unwrap() > 0);
        let gone = Live { admin: Conn::unreachable(), data: d.path().join("nowhere") };
        assert!(gone.free_bytes().unwrap_err().contains("cannot stat"));
    }

    #[test]
    fn the_live_port_refuses_a_name_that_is_not_plain_before_it_connects() {
        let live = Live { admin: Conn::unreachable(), data: PathBuf::new() };
        assert!(live.gc("a`; DROP DATABASE b; --").unwrap_err().contains("not a plain database name"));
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_collected_database_reports_noms_and_mount_free_before_and_after() {
        let fake = FakeStoreGc::new(&["beads", "information_schema", "mysql"]);
        let (code, out) = run(&[], &fake);
        assert_eq!(code, 0);
        assert_eq!(out, "beads: collected, noms 900 B -> 100 B; mount free 5000 B -> 5800 B");
    }

    #[test]
    fn a_failed_collection_exits_one_naming_dolts_error_and_the_rest_still_run() {
        let mut fake = FakeStoreGc::new(&["a", "b"]);
        fake.failing.insert("a", "dolt_gc: no space left");
        let (code, out) = run(&[], &fake);
        assert_eq!(code, 1);
        assert!(out.contains("a: collection failed: dolt_gc: no space left"), "{out}");
        assert!(out.contains("b: collected"), "{out}");
    }

    #[test]
    fn db_limits_the_run_to_the_named_databases() {
        let fake = FakeStoreGc::new(&["a", "b", "c"]);
        let (code, out) = run(&args(&["--db", "b", "--db", "c"]), &fake);
        assert_eq!(code, 0);
        assert_eq!(*fake.collected.borrow(), ["b", "c"]);
        assert!(!out.contains("a:"), "{out}");
    }

    #[test]
    fn a_bare_positional_is_refused_naming_db() {
        let fake = FakeStoreGc::new(&["a"]);
        let (code, out) = run(&args(&["a"]), &fake);
        assert_eq!(code, CANNOT_TELL);
        assert!(out.contains("--db a"), "{out}");
        assert!(fake.collected.borrow().is_empty());
    }

    #[test]
    fn a_database_name_that_is_not_plain_is_refused() {
        let fake = FakeStoreGc::new(&[]);
        assert_eq!(run(&args(&["--db", "a`b"]), &fake).0, CANNOT_TELL);
    }
}
