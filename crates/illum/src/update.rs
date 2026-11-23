// SPDX-License-Identifier: GPL-3.0-only
//! Updates: finding a newer release on GitHub, checking that it is ours, and installing it.
//!
//! The app asks GitHub for the manifest of the latest release (`latest.json`) and shows what
//! it offers. Installing is the installed host's job, because it has the system's rights and
//! so runs the installer without asking for an administrator. When the app asks, the host
//! fetches the manifest itself, downloads the installer into the home folder (which only the
//! system may write), checks its signature against the key compiled in here, and runs it.
//! The installer stops the service and replaces the program; the new service opens the app
//! again. Nothing is downloaded before the person says so, and only an installer signed with
//! the update key is ever run.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The manifest of whichever release is marked latest.
pub const MANIFEST_URL: &str =
    "https://github.com/Yooouuuuuuu/illumination/releases/latest/download/latest.json";
/// Where every release's files are; an installer anywhere else is refused.
const DOWNLOADS: &str = "https://github.com/Yooouuuuuuu/illumination/releases/download/";
/// The update key's public half, in minisign's format. The private half signs releases
/// (`illum-sign`) and never leaves the machine that builds them.
pub const PUBLIC_KEY: &str = include_str!("../assets/update-key.pub");
/// This program's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The manifest is small; anything bigger is not one.
const LONGEST_MANIFEST: u64 = 1024 * 1024;
/// The installer is tens of megabytes; anything past this is not one.
const LONGEST_INSTALLER: u64 = 512 * 1024 * 1024;
/// The marker the host leaves for the next service: open the app again after the update.
const REOPEN: &str = "reopen-app";

/// What the manifest says (the shape Tauri's updater uses).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// The release's notes: its changelog entry.
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    /// One entry per system and processor.
    #[serde(default)]
    pub platforms: BTreeMap<String, Platform>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    /// The installer's minisign signature, as the `.sig` file holds it.
    pub signature: String,
    pub url: String,
}

/// A release newer than this program, for this system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub url: String,
    pub signature: String,
}

/// The manifest's name for the system this program runs on; `None` where there are no
/// releases yet.
fn platform() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else {
        None
    }
}

/// The installer's file name for a version.
pub fn installer_name(version: &str) -> String {
    format!("Illumination_{version}_x64-setup.exe")
}

/// A version as its numbers: `0.15.1` is `[0, 15, 1]`. `None` when it is not one.
fn numbers(version: &str) -> Option<Vec<u64>> {
    let version = version.trim().trim_start_matches('v');
    version
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()
        .filter(|n| !n.is_empty())
}

/// Whether `candidate` comes after `current`, number by number.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (numbers(candidate), numbers(current)) {
        (Some(new), Some(old)) => new > old,
        _ => false,
    }
}

/// What the manifest offers a program at `current`: `None` when nothing newer is there for
/// this system. A manifest whose installer is not where this project's releases are is
/// refused.
pub fn offered(manifest: &Manifest, current: &str) -> Result<Option<Release>, String> {
    if numbers(&manifest.version).is_none() {
        return Err(format!(
            "the release's version \"{}\" is not a version",
            manifest.version
        ));
    }
    if !is_newer(&manifest.version, current) {
        return Ok(None);
    }
    let Some(entry) = platform().and_then(|p| manifest.platforms.get(p)) else {
        return Ok(None);
    };
    let version = manifest.version.trim().trim_start_matches('v');
    let expected = format!("{DOWNLOADS}v{version}/{}", installer_name(version));
    if entry.url != expected {
        return Err(format!("the release points somewhere else ({})", entry.url));
    }
    Ok(Some(Release {
        version: version.to_string(),
        notes: manifest.notes.trim().to_string(),
        url: entry.url.clone(),
        signature: entry.signature.clone(),
    }))
}

fn agent(patience: Duration) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .timeout_global(Some(patience))
            // Who asks, and nothing about the desk.
            .user_agent(format!("Illumination/{VERSION}"))
            .build(),
    )
}

/// Words for a failed request.
fn unreachable(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(404) => "GitHub has no release to offer".to_string(),
        ureq::Error::StatusCode(code) => format!("GitHub answered {code}"),
        other => {
            tracing::warn!(target: "app", "GitHub not reached: {other}");
            "could not reach GitHub".to_string()
        }
    }
}

