//! Synchronous loopback TLS transport for the seat control protocol.
//!
//! The service creates one P-256 self-signed identity and random bearer below
//! its supplied secret root. It publishes only a loopback socket. A client
//! reads the existing leaf, token, and endpoint; it never creates credentials.
//! The client completes a pinned rustls handshake before writing the request,
//! so a port squatter receives no bearer bytes. The server compares bearer
//! bytes in constant time. Each connection carries one framed JSON request and
//! response, with no HTTP parser, redirect path, or plaintext fallback.

use crate::backend::PlatformBackend;
use crate::persistence::{read_existing_file, CandidateKind, SecretRoot};
use crate::protocol::{
    read_json_frame, write_json_frame, ApiError, BearerToken, Command, ErrorCode, FrameError,
    Request, Response, TokenError, CONTROL_SCHEMA_VERSION,
};
use crate::service::SeatService;
use rand::RngCore;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde::{Deserialize, Serialize};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use subtle::ConstantTimeEq;

pub const IDENTITY_FILE: &str = "control-identity.json";
pub const CERT_FILE: &str = "control-cert.pem";
pub const TOKEN_FILE: &str = "control-token";
pub const ENDPOINT_FILE: &str = "control-endpoint.json";
const CONFIG_SCHEMA_VERSION: u32 = 1;
const CONFIG_FILE_CAP: usize = 64 * 1024;
const ALPN: &[u8] = b"punktfunk-seats/1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 12;

pub struct ControlServer<B> {
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    token: BearerToken,
    service: Arc<SeatService<B>>,
    active: Arc<AtomicUsize>,
}

struct ConnectionPermit(Arc<AtomicUsize>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn acquire_connection(active: &Arc<AtomicUsize>) -> Option<ConnectionPermit> {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < MAX_CONNECTIONS).then_some(count + 1)
        })
        .ok()
        .map(|_| ConnectionPermit(active.clone()))
}

