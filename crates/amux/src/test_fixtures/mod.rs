//! Hermetic identity and relay fixtures for integration tests.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::sync::{RwLock, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::Config;
use crate::routing::{AuthenticatedLinkUser, LinkTokenAuthenticator};
use crate::services::CloudLinkService;
use crate::user_state::ServerState;

#[derive(Clone, Debug)]
pub struct TestAccount {
    pub sub: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Fault {
    UserinfoTimeout,
    MissingSubject,
    RejectRefresh(String),
    SwapSubject { from: String, to: String },
}

struct IdentityState {
    accounts: HashMap<String, TestAccount>,
    default_sub: String,
    refresh_tokens: HashMap<String, String>,
    access_tokens: HashMap<String, String>,
    faults: Vec<Fault>,
    relay: Option<SocketAddr>,
    userinfo_gate: Option<UserinfoGate>,
}

struct UserinfoGate {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

/// Holds one userinfo response after its access token has been issued, allowing
/// tests to order credential invalidation without relying on scheduler delays.
pub struct IdentityRequestHold {
    entered: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
}
impl IdentityRequestHold {
    pub async fn entered(&mut self) {
        (&mut self.entered)
            .await
            .expect("identity request reached userinfo");
    }
    pub fn release(self) {
        let _ = self.release.send(());
    }
}

pub struct IdentityServer {
    addr: SocketAddr,
    state: Arc<Mutex<IdentityState>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl IdentityServer {
    pub async fn start(accounts: Vec<TestAccount>, relay: Option<SocketAddr>) -> Self {
        assert!(
            !accounts.is_empty(),
            "identity fixture needs at least one account"
        );
        let default_sub = accounts[0].sub.clone();
        let accounts = accounts
            .into_iter()
            .map(|account| (account.sub.clone(), account))
            .collect();
        let state = Arc::new(Mutex::new(IdentityState {
            accounts,
            default_sub,
            refresh_tokens: HashMap::new(),
            access_tokens: HashMap::new(),
            faults: Vec::new(),
            relay,
            userinfo_gate: None,
        }));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind identity fixture");
        let addr = listener.local_addr().expect("identity fixture address");
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
                let connection_state = server_state.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        handle_identity_request(request, connection_state.clone())
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self {
            addr,
            state,
            shutdown: Some(shutdown),
            task,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn refresh_token_for(&self, sub: &str) -> String {
        let mut state = self.state.lock().expect("identity fixture state poisoned");
        assert!(
            state.accounts.contains_key(sub),
            "unknown fixture account {sub}"
        );
        issue_refresh_token(&mut state, sub)
    }

    pub fn hold_next_userinfo(&self) -> IdentityRequestHold {
        let (entered_tx, entered) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let mut state = self.state.lock().unwrap();
        assert!(
            state.userinfo_gate.is_none(),
            "a userinfo hold is already pending"
        );
        state.userinfo_gate = Some(UserinfoGate {
            entered: entered_tx,
            release: release_rx,
        });
        IdentityRequestHold { entered, release }
    }

    pub fn inject(&self, fault: Fault) {
        self.state
            .lock()
            .expect("identity fixture state poisoned")
            .faults
            .push(fault);
    }
}

impl Drop for IdentityServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn handle_identity_request(
    request: Request<Incoming>,
    state: Arc<Mutex<IdentityState>>,
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
        .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned())
        .unwrap_or_default();
    let response = match (method, path.as_str()) {
        (Method::POST, "/connect/deviceauthorization") => json_response(
            StatusCode::OK,
            serde_json::json!({
                "device_code": "approved-device-code",
                "user_code": "AMUX-TEST",
                "verification_uri": "https://example.test/verify",
                "verification_uri_complete": "https://example.test/verify?code=AMUX-TEST",
                "expires_in": 300,
                "interval": 1
            }),
        ),
        (Method::POST, "/connect/token") => token_response(&body, &state),
        (Method::GET, "/connect/userinfo") | (Method::GET, "/userinfo") => {
            userinfo_response(&authorization, &state).await
        }
        (Method::GET, "/.well-known/openid-configuration/jwks") | (Method::GET, "/jwks") => {
            json_response(StatusCode::OK, serde_json::json!({ "keys": [] }))
        }
        (Method::GET, "/api/connect") => api_connect_response(&authorization, &state),
        _ => json_response(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "not_found" }),
        ),
    };
    Ok(response)
}

fn token_response(body: &str, state: &Arc<Mutex<IdentityState>>) -> Response<Full<Bytes>> {
    let mut state = state.lock().expect("identity fixture state poisoned");
    match form_value(body, "grant_type").as_deref() {
        Some("urn:ietf:params:oauth:grant-type:device_code") => {
            let sub = state.default_sub.clone();
            token_success(&mut state, &sub)
        }
        Some("refresh_token") => {
            let Some(refresh_token) = form_value(body, "refresh_token") else {
                return oauth_error("invalid_request", "refresh_token is required");
            };
            if let Some(index) = state
                .faults
                .iter()
                .position(|fault| matches!(fault, Fault::RejectRefresh(_)))
            {
                let Fault::RejectRefresh(message) = state.faults.remove(index) else {
                    unreachable!()
                };
                return oauth_error("invalid_grant", &message);
            }
            let Some(mut sub) = state.refresh_tokens.remove(&refresh_token) else {
                return oauth_error("invalid_grant", "refresh token already used or unknown");
            };
            if let Some(index) = state
                .faults
                .iter()
                .position(|fault| matches!(fault, Fault::SwapSubject { from, .. } if from == &sub))
            {
                let Fault::SwapSubject { to, .. } = state.faults.remove(index) else {
                    unreachable!()
                };
                sub = to;
            }
            token_success(&mut state, &sub)
        }
        _ => oauth_error("unsupported_grant_type", "unsupported grant_type"),
    }
}

fn token_success(state: &mut IdentityState, sub: &str) -> Response<Full<Bytes>> {
    if !state.accounts.contains_key(sub) {
        return oauth_error("invalid_grant", "unknown account");
    }
    let access_token = unique_token("access", sub);
    state
        .access_tokens
        .insert(access_token.clone(), sub.to_string());
    let refresh_token = issue_refresh_token(state, sub);
    json_response(
        StatusCode::OK,
        serde_json::json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": refresh_token,
            "scope": "openid profile email offline_access api"
        }),
    )
}

