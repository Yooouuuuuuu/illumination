// SPDX-License-Identifier: GPL-3.0-only
//! Trust store: pinned host fingerprints (controller side) and paired peers (host side).
//! Stored as versioned JSON (architecture §9).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Whether a desk may connect to this one. A desk without one of its own follows the
/// setting for all desks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// It connects without anyone being asked.
    Always,
    /// The person at this desk is asked each time.
    Ask,
    /// It is refused, and nobody is asked.
    Never,
}

/// What this desk knows about another one, whichever way they met. Kept by the other
/// desk's key, so a desk is one entry whether it was connected to, connected here, or both.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeskInfo {
    /// The person's own name for it. Empty: the machine's name is shown.
    pub name: String,
    /// What the desk calls itself.
    pub machine: String,
    /// Its system, as it said: "Windows 11".
    pub os: String,
    /// Where it was last seen, without a port.
    pub address: String,
    /// `Ask` or `Never`, when set for this desk. `Always` is not kept here: it is being
    /// among the paired peers, as it was before desks had entries.
    pub permission: Option<Permission>,
    pub favorite: bool,
    /// Unix seconds of the last connection from here to it; 0: never.
    pub last_out: u64,
    /// Of its last connection to here that was let in.
    pub last_in: u64,
    /// Of its last try that was not let in.
    pub tried_in: u64,
}

