//! Production-protocol account fixture for tests that spawn the real binary.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::{TestAccount, Tier};

const JWT_KID: &str = "process-test-key";
const JWT_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDa10VP9rc+oAG3
+JhkaPK/OJSo2y00s5pUobICMpfWApDpnoEsPJf/3yvRvlIJnQMK+eQNtxSdngFP
1O3P6vgpL0MkB7CAOxbe2WB4LFZ0wHuQzxyO0Bv9YDLvZidNg7FKyxhnARyVK0m0
OFwvW5dn/L6POAxEouadWWHbeyDem1BsOcEAT2spQzqeVKZc2VlZJ2FO/CapGYvi
p7bMOAMbiIPdklfTFRj1eGA4BlLlrw645YEvlo0fCMrVZNYb+sGI1SImVfoToaxl
dcT1lagnKE1+ERL8jPqLcbx66jIOq/Nf5hRJOHuayfjz2uUqfYduumKu5NlryBv+
OiVeQmm3AgMBAAECggEARcoJDKs9XPdiFO1ui/b8EwdUQVVEYV41hW/beN/xlApV
dGtb/mOEhdECBG2RdAdihQmUNNuB85IEERVyka/5XAj6fG8HVp2BeagRH8HkAG+x
+EhUbybnBjK7i6UkO5AX5iZGrfKoztlzM8oVe/TVoA/2JW5WWz0oFl3+2yO1I8gE
4qTcP+iNFgNa2SDu0ALjiDgVUDhap+Rs4R9qd5mxswdGYUfD9oqBcouxGZVPgv6n
Xe66iowrnWfc25bD3swXPmsTBF1lncGPuSHrVwPYBFLb6rSbjtOh8aJf61qqRO/s
w44UIcOAhZ15Qv5I1rbbPHoDiRK1a1VUEpPyWxZ/IQKBgQD8YwovOoSRSnzVorIa
RlstKai7iOE6cFkrEVQUvojJcUNmfW99cMtCrGDkXQTahnpov7m2qRho/oNCPYpr
tECo8vyiMW857CaLVZVQiHO5PvzkdCKqhdR4CNsDYpXCMqBtL0Qgn6WagsU1Qw4X
uj9wgOxtBHoSgufe8rf1hXyLWwKBgQDd+Uo8gvP+rua4g4zkXZzyeucvQC5KJHqM
8YNPdaZ8+cb72yMIp/p3BqPoj2zzyX+uGW7opEwwQjAO5VG5wh+jW6j09s6e5Zes
3IJ55v5f40ioUkpxPaa3EQOSRQfi9EVRVJv6bPidRVCS4rtQMccB3oCxq+iQ8OCj
QAf7PNwV1QKBgCG9N6pSn1Aw7fk9M6PxjdS+wfC3/qvqQvFP8raHNg//1SvJTvMs
9e8mzhkZGkIAQjLolnIFrt6yT2e2hF+bjB1Jxl4ET8Mlf42W1kwawaWc9v+vSscS
9vFI9cZBEpYQYIPYErptvRynqKdTHHotireGdJSqSYtZ9pdGSTNIMfsLAoGAYSHj
MFOFfZ7/ayJ1lsC4GwtY+r41A1CvJ9nPQggTkICkaDVeQT1wRoFrXCrW3F8CNib+
92JdzIhKC1qhxo2B1rQXXQpbJAEHvCbKGZnRGhiVBMLtvFvkBhu12l3Gs7N8WbiS
gKUKrZdVSNFach82HEVHP3ggTrx5MDamx3O8QvkCgYBytDiq72xVjklJLWOsSbBo
X7zVUE2d5y01FsN30UQ4nNdCGmEdvu206B1n3Clel1kepYd9Mn7JQgzrBQQwz5UP
06MxC3IfJVYcFmiZ7Kb4ggeBW1QbUbbsb2Jbuv7wNoPcIAE2c5PwnmgacnhVB58O
qr8VpwUTpFt0PnPahUNCRw==
-----END PRIVATE KEY-----"#;
const JWK_N: &str = "2tdFT_a3PqABt_iYZGjyvziUqNstNLOaVKGyAjKX1gKQ6Z6BLDyX_98r0b5SCZ0DCvnkDbcUnZ4BT9Ttz-r4KS9DJAewgDsW3tlgeCxWdMB7kM8cjtAb_WAy72YnTYOxSssYZwEclStJtDhcL1uXZ_y-jzgMRKLmnVlh23sg3ptQbDnBAE9rKUM6nlSmXNlZWSdhTvwmqRmL4qe2zDgDG4iD3ZJX0xUY9XhgOAZS5a8OuOWBL5aNHwjK1WTWG_rBiNUiJlX6E6GsZXXE9ZWoJyhNfhES_Iz6i3G8euoyDqvzX-YUSTh7msn489rlKn2HbrpiruTZa8gb_jolXkJptw";