impl<B: PlatformBackend> ControlServer<B> {
    pub fn bind(address: SocketAddr, service: Arc<SeatService<B>>) -> Result<Self, ControlError> {
        if !address.ip().is_loopback() {
            return Err(ControlError::Config(format!(
                "control address {address} is not loopback"
            )));
        }
        let auth = ServerAuth::load_or_create(service.store().root())?;
        let listener = TcpListener::bind(address)?;
        let local = listener.local_addr()?;
        publish_endpoint(service.store().root(), local)?;
        Ok(Self {
            listener,
            config: auth.config,
            token: auth.token,
            service,
            active: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn serve_once(&self) -> Result<(), ControlError> {
        let (stream, peer) = self.listener.accept()?;
        let _permit =
            acquire_connection(&self.active).ok_or(ControlError::Busy(MAX_CONNECTIONS))?;
        handle_connection(
            stream,
            peer,
            self.config.clone(),
            self.token.clone(),
            self.service.clone(),
        )
    }

    pub fn serve(self) -> Result<(), ControlError> {
        let stop = AtomicBool::new(false);
        self.serve_until(&stop)
    }

    pub fn serve_until(self, stop: &AtomicBool) -> Result<(), ControlError> {
        self.listener.set_nonblocking(true)?;
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        while !stop.load(Ordering::Acquire) {
            reap_workers(&mut workers);
            match self.listener.accept() {
                Ok((stream, peer)) => {
                    let Some(permit) = acquire_connection(&self.active) else {
                        drop(stream);
                        continue;
                    };
                    let config = self.config.clone();
                    let token = self.token.clone();
                    let service = self.service.clone();
                    workers.push(std::thread::spawn(move || {
                        let _permit = permit;
                        let _ = handle_connection(stream, peer, config, token, service);
                    }));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        for worker in workers {
            let _ = worker.join();
        }
        Ok(())
    }
}

fn reap_workers(workers: &mut Vec<JoinHandle<()>>) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let worker = workers.swap_remove(index);
            let _ = worker.join();
        } else {
            index += 1;
        }
    }
}

pub struct ControlClient {
    address: SocketAddr,
    pin: [u8; 32],
    token: BearerToken,
}

impl ControlClient {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ClientError> {
        let root = root.as_ref();
        let cert_pem = read_existing_file(root, CERT_FILE, CONFIG_FILE_CAP)
            .map_err(|error| ClientError::Config(error.to_string()))?;
        let cert = CertificateDer::from_pem_slice(&cert_pem).map_err(|error| {
            ClientError::Config(format!("invalid control certificate: {error}"))
        })?;
        let pin = crate::tls::cert_fingerprint(cert.as_ref());

        let token_bytes = read_existing_file(root, TOKEN_FILE, 512)
            .map_err(|error| ClientError::Config(error.to_string()))?;
        let token_text = std::str::from_utf8(&token_bytes)
            .map_err(|error| ClientError::Config(format!("control token is not UTF-8: {error}")))?;
        let token = BearerToken::new(token_text.trim())?;
        let endpoint_bytes = read_existing_file(root, ENDPOINT_FILE, 4096)
            .map_err(|error| ClientError::Config(error.to_string()))?;
        let endpoint: EndpointFile = serde_json::from_slice(&endpoint_bytes)
            .map_err(|error| ClientError::Config(format!("invalid control endpoint: {error}")))?;
        if endpoint.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(ClientError::Config(format!(
                "control endpoint schema {} is unsupported",
                endpoint.schema_version
            )));
        }
        let address: SocketAddr = endpoint
            .address
            .parse()
            .map_err(|error| ClientError::Config(format!("invalid control address: {error}")))?;
        if !address.ip().is_loopback() {
            return Err(ClientError::Config(format!(
                "control endpoint {address} is not loopback"
            )));
        }
        Ok(Self {
            address,
            pin,
            token,
        })
    }

    pub fn request(&self, command: Command) -> Result<Response, ClientError> {
        let observed = Arc::new(Mutex::new(None));
        let config = client_config(self.pin, observed.clone())?;
        let mut socket = TcpStream::connect_timeout(&self.address, CONNECT_TIMEOUT)?;
        socket.set_read_timeout(Some(IO_TIMEOUT))?;
        socket.set_write_timeout(Some(IO_TIMEOUT))?;
        let name = ServerName::try_from("localhost").expect("localhost is a valid server name");
        let mut connection = rustls::ClientConnection::new(config, name)
            .map_err(|error| ClientError::Tls(error.to_string()))?;
        while connection.is_handshaking() {
            if let Err(error) = connection.complete_io(&mut socket) {
                return Err(handshake_error(error, self.pin, &observed));
            }
        }
        if connection.alpn_protocol() != Some(ALPN) {
            return Err(ClientError::Tls(
                "the peer did not negotiate punktfunk-seats/1".into(),
            ));
        }

        let request = Request::new(self.token.clone(), command);
        let mut stream = rustls::Stream::new(&mut connection, &mut socket);
        write_json_frame(&mut stream, &request)?;
        let response: Response = read_json_frame(&mut stream)?;
        let version = match &response {
            Response::Success { schema_version, .. } | Response::Error { schema_version, .. } => {
                *schema_version
            }
        };
        if version != CONTROL_SCHEMA_VERSION {
            return Err(ClientError::SchemaVersion(version));
        }
        Ok(response)
    }
}

fn handle_connection<B: PlatformBackend>(
    mut socket: TcpStream,
    peer: SocketAddr,
    config: Arc<rustls::ServerConfig>,
    expected_token: BearerToken,
    service: Arc<SeatService<B>>,
) -> Result<(), ControlError> {
    if !peer.ip().is_loopback() {
        return Err(ControlError::Config(format!(
            "non-loopback control peer {peer}"
        )));
    }
    socket.set_read_timeout(Some(IO_TIMEOUT))?;
    socket.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut connection = rustls::ServerConnection::new(config)
        .map_err(|error| ControlError::Tls(error.to_string()))?;
    while connection.is_handshaking() {
        connection.complete_io(&mut socket)?;
    }
    if connection.alpn_protocol() != Some(ALPN) {
        return Err(ControlError::Tls(
            "client did not negotiate punktfunk-seats/1".into(),
        ));
    }

    let mut stream = rustls::Stream::new(&mut connection, &mut socket);
    let request: Request = match read_json_frame(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let response = Response::error(frame_api_error(&error));
            let _ = write_json_frame(&mut stream, &response);
            return Ok(());
        }
    };
    let response = if !token_matches(&expected_token, &request.bearer) {
        Response::error(ApiError::new(
            ErrorCode::Unauthorized,
            "missing or invalid control bearer",
        ))
    } else if request.schema_version != CONTROL_SCHEMA_VERSION {
        Response::error(ApiError::new(
            ErrorCode::SchemaVersion,
            format!(
                "control schema {} is unsupported; expected {}",
                request.schema_version, CONTROL_SCHEMA_VERSION
            ),
        ))
    } else {
        match service.dispatch(request.command) {
            Ok(result) => Response::success(result),
            Err(error) => Response::error(error),
        }
    };
    write_json_frame(&mut stream, &response)?;
    Ok(())
}

pub fn token_matches(expected: &BearerToken, presented: &BearerToken) -> bool {
    bool::from(
        expected
            .expose()
            .as_bytes()
            .ct_eq(presented.expose().as_bytes()),
    )
}

struct ServerAuth {
    config: Arc<rustls::ServerConfig>,
    token: BearerToken,
}

impl ServerAuth {
    fn load_or_create(root: &SecretRoot) -> Result<Self, ControlError> {
        let identity = load_or_create_identity(root)?;
        let config = server_config(&identity)?;
        root.write_atomic(CERT_FILE, identity.cert_pem.as_bytes())?;
        let token = load_or_create_token(root)?;
        Ok(Self { config, token })
    }
}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    schema_version: u32,
    cert_pem: String,
    key_pem: String,
}