async fn userinfo_response(
    authorization: &Option<String>,
    state: &Arc<Mutex<IdentityState>>,
) -> Response<Full<Bytes>> {
    let gate = state.lock().unwrap().userinfo_gate.take();
    if let Some(gate) = gate {
        let _ = gate.entered.send(());
        let _ = gate.release.await;
    }
    let timeout = {
        let mut state = state.lock().expect("identity fixture state poisoned");
        state
            .faults
            .iter()
            .position(|fault| matches!(fault, Fault::UserinfoTimeout))
            .map(|index| state.faults.remove(index))
            .is_some()
    };
    if timeout {
        tokio::time::sleep(Duration::from_secs(30)).await;
        return json_response(StatusCode::GATEWAY_TIMEOUT, serde_json::json!({}));
    }
    let mut state = state.lock().expect("identity fixture state poisoned");
    let Some(token) = bearer_token(authorization) else {
        return json_response(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "invalid_token" }),
        );
    };
    let Some(sub) = state.access_tokens.get(token).cloned() else {
        return json_response(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "invalid_token" }),
        );
    };
    let account = state
        .accounts
        .get(&sub)
        .expect("access token account")
        .clone();
    let missing_sub = state
        .faults
        .iter()
        .position(|fault| matches!(fault, Fault::MissingSubject))
        .map(|index| state.faults.remove(index))
        .is_some();
    let mut body = serde_json::Map::new();
    if !missing_sub {
        body.insert("sub".to_string(), serde_json::Value::String(account.sub));
    }
    if let Some(name) = account.name {
        body.insert("name".to_string(), serde_json::Value::String(name));
    }
    if let Some(email) = account.email {
        body.insert("email".to_string(), serde_json::Value::String(email));
    }
    json_response(StatusCode::OK, serde_json::Value::Object(body))
}

