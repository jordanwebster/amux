use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct,
    DistinguishedName as TlsDistinguishedName, Error as TlsError, ServerConfig, SignatureScheme,
};

use crate::paths::default_data_dir;
use crate::trust::{SharedTrustStore, TrustStore};
use crate::{HostId, audit, dispatcher};

const DEVICE_KEY_FILE: &str = "device.key";
const HOST_ID_FILE: &str = "host_id";
const HOST_ID_LEN: usize = 16;
const ED25519_SEED_LEN: usize = 32;
pub(crate) const ED25519_PUBKEY_LEN: usize = 32;
const ED25519_PKCS8_V1_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Debug, thiserror::Error)]
pub(crate) enum IdentityError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{field} must be {expected} bytes, got {actual}")]
    InvalidLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("failed to generate Ed25519 device key")]
    KeyGeneration,
    #[error("invalid Ed25519 PKCS#8 v1 device key")]
    InvalidDeviceKey,
    #[error("invalid Ed25519 device certificate")]
    InvalidDeviceCertificate,
    #[error("TLS config error: {0}")]
    TlsConfig(String),
    #[error("pubkey is already trusted for host {0}")]
    DuplicatePubkey(HostId),
    #[error("trust store lock is poisoned")]
    TrustStorePoisoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceIdentity {
    pub(crate) host_id: HostId,
    private_key_pkcs8: Vec<u8>,
    pubkey: Vec<u8>,
}

impl DeviceIdentity {
    #[allow(dead_code)]
    pub(crate) fn public_key(&self) -> &[u8] {
        &self.pubkey
    }

    #[cfg(test)]
    pub(crate) fn private_key_pkcs8(&self) -> &[u8] {
        &self.private_key_pkcs8
    }

    #[cfg(test)]
    pub(crate) fn for_test(host_id: HostId) -> Self {
        Self::from_parts(host_id, generate_ed25519_pkcs8_v1().unwrap()).unwrap()
    }

    fn from_parts(host_id: HostId, private_key_pkcs8: Vec<u8>) -> Result<Self, IdentityError> {
        let seed = ed25519_seed_from_pkcs8_v1(&private_key_pkcs8)?;
        let key_pair = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| IdentityError::InvalidDeviceKey)?;
        Ok(Self {
            host_id,
            private_key_pkcs8,
            pubkey: key_pair.public_key().as_ref().to_vec(),
        })
    }

    pub(crate) fn certificate_der(&self) -> Result<Vec<u8>, IdentityError> {
        let signing_key = KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(self.private_key_pkcs8.as_slice()),
            &rcgen::PKCS_ED25519,
        )
        .map_err(|_| IdentityError::InvalidDeviceKey)?;

        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, format!("amux:{}", self.host_id));
        let mut params = CertificateParams::default();
        params.distinguished_name = distinguished_name;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let cert = params
            .self_signed(&signing_key)
            .map_err(|_| IdentityError::InvalidDeviceCertificate)?;
        Ok(cert.der().as_ref().to_vec())
    }

    pub(crate) fn server_tls_config(
        &self,
        trust_store: SharedTrustStore,
    ) -> Result<ServerConfig, IdentityError> {
        let verifier = std::sync::Arc::new(PinnedClientCertVerifier::new(trust_store));
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.private_key_pkcs8.clone()));
        let mut config = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![CertificateDer::from(self.certificate_der()?)], key)
            .map_err(|error| IdentityError::TlsConfig(error.to_string()))?;
        config.alpn_protocols = vec![b"h2".to_vec()];
        Ok(config)
    }

    pub(crate) fn client_tls_config_for_peer(
        &self,
        trust_store: SharedTrustStore,
        peer: HostId,
    ) -> Result<ClientConfig, IdentityError> {
        let verifier = std::sync::Arc::new(PinnedServerCertVerifier::new(peer, trust_store));
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.private_key_pkcs8.clone()));
        let mut config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![CertificateDer::from(self.certificate_der()?)], key)
            .map_err(|error| IdentityError::TlsConfig(error.to_string()))?;
        config.alpn_protocols = vec![b"h2".to_vec()];
        Ok(config)
    }
}

