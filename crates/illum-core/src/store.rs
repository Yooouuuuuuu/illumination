// SPDX-License-Identifier: GPL-3.0-only
//! Trust store: pinned host fingerprints (controller side) and paired peers (host side).
//! Stored as versioned JSON (architecture §9).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    data: TrustData,
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
        Ok(TrustStore { path, data })
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
        self.data.known_hosts.insert(
            address.to_string(),
            KnownHost {
                fingerprint: fingerprint.to_string(),
                name: name.to_string(),
                first_seen: now(),
            },
        );
        self.save()
    }

    pub fn forget_host(&mut self, address: &str) -> Result<bool, StoreError> {
        let removed = self.data.known_hosts.remove(address).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn paired_peer(&self, fingerprint: &str) -> Option<&PairedPeer> {
        self.data.paired_peers.get(fingerprint)
    }

    pub fn pair(&mut self, fingerprint: &str, name: &str, address: &str) -> Result<(), StoreError> {
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
    fn save(&self) -> Result<(), StoreError> {
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
        std::fs::rename(&tmp, &self.path).map_err(io)
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
}