struct State {
    accounts: HashMap<String, TestAccount>,
    selected: String,
    devices: HashMap<String, String>,
    refresh: HashMap<String, String>,
    access: HashMap<String, String>,
    routing_host: String,
    routing_port: u16,
    update: Option<Update>,
}

struct Update {
    manifest: Bytes,
    binary: Bytes,
}

/// Fake identity/update service plus TLS material for a spawned production relay.
pub struct ProcessAccountFixture {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    tls_ca: PathBuf,
    tls_cert: PathBuf,
    tls_key: PathBuf,
    _tls_dir: TempDir,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl ProcessAccountFixture {
    pub async fn start(
        accounts: Vec<TestAccount>,
        routing_host: impl Into<String>,
        routing_port: u16,
    ) -> Self {
        Self::start_with_update(accounts, routing_host, routing_port, None).await
    }

    pub async fn start_with_update(
        accounts: Vec<TestAccount>,
        routing_host: impl Into<String>,
        routing_port: u16,
        update: Option<(&str, &Path)>,
    ) -> Self {
        assert!(!accounts.is_empty());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let update = update.map(|(version, executable)| {
            let binary = std::fs::read(executable).unwrap();
            let sha256 = format!("{:x}", Sha256::digest(&binary));
            let platforms = ["macos-arm64", "macos-x86_64", "linux-arm64", "linux-x86_64"]
                .into_iter()
                .map(|platform| {
                    (
                        platform.to_string(),
                        serde_json::json!({
                            "url": format!("http://{addr}/update/amux"),
                            "sha256": sha256,
                        }),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            Update {
                manifest: Bytes::from(
                    serde_json::json!({
                        "version": version,
                        "release_notes": "process-test replacement fixture",
                        "platforms": platforms,
                    })
                    .to_string(),
                ),
                binary: Bytes::from(binary),
            }
        });
        let selected = accounts[0].sub.clone();
        let accounts = accounts
            .into_iter()
            .map(|account| (account.sub.clone(), account))
            .collect();
        let state = Arc::new(Mutex::new(State {
            accounts,
            selected,
            devices: HashMap::new(),
            refresh: HashMap::new(),
            access: HashMap::new(),
            routing_host: routing_host.into(),
            routing_port,
            update,
        }));
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let server_state = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(_) => break,
                    },
                };
                let state = server_state.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| handle(request, state.clone()));
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });

        let tls_dir = tempfile::tempdir().unwrap();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let tls_ca = tls_dir.path().join("relay-ca.pem");
        let tls_cert = tls_dir.path().join("relay-cert.pem");
        let tls_key = tls_dir.path().join("relay-key.pem");
        std::fs::write(&tls_ca, cert.pem()).unwrap();
        std::fs::write(&tls_cert, cert.pem()).unwrap();
        std::fs::write(&tls_key, signing_key.serialize_pem()).unwrap();

