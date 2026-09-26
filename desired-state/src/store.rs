//! The IO seam: turns a [`Composite`] into a durable, versioned artifact on disk. A new
//! version is written only when [`Composite::content_hash`] changes; every version keeps the
//! provenance of the fragments that produced it.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::compose::{ComposedResource, Composite};
use crate::producer::Provenance;
use crate::resource::{parse_resource, Metadata, RawResource};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredResource {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    metadata: Metadata,
    spec: toml::Value,
    producer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMeta {
    version: u64,
    content_hash: String,
    materialized_at: String,
    #[serde(default)]
    contributor: Vec<Provenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredDocument {
    meta: StoredMeta,
    #[serde(default)]
    resource: Vec<StoredResource>,
}

#[derive(Debug, Clone)]
pub struct StoredVersion {
    pub version: u64,
    pub content_hash: String,
    pub path: PathBuf,
}

struct FoundDocument {
    version: u64,
    path: PathBuf,
    doc: StoredDocument,
}

/// Where a host's composite and version history live absent `$SPIRA_DESIRED_DIR` —
/// `${XDG_CONFIG_HOME:-$HOME/.config}/spira/desired`. The one place this path is computed, so
/// `spira compose`/`spira apply` and every reader of the materialised composite agree on it.
pub fn default_dir() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    std::env::var("SPIRA_DESIRED_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| config_home.join("spira").join("desired"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializeOutcome {
    Unchanged { version: u64 },
    NewVersion { version: u64 },
}

/// The host's composite and its version history, rooted at one directory —
/// `${XDG_CONFIG_HOME:-$HOME/.config}/spira/desired` on a real host, a tempdir in tests.
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    fn current_pointer(&self) -> PathBuf {
        self.root.join("current")
    }

    fn version_path(&self, version: u64) -> PathBuf {
        self.versions_dir().join(format!("{version:06}.toml"))
    }

    pub fn latest(&self) -> Result<Option<StoredVersion>, String> {
        let found = match self.latest_document()? {
            Some(f) => f,
            None => return Ok(None),
        };
        Ok(Some(StoredVersion {
            version: found.version,
            content_hash: found.doc.meta.content_hash,
            path: found.path,
        }))
    }

    /// The composite this host last materialised — `None` before the first `spira apply` or
    /// `spira compose` — reparsed against today's resource schema, so a reader on a newer
    /// binary than the writer sees the same refusal `compose` would give a fresh spec
    /// (law-fail-closed-at-the-source): a producer this host still runs consults the same
    /// document a fresh compose would, not a copy it can drift from.
    pub fn read_current(&self) -> Result<Option<Composite>, String> {
        let found = match self.latest_document()? {
            Some(f) => f,
            None => return Ok(None),
        };
        let mut resources = BTreeMap::new();
        for r in found.doc.resource {
            let raw = RawResource {
                api_version: r.api_version,
                kind: r.kind,
                metadata: r.metadata,
                spec: r.spec,
            };
            parse_resource(&raw).map_err(|e| format!("{}: {e}", found.path.display()))?;
            let key = (raw.kind.clone(), raw.metadata.name.clone());
            resources.insert(key, ComposedResource { raw, producer: r.producer });
        }
        Ok(Some(Composite { resources }))
    }

    fn latest_document(&self) -> Result<Option<FoundDocument>, String> {
        let pointer = self.current_pointer();
        let text = match fs::read_to_string(&pointer) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", pointer.display())),
        };
        let version: u64 = text
            .trim()
            .parse()
            .map_err(|e| format!("{}: {e}", pointer.display()))?;
        let path = self.version_path(version);
        let doc_text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc: StoredDocument =
            toml::from_str(&doc_text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(FoundDocument { version, path, doc }))
    }

    /// Writes `composite` as the next version if its content differs from the current one.
    /// `contributors` is the provenance of every fragment that fed the compose, recorded on
    /// this version even though it plays no part in [`Composite::content_hash`].
    pub fn write_version(
        &self,
        composite: &Composite,
        contributors: &[Provenance],
        materialized_at: &str,
    ) -> Result<MaterializeOutcome, String> {
        let content_hash = composite.content_hash();
        let latest = self.latest()?;
        if let Some(latest) = &latest {
            if latest.content_hash == content_hash {
                return Ok(MaterializeOutcome::Unchanged { version: latest.version });
            }
        }
        let next_version = latest.map(|v| v.version + 1).unwrap_or(1);

        let resource = composite
            .resources
            .values()
            .map(|r| StoredResource {
                api_version: r.raw.api_version.clone(),
                kind: r.raw.kind.clone(),
                metadata: r.raw.metadata.clone(),
                spec: r.raw.spec.clone(),
                producer: r.producer.clone(),
            })
            .collect();
        let doc = StoredDocument {
            meta: StoredMeta {
                version: next_version,
                content_hash,
                materialized_at: materialized_at.to_string(),
                contributor: contributors.to_vec(),
            },
            resource,
        };
        let text = toml::to_string_pretty(&doc).map_err(|e| e.to_string())?;

        fs::create_dir_all(self.versions_dir()).map_err(|e| e.to_string())?;
        let path = self.version_path(next_version);
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;

        let tmp = self.root.join(format!(".current.new.{}", std::process::id()));
        fs::write(&tmp, format!("{next_version}\n")).map_err(|e| e.to_string())?;
        fs::rename(&tmp, self.current_pointer()).map_err(|e| e.to_string())?;

        Ok(MaterializeOutcome::NewVersion { version: next_version })
    }
}

