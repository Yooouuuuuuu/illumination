// SPDX-License-Identifier: GPL-3.0-only
//! Device identity: a self-signed Ed25519 certificate made on first start and kept
//! (as AnyDesk does with its `CN=AnyDesk Client` certificate).

use crate::{NetError, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::Path;

const CERT_FILE: &str = "device-cert.der";
const KEY_FILE: &str = "device-key.der";

/// SHA-256 of a certificate's DER bytes, as 64 lowercase hex characters.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Fingerprint(String);

impl Fingerprint {
    pub fn of(cert_der: &[u8]) -> Fingerprint {
        let digest = Sha256::digest(cert_der);
        Fingerprint(digest.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Wraps a stored hex string. Returns `None` if it isn't 64 hex characters.
    pub fn parse(hex: &str) -> Option<Fingerprint> {
        let ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        ok.then(|| Fingerprint(hex.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Groups of four for reading aloud and comparing on two screens.
    pub fn grouped(&self) -> String {
        self.0
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).expect("hex is ascii"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.grouped())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", &self.0[..16])
    }
}

pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    pub fingerprint: Fingerprint,
}

impl Identity {
    /// Makes a new identity in memory.
    pub fn generate() -> Result<Identity> {
        let cert_err = |e: rcgen::Error| NetError::Certificate(e.to_string());
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).map_err(cert_err)?;
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(cert_err)?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Illumination Device");
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2075, 1, 1);
        let cert = params.self_signed(&key_pair).map_err(cert_err)?;
        let cert = cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(key_pair.serialize_der());
        Ok(Identity::from_parts(cert, key))
    }

    /// Loads the identity from `dir`, creating and saving one if there is none yet.
    pub fn load_or_create(dir: &Path) -> Result<Identity> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if cert_path.exists() && key_path.exists() {
            let cert = CertificateDer::from(std::fs::read(&cert_path)?);
            let key = PrivatePkcs8KeyDer::from(std::fs::read(&key_path)?);
            return Ok(Identity::from_parts(cert, key));
        }
        let identity = Identity::generate()?;
        std::fs::create_dir_all(dir)?;
        std::fs::write(&key_path, identity.key.secret_pkcs8_der())?;
        std::fs::write(&cert_path, identity.cert.as_ref())?;
        tracing::info!(target: "identity", "Generated a new device certificate ({})", identity.fingerprint);
        Ok(identity)
    }

    fn from_parts(cert: CertificateDer<'static>, key: PrivatePkcs8KeyDer<'static>) -> Identity {
        let fingerprint = Fingerprint::of(cert.as_ref());
        Identity {
            cert,
            key,
            fingerprint,
        }
    }

    pub fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.clone_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let fp = Fingerprint::of(b"hello");
        assert_eq!(fp.as_str().len(), 64);
        assert_eq!(fp.grouped().split(' ').count(), 16);
        assert_eq!(Fingerprint::parse(fp.as_str()), Some(fp.clone()));
        assert_eq!(Fingerprint::parse("xyz"), None);
    }

    #[test]
    fn identity_persists() {
        let dir = std::env::temp_dir().join(format!("illum-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = Identity::load_or_create(&dir).unwrap();
        let second = Identity::load_or_create(&dir).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_ne!(first.fingerprint, Identity::generate().unwrap().fingerprint);
    }
}
