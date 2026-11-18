// SPDX-License-Identifier: GPL-3.0-only
//! `illum-sign`: the update key of Illumination's releases.
//!
//!   illum-sign generate [--key PATH]
//!       Makes the key pair once. The private key goes to PATH (by default
//!       %USERPROFILE%\.illumination\update.key), protected by a password asked for here; the
//!       public key goes into the program (crates/illum/assets/update-key.pub).
//!
//!   illum-sign release INSTALLER --version X.Y.Z [--key PATH] [--changelog PATH]
//!       Signs a release's installer and writes its manifest: INSTALLER.sig and latest.json
//!       beside it, with the version's changelog entry as the notes.
//!
//! The password is read from ILLUM_SIGNING_KEY_PASSWORD when that is set, else asked for.
//! It is never written anywhere.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The public key the program checks updates with; `release` refuses a private key that does
/// not belong to it.
const PUBLIC_KEY_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../illum/assets/update-key.pub"
);
const DOWNLOADS: &str = "https://github.com/Yooouuuuuuu/illumination/releases/download/";

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("illum-sign: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<()> {
    let mut args = args.into_iter();
    let command = args.next().unwrap_or_default();
    let mut key = None;
    let mut version = None;
    let mut changelog = None;
    let mut file = None;
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--key" => key = Some(PathBuf::from(value()?)),
            "--version" => version = Some(value()?),
            "--changelog" => changelog = Some(PathBuf::from(value()?)),
            other if !other.starts_with("--") && file.is_none() => {
                file = Some(PathBuf::from(other))
            }
            other => bail!("unknown argument {other}"),
        }
    }
    let key = match key.or_else(|| std::env::var_os("ILLUM_SIGNING_KEY").map(PathBuf::from)) {
        Some(key) => key,
        None => default_key()?,
    };
    match command.as_str() {
        "generate" => generate(&key, Path::new(PUBLIC_KEY_FILE)),
        "release" => {
            let file =
                file.context("which installer? illum-sign release INSTALLER --version X.Y.Z")?;
            let version = version.context("--version X.Y.Z is needed")?;
            let changelog = changelog.unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md")
            });
            release(&key, &file, &version, &changelog)
        }
        _ => bail!("illum-sign generate | illum-sign release INSTALLER --version X.Y.Z"),
    }
}

fn default_key() -> Result<PathBuf> {
    let profile = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .context("no home folder to keep the key in; give --key")?;
    Ok(PathBuf::from(profile)
        .join(".illumination")
        .join("update.key"))
}