pub(crate) fn host_id_for_certificate(
    trust_store: &TrustStore,
    cert: &CertificateDer<'_>,
) -> Result<Option<HostId>, IdentityError> {
    let pubkey = ed25519_public_key_from_certificate(cert)?;
    Ok(trust_store.host_id_for_pubkey(&pubkey))
}

fn emit_mtls_audit(reason: impl fmt::Display) {
    dispatcher::mark_mtls_audit_emitted();
    audit::auth_mtls_handshake_failure(reason);
}

fn emit_mtls_audit_for_host(host_id: HostId, reason: impl fmt::Display) {
    dispatcher::mark_mtls_audit_emitted();
    audit::auth_mtls_handshake_failure_for_host(host_id, reason);
}

#[derive(Clone)]
struct PinnedClientCertVerifier {
    trust_store: SharedTrustStore,
    supported_algs: WebPkiSupportedAlgorithms,
}

impl PinnedClientCertVerifier {
    fn new(trust_store: SharedTrustStore) -> Self {
        Self {
            trust_store,
            supported_algs: rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        }
    }
}

impl fmt::Debug for PinnedClientCertVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedClientCertVerifier")
            .finish_non_exhaustive()
    }
}

impl ClientCertVerifier for PinnedClientCertVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[TlsDistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, TlsError> {
        let pubkey = ed25519_public_key_from_certificate(end_entity).map_err(|_| {
            emit_mtls_audit("client certificate pubkey decode failed");
            TlsError::InvalidCertificate(CertificateError::BadEncoding)
        })?;
        let trust_store = self
            .trust_store
            .read()
            .map_err(|_| TlsError::General("trust store lock is poisoned".to_string()))?;
        if trust_store.host_id_for_pubkey(&pubkey).is_some() {
            Ok(ClientCertVerified::assertion())
        } else {
            emit_mtls_audit("client certificate pubkey is not trusted");
            Err(TlsError::InvalidCertificate(
                CertificateError::UnknownIssuer,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
    }
}

#[derive(Clone)]
struct PinnedServerCertVerifier {
    expected_host_id: HostId,
    trust_store: SharedTrustStore,
    supported_algs: WebPkiSupportedAlgorithms,
}

impl PinnedServerCertVerifier {
    fn new(expected_host_id: HostId, trust_store: SharedTrustStore) -> Self {
        Self {
            expected_host_id,
            trust_store,
            supported_algs: rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        }
    }
}

impl fmt::Debug for PinnedServerCertVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedServerCertVerifier")
            .field("expected_host_id", &self.expected_host_id)
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for PinnedServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let pubkey = ed25519_public_key_from_certificate(end_entity).map_err(|_| {
            emit_mtls_audit_for_host(
                self.expected_host_id,
                "server certificate pubkey decode failed",
            );
            TlsError::InvalidCertificate(CertificateError::BadEncoding)
        })?;
        let trust_store = self
            .trust_store
            .read()
            .map_err(|_| TlsError::General("trust store lock is poisoned".to_string()))?;
        match trust_store.pubkey_for_host(self.expected_host_id) {
            Some(expected) if expected == pubkey.as_slice() => Ok(ServerCertVerified::assertion()),
            Some(_) => {
                emit_mtls_audit_for_host(
                    self.expected_host_id,
                    "server certificate pubkey does not match trust store",
                );
                Err(TlsError::InvalidCertificate(CertificateError::BadSignature))
            }
            None => {
                emit_mtls_audit_for_host(
                    self.expected_host_id,
                    "server certificate host is not trusted",
                );
                Err(TlsError::InvalidCertificate(
                    CertificateError::UnknownIssuer,
                ))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
    }
}

pub(crate) struct DeviceFiles {
    pub(crate) identity: DeviceIdentity,
    pub(crate) trust_store: TrustStore,
}

pub(crate) fn default_device_files_ready() -> bool {
    let data_dir = default_data_dir();
    data_dir_mode_ready(&data_dir)
        && load_device_identity_in(&data_dir).is_ok()
        && TrustStore::load_in(&data_dir).is_ok()
}

pub(crate) fn ensure_default_device_files() -> Result<DeviceIdentity, IdentityError> {
    ensure_device_files_in(&default_data_dir())
}

pub(crate) fn ensure_device_files_in(data_dir: &Path) -> Result<DeviceIdentity, IdentityError> {
    Ok(ensure_device_files_with_trust_in(data_dir)?.identity)
}

pub(crate) fn ensure_device_files_with_trust_in(
    data_dir: &Path,
) -> Result<DeviceFiles, IdentityError> {
    let identity = load_or_create_device_identity_in(data_dir)?;
    let trust_store = TrustStore::load_or_create_in(data_dir)?;
    Ok(DeviceFiles {
        identity,
        trust_store,
    })
}

pub(crate) fn load_or_create_device_identity_in(
    data_dir: &Path,
) -> Result<DeviceIdentity, IdentityError> {
    ensure_data_dir(data_dir)?;
    let host_id = load_or_create_host_id(&host_id_path(data_dir))?;
    let private_key_pkcs8 = load_or_create_device_key(&device_key_path(data_dir))?;
    DeviceIdentity::from_parts(host_id, private_key_pkcs8)
}

fn load_device_identity_in(data_dir: &Path) -> Result<DeviceIdentity, IdentityError> {
    let host_id = load_host_id(&host_id_path(data_dir))?;
    let private_key_pkcs8 = load_device_key(&device_key_path(data_dir))?;
    DeviceIdentity::from_parts(host_id, private_key_pkcs8)
}

fn load_or_create_host_id(path: &Path) -> Result<HostId, IdentityError> {
    match load_host_id(path) {
        Ok(host_id) => Ok(host_id),
        Err(IdentityError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let host_id = HostId::new_v4();
            match create_private_file(path, host_id.as_bytes()) {
                Ok(()) => Ok(host_id),
                Err(IdentityError::Io(error))
                    if error.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    load_host_id(path)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn load_host_id(path: &Path) -> Result<HostId, IdentityError> {
    ensure_private_file_mode(path)?;
    let bytes = fs::read(path)?;
    validate_len(HOST_ID_FILE, &bytes, HOST_ID_LEN)?;
    let mut host_id = [0_u8; HOST_ID_LEN];
    host_id.copy_from_slice(&bytes);
    Ok(HostId::from_bytes(host_id))
}

fn load_or_create_device_key(path: &Path) -> Result<Vec<u8>, IdentityError> {
    match load_device_key(path) {
        Ok(device_key) => Ok(device_key),
        Err(IdentityError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let device_key = generate_ed25519_pkcs8_v1()?;
            match create_private_file(path, &device_key) {
                Ok(()) => Ok(device_key),
                Err(IdentityError::Io(error))
                    if error.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    load_device_key(path)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn load_device_key(path: &Path) -> Result<Vec<u8>, IdentityError> {
    ensure_private_file_mode(path)?;
    let private_key_pkcs8 = fs::read(path)?;
    ed25519_seed_from_pkcs8_v1(&private_key_pkcs8)?;
    Ok(private_key_pkcs8)
}

fn generate_ed25519_pkcs8_v1() -> Result<Vec<u8>, IdentityError> {
    let rng = SystemRandom::new();
    let mut seed = [0_u8; ED25519_SEED_LEN];
    rng.fill(&mut seed)
        .map_err(|_| IdentityError::KeyGeneration)?;
    let mut private_key_pkcs8 =
        Vec::with_capacity(ED25519_PKCS8_V1_PREFIX.len() + ED25519_SEED_LEN);
    private_key_pkcs8.extend_from_slice(&ED25519_PKCS8_V1_PREFIX);
    private_key_pkcs8.extend_from_slice(&seed);
    Ok(private_key_pkcs8)
}

fn ed25519_seed_from_pkcs8_v1(pkcs8: &[u8]) -> Result<&[u8], IdentityError> {
    let expected = ED25519_PKCS8_V1_PREFIX.len() + ED25519_SEED_LEN;
    if pkcs8.len() != expected || !pkcs8.starts_with(&ED25519_PKCS8_V1_PREFIX) {
        return Err(IdentityError::InvalidDeviceKey);
    }
    Ok(&pkcs8[ED25519_PKCS8_V1_PREFIX.len()..])
}

pub(crate) fn ed25519_public_key_from_certificate(
    cert: &CertificateDer<'_>,
) -> Result<[u8; ED25519_PUBKEY_LEN], IdentityError> {
    let parsed =
        ParsedCertificate::try_from(cert).map_err(|_| IdentityError::InvalidDeviceCertificate)?;
    ed25519_public_key_from_spki(parsed.subject_public_key_info().as_ref())
}

fn ed25519_public_key_from_spki(spki: &[u8]) -> Result<[u8; ED25519_PUBKEY_LEN], IdentityError> {
    let expected = ED25519_SPKI_PREFIX.len() + ED25519_PUBKEY_LEN;
    if spki.len() != expected || !spki.starts_with(&ED25519_SPKI_PREFIX) {
        return Err(IdentityError::InvalidDeviceCertificate);
    }
    let mut pubkey = [0_u8; ED25519_PUBKEY_LEN];
    pubkey.copy_from_slice(&spki[ED25519_SPKI_PREFIX.len()..]);
    Ok(pubkey)
}

pub(crate) fn validate_len(
    field: &'static str,
    bytes: &[u8],
    expected: usize,
) -> Result<(), IdentityError> {
    if bytes.len() != expected {
        return Err(IdentityError::InvalidLength {
            field,
            expected,
            actual: bytes.len(),
        });
    }
    Ok(())
}

pub(crate) fn create_private_file(path: &Path, bytes: &[u8]) -> Result<(), IdentityError> {
    if let Some(parent) = path.parent() {
        ensure_data_dir(parent)?;
    }
    let tmp = temp_path_for(path);
    let mut options = private_write_options();
    options.create_new(true);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    match fs::hard_link(&tmp, path) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(IdentityError::Io(error))
        }
    }
}

pub(crate) fn atomic_replace_private(path: &Path, bytes: &[u8]) -> Result<(), IdentityError> {
    if let Some(parent) = path.parent() {
        ensure_data_dir(parent)?;
    }
    let tmp = temp_path_for(path);
    let mut options = private_write_options();
    options.create_new(true);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    match replace_file(&tmp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

fn private_write_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
}

fn ensure_data_dir(path: &Path) -> Result<(), IdentityError> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn data_dir_mode_ready(path: &Path) -> bool {
    #[cfg(unix)]
    {
        let Ok(metadata) = fs::metadata(path) else {
            return false;
        };
        metadata.permissions().mode() & 0o777 == 0o700
    }
    #[cfg(not(unix))]
    {
        path.is_dir()
    }
}

pub(crate) fn ensure_private_file_mode(path: &Path) -> Result<(), IdentityError> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
fn replace_file(from: &Path, to: &Path) -> Result<(), IdentityError> {
    fs::rename(from, to)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> Result<(), IdentityError> {
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let from = wide_path(from);
    let to = wide_path(to);
    let replaced = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        return Err(IdentityError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("amux-data");
    path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()))
}

fn device_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DEVICE_KEY_FILE)
}

fn host_id_path(data_dir: &Path) -> PathBuf {
    data_dir.join(HOST_ID_FILE)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use rustls::pki_types::{CertificateDer, UnixTime};
    use rustls::server::danger::ClientCertVerifier as _;
    use tempfile::TempDir;

    use super::*;
    use crate::trust::{Reachability, TrustEntry, TrustStorePairingUpdate};

    fn temp_data_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    fn trust_store_with_pubkey(host_id: HostId, pubkey: &[u8]) -> TrustStore {
        let mut store = TrustStore::default();
        store.insert_for_test(
            host_id,
            TrustEntry {
                pubkey: pubkey.to_vec(),
                name: "peer".to_string(),
                paired_at: DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );
        store
    }

    #[test]
    fn device_identity_is_created_and_reused() {
        let dir = temp_data_dir();

        let first = load_or_create_device_identity_in(dir.path()).unwrap();
        let second = load_or_create_device_identity_in(dir.path()).unwrap();

        assert_eq!(first.host_id, second.host_id);
        assert_eq!(first.public_key(), second.public_key());
        assert_eq!(first.private_key_pkcs8(), second.private_key_pkcs8());
        assert!(
            first
                .private_key_pkcs8()
                .starts_with(&ED25519_PKCS8_V1_PREFIX)
        );
        assert_eq!(
            fs::read(host_id_path(dir.path())).unwrap().len(),
            HOST_ID_LEN
        );

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(host_id_path(dir.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(device_key_path(dir.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn device_certificate_contains_persisted_public_key() {
        let dir = temp_data_dir();
        let identity = load_or_create_device_identity_in(dir.path()).unwrap();
        let cert = CertificateDer::from(identity.certificate_der().unwrap());

        assert_eq!(
            ed25519_public_key_from_certificate(&cert).unwrap(),
            identity.public_key()
        );
        assert!(
            identity
                .server_tls_config(Arc::new(std::sync::RwLock::new(TrustStore::default())))
                .is_ok()
        );
    }

    #[test]
    fn pinned_client_cert_verifier_accepts_only_trusted_device_pubkeys() {
        let trusted_dir = temp_data_dir();
        let untrusted_dir = temp_data_dir();
        let trusted = load_or_create_device_identity_in(trusted_dir.path()).unwrap();
        let untrusted = load_or_create_device_identity_in(untrusted_dir.path()).unwrap();
        let trust_store = trust_store_with_pubkey(trusted.host_id, trusted.public_key());
        let verifier = PinnedClientCertVerifier::new(Arc::new(std::sync::RwLock::new(trust_store)));
        let now = UnixTime::since_unix_epoch(Duration::from_secs(1_700_000_000));

        let trusted_cert = CertificateDer::from(trusted.certificate_der().unwrap());
        verifier
            .verify_client_cert(&trusted_cert, &[], now)
            .unwrap();

        let untrusted_cert = CertificateDer::from(untrusted.certificate_der().unwrap());
        assert!(
            verifier
                .verify_client_cert(&untrusted_cert, &[], now)
                .is_err()
        );
    }

    #[test]
    fn pinned_client_cert_verifier_reads_live_trust_store_updates() {
        let peer_dir = temp_data_dir();
        let peer = load_or_create_device_identity_in(peer_dir.path()).unwrap();
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let verifier = PinnedClientCertVerifier::new(trust_store.clone());
        let now = UnixTime::since_unix_epoch(Duration::from_secs(1_700_000_000));
        let cert = CertificateDer::from(peer.certificate_der().unwrap());

        assert!(verifier.verify_client_cert(&cert, &[], now).is_err());

        trust_store
            .write()
            .unwrap()
            .upsert_paired_peer(
                peer.host_id,
                peer.public_key().to_vec(),
                "peer".to_string(),
                Reachability::Cloud,
                DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
            )
            .unwrap();

        verifier.verify_client_cert(&cert, &[], now).unwrap();

        assert_eq!(
            trust_store
                .write()
                .unwrap()
                .remove(peer.host_id)
                .unwrap()
                .pubkey,
            peer.public_key()
        );
        assert!(
            verifier.verify_client_cert(&cert, &[], now).is_err(),
            "revoked peer certificate must not be accepted from a stale cache"
        );
    }

    #[test]
    fn pinned_server_cert_verifier_reads_live_trust_store_updates_and_replacements() {
        let peer_dir = temp_data_dir();
        let peer = load_or_create_device_identity_in(peer_dir.path()).unwrap();
        let rotated_peer = DeviceIdentity::for_test(peer.host_id);
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let verifier = PinnedServerCertVerifier::new(peer.host_id, trust_store.clone());
        let now = UnixTime::since_unix_epoch(Duration::from_secs(1_700_000_000));
        let server_name = ServerName::try_from("peer.amux.invalid").unwrap();
        let peer_cert = CertificateDer::from(peer.certificate_der().unwrap());
        let rotated_cert = CertificateDer::from(rotated_peer.certificate_der().unwrap());

        assert!(
            verifier
                .verify_server_cert(&peer_cert, &[], &server_name, &[], now)
                .is_err()
        );

        trust_store
            .write()
            .unwrap()
            .upsert_paired_peer(
                peer.host_id,
                peer.public_key().to_vec(),
                "peer".to_string(),
                Reachability::Cloud,
                DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
            )
            .unwrap();
        verifier
            .verify_server_cert(&peer_cert, &[], &server_name, &[], now)
            .unwrap();

        assert_eq!(
            trust_store
                .write()
                .unwrap()
                .upsert_paired_peer(
                    peer.host_id,
                    rotated_peer.public_key().to_vec(),
                    "peer-rotated".to_string(),
                    Reachability::Cloud,
                    DateTime::<Utc>::from_timestamp(300, 0).unwrap(),
                )
                .unwrap(),
            TrustStorePairingUpdate::PubkeyReplacementRequired
        );
        assert!(
            verifier
                .verify_server_cert(&peer_cert, &[], &server_name, &[], now)
                .is_err()
        );
        assert!(
            verifier
                .verify_server_cert(&rotated_cert, &[], &server_name, &[], now)
                .is_err()
        );

        trust_store
            .write()
            .unwrap()
            .replace_paired_peer_after_teardown(
                peer.host_id,
                rotated_peer.public_key().to_vec(),
                "peer-rotated".to_string(),
                Reachability::Cloud,
                DateTime::<Utc>::from_timestamp(300, 0).unwrap(),
            )
            .unwrap();
        assert!(
            verifier
                .verify_server_cert(&peer_cert, &[], &server_name, &[], now)
                .is_err()
        );
        verifier
            .verify_server_cert(&rotated_cert, &[], &server_name, &[], now)
            .unwrap();
    }

    #[test]
    fn concurrent_first_run_uses_one_persisted_identity() {
        let dir = temp_data_dir();
        let data_dir = Arc::new(dir.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let data_dir = data_dir.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    load_or_create_device_identity_in(&data_dir).unwrap()
                })
            })
            .collect::<Vec<_>>();

        let identities = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        let first = identities.first().unwrap();
        assert!(
            identities
                .iter()
                .all(|identity| identity.host_id == first.host_id
                    && identity.public_key() == first.public_key())
        );

        let reloaded = load_or_create_device_identity_in(&data_dir).unwrap();
        assert_eq!(reloaded.host_id, first.host_id);
        assert_eq!(reloaded.public_key(), first.public_key());
    }

    #[test]
    fn invalid_host_id_length_is_rejected() {
        let dir = temp_data_dir();
        fs::create_dir_all(dir.path()).unwrap();
        create_private_file(&host_id_path(dir.path()), b"short").unwrap();

        let error = load_or_create_device_identity_in(dir.path()).unwrap_err();
        assert!(matches!(
            error,
            IdentityError::InvalidLength {
                field: HOST_ID_FILE,
                expected: HOST_ID_LEN,
                actual: 5
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn readiness_requires_private_data_dir_mode() {
        let dir = temp_data_dir();
        ensure_device_files_in(dir.path()).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        assert!(!data_dir_mode_ready(dir.path()));

        ensure_device_files_in(dir.path()).unwrap();
        assert!(data_dir_mode_ready(dir.path()));
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