impl DeskInfo {
    /// The name to show: the person's own, else the machine's.
    pub fn shown_name(&self) -> &str {
        if self.name.is_empty() {
            &self.machine
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustData {
    version: u32,
    /// Keyed by the address as dialled, e.g. `100.100.101.1:7171`.
    known_hosts: BTreeMap<String, KnownHost>,
    /// Keyed by the peer's certificate fingerprint.
    paired_peers: BTreeMap<String, PairedPeer>,
    /// Keyed by the desk's certificate fingerprint. Absent in files written before 0.14.0.
    #[serde(default)]
    desks: BTreeMap<String, DeskInfo>,
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
    /// The person's rule for one desk; `None` puts it back on the setting for all desks.
    SetPermission {
        fingerprint: String,
        permission: Option<Permission>,
    },
    /// The person's own name for a desk; empty gives it its machine name back.
    Rename {
        fingerprint: String,
        name: String,
    },
    Favorite {
        fingerprint: String,
        on: bool,
    },
    /// This desk just connected to that one.
    ConnectedTo {
        fingerprint: String,
        machine: String,
        os: String,
        address: String,
    },
    /// Everything about one desk goes: its key as a host, its permission, its entry.
    ForgetDesk {
        fingerprint: String,
    },
}

/// Makes a [`Change`] somewhere else (the installed service) and says whether anything
/// changed.
pub type Through = Arc<dyn Fn(&Change) -> Result<bool, String> + Send + Sync>;

/// How long a desk accepted "this time" may come back after its last session here ended: a
/// reconnection after a dropped link, or the session that follows the app's first reach.
pub const GUEST_GRACE: Duration = Duration::from_secs(120);

/// A desk accepted for this time only. Kept in memory, never written.
#[derive(Debug, Clone, Copy)]
struct Guest {
    sessions: u32,
    /// With no session: until when it may still come back.
    until: Instant,
}

/// One writer at a time in this process, from reading the file to replacing it.
static WRITING: Mutex<()> = Mutex::new(());

pub struct TrustStore {
    path: PathBuf,
    data: TrustData,
    /// The file's modification time when it was last read or written here.
    seen: Option<SystemTime>,
    /// Set when this process may not write the file itself.
    through: Option<Through>,
    guests: HashMap<String, Guest>,
    /// The setting for desks without a permission of their own: let them in, ask, or refuse
    /// them. Told by the host from the settings; not part of this file.
    new_desks: Permission,
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
            guests: HashMap::new(),
            new_desks: Permission::Ask,
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
            Change::SetPermission {
                fingerprint,
                permission,
            } => {
                let before = self.permission(fingerprint);
                let desk = self.data.desks.entry(fingerprint.clone()).or_default();
                match permission {
                    Some(Permission::Always) => {
                        desk.permission = None;
                        let (name, last_address) = (desk.machine.clone(), desk.address.clone());
                        self.data
                            .paired_peers
                            .entry(fingerprint.clone())
                            .or_insert(PairedPeer {
                                name,
                                first_seen: now(),
                                last_address,
                            });
                    }
                    other => {
                        desk.permission = *other;
                        self.data.paired_peers.remove(fingerprint);
                    }
                }
                before != *permission
            }
            Change::Rename { fingerprint, name } => {
                let desk = self.data.desks.entry(fingerprint.clone()).or_default();
                let name = name.trim().chars().take(64).collect::<String>();
                let changed = desk.name != name;
                desk.name = name;
                changed
            }
            Change::Favorite { fingerprint, on } => {
                let desk = self.data.desks.entry(fingerprint.clone()).or_default();
                let changed = desk.favorite != *on;
                desk.favorite = *on;
                changed
            }
            Change::ConnectedTo {
                fingerprint,
                machine,
                os,
                address,
            } => {
                let desk = self.data.desks.entry(fingerprint.clone()).or_default();
                desk.machine = machine.clone();
                desk.os = crate::os::clean(os);
                desk.address = address.clone();
                desk.last_out = now();
                true
            }
            Change::ForgetDesk { fingerprint } => {
                let hosts = self.data.known_hosts.len();
                self.data
                    .known_hosts
                    .retain(|_, host| host.fingerprint != *fingerprint);
                let was_host = self.data.known_hosts.len() != hosts;
                let was_peer = self.data.paired_peers.remove(fingerprint).is_some();
                let was_desk = self.data.desks.remove(fingerprint).is_some();
                was_host || was_peer || was_desk
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
        if let Some(desk) = self.data.desks.get_mut(fingerprint) {
            desk.permission = None;
        }
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

    /// Lets a desk in for this time without remembering it (the person chose "this time").
    pub fn admit_guest(&mut self, fingerprint: &str) {
        self.guests.insert(
            fingerprint.to_string(),
            Guest {
                sessions: 0,
                until: Instant::now() + GUEST_GRACE,
            },
        );
    }

    /// Whether a desk accepted for this time may (still) connect without being asked about.
    pub fn guest_allowed(&mut self, fingerprint: &str) -> bool {
        let now = Instant::now();
        self.guests
            .retain(|_, guest| guest.sessions > 0 || guest.until > now);
        self.guests.contains_key(fingerprint)
    }

    /// A session with a guest began.
    pub fn guest_entered(&mut self, fingerprint: &str) {
        if let Some(guest) = self.guests.get_mut(fingerprint) {
            guest.sessions += 1;
        }
    }

    /// A session with a guest ended; after the last, it may come back for [`GUEST_GRACE`].
    pub fn guest_left(&mut self, fingerprint: &str) {
        if let Some(guest) = self.guests.get_mut(fingerprint) {
            guest.sessions = guest.sessions.saturating_sub(1);
            guest.until = Instant::now() + GUEST_GRACE;
        }
    }

    /// The person ended a guest's session: it is asked about again at once.
    pub fn dismiss_guest(&mut self, fingerprint: &str) {
        self.guests.remove(fingerprint);
    }

    /// The desks this one knows, by key.
    pub fn desks(&self) -> impl Iterator<Item = (&String, &DeskInfo)> {
        self.data.desks.iter()
    }

    pub fn desk(&self, fingerprint: &str) -> Option<&DeskInfo> {
        self.data.desks.get(fingerprint)
    }

    /// The person's rule for this desk, if it has one of its own.
    pub fn permission(&self, fingerprint: &str) -> Option<Permission> {
        if self.data.paired_peers.contains_key(fingerprint) {
            return Some(Permission::Always);
        }
        self.data.desks.get(fingerprint).and_then(|d| d.permission)
    }

    pub fn set_permission(
        &mut self,
        fingerprint: &str,
        permission: Option<Permission>,
    ) -> Result<bool, StoreError> {
        self.change(Change::SetPermission {
            fingerprint: fingerprint.to_string(),
            permission,
        })
    }

    pub fn rename(&mut self, fingerprint: &str, name: &str) -> Result<bool, StoreError> {
        self.change(Change::Rename {
            fingerprint: fingerprint.to_string(),
            name: name.to_string(),
        })
    }

    pub fn set_favorite(&mut self, fingerprint: &str, on: bool) -> Result<bool, StoreError> {
        self.change(Change::Favorite {
            fingerprint: fingerprint.to_string(),
            on,
        })
    }

    pub fn forget_desk(&mut self, fingerprint: &str) -> Result<bool, StoreError> {
        self.change(Change::ForgetDesk {
            fingerprint: fingerprint.to_string(),
        })
    }

    /// This desk just connected to that one: when, and what it said it is.
    pub fn connected_to(
        &mut self,
        fingerprint: &str,
        machine: &str,
        os: &str,
        address: &str,
    ) -> Result<(), StoreError> {
        self.change(Change::ConnectedTo {
            fingerprint: fingerprint.to_string(),
            machine: machine.to_string(),
            os: os.to_string(),
            address: address.to_string(),
        })
        .map(|_| ())
    }

    /// That desk just connected here, or tried to. Written by the host, which owns the file.
    pub fn came_in(
        &mut self,
        fingerprint: &str,
        machine: &str,
        os: &str,
        address: &str,
        let_in: bool,
    ) -> Result<(), StoreError> {
        let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        self.refresh();
        let desk = self.data.desks.entry(fingerprint.to_string()).or_default();
        desk.machine = machine
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        desk.os = crate::os::clean(os);
        desk.address = address.to_string();
        if let_in {
            desk.last_in = now();
        } else {
            desk.tried_in = now();
        }
        self.save()
    }

    /// What holds for a desk without a permission of its own.
    pub fn new_desks(&self) -> Permission {
        self.new_desks
    }

    pub fn set_new_desks(&mut self, permission: Permission) {
        self.new_desks = permission;
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
        // A guest gets in while it is connected and for a while after, and is never written.
        assert!(!app.guest_allowed("9a"));
        app.admit_guest("9a");
        app.guest_entered("9a");
        assert!(app.guest_allowed("9a"));
        app.guest_left("9a");
        assert!(app.guest_allowed("9a"), "may come back");
        app.dismiss_guest("9a");
        assert!(!app.guest_allowed("9a"));
        assert!(app.paired_peer("9a").is_none());
        // A service that refuses is an error, not a silent success.
        app.write_through(Arc::new(|_| Err("no".to_string())));
        assert!(matches!(
            app.pin_host("h:2", "bb", "x"),
            Err(StoreError::Service(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod desk_tests {
    use super::*;

    fn store(name: &str) -> (TrustStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("illum-desks-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (TrustStore::load(dir.join("trust.json")).unwrap(), dir)
    }

    /// A desk is one entry however it was met, and its permission has four states: none of
    /// its own, always, ask, never.
    #[test]
    fn a_desk_has_one_entry_and_a_permission() {
        let (mut store, dir) = store("permission");
        assert_eq!(store.permission("aa"), None);
        store
            .came_in("aa", "LAPTOP\n", "Windows 11", "100.64.0.2", false)
            .unwrap();
        store
            .connected_to("aa", "LAPTOP", "Windows 11", "100.64.0.2")
            .unwrap();
        assert_eq!(store.desks().count(), 1);
        let desk = store.desk("aa").unwrap().clone();
        assert_eq!(
            (desk.machine.as_str(), desk.os.as_str()),
            ("LAPTOP", "Windows 11")
        );
        assert!(desk.tried_in > 0 && desk.last_out > 0 && desk.last_in == 0);

        assert!(
            store
                .set_permission("aa", Some(Permission::Always))
                .unwrap()
        );
        assert!(store.paired_peer("aa").is_some(), "always is being paired");
        assert_eq!(store.permission("aa"), Some(Permission::Always));
        assert!(store.set_permission("aa", Some(Permission::Never)).unwrap());
        assert!(store.paired_peer("aa").is_none());
        assert_eq!(store.permission("aa"), Some(Permission::Never));
        assert!(
            !store.set_permission("aa", Some(Permission::Never)).unwrap(),
            "no change"
        );
        // Accepting it for good in the question overrides a rule of its own.
        store.set_permission("aa", Some(Permission::Ask)).unwrap();
        store.pair("aa", "LAPTOP", "100.64.0.2").unwrap();
        assert_eq!(store.permission("aa"), Some(Permission::Always));
        assert!(store.set_permission("aa", None).unwrap());
        assert_eq!(store.permission("aa"), None);

        // All of it survives the file.
        store.rename("aa", "  Laptop  ").unwrap();
        store.set_favorite("aa", true).unwrap();
        let read = TrustStore::load(store.path()).unwrap();
        let desk = read.desk("aa").unwrap();
        assert_eq!(desk.shown_name(), "Laptop");
        assert!(desk.favorite);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn forgetting_a_desk_removes_every_trace() {
        let (mut store, dir) = store("forget");
        store.pin_host("100.64.0.2:28326", "aa", "LAPTOP").unwrap();
        store.pin_host("100.64.0.3:28326", "bb", "OTHER").unwrap();
        store.pair("aa", "LAPTOP", "100.64.0.2").unwrap();
        store
            .connected_to("aa", "LAPTOP", "", "100.64.0.2")
            .unwrap();
        assert!(store.forget_desk("aa").unwrap());
        assert!(store.known_host("100.64.0.2:28326").is_none());
        assert!(store.paired_peer("aa").is_none() && store.desk("aa").is_none());
        assert!(
            store.known_host("100.64.0.3:28326").is_some(),
            "others stay"
        );
        assert!(!store.forget_desk("aa").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file from before 0.14.0 has no desks; it reads, and nothing is lost on writing.
    #[test]
    fn an_older_file_reads() {
        let (_, dir) = store("older");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trust.json");
        std::fs::write(
            &path,
            r#"{"version":1,"known_hosts":{},"paired_peers":{"aa":{"name":"L","first_seen":1,"last_address":"1.2.3.4:5"}}}"#,
        )
        .unwrap();
        let mut store = TrustStore::load(&path).unwrap();
        assert_eq!(store.permission("aa"), Some(Permission::Always));
        store.set_favorite("aa", true).unwrap();
        assert!(TrustStore::load(&path).unwrap().paired_peer("aa").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
