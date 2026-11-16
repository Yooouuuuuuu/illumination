// SPDX-License-Identifier: GPL-3.0-only
//! Trust store: pinned host fingerprints (controller side) and paired peers (host side).
//! Stored as versioned JSON (architecture §9).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const STORE_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("trust store {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("trust store {path} is corrupt: {source}")]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("trust store {path} has version {found}, this build understands {STORE_VERSION}")]
    Version { path: PathBuf, found: u32 },
    #[error("the Illumination service did not make the change: {0}")]
    Service(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHost {
    pub fingerprint: String,
    pub name: String,
    pub first_seen: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedPeer {
    pub name: String,
    pub first_seen: u64,
    pub last_address: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustData {
    version: u32,
    /// Keyed by the address as dialled, e.g. `100.100.101.1:7171`.
    known_hosts: BTreeMap<String, KnownHost>,
    /// Keyed by the peer's certificate fingerprint.
    paired_peers: BTreeMap<String, PairedPeer>,
}

/// One change to the store that a person's program makes. On an installed desk the store
/// belongs to the service, and the change is asked of it (architecture §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    PinHost {
        address: String,
        fingerprint: String,
        name: String,
    },
    ForgetHost {
        address: String,
    },
    ForgetPeer {
        fingerprint: String,
    },
}

/// Makes a [`Change`] somewhere else (the installed service) and says whether anything
/// changed.
pub type Through = Arc<dyn Fn(&Change) -> Result<bool, String> + Send + Sync>;

/// One writer at a time in this process, from reading the file to replacing it.
static WRITING: Mutex<()> = Mutex::new(());

pub struct TrustStore {
    path: PathBuf,
    data: TrustData,
    /// The file's modification time when it was last read or written here.
    seen: Option<SystemTime>,
    /// Set when this process may not write the file itself.
    through: Option<Through>,
}

impl std::fmt::Debug for TrustStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustStore")
            .field("path", &self.path)
            .field("data", &self.data)
            .field("through", &self.through.is_some())
            .finish()
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl TrustStore {
    /// Loads the store, or starts an empty one if the file doesn't exist yet.
    pub fn load(path: impl Into<PathBuf>) -> Result<TrustStore, StoreError> {
        let path = path.into();
        let data = match std::fs::read(&path) {
            Ok(bytes) => {
                let data: TrustData =
                    serde_json::from_slice(&bytes).map_err(|source| StoreError::Corrupt {
                        path: path.clone(),
                        source,
                    })?;
                if data.version != STORE_VERSION {
                    return Err(StoreError::Version {
                        path,
                        found: data.version,
                    });
                }
                data
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TrustData {
                version: STORE_VERSION,
                ..TrustData::default()
            },
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        let seen = modified(&path);
        Ok(TrustStore {
            path,
            data,
            seen,
            through: None,
        })
    }

    /// Takes up what another process wrote since this one last looked: the app and the
    /// installed host share the file. A file that cannot be read leaves what is known as it is.
    pub fn refresh(&mut self) {
        if modified(&self.path) != self.seen {
            self.reload();
        }
    }

    fn reload(&mut self) {
        if let Ok(fresh) = TrustStore::load(self.path.clone()) {
            self.data = fresh.data;
            self.seen = fresh.seen;
        }
    }

    /// From here on this store's changes of [`Change`] are made by `through`, not written
    /// here.
    pub fn write_through(&mut self, through: Through) {
        self.through = Some(through);
    }

    /// Makes a change: through the service if this store was told so, else in the file.
    fn change(&mut self, change: Change) -> Result<bool, StoreError> {
        match self.through.clone() {
            Some(through) => {
                let changed = through(&change).map_err(StoreError::Service)?;
                self.reload();
                Ok(changed)
            }
            None => self.apply(&change),
        }
    }

    /// Makes a change in the file itself. Returns whether anything changed.
    pub fn apply(&mut self, change: &Change) -> Result<bool, StoreError> {
        let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        self.refresh();
        let changed = match change {
            Change::PinHost {
                address,
                fingerprint,
                name,
            } => {
                self.data.known_hosts.insert(
                    address.clone(),
                    KnownHost {
                        fingerprint: fingerprint.clone(),
                        name: name.clone(),
                        first_seen: now(),
                    },
                );
                true
            }
            Change::ForgetHost { address } => self.data.known_hosts.remove(address).is_some(),
            Change::ForgetPeer { fingerprint } => {
                self.data.paired_peers.remove(fingerprint).is_some()
            }
        };
        if changed {
            self.save()?;
        }
        Ok(changed)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn known_host(&self, address: &str) -> Option<&KnownHost> {
        self.data.known_hosts.get(address)
    }

    pub fn pin_host(
        &mut self,
        address: &str,
        fingerprint: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        self.change(Change::PinHost {
            address: address.to_string(),
            fingerprint: fingerprint.to_string(),
            name: name.to_string(),
        })
        .map(|_| ())
    }

    pub fn forget_host(&mut self, address: &str) -> Result<bool, StoreError> {
        self.change(Change::ForgetHost {
            address: address.to_string(),
        })
    }

    /// Unpairs a device: its next connection has to be accepted again.
    pub fn forget_peer(&mut self, fingerprint: &str) -> Result<bool, StoreError> {
        self.change(Change::ForgetPeer {
            fingerprint: fingerprint.to_string(),
        })
    }

    pub fn paired_peer(&self, fingerprint: &str) -> Option<&PairedPeer> {
        self.data.paired_peers.get(fingerprint)
    }

    pub fn pair(&mut self, fingerprint: &str, name: &str, address: &str) -> Result<(), StoreError> {
        let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        self.refresh();
        self.data.paired_peers.insert(
            fingerprint.to_string(),
            PairedPeer {
                name: name.to_string(),
                first_seen: now(),
                last_address: address.to_string(),
            },
        );
        self.save()
    }

    /// Records where a paired peer last connected from. Not saved if nothing changed.
    pub fn touch_peer(&mut self, fingerprint: &str, address: &str) -> Result<(), StoreError> {
        let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        self.refresh();
        match self.data.paired_peers.get_mut(fingerprint) {
            Some(peer) if peer.last_address != address => {
                peer.last_address = address.to_string();
                self.save()
            }
            _ => Ok(()),
        }
    }

    pub fn paired_peers(&self) -> impl Iterator<Item = (&String, &PairedPeer)> {
        self.data.paired_peers.iter()
    }

    pub fn known_hosts(&self) -> impl Iterator<Item = (&String, &KnownHost)> {
        self.data.known_hosts.iter()
    }

    /// Writes to a temporary file and renames it over the old one.
    fn save(&mut self) -> Result<(), StoreError> {
        let io = |source| StoreError::Io {
            path: self.path.clone(),
            source,
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.data).expect("trust data always serialises");
        std::fs::write(&tmp, bytes).map_err(io)?;
        std::fs::rename(&tmp, &self.path).map_err(io)?;
        self.seen = modified(&self.path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("illum-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("trust.json")
    }

    #[test]
    fn missing_file_is_empty_store() {
        let store = TrustStore::load(temp_path("missing")).unwrap();
        assert!(store.known_host("x:1").is_none());
        assert_eq!(store.paired_peers().count(), 0);
    }

    #[test]
    fn pins_and_pairs_survive_reload() {
        let path = temp_path("reload");
        let mut store = TrustStore::load(&path).unwrap();
        store.pin_host("100.1.2.3:7171", "aa", "laptop").unwrap();
        store.pair("bb", "desktop", "100.1.2.4:5000").unwrap();
        let store = TrustStore::load(&path).unwrap();
        assert_eq!(
            store.known_host("100.1.2.3:7171").unwrap().fingerprint,
            "aa"
        );
        assert_eq!(store.paired_peer("bb").unwrap().name, "desktop");
    }

    #[test]
    fn wrong_version_is_refused() {
        let path = temp_path("version");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"version":99,"known_hosts":{},"paired_peers":{}}"#,
        )
        .unwrap();
        assert!(matches!(
            TrustStore::load(&path),
            Err(StoreError::Version { found: 99, .. })
        ));
    }

    #[test]
    fn forget_removes_pin() {
        let path = temp_path("forget");
        let mut store = TrustStore::load(&path).unwrap();
        store.pin_host("h:1", "aa", "n").unwrap();
        assert!(store.forget_host("h:1").unwrap());
        assert!(!store.forget_host("h:1").unwrap());
        assert!(TrustStore::load(&path).unwrap().known_host("h:1").is_none());
    }

    #[test]
    fn forget_removes_pairing() {
        let path = temp_path("unpair");
        let mut store = TrustStore::load(&path).unwrap();
        store.pair("ff", "laptop", "1.2.3.4").unwrap();
        assert!(store.forget_peer("ff").unwrap());
        assert!(!store.forget_peer("ff").unwrap());
        assert!(TrustStore::load(&path).unwrap().paired_peer("ff").is_none());
    }
}

#[cfg(test)]
mod through_tests {
    use super::*;

    /// A store told to write through something else never touches the file itself, and shows
    /// what the other side wrote.
    #[test]
    fn changes_go_through_and_come_back() {
        let dir = std::env::temp_dir().join(format!("illum-store-through-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("trust.json");
        // The "service": its own store on the same file.
        let service = Arc::new(Mutex::new(TrustStore::load(&path).unwrap()));
        let mut app = TrustStore::load(&path).unwrap();
        let asked = service.clone();
        app.write_through(Arc::new(move |change| {
            asked
                .lock()
                .unwrap()
                .apply(change)
                .map_err(|e| e.to_string())
        }));
        app.pin_host("h:1", "aa", "desk").unwrap();
        assert_eq!(app.known_host("h:1").unwrap().fingerprint, "aa");
        assert!(app.forget_host("h:1").unwrap());
        assert!(!app.forget_host("h:1").unwrap(), "nothing left to forget");
        service
            .lock()
            .unwrap()
            .pair("ff", "laptop", "1.2.3.4")
            .unwrap();
        assert!(app.forget_peer("ff").unwrap());
        assert!(service.lock().unwrap().paired_peer("ff").is_none());
        // A service that refuses is an error, not a silent success.
        app.write_through(Arc::new(|_| Err("no".to_string())));
        assert!(matches!(
            app.pin_host("h:2", "bb", "x"),
            Err(StoreError::Service(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