fn load_or_create_identity(root: &SecretRoot) -> Result<IdentityFile, ControlError> {
    let candidates = root.candidates(IDENTITY_FILE, CONFIG_FILE_CAP)?;
    if candidates.is_empty() {
        let identity = generate_identity()?;
        root.write_atomic(IDENTITY_FILE, &serde_json::to_vec_pretty(&identity)?)?;
        return Ok(identity);
    }
    let mut errors = Vec::new();
    for candidate in candidates {
        let parsed = serde_json::from_slice::<IdentityFile>(&candidate.bytes);
        match parsed {
            Ok(identity)
                if identity.schema_version == CONFIG_SCHEMA_VERSION
                    && server_config(&identity).is_ok() =>
            {
                if candidate.kind != CandidateKind::Live {
                    root.write_atomic(IDENTITY_FILE, &candidate.bytes)?;
                }
                root.cleanup_temporary(IDENTITY_FILE)?;
                return Ok(identity);
            }
            Ok(identity) => errors.push(format!(
                "identity schema {} or certificate/key pair is invalid",
                identity.schema_version
            )),
            Err(error) => errors.push(error.to_string()),
        }
    }
    Err(ControlError::Config(format!(
        "no valid service identity remains: {}",
        errors.join("; ")
    )))
}

fn generate_identity() -> Result<IdentityFile, ControlError> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|error| ControlError::Config(format!("generate control key: {error}")))?;
    let mut params =
        rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into(), "::1".into()])
            .map_err(|error| ControlError::Config(format!("control certificate params: {error}")))?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "punktfunk-seats");
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2040, 1, 1);
    let cert = params
        .self_signed(&key)
        .map_err(|error| ControlError::Config(format!("self-sign control certificate: {error}")))?;
    Ok(IdentityFile {
        schema_version: CONFIG_SCHEMA_VERSION,
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

fn server_config(identity: &IdentityFile) -> Result<Arc<rustls::ServerConfig>, ControlError> {
    let certs = CertificateDer::pem_slice_iter(identity.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ControlError::Tls(format!("parse control certificate: {error}")))?;
    let key = PrivateKeyDer::from_pem_slice(identity.key_pem.as_bytes())
        .map_err(|error| ControlError::Tls(format!("parse control key: {error}")))?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| ControlError::Tls(error.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| ControlError::Tls(error.to_string()))?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(config))
}

fn client_config(
    pin: [u8; 32],
    observed: Arc<Mutex<Option<[u8; 32]>>>,
) -> Result<Arc<rustls::ClientConfig>, ClientError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| ClientError::Tls(error.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(crate::tls::PinVerify::with_observed(
            Some(pin),
            observed,
        )))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(config))
}

fn load_or_create_token(root: &SecretRoot) -> Result<BearerToken, ControlError> {
    let candidates = root.candidates(TOKEN_FILE, 512)?;
    if candidates.is_empty() {
        let mut random = [0_u8; 32];
        rand::rng().fill_bytes(&mut random);
        let token = BearerToken::new(hex::encode(random))?;
        root.write_atomic(TOKEN_FILE, format!("{}\n", token.expose()).as_bytes())?;
        return Ok(token);
    }
    for candidate in candidates {
        let Ok(text) = std::str::from_utf8(&candidate.bytes) else {
            continue;
        };
        let Ok(token) = BearerToken::new(text.trim()) else {
            continue;
        };
        if candidate.kind != CandidateKind::Live {
            root.write_atomic(TOKEN_FILE, &candidate.bytes)?;
        }
        root.cleanup_temporary(TOKEN_FILE)?;
        return Ok(token);
    }
    Err(ControlError::Config(
        "no valid control token remains; refusing to rotate it implicitly".into(),
    ))
}