/// Fetches the latest release's manifest.
pub fn fetch_manifest() -> Result<Manifest, String> {
    let mut response = agent(Duration::from_secs(30))
        .get(MANIFEST_URL)
        .call()
        .map_err(unreachable)?;
    let text = response
        .body_mut()
        .with_config()
        .limit(LONGEST_MANIFEST)
        .read_to_string()
        .map_err(unreachable)?;
    serde_json::from_str(&text).map_err(|e| format!("the release's manifest is not readable ({e})"))
}

/// Looks on GitHub: the newer release for this program, if there is one.
pub fn check() -> Result<Option<Release>, String> {
    offered(&fetch_manifest()?, VERSION)
}

/// Checks `file` against `signature` and the public key `key`. The signature's trusted
/// comment must name the file too, so that a release's installer cannot be passed off as
/// another version's.
pub fn verify(file: &Path, signature: &str, name: &str, key: &str) -> Result<(), String> {
    let key = minisign_verify::PublicKey::decode(key.trim())
        .map_err(|e| format!("this program's update key is not readable ({e})"))?;
    let signature = minisign_verify::Signature::decode(signature.trim())
        .map_err(|e| format!("the signature is not readable ({e})"))?;
    let named = format!("file:{name}");
    if !signature
        .trusted_comment()
        .split('\t')
        .any(|part| part == named)
    {
        return Err("the signature is for another file".to_string());
    }
    let mut checking = key
        .verify_stream(&signature)
        .map_err(|e| format!("the signature is not by this program's update key ({e})"))?;
    let mut reading = std::fs::File::open(file).map_err(|e| format!("reading {name}: {e}"))?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = reading
            .read(&mut buffer)
            .map_err(|e| format!("reading {name}: {e}"))?;
        if n == 0 {
            break;
        }
        checking.update(&buffer[..n]);
    }
    checking
        .finalize()
        .map_err(|_| "the file does not match its signature".to_string())
}

/// Downloads `url` to `to`, telling `progress` (bytes so far, bytes in all; 0 when not said)
/// as it goes. Stops when `cancel` turns true.
fn download(
    url: &str,
    to: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let response = agent(Duration::from_secs(30 * 60))
        .get(url)
        .call()
        .map_err(unreachable)?;
    let total = response.body().content_length().unwrap_or(0);
    let mut reading = response.into_body().into_reader();
    let mut file = std::fs::File::create(to).map_err(|e| format!("saving the download: {e}"))?;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    progress(0, total);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        let n = reading
            .read(&mut buffer)
            .map_err(|e| format!("the download broke off ({e})"))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > LONGEST_INSTALLER {
            return Err("the download is far larger than an installer".to_string());
        }
        file.write_all(&buffer[..n])
            .map_err(|e| format!("saving the download: {e}"))?;
        progress(done, total);
    }
    file.flush()
        .map_err(|e| format!("saving the download: {e}"))?;
    Ok(())
}

/// What a cancelled download fails with; not shown as a failure.
pub const CANCELLED: &str = "cancelled";

/// Where the host keeps a download, in the home folder.
fn updates_folder(home: &Path) -> PathBuf {
    home.join("updates")
}

/// What the host tells the app about an update it was asked for (in `host-status.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateStatus {
    /// "" (nothing asked), "downloading", "installing" or "failed".
    pub state: String,
    pub version: String,
    /// Bytes downloaded, and in all (0 while not known).
    pub done: u64,
    pub total: u64,
    /// Why, when it failed.
    pub why: String,
}

/// The host's side: one update at a time, started and cancelled by the app.
pub struct Installer {
    home: PathBuf,
    busy: AtomicBool,
    cancel: AtomicBool,
    /// Ends every session here, saying it is for an update; returns how many there were.
    end_all: Box<dyn Fn() -> usize + Send + Sync>,
    /// Says how it goes, for the app.
    show: Box<dyn Fn(UpdateStatus) + Send + Sync>,
}

impl Installer {
    pub fn new(
        home: &Path,
        end_all: impl Fn() -> usize + Send + Sync + 'static,
        show: impl Fn(UpdateStatus) + Send + Sync + 'static,
    ) -> std::sync::Arc<Installer> {
        std::sync::Arc::new(Installer {
            home: home.to_path_buf(),
            busy: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            end_all: Box::new(end_all),
            show: Box::new(show),
        })
    }