/// The wall clock at materialization time, not a producer's own stamp — this is the store's
/// own act, recorded on the version it writes.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_rfc3339(secs)
}

/// UTC, second precision. civil_from_days (Howard Hinnant, <http://howardhinnant.github.io/date_algorithms.html>).
fn format_rfc3339(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86400) as i64;
    let secs_of_day = epoch_secs % 86400;

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    let h = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::compose;
    use crate::producer::Fragment;
    use crate::resource::{Metadata, RawResource};

    fn fleet_fragment(producer: &str, ceiling: i64) -> Fragment {
        let mut spec = toml::map::Map::new();
        spec.insert("ceiling".into(), toml::Value::Integer(ceiling));
        spec.insert("lane_cap".into(), toml::Value::Integer(2));
        Fragment {
            producer: Provenance {
                producer: producer.to_string(),
                version: "1".to_string(),
                time: "2026-09-25T00:00:00Z".to_string(),
            },
            resources: vec![RawResource {
                api_version: crate::resource::API_VERSION.to_string(),
                kind: "Fleet".to_string(),
                metadata: Metadata { name: "fleet".to_string() },
                spec: toml::Value::Table(spec),
            }],
        }
    }

    #[test]
    fn known_epoch_formats_correctly() {
        // 2026-09-25T00:00:00Z, cross-checked with `date -u -d @1790294400`.
        assert_eq!(format_rfc3339(1790294400), "2026-09-25T00:00:00Z");
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn first_write_creates_version_one() {
        let dir = tempdir();
        let store = FsStore::new(dir.path());
        let composite = compose(&[fleet_fragment("operator", 8)]).unwrap();
        let outcome = store
            .write_version(&composite, &[], "2026-09-25T00:00:00Z")
            .unwrap();
        assert_eq!(outcome, MaterializeOutcome::NewVersion { version: 1 });
    }

    #[test]
    fn read_current_with_nothing_materialised_is_none() {
        let dir = tempdir();
        let store = FsStore::new(dir.path());
        assert!(store.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_round_trips_the_written_composite() {
        let dir = tempdir();
        let store = FsStore::new(dir.path());
        let composite = compose(&[fleet_fragment("operator", 8)]).unwrap();
        store
            .write_version(&composite, &[], "2026-09-25T00:00:00Z")
            .unwrap();

        let read_back = store.read_current().unwrap().expect("a composite was written");
        let key = ("Fleet".to_string(), "fleet".to_string());
        let resource = &read_back.resources[&key];
        assert_eq!(resource.producer, "operator");
        assert_eq!(read_back.content_hash(), composite.content_hash());
    }

    #[test]
    fn unchanged_content_does_not_advance_the_version() {
        let dir = tempdir();
        let store = FsStore::new(dir.path());
        let composite = compose(&[fleet_fragment("operator", 8)]).unwrap();
        store
            .write_version(&composite, &[], "2026-09-25T00:00:00Z")
            .unwrap();

        let same_content = compose(&[fleet_fragment("operator-restarted", 8)]).unwrap();
        let outcome = store
            .write_version(&same_content, &[], "2026-09-25T01:00:00Z")
            .unwrap();
        assert_eq!(outcome, MaterializeOutcome::Unchanged { version: 1 });
    }

    #[test]
    fn changed_content_advances_the_version_and_keeps_history() {
        let dir = tempdir();
        let store = FsStore::new(dir.path());
        let v1 = compose(&[fleet_fragment("operator", 8)]).unwrap();
        store.write_version(&v1, &[], "2026-09-25T00:00:00Z").unwrap();

        let v2 = compose(&[fleet_fragment("operator", 9)]).unwrap();
        let outcome = store.write_version(&v2, &[], "2026-09-25T01:00:00Z").unwrap();
        assert_eq!(outcome, MaterializeOutcome::NewVersion { version: 2 });

        assert!(dir.path().join("versions").join("000001.toml").exists());
        assert!(dir.path().join("versions").join("000002.toml").exists());
        assert_eq!(store.latest().unwrap().unwrap().version, 2);
    }

    // A minimal tempdir helper — no external crate, cleaned up on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "spira-desired-state-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}
