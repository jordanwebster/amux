//! JWT validation for cloud server mode.
//!
//! Validates connection tokens using JWKS from the cloud service.

use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock;

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
}

/// Claims from a connection token
#[derive(Debug, Deserialize)]
pub struct ConnectionClaims {
    /// User ID (subject)
    pub sub: String,
    /// Expected host this token is for
    pub host: String,
    /// Expected port this token is for
    pub port: u16,
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
}

impl JwtValidator {
    /// Create a new validator for the given cloud URL
    pub fn new(cloud_url: &str) -> Self {
        Self {
            jwks_url: format!("{}/.well-known/openid-configuration/jwks", cloud_url),
            http_client: Client::new(),
            keys: Arc::new(RwLock::new(HashMap::new())),
            last_fetch: Arc::new(RwLock::new(None)),
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
        validation.set_audience(&["amux_token"]);

        let token_data = decode::<ConnectionClaims>(token, key, &validation)?;
        let claims = token_data.claims;

        // Verify host/port match
        if claims.host != expected_host || claims.port != expected_port {
            tracing::warn!(token_host = %claims.host, token_port = claims.port, expected_host, expected_port, "token host/port mismatch");
            return Err(JwtError::HostMismatch);
        }

        Ok(claims)
    }

    async fn ensure_jwks_fresh(&self) -> Result<(), JwtError> {
        let last = self.last_fetch.read().await;
        if let Some(t) = *last {
            if t.elapsed() < Duration::from_secs(3600) {
                return Ok(()); // Cache still valid
            }
        }
        drop(last);

        // Fetch JWKS
        tracing::debug!("fetching JWKS");
        let response = self.http_client.get(&self.jwks_url).send().await?;
        let jwks: JwkSet = response.json().await?;

        let mut keys = self.keys.write().await;
        keys.clear();

        for jwk in jwks.keys {
            if let (Some(kid), Some(n), Some(e)) = (&jwk.kid, &jwk.n, &jwk.e) {
                if jwk.kty == "RSA" {
                    if let Ok(key) = DecodingKey::from_rsa_components(n, e) {
                        keys.insert(kid.clone(), key);
                    }
                }
            }
        }

        *self.last_fetch.write().await = Some(Instant::now());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validator_creation() {
        let validator = JwtValidator::new("https://amux.sh");
        assert_eq!(
            validator.jwks_url,
            "https://amux.sh/.well-known/openid-configuration/jwks"
        );
    }
}
