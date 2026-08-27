use anyhow::Context;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ED25519};
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::crypto::verify_tls13_signature_with_raw_key;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, ServerConfig, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use std::sync::Arc;
use x509_parser::prelude::FromDer;

use super::identity::NodeIdentity;
use super::protocol::PeerTlsIdentity;

pub fn certificate_for_identity(identity: &NodeIdentity) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let pem = identity.private_key_pem()?;
    let key_pair = KeyPair::from_pem(&pem).context("failed to load node key into certificate")?;
    if key_pair.algorithm() != &PKCS_ED25519 {
        anyhow::bail!("node certificate key is not ed25519");
    }
    let mut params = CertificateParams::new(vec![
        identity.node_id.clone(),
        "codex-switch-peer".to_string(),
        "localhost".to_string(),
    ])?;
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, identity.fingerprint());
    params.distinguished_name = distinguished_name;
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let certificate = params.self_signed(&key_pair)?;
    let cert_der = CertificateDer::from(certificate.der().to_vec());
    let key_der = PrivateKeyDer::from_pem_slice(pem.as_bytes())
        .context("failed to parse node private key der")?;
    Ok((cert_der, key_der))
}

pub fn public_key_from_cert(cert: &CertificateDer<'_>) -> anyhow::Result<[u8; 32]> {
    let (_, parsed) =
        x509_parser::certificate::X509Certificate::from_der(cert.as_ref()).context("invalid peer certificate")?;
    let bytes = parsed
        .public_key()
        .subject_public_key
        .data
        .to_vec();
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("peer certificate public key must be 32 bytes"))?;
    Ok(bytes)
}

pub fn tls_identity_from_certs(certs: &[CertificateDer<'_>]) -> anyhow::Result<PeerTlsIdentity> {
    let cert = certs
        .first()
        .ok_or_else(|| anyhow::anyhow!("peer did not present a certificate"))?;
    let public_key = public_key_from_cert(cert)?;
    Ok(PeerTlsIdentity {
        fingerprint: super::identity::fingerprint_from_public_key(&public_key),
        public_key,
    })
}

pub fn server_config(identity: &NodeIdentity) -> anyhow::Result<ServerConfig> {
    let provider = crypto_provider();
    let (cert, key) = certificate_for_identity(identity)?;
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(Arc::new(AnyEd25519ClientCertVerifier))
        .with_single_cert(vec![cert], key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

pub fn pinned_client_config(
    identity: &NodeIdentity,
    expected_public_key: [u8; 32],
) -> anyhow::Result<ClientConfig> {
    client_config(identity, Some(expected_public_key))
}

pub fn tofu_client_config(identity: &NodeIdentity) -> anyhow::Result<ClientConfig> {
    client_config(identity, None)
}

fn client_config(
    identity: &NodeIdentity,
    expected_public_key: Option<[u8; 32]>,
) -> anyhow::Result<ClientConfig> {
    let provider = crypto_provider();
    let (cert, key) = certificate_for_identity(identity)?;
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PeerServerCertVerifier { expected_public_key }))
        .with_client_auth_cert(vec![cert], key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

fn crypto_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct AnyEd25519ClientCertVerifier;

impl ClientCertVerifier for AnyEd25519ClientCertVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        public_key_from_cert(end_entity).map_err(to_tls_error)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct PeerServerCertVerifier {
    expected_public_key: Option<[u8; 32]>,
}

impl ServerCertVerifier for PeerServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let public_key = public_key_from_cert(end_entity).map_err(to_tls_error)?;
        if let Some(expected) = self.expected_public_key
            && public_key != expected
        {
            return Err(rustls::Error::General(
                "peer certificate public key does not match paired identity".to_string(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

fn verify_tls13_signature(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    let public_key = public_key_from_cert(cert).map_err(to_tls_error)?;
    let spki = rustls::pki_types::SubjectPublicKeyInfoDer::from(ed25519_spki(&public_key).to_vec());
    verify_tls13_signature_with_raw_key(
        message,
        &spki,
        dss,
        &rustls::crypto::ring::default_provider().signature_verification_algorithms,
    )
}

fn ed25519_spki(public_key: &[u8; 32]) -> [u8; 44] {
    let mut spki = [0_u8; 44];
    spki[..12].copy_from_slice(&[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ]);
    spki[12..].copy_from_slice(public_key);
    spki
}

fn to_tls_error(err: anyhow::Error) -> rustls::Error {
    rustls::Error::General(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::identity::NodeIdentity;

    #[test]
    fn certificate_public_key_matches_identity() {
        let identity = NodeIdentity::generate("box".to_string()).unwrap();
        let (cert, _) = certificate_for_identity(&identity).unwrap();
        let public_key = public_key_from_cert(&cert).unwrap();
        assert_eq!(public_key, identity.public_key_bytes());
    }
}