#[derive(Serialize, Deserialize)]
struct EndpointFile {
    schema_version: u32,
    address: String,
}

fn publish_endpoint(root: &SecretRoot, address: SocketAddr) -> Result<(), ControlError> {
    let endpoint = EndpointFile {
        schema_version: CONFIG_SCHEMA_VERSION,
        address: address.to_string(),
    };
    root.write_atomic(ENDPOINT_FILE, &serde_json::to_vec_pretty(&endpoint)?)?;
    Ok(())
}

fn handshake_error(
    error: io::Error,
    expected: [u8; 32],
    observed: &Arc<Mutex<Option<[u8; 32]>>>,
) -> ClientError {
    let seen = *observed.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(seen) = seen
        && seen != expected
    {
        return ClientError::PinMismatch {
            expected: hex::encode(expected),
            observed: hex::encode(seen),
        };
    }
    ClientError::Io(error)
}

fn frame_api_error(error: &FrameError) -> ApiError {
    match error {
        FrameError::TooLarge(_) => ApiError::new(ErrorCode::FrameTooLarge, error.to_string()),
        _ => ApiError::new(ErrorCode::MalformedFrame, error.to_string()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("control I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("control JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("control TLS failed: {0}")]
    Tls(String),
    #[error("control configuration failed: {0}")]
    Config(String),
    #[error("control connection limit ({0}) reached")]
    Busy(usize),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Token(#[from] TokenError),
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("control client configuration failed: {0}")]
    Config(String),
    #[error("control connection failed: {0}")]
    Io(#[from] io::Error),
    #[error("control TLS failed: {0}")]
    Tls(String),
    #[error(
        "certificate pin mismatch: expected {expected}, observed {observed}; no token was sent"
    )]
    PinMismatch { expected: String, observed: String },
    #[error("control response schema {0} is unsupported")]
    SchemaVersion(u32),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Token(#[from] TokenError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendError, PlatformBackend};
    use crate::model::{Ledger, PortPool, RuntimeStatus, Seat};
    use crate::protocol::{CommandResult, Diagnostic};
    use std::io::Read as _;

    #[derive(Clone, Copy)]
    struct FakeBackend;

    impl PlatformBackend for FakeBackend {
        fn provision(&self, _seat: &Seat) -> Result<(), BackendError> {
            Ok(())
        }

        fn start(&self, _seat: &Seat) -> Result<RuntimeStatus, BackendError> {
            Ok(RuntimeStatus::running())
        }

        fn stop(&self, _seat: &Seat) -> Result<RuntimeStatus, BackendError> {
            Ok(RuntimeStatus::stopped())
        }

        fn remove(&self, _seat: &Seat) -> Result<(), BackendError> {
            Ok(())
        }

        fn status(&self, _seat: &Seat) -> Result<RuntimeStatus, BackendError> {
            Ok(RuntimeStatus::stopped())
        }

        fn doctor(&self, _ledger: &Ledger) -> Result<Vec<Diagnostic>, BackendError> {
            Ok(Vec::new())
        }
    }

    fn service(path: &Path) -> Arc<SeatService<FakeBackend>> {
        Arc::new(SeatService::open(path, FakeBackend, PortPool::default()).unwrap())
    }

    #[test]
    fn valid_pin_and_bearer_dispatch_a_command() {
        let temp = tempfile::tempdir().unwrap();
        let server =
            ControlServer::bind("127.0.0.1:0".parse().unwrap(), service(temp.path())).unwrap();
        let client = ControlClient::open(temp.path()).unwrap();
        let serving = std::thread::spawn(move || server.serve_once());
        let response = client.request(Command::List).unwrap();
        assert!(matches!(
            response,
            Response::Success {
                result: CommandResult::List { ref seats },
                ..
            } if seats.is_empty()
        ));
        serving.join().unwrap().unwrap();
    }

    #[test]
    fn a_wrong_bearer_gets_a_structured_auth_error() {
        let temp = tempfile::tempdir().unwrap();
        let server =
            ControlServer::bind("127.0.0.1:0".parse().unwrap(), service(temp.path())).unwrap();
        let mut client = ControlClient::open(temp.path()).unwrap();
        client.token = BearerToken::new("fedcba9876543210").unwrap();
        let serving = std::thread::spawn(move || server.serve_once());
        let response = client.request(Command::Doctor).unwrap();
        assert!(matches!(
            response,
            Response::Error {
                error: ApiError {
                    code: ErrorCode::Unauthorized,
                    ..
                },
                ..
            }
        ));
        serving.join().unwrap().unwrap();
    }

    #[test]
    fn pin_mismatch_sends_no_application_bytes_to_a_squatter() {
        let temp = tempfile::tempdir().unwrap();
        let ours =
            ControlServer::bind("127.0.0.1:0".parse().unwrap(), service(temp.path())).unwrap();
        let address = ours.local_addr().unwrap();
        drop(ours);

        let squatter_identity = generate_identity().unwrap();
        let squatter_config = server_config(&squatter_identity).unwrap();
        let listener = TcpListener::bind(address).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let squatter = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut connection = rustls::ServerConnection::new(squatter_config).unwrap();
            let _ = connection.complete_io(&mut socket);
            let mut plaintext = Vec::new();
            let _ = connection.reader().read_to_end(&mut plaintext);
            *recorder.lock().unwrap() = plaintext;
        });

        let client = ControlClient::open(temp.path()).unwrap();
        let error = client.request(Command::List).unwrap_err();
        assert!(matches!(error, ClientError::PinMismatch { .. }));
        squatter.join().unwrap();
        let plaintext = seen.lock().unwrap();
        assert!(plaintext.is_empty());
        let token = std::fs::read_to_string(temp.path().join(TOKEN_FILE)).unwrap();
        assert!(!String::from_utf8_lossy(&plaintext).contains(token.trim()));
    }

    #[test]
    fn service_reuses_its_identity_and_token() {
        let temp = tempfile::tempdir().unwrap();
        let service = service(temp.path());
        let first = ControlServer::bind("127.0.0.1:0".parse().unwrap(), service.clone()).unwrap();
        drop(first);
        let cert = std::fs::read(temp.path().join(CERT_FILE)).unwrap();
        let token = std::fs::read(temp.path().join(TOKEN_FILE)).unwrap();

        let second = ControlServer::bind("127.0.0.1:0".parse().unwrap(), service).unwrap();
        drop(second);
        assert_eq!(std::fs::read(temp.path().join(CERT_FILE)).unwrap(), cert);
        assert_eq!(std::fs::read(temp.path().join(TOKEN_FILE)).unwrap(), token);
    }

    #[test]
    fn client_never_mints_missing_service_credentials() {
        let temp = tempfile::tempdir().unwrap();
        assert!(ControlClient::open(temp.path()).is_err());
        assert!(!temp.path().join(IDENTITY_FILE).exists());
        assert!(!temp.path().join(CERT_FILE).exists());
        assert!(!temp.path().join(TOKEN_FILE).exists());
        assert!(!temp.path().join(ENDPOINT_FILE).exists());
    }

    #[test]
    fn token_comparison_rejects_content_and_length_mismatches() {
        let expected = BearerToken::new("0123456789abcdef").unwrap();
        let same = BearerToken::new("0123456789abcdef").unwrap();
        let different = BearerToken::new("0123456789abcdee").unwrap();
        let longer = BearerToken::new("0123456789abcdef0").unwrap();
        assert!(token_matches(&expected, &same));
        assert!(!token_matches(&expected, &different));
        assert!(!token_matches(&expected, &longer));
    }

    #[test]
    fn connection_permits_bound_unauthenticated_clients() {
        let active = Arc::new(AtomicUsize::new(0));
        let permits: Vec<_> = (0..MAX_CONNECTIONS)
            .map(|_| acquire_connection(&active).expect("permit"))
            .collect();
        assert!(acquire_connection(&active).is_none());
        drop(permits);
        assert!(acquire_connection(&active).is_some());
    }

    #[test]
    fn accept_loop_observes_stop_without_a_connection() {
        let temp = tempfile::tempdir().unwrap();
        let server =
            ControlServer::bind("127.0.0.1:0".parse().unwrap(), service(temp.path())).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let serving = std::thread::spawn(move || server.serve_until(&signal));
        std::thread::sleep(Duration::from_millis(75));
        stop.store(true, Ordering::Release);
        serving.join().unwrap().unwrap();
    }
}