fn password() -> Option<String> {
    std::env::var("ILLUM_SIGNING_KEY_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
}

fn generate(key: &Path, public: &Path) -> Result<()> {
    if key.exists() {
        bail!(
            "{} exists already. A new key means every installed copy updates by hand once; \
             move the old one away first if that is really wanted.",
            key.display()
        );
    }
    let pair =
        minisign::KeyPair::generate_encrypted_keypair(password()).context("making the key")?;
    if let Some(folder) = key.parent() {
        std::fs::create_dir_all(folder).with_context(|| format!("making {}", folder.display()))?;
    }
    let secret = pair
        .sk
        .to_box(Some("Illumination update key"))
        .context("writing the private key")?
        .into_string();
    std::fs::write(key, secret).with_context(|| format!("writing {}", key.display()))?;
    let public_box = pair
        .pk
        .to_box()
        .context("writing the public key")?
        .into_string();
    std::fs::write(public, &public_box).with_context(|| format!("writing {}", public.display()))?;
    println!(
        "Private key: {} (back it up, with its password).",
        key.display()
    );
    println!("Public key:  {} (commit it).", public.display());
    Ok(())
}

/// The changelog entry of `version`: what is under its heading, up to the next version's.
fn changelog_entry(changelog: &str, version: &str) -> Option<String> {
    let heading = format!("## [{version}]");
    let mut lines = changelog.lines().skip_while(|l| !l.starts_with(&heading));
    lines.next()?;
    let entry: Vec<&str> = lines.take_while(|l| !l.starts_with("## [")).collect();
    let text = entry.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn release(key: &Path, installer: &Path, version: &str, changelog: &Path) -> Result<()> {
    let name = installer
        .file_name()
        .and_then(|n| n.to_str())
        .context("the installer has no file name")?
        .to_string();
    let expected = format!("Illumination_{version}_x64-setup.exe");
    if name != expected {
        bail!("the installer is {name}, but version {version}'s is {expected}");
    }
    let notes = std::fs::read_to_string(changelog)
        .with_context(|| format!("reading {}", changelog.display()))?;
    let notes = changelog_entry(&notes, version)
        .with_context(|| format!("{} has no entry for {version}", changelog.display()))?;

    let public = std::fs::read_to_string(PUBLIC_KEY_FILE).context("reading the public key")?;
    let public = minisign::PublicKeyBox::from_string(&public)
        .and_then(|b| b.into_public_key())
        .context("the program has no update key yet: run illum-sign generate")?;
    let secret = std::fs::read_to_string(key)
        .with_context(|| format!("reading the private key {}", key.display()))?;
    let secret = minisign::SecretKeyBox::from_string(&secret)
        .and_then(|b| b.into_secret_key(password()))
        .context("opening the private key (is the password right?)")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let trusted = format!("timestamp:{now}\tfile:{name}");
    let reading = std::fs::File::open(installer)
        .with_context(|| format!("reading {}", installer.display()))?;
    let signature = minisign::sign(Some(&public), &secret, reading, Some(&trusted), None)
        .context("signing (is this the private key of the program's public key?)")?;
    // Checked as the program will check it.
    let reading = std::fs::File::open(installer)?;
    minisign::verify(&public, &signature, reading, true, false, false)
        .context("the signature does not check against the program's public key")?;
    let signature = signature.into_string();

    let sig_path = PathBuf::from(format!("{}.sig", installer.display()));
    std::fs::write(&sig_path, &signature)
        .with_context(|| format!("writing {}", sig_path.display()))?;
    let manifest = serde_json::json!({
        "version": version,
        "notes": notes,
        "pub_date": rfc3339(now),
        "platforms": {
            "windows-x86_64": {
                "signature": signature,
                "url": format!("{DOWNLOADS}v{version}/{name}"),
            }
        }
    });
    let manifest_path = installer.with_file_name("latest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest)? + "\n",
    )
    .with_context(|| format!("writing {}", manifest_path.display()))?;
    println!("Signed:   {}", sig_path.display());
    println!("Manifest: {}", manifest_path.display());
    Ok(())
}

/// `2026-11-20T08:15:00Z`, from Unix seconds (civil-from-days, Howard Hinnant).
fn rfc3339(unix: u64) -> String {
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let secs = unix % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_is_what_is_under_its_heading() {
        let log = "# Changelog\n\n## [Unreleased]\n\n## [0.15.1] - 2026-11-20\n\n### Fixed\n\n- A.\n  More.\n\n## [0.15.0] - 2026-11-19\n\n- B.\n";
        assert_eq!(
            changelog_entry(log, "0.15.1").as_deref(),
            Some("### Fixed\n\n- A.\n  More.")
        );
        assert_eq!(changelog_entry(log, "0.15.0").as_deref(), Some("- B."));
        assert_eq!(changelog_entry(log, "0.14.0"), None);
        assert_eq!(changelog_entry(log, "0.15"), None);
    }

    #[test]
    fn dates_read_as_rfc3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_795_003_200), "2026-11-18T12:00:00Z");
    }

    /// A key made here signs an installer, and the program's check accepts it.
    #[test]
    fn a_release_signed_here_checks_out() {
        let dir = std::env::temp_dir().join(format!("illum-sign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pair = minisign::KeyPair::generate_encrypted_keypair(Some("test".into())).unwrap();
        let secret = pair.sk.to_box(None).unwrap().into_string();
        let opened = minisign::SecretKeyBox::from_string(&secret)
            .unwrap()
            .into_secret_key(Some("test".into()))
            .unwrap();
        let file = dir.join("Illumination_0.15.1_x64-setup.exe");
        std::fs::write(&file, b"installer").unwrap();
        let signature = minisign::sign(
            Some(&pair.pk),
            &opened,
            std::fs::File::open(&file).unwrap(),
            Some("timestamp:1\tfile:Illumination_0.15.1_x64-setup.exe"),
            None,
        )
        .unwrap();
        minisign::verify(
            &pair.pk,
            &signature,
            std::fs::File::open(&file).unwrap(),
            true,
            false,
            false,
        )
        .unwrap();
        // The wrong password does not open the key.
        assert!(
            minisign::SecretKeyBox::from_string(&secret)
                .unwrap()
                .into_secret_key(Some("wrong".into()))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
