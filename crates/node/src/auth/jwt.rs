//! JWT validation for cloud server mode.
//!
//! Validates connection tokens using JWKS from the cloud service.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::auth::claims::ConnectionClaims;

/// How long to cache JWKS keys before re-fetching.
const JWKS_CACHE_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Error)]
pub(crate) enum JwtError {
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
pub(crate) struct JwtValidator {
    jwks_url: String,
    http_client: Client,
    keys: Arc<RwLock<HashMap<String, DecodingKey>>>,
    last_fetch: Arc<RwLock<Option<Instant>>>,
}

impl JwtValidator {
    /// Create a new validator for the given cloud URL
    pub(crate) fn new(cloud_url: &str) -> Self {
        Self {
            jwks_url: format!("{}/.well-known/openid-configuration/jwks", cloud_url),
            http_client: Client::new(),
            keys: Arc::new(RwLock::new(HashMap::new())),
            last_fetch: Arc::new(RwLock::new(None)),
        }
    }

    /// Validate a connection token
    pub(crate) async fn validate(
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

        let token_data = decode::<ConnectionClaims>(token, key, &validation)?;
        let claims = token_data.claims;

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

    fn token(secret: &[u8], aud: Option<&str>) -> String {
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("test-key".to_string());
        encode(
            &header,
            &TestClaims {
                sub: "00000000-0000-0000-0000-000000000001",
                client_id: "cli",
                host: "relay",
                port: 9443,
                exp: 4_102_444_800,
                aud,
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
            .validate(&token(secret, None), "relay", 9443)
            .await
            .unwrap_err();

        assert!(matches!(error, JwtError::Jwt(_)));
        assert!(
            validator
                .validate(&token(secret, Some("amux_token")), "relay", 9443)
                .await
                .is_ok()
        );
    }
}