fn api_connect_response(
    authorization: &Option<String>,
    state: &Arc<Mutex<IdentityState>>,
) -> Response<Full<Bytes>> {
    let state = state.lock().expect("identity fixture state poisoned");
    let Some(token) = bearer_token(authorization) else {
        return json_response(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "invalid_token" }),
        );
    };
    let Some(sub) = state.access_tokens.get(token) else {
        return json_response(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "invalid_token" }),
        );
    };
    let Some(relay) = state.relay else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "no_relay" }),
        );
    };
    json_response(
        StatusCode::OK,
        serde_json::json!({
            "host": relay.ip().to_string(),
            "port": relay.port(),
            "token": relay_token(sub),
            "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
        }),
    )
}

fn oauth_error(error: &str, description: &str) -> Response<Full<Bytes>> {
    json_response(
        StatusCode::BAD_REQUEST,
        serde_json::json!({ "error": error, "error_description": description }),
    )
}

fn issue_refresh_token(state: &mut IdentityState, sub: &str) -> String {
    let token = unique_token("refresh", sub);
    state.refresh_tokens.insert(token.clone(), sub.to_string());
    token
}

static TOKEN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn unique_token(kind: &str, sub: &str) -> String {
    format!(
        "fixture-{kind}-{sub}-{}",
        TOKEN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn relay_token(label: &str) -> String {
    format!("fixture-relay-{label}")
}

fn form_value(body: &str, name: &str) -> Option<String> {
    body.split('&').find_map(|field| {
        let (key, value) = field.split_once('=')?;
        (percent_decode(key) == name).then(|| percent_decode(value))
    })
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

fn bearer_token(authorization: &Option<String>) -> Option<&str> {
    authorization.as_deref()?.strip_prefix("Bearer ")
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("valid identity fixture response")
}

#[derive(Clone, Debug)]
pub struct RelayUser {
    pub user_id: Uuid,
    pub token: String,
}

#[derive(Default)]
struct RelayAuthenticator {
    users: Mutex<HashMap<String, Uuid>>,
}

#[tonic::async_trait]
impl LinkTokenAuthenticator for RelayAuthenticator {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        let user_id = self
            .users
            .lock()
            .expect("test relay users poisoned")
            .get(token)
            .copied()
            .ok_or_else(|| tonic::Status::unauthenticated("unknown test relay token"))?;
        Ok(AuthenticatedLinkUser {
            user_id,
            client_id: "test-fixture".to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(3600),
        })
    }
}

pub struct TestRelay {
    pub addr: SocketAddr,
    authenticator: Arc<RelayAuthenticator>,
    task: JoinHandle<()>,
}

impl TestRelay {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test relay");
        let addr = listener.local_addr().expect("test relay address");
        let authenticator = Arc::new(RelayAuthenticator::default());

        let state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            Uuid::new_v4(),
            None,
            None,
        )));
        state.write().await.is_cloud_server = true;
        let service = CloudLinkService::with_authenticator(state, authenticator.clone());
        let task = service.serve_on_tcp_listener(listener);
        Self {
            addr,
            authenticator,
            task,
        }
    }

    pub fn register_user(&self, label: &str) -> RelayUser {
        let user = RelayUser {
            user_id: Uuid::new_v4(),
            token: relay_token(label),
        };
        self.authenticator
            .users
            .lock()
            .expect("test relay users poisoned")
            .insert(user.token.clone(), user.user_id);
        user
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