        Self {
            addr,
            state,
            tls_ca,
            tls_cert,
            tls_key,
            _tls_dir: tls_dir,
            shutdown: Some(shutdown),
            task,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn tls_ca(&self) -> &Path {
        &self.tls_ca
    }

    pub fn tls_cert(&self) -> &Path {
        &self.tls_cert
    }

    pub fn tls_key(&self) -> &Path {
        &self.tls_key
    }

    pub fn select_account(&self, sub: &str) {
        let mut state = self.state.lock().unwrap();
        assert!(state.accounts.contains_key(sub));
        state.selected = sub.to_string();
    }

    pub fn set_tier(&self, sub: &str, tier: Tier) {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get_mut(sub)
            .unwrap()
            .tier = tier;
    }
}

impl Drop for ProcessAccountFixture {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn handle(
    request: Request<Incoming>,
    state: Arc<Mutex<State>>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let authorization = request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = request
        .into_body()
        .collect()
        .await
        .map(|body| String::from_utf8_lossy(&body.to_bytes()).into_owned())
        .unwrap_or_default();
    let response = match (method, path.as_str()) {
        (Method::GET, "/.well-known/openid-configuration/jwks") => json(
            StatusCode::OK,
            serde_json::json!({"keys": [{"kid": JWT_KID, "kty": "RSA", "alg": "RS256", "use": "sig", "n": JWK_N, "e": "AQAB"}]}),
        ),
        (Method::POST, "/connect/deviceauthorization") => {
            let mut state = state.lock().unwrap();
            let device = uuid::Uuid::new_v4().to_string();
            let selected = state.selected.clone();
            state.devices.insert(device.clone(), selected);
            json(
                StatusCode::OK,
                serde_json::json!({"device_code": device, "user_code": "TEST-APPROVED", "verification_uri": "https://example.test/activate", "expires_in": 600, "interval": 1}),
            )
        }
        (Method::POST, "/connect/token") => token(&body, &state),
        (Method::GET, "/connect/userinfo") => userinfo(&authorization, &state),
        (Method::GET, "/api/connect") => connect(&authorization, &state),
        (Method::GET, "/update/manifest.json") => binary_response(&state, true),
        (Method::GET, "/update/amux") => binary_response(&state, false),
        _ => json(StatusCode::NOT_FOUND, serde_json::json!({})),
    };
    Ok(response)
}

fn token(body: &str, state: &Arc<Mutex<State>>) -> Response<Full<Bytes>> {
    let fields = form(body);
    let mut state = state.lock().unwrap();
    let sub = match fields.get("grant_type").map(String::as_str) {
        Some("urn:ietf:params:oauth:grant-type:device_code") => fields
            .get("device_code")
            .and_then(|token| state.devices.remove(token)),
        Some("refresh_token") => fields
            .get("refresh_token")
            .and_then(|token| state.refresh.remove(token)),
        _ => None,
    };
    let Some(sub) = sub else {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "invalid_grant"}),
        );
    };
    let access = uuid::Uuid::new_v4().to_string();
    let refresh = uuid::Uuid::new_v4().to_string();
    state.access.insert(access.clone(), sub.clone());
    state.refresh.insert(refresh.clone(), sub);
    json(
        StatusCode::OK,
        serde_json::json!({"access_token": access, "refresh_token": refresh, "token_type": "Bearer", "expires_in": 3600}),
    )
}

fn userinfo(authorization: &Option<String>, state: &Arc<Mutex<State>>) -> Response<Full<Bytes>> {
    let state = state.lock().unwrap();
    let Some(account) = account_for(authorization, &state) else {
        return json(StatusCode::UNAUTHORIZED, serde_json::json!({}));
    };
    json(
        StatusCode::OK,
        serde_json::json!({"sub": account.sub, "name": account.name, "email": account.email}),
    )
}

fn connect(authorization: &Option<String>, state: &Arc<Mutex<State>>) -> Response<Full<Bytes>> {
    let state = state.lock().unwrap();
    let Some(account) = account_for(authorization, &state) else {
        return json(StatusCode::UNAUTHORIZED, serde_json::json!({}));
    };
    let token = routing_token(
        &state.routing_host,
        state.routing_port,
        &account.sub,
        account.tier,
    );
    json(
        StatusCode::OK,
        serde_json::json!({
            "host": "localhost",
            "port": state.routing_port,
            "token": token,
            "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            "tier": account.tier,
        }),
    )
}

fn account_for<'a>(authorization: &Option<String>, state: &'a State) -> Option<&'a TestAccount> {
    let token = authorization.as_deref()?.strip_prefix("Bearer ")?;
    let sub = state.access.get(token)?;
    state.accounts.get(sub)
}

fn binary_response(state: &Arc<Mutex<State>>, manifest: bool) -> Response<Full<Bytes>> {
    let state = state.lock().unwrap();
    let Some(update) = state.update.as_ref() else {
        return json(StatusCode::NOT_FOUND, serde_json::json!({}));
    };
    let (content_type, body) = if manifest {
        ("application/json", update.manifest.clone())
    } else {
        ("application/octet-stream", update.binary.clone())
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .body(Full::new(body))
        .unwrap()
}

fn form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (percent_decode(key), percent_decode(value)))
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    decoded.push(byte);
                    index += 2;
                } else {
                    decoded.push(bytes[index]);
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn json(status: StatusCode, body: serde_json::Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

#[derive(Serialize)]
struct Claims<'a> {
    sub: &'a str,
    client_id: &'a str,
    host: &'a str,
    port: u16,
    exp: u64,
    aud: &'a str,
    tier: &'a str,
}

fn routing_token(host: &str, port: u16, sub: &str, tier: Tier) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(JWT_KID.into());
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        + 3600;
    encode(
        &header,
        &Claims {
            sub,
            client_id: "process-test",
            host,
            port,
            exp,
            aud: "amux_token",
            tier: match tier {
                Tier::Free => "free",
                Tier::Pro => "pro",
            },
        },
        &EncodingKey::from_rsa_pem(JWT_PRIVATE_KEY.as_bytes()).unwrap(),
    )
    .unwrap()
}
