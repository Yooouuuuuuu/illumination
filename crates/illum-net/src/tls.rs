// SPDX-License-Identifier: GPL-3.0-only
//! TLS 1.3 configuration with certificates on both sides.
//!
//! Certificates are self-signed, so there is no chain to validate: the verifiers below accept
//! any certificate but still check every handshake signature. Trust is decided right after the
//! handshake by comparing the peer's fingerprint with the trust store (see `session`).

use crate::identity::{Fingerprint, Identity};
use crate::{NetError, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
};
use std::sync::Arc;

/// ALPN identifier; a peer speaking anything else is rejected during the handshake.
pub const ALPN: &[u8] = b"illum/1";

/// Name sent in the TLS ClientHello. Never checked: peers are identified by fingerprint.
pub const SERVER_NAME: &str = "illumination.invalid";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct PinnedLater {
    algorithms: WebPkiSupportedAlgorithms,
}

impl PinnedLater {
    fn new(provider: &CryptoProvider) -> PinnedLater {
        PinnedLater {
            algorithms: provider.signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for PinnedLater {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for PinnedLater {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

pub fn server_config(identity: &Identity) -> Result<Arc<ServerConfig>> {
    let provider = provider();
    let verifier = Arc::new(PinnedLater::new(&provider));
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.cert.clone()], identity.private_key())?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(config))
}

pub fn client_config(identity: &Identity) -> Result<Arc<ClientConfig>> {
    let provider = provider();
    let verifier = Arc::new(PinnedLater::new(&provider));
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![identity.cert.clone()], identity.private_key())?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(config))
}

/// Fingerprint of the peer's end-entity certificate after a completed handshake.
pub fn peer_fingerprint(certs: Option<&[CertificateDer<'static>]>) -> Result<Fingerprint> {
    certs
        .and_then(|c| c.first())
        .map(|c| Fingerprint::of(c.as_ref()))
        .ok_or_else(|| NetError::Certificate("peer presented no certificate".into()))
}