    /// Starts updating to the latest release, in the background.
    pub fn start(self: &std::sync::Arc<Self>) -> Result<(), String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Ok(()); // already on its way
        }
        self.cancel.store(false, Ordering::SeqCst);
        let this = self.clone();
        std::thread::spawn(move || {
            let outcome = this.install();
            match outcome {
                Ok(()) => {}
                Err(why) if why == CANCELLED => {
                    tracing::info!(target: "service", "The update was cancelled.");
                    (this.show)(UpdateStatus::default());
                }
                Err(why) => {
                    tracing::warn!(target: "service", "The update failed: {why}");
                    (this.show)(UpdateStatus {
                        state: "failed".into(),
                        why,
                        ..UpdateStatus::default()
                    });
                }
            }
            this.busy.store(false, Ordering::SeqCst);
        });
        Ok(())
    }

    /// Stops a download on its way. Once the installer runs, there is nothing to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn install(&self) -> Result<(), String> {
        (self.show)(UpdateStatus {
            state: "downloading".into(),
            ..UpdateStatus::default()
        });
        let release = check()?.ok_or("this desk has the newest version already")?;
        tracing::info!(target: "service", "Updating to {}.", release.version);
        let folder = updates_folder(&self.home);
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("making {}: {e}", folder.display()))?;
        let name = installer_name(&release.version);
        let part = folder.join(format!("{name}.part"));
        let ready = folder.join(&name);
        let mut shown_percent = None;
        let downloaded = download(&release.url, &part, &self.cancel, &mut |done, total| {
            let percent = (done * 100).checked_div(total);
            if shown_percent != Some(percent) {
                shown_percent = Some(percent);
                (self.show)(UpdateStatus {
                    state: "downloading".into(),
                    version: release.version.clone(),
                    done,
                    total,
                    why: String::new(),
                });
            }
        });
        if let Err(why) = downloaded {
            let _ = std::fs::remove_file(&part);
            return Err(why);
        }
        if let Err(why) = verify(&part, &release.signature, &name, PUBLIC_KEY) {
            let _ = std::fs::remove_file(&part);
            return Err(format!("the download was not run: {why}"));
        }
        std::fs::rename(&part, &ready).map_err(|e| format!("keeping the download: {e}"))?;
        if self.cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        // Only now, so that the connections last through the download, and through one that
        // fails. A moment for the goodbyes to leave before the installer stops the host.
        let ended = (self.end_all)();
        if ended > 0 {
            tracing::info!(target: "service", "Ended {ended} sessions for the update.");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        // For the service the installer starts: open the app again.
        std::fs::write(folder.join(REOPEN), release.version.as_bytes())
            .map_err(|e| format!("preparing the update: {e}"))?;
        (self.show)(UpdateStatus {
            state: "installing".into(),
            version: release.version.clone(),
            ..UpdateStatus::default()
        });
        tracing::info!(target: "service", "Running {name}.");
        run_installer(&ready).inspect_err(|_| {
            let _ = std::fs::remove_file(folder.join(REOPEN));
        })
    }
}

/// Runs the installer without its pages. It outlives this host: stopping the service ends
/// the host, and the installer goes on to replace the program.
#[cfg(windows)]
fn run_installer(installer: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP.
    std::process::Command::new(installer)
        .arg("/S")
        .creation_flags(0x0800_0000 | 0x0000_0008 | 0x0000_0200)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("starting the installer: {e}"))
}

#[cfg(not(windows))]
fn run_installer(_installer: &Path) -> Result<(), String> {
    Err("updating is for Windows only so far".to_string())
}

/// Whether the last host asked for the app to be opened again (an update just ran). Asking
/// takes the marker away.
pub fn take_reopen(home: &Path) -> bool {
    std::fs::remove_file(updates_folder(home).join(REOPEN)).is_ok()
}

