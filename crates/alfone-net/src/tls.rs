use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::identity::{fingerprint_of, hex};
use crate::NetError;

pub const ALPN: &[u8] = b"alfone/1";

/// How often QUIC sends a keepalive when the link is idle.
///
/// Input does not generate traffic while the user is not moving the mouse, so a
/// keepalive is what makes a dead server detectable quickly.
const KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(2);
/// Give up on a silent peer after ~3 missed keepalives. LAN links are reliable,
/// so a shorter timeout than the 30 s default is worth the faster failover.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

/// Accepts exactly one certificate fingerprint, or records it on first use.
///
/// `expected = None` means trust-on-first-use: the TLS handshake succeeds and
/// the caller must compare [`crate::short_code`] out of band before sending
/// input. The observed fingerprint is stored so the caller can persist it.
#[derive(Debug)]
pub struct FingerprintVerifier {
    expected: Option<[u8; 32]>,
    observed: std::sync::Mutex<Option<[u8; 32]>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl FingerprintVerifier {
    pub fn new(expected: Option<[u8; 32]>) -> Self {
        Self {
            expected,
            observed: std::sync::Mutex::new(None),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }

    pub fn observed(&self) -> Option<[u8; 32]> {
        *self.observed.lock().expect("verifier mutex")
    }
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = fingerprint_of(end_entity.as_ref());
        *self.observed.lock().expect("verifier mutex") = Some(actual);
        match self.expected {
            Some(expected) if actual != expected => Err(rustls::Error::General(format!(
                "server fingerprint mismatch: expected {}, got {}",
                hex(&expected),
                hex(&actual)
            ))),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(crate) fn server_quinn_config(cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<quinn::ServerConfig, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let key = PrivatePkcs8KeyDer::from(key_der);
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(cert_der)], key.into())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicServerConfig::try_from(tls)
        .map_err(|e| NetError::Tls(rustls::Error::General(e.to_string())))?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(quic));

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(IDLE_TIMEOUT)
            .map_err(|e| NetError::Protocol(e.to_string()))?,
    ));
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    transport.datagram_receive_buffer_size(Some(4 * 1024 * 1024));
    config.transport = Arc::new(transport);
    Ok(config)
}

pub(crate) fn client_quinn_config(
    verifier: Arc<FingerprintVerifier>,
) -> Result<quinn::ClientConfig, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicClientConfig::try_from(tls)
        .map_err(|e| NetError::Tls(rustls::Error::General(e.to_string())))?;
    let mut config = quinn::ClientConfig::new(Arc::new(quic));

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(IDLE_TIMEOUT)
            .map_err(|e| NetError::Protocol(e.to_string()))?,
    ));
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    transport.datagram_receive_buffer_size(Some(4 * 1024 * 1024));
    config.transport_config(Arc::new(transport));
    Ok(config)
}
