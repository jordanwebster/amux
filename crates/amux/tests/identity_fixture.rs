#![cfg(test_fixtures)]

use amux::test_fixtures::{IdentityServer, TestAccount};
use amux::{OAuthError, refresh_access_token, run_device_flow};

#[tokio::test]
async fn identity_fixture_drives_device_refresh_and_userinfo_flows() {
    let identity = IdentityServer::start(
        vec![TestAccount {
            sub: "alice".to_string(),
            name: Some("Alice Example".to_string()),
            email: Some("alice@example.test".to_string()),
        }],
        None,
    )
    .await;

    let refresh_token = run_device_flow(&identity.url())
        .await
        .expect("production device flow should complete");
    let (access_token, rotated_refresh) = refresh_access_token(&identity.url(), &refresh_token)
        .await
        .expect("production refresh should complete");
    assert!(rotated_refresh.is_some(), "fixture rotates refresh tokens");

    let userinfo: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/connect/userinfo", identity.url()))
        .bearer_auth(&access_token.bearer)
        .send()
        .await
        .expect("request fixture userinfo")
        .error_for_status()
        .expect("userinfo status")
        .json()
        .await
        .expect("decode userinfo");
    assert_eq!(userinfo["sub"], "alice");
    assert_eq!(userinfo["name"], "Alice Example");
    assert_eq!(userinfo["email"], "alice@example.test");

    assert!(matches!(
        refresh_access_token(&identity.url(), &refresh_token).await,
        Err(OAuthError::RefreshTokenExpired)
    ));
}
