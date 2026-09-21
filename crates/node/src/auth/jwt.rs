//! JWT validation for cloud server mode.
//!
//! Validates connection tokens using JWKS from the cloud service.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::auth::claims::{ConnectionClaims, Tier};
use crate::{Clock, WallClock};

/// How long to cache JWKS keys before re-fetching.
const JWKS_CACHE_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Error)]
pub enum JwtError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error("Missing key ID in token header")]
    MissingKeyId,

    #[error("Unknown signing key")]
    UnknownKey,

    #[error("Token host/port mismatch")]
    HostMismatch,

    #[error("Missing client_id in token")]
    MissingClientId,

    #[error("Missing or unrecognised tier in token")]
    MissingTier,
}

#[derive(Debug, Deserialize)]
struct RawConnectionClaims {
    sub: String,
    client_id: String,
    host: String,
    port: u16,
    exp: u64,
    tier: Option<serde_json::Value>,
}

/// JWKS key set structure
#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// Individual JWK key
#[derive(Debug, Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

/// JWT validator with JWKS caching
pub struct JwtValidator {
    jwks_url: String,
    http_client: Client,
    keys: Arc<RwLock<HashMap<String, DecodingKey>>>,
    last_fetch: Arc<RwLock<Option<Instant>>>,
    clock: Arc<dyn Clock>,
}

impl JwtValidator {
    /// Create a new validator for the given cloud URL
    pub fn new(cloud_url: &str) -> Self {
        Self::new_with_clock(cloud_url, Arc::new(WallClock))
    }

    pub(crate) fn new_with_clock(cloud_url: &str, clock: Arc<dyn Clock>) -> Self {
        Self {
            jwks_url: format!("{}/.well-known/openid-configuration/jwks", cloud_url),
            http_client: Client::new(),
            keys: Arc::new(RwLock::new(HashMap::new())),
            last_fetch: Arc::new(RwLock::new(None)),
            clock,
        }
    }

    /// Validate a connection token
    pub async fn validate(
        &self,
        token: &str,
        expected_host: &str,
        expected_port: u16,
    ) -> Result<ConnectionClaims, JwtError> {
        // Ensure JWKS is fresh (cache for 1 hour)
        self.ensure_jwks_fresh().await?;

        // Decode header to get key ID
        let header = decode_header(token)?;
        let kid = header.kid.ok_or(JwtError::MissingKeyId)?;

        // Get the signing key
        let keys = self.keys.read().await;
        let key = keys.get(&kid).ok_or(JwtError::UnknownKey)?;

        // Validate and decode the token
        let mut validation = Validation::new(header.alg);
        validation.set_required_spec_claims(&["exp", "aud"]);
        validation.set_audience(&["amux_token"]);
        // jsonwebtoken consults the process wall clock internally. Validate
        // expiration below so test issuers and this verifier can share one
        // explicitly driven policy clock.
        validation.validate_exp = false;

        let token_data = decode::<RawConnectionClaims>(token, key, &validation)?;
        let raw = token_data.claims;
        let now = self
            .clock
            .system_now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if raw.exp < validation.reject_tokens_expiring_in_less_than {
            return Err(jsonwebtoken::errors::Error::from(ErrorKind::InvalidToken).into());
        }
        if raw
            .exp
            .saturating_sub(validation.reject_tokens_expiring_in_less_than)
            < now.saturating_sub(validation.leeway)
        {
            return Err(jsonwebtoken::errors::Error::from(ErrorKind::ExpiredSignature).into());
        }
        let tier = match raw.tier.as_ref().and_then(serde_json::Value::as_str) {
            Some("free") => Tier::Free,
            Some("pro") => Tier::Pro,
            _ => return Err(JwtError::MissingTier),
        };
        let claims = ConnectionClaims {
            sub: raw.sub,
            client_id: raw.client_id,
            host: raw.host,
            port: raw.port,
            exp: raw.exp,
            tier,
        };

        if claims.client_id.is_empty() {
            return Err(JwtError::MissingClientId);
        }

        // Verify host/port match
        if claims.host != expected_host || claims.port != expected_port {
            tracing::warn!(token_host = %claims.host, token_port = claims.port, expected_host, expected_port, "token host/port mismatch");
            return Err(JwtError::HostMismatch);
        }

        Ok(claims)
    }

    async fn ensure_jwks_fresh(&self) -> Result<(), JwtError> {
        if self.is_cache_fresh().await {
            return Ok(());
        }
        self.refresh_jwks().await
    }

    async fn is_cache_fresh(&self) -> bool {
        matches!(*self.last_fetch.read().await, Some(t) if t.elapsed() < JWKS_CACHE_TTL)
    }