/// Removes what earlier updates left. A file still in use stays until next time.
pub fn clean(home: &Path) {
    let Ok(entries) = std::fs::read_dir(updates_folder(home)) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() != REOPEN {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, url: &str) -> Manifest {
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "windows-x86_64".to_string(),
            Platform {
                signature: "sig".into(),
                url: url.into(),
            },
        );
        Manifest {
            version: version.into(),
            notes: "### Fixed\n\n- A thing.\n".into(),
            pub_date: String::new(),
            platforms,
        }
    }

    #[test]
    fn versions_compare_number_by_number() {
        assert!(is_newer("0.15.1", "0.15.0"));
        assert!(is_newer("0.15.10", "0.15.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("v0.16.0", "0.15.3"));
        assert!(!is_newer("0.15.0", "0.15.0"));
        assert!(!is_newer("0.14.9", "0.15.0"));
        assert!(!is_newer("0.15.1-beta", "0.15.0"));
        assert!(!is_newer("", "0.15.0"));
    }

    #[test]
    fn only_a_newer_release_from_its_own_place_is_offered() {
        let at = "https://github.com/Yooouuuuuuu/illumination/releases/download/v0.15.1/Illumination_0.15.1_x64-setup.exe";
        let offer = offered(&manifest("0.15.1", at), "0.15.0").unwrap();
        if platform().is_some() {
            let release = offer.expect("newer");
            assert_eq!(release.version, "0.15.1");
            assert_eq!(release.url, at);
            assert_eq!(release.notes, "### Fixed\n\n- A thing.");
            let elsewhere = manifest(
                "0.15.1",
                "https://example.com/Illumination_0.15.1_x64-setup.exe",
            );
            assert!(offered(&elsewhere, "0.15.0").is_err());
            // The address must be that version's.
            let older_file = manifest(
                "0.15.1",
                "https://github.com/Yooouuuuuuu/illumination/releases/download/v0.15.0/Illumination_0.15.0_x64-setup.exe",
            );
            assert!(offered(&older_file, "0.15.0").is_err());
        }
        assert_eq!(offered(&manifest("0.15.0", at), "0.15.0"), Ok(None));
        assert_eq!(offered(&manifest("0.14.2", at), "0.15.0"), Ok(None));
        assert!(offered(&manifest("soon", at), "0.15.0").is_err());
        let mut none_here = manifest("0.15.1", at);
        none_here.platforms.clear();
        assert_eq!(offered(&none_here, "0.15.0"), Ok(None));
    }

    #[test]
    fn the_manifest_reads_as_written() {
        let text = r#"{
            "version": "0.15.1",
            "notes": "- One.",
            "pub_date": "2026-11-20T00:00:00Z",
            "platforms": {
                "windows-x86_64": {
                    "signature": "untrusted comment: x\nRUQ\ntrusted comment: y\nZZ",
                    "url": "https://github.com/Yooouuuuuuu/illumination/releases/download/v0.15.1/Illumination_0.15.1_x64-setup.exe"
                }
            }
        }"#;
        let read: Manifest = serde_json::from_str(text).unwrap();
        assert_eq!(read.version, "0.15.1");
        assert_eq!(read.platforms.len(), 1);
    }

    /// A file signed with a key passes against that key, and fails when it changed, when
    /// the signature names another file, and against another key.
    #[test]
    fn only_a_signed_file_passes() {
        let dir = std::env::temp_dir().join(format!("illum-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let name = installer_name("0.15.1");
        let file = dir.join(&name);
        std::fs::write(&file, b"an installer, as far as this test knows").unwrap();
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let public = pair.pk.to_box().unwrap().into_string();
        let sign = |comment: &str| {
            minisign::sign(
                Some(&pair.pk),
                &pair.sk,
                std::fs::File::open(&file).unwrap(),
                Some(comment),
                None,
            )
            .unwrap()
            .into_string()
        };
        let good = sign(&format!("timestamp:1\tfile:{name}"));
        assert_eq!(verify(&file, &good, &name, &public), Ok(()));

        let other_file = sign("timestamp:1\tfile:Illumination_0.14.0_x64-setup.exe");
        assert!(verify(&file, &other_file, &name, &public).is_err());

        let stranger = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let theirs = stranger.pk.to_box().unwrap().into_string();
        assert!(verify(&file, &good, &name, &theirs).is_err());

        std::fs::write(&file, b"an installer, changed on its way").unwrap();
        assert!(verify(&file, &good, &name, &public).is_err());

        // The placeholder key of a build without one refuses everything.
        assert!(verify(&file, &good, &name, "untrusted comment: no key\n").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_reopen_marker_is_taken_once_and_downloads_are_cleaned() {
        let home = std::env::temp_dir().join(format!("illum-reopen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(!take_reopen(&home));
        std::fs::create_dir_all(updates_folder(&home)).unwrap();
        std::fs::write(updates_folder(&home).join(REOPEN), b"0.15.1").unwrap();
        std::fs::write(updates_folder(&home).join("old-setup.exe"), b"x").unwrap();
        clean(&home);
        assert!(!updates_folder(&home).join("old-setup.exe").exists());
        assert!(take_reopen(&home));
        assert!(!take_reopen(&home));
        let _ = std::fs::remove_dir_all(&home);
    }
}