    async fn refresh_jwks(&self) -> Result<(), JwtError> {
        tracing::debug!("fetching JWKS");
        let response = self.http_client.get(&self.jwks_url).send().await?;
        let jwks: JwkSet = response.json().await?;

        let mut keys = self.keys.write().await;
        keys.clear();

        for jwk in jwks.keys {
            if let (Some(kid), Some(n), Some(e)) = (&jwk.kid, &jwk.n, &jwk.e)
                && jwk.kty == "RSA"
                && let Ok(key) = DecodingKey::from_rsa_components(n, e)
            {
                keys.insert(kid.clone(), key);
            }
        }

        *self.last_fetch.write().await = Some(Instant::now());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde::Serialize;

    use super::*;

    #[derive(Serialize)]
    struct TestClaims<'a> {
        sub: &'a str,
        client_id: &'a str,
        host: &'a str,
        port: u16,
        exp: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        aud: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tier: Option<&'a str>,
    }

    async fn validator_with_hs256_key(secret: &[u8]) -> JwtValidator {
        let validator = JwtValidator::new("http://cloud.test");
        validator
            .keys
            .write()
            .await
            .insert("test-key".to_string(), DecodingKey::from_secret(secret));
        *validator.last_fetch.write().await = Some(Instant::now());
        validator
    }

    fn token(secret: &[u8], aud: Option<&str>, tier: Option<&str>) -> String {
        token_with_exp(secret, aud, tier, 4_102_444_800)
    }

    fn token_with_exp(secret: &[u8], aud: Option<&str>, tier: Option<&str>, exp: u64) -> String {
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("test-key".to_string());
        encode(
            &header,
            &TestClaims {
                sub: "00000000-0000-0000-0000-000000000001",
                client_id: "cli",
                host: "relay",
                port: 9443,
                exp,
                aud,
                tier,
            },
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn validator_requires_expected_audience_claim() {
        let secret = b"secret";
        let validator = validator_with_hs256_key(secret).await;

        let error = validator
            .validate(&token(secret, None, Some("pro")), "relay", 9443)
            .await
            .unwrap_err();

        assert!(matches!(error, JwtError::Jwt(_)));
        assert!(
            validator
                .validate(
                    &token(secret, Some("amux_token"), Some("pro")),
                    "relay",
                    9443,
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn validator_requires_a_recognised_tier_claim() {
        let secret = b"secret";
        let validator = validator_with_hs256_key(secret).await;

        for tier in [None, Some("enterprise")] {
            let error = validator
                .validate(&token(secret, Some("amux_token"), tier), "relay", 9443)
                .await
                .unwrap_err();
            assert!(matches!(error, JwtError::MissingTier));
        }

        let claims = validator
            .validate(
                &token(secret, Some("amux_token"), Some("free")),
                "relay",
                9443,
            )
            .await
            .unwrap();
        assert_eq!(claims.tier, Tier::Free);
    }

    struct TestClock {
        monotonic: Mutex<tokio::time::Instant>,
        wall: Mutex<SystemTime>,
    }

    impl TestClock {
        fn new(wall: SystemTime) -> Self {
            Self {
                monotonic: Mutex::new(tokio::time::Instant::now()),
                wall: Mutex::new(wall),
            }
        }

        fn advance(&self, duration: Duration) {
            *self.monotonic.lock().unwrap() += duration;
            *self.wall.lock().unwrap() += duration;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> tokio::time::Instant {
            *self.monotonic.lock().unwrap()
        }

        fn system_now(&self) -> SystemTime {
            *self.wall.lock().unwrap()
        }

        fn sleep_until(
            &self,
            _deadline: tokio::time::Instant,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn expiration_uses_the_injected_policy_clock() {
        let secret = b"secret";
        let epoch = 1_700_000_000;
        let clock = Arc::new(TestClock::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(epoch),
        ));
        let validator = JwtValidator::new_with_clock("http://cloud.test", clock.clone());
        validator
            .keys
            .write()
            .await
            .insert("test-key".to_string(), DecodingKey::from_secret(secret));
        *validator.last_fetch.write().await = Some(Instant::now());
        let token = token_with_exp(secret, Some("amux_token"), Some("pro"), epoch + 100);

        assert!(validator.validate(&token, "relay", 9443).await.is_ok());
        clock.advance(Duration::from_secs(160));
        assert!(validator.validate(&token, "relay", 9443).await.is_ok());
        clock.advance(Duration::from_secs(1));
        assert!(matches!(
            validator.validate(&token, "relay", 9443).await,
            Err(JwtError::Jwt(error)) if matches!(error.kind(), ErrorKind::ExpiredSignature)
        ));
    }
}
