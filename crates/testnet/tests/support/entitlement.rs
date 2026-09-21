use std::time::{Duration, SystemTime};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use node::Tier;
use node::harness::{
    AuthenticatedLinkUser, JwtError, JwtValidator, LinkAuthSession, LinkTokenAuthenticator,
};
use serde::Serialize;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use uuid::Uuid;

const JWT_KID: &str = "test-key";
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

#[derive(Serialize)]
struct Claims<'a> {
    sub: &'a str,
    client_id: &'a str,
    host: &'a str,
    port: u16,
    exp: u64,
    aud: &'a str,
}

pub async fn relay_rejects_token_without_tier() -> bool {
    let (cloud_url, jwks) = jwks_server().await;
    let validator = JwtValidator::new(&cloud_url);
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(JWT_KID.to_string());
    let token = encode(
        &header,
        &Claims {
            sub: "00000000-0000-0000-0000-000000000001",
            client_id: "spec",
            host: "relay",
            port: 9443,
            exp: 4_102_444_800,
            aud: "amux_token",
        },
        &EncodingKey::from_rsa_pem(JWT_PRIVATE_KEY.as_bytes()).expect("fixture private key"),
    )
    .expect("encode routing token without a tier");

    let result = validator.validate(&token, "relay", 9443).await;
    jwks.await.expect("JWKS fixture serves its request");
    matches!(result, Err(JwtError::MissingTier))
}

pub async fn link_tier_across_reauth() -> (Tier, Tier) {
    #[derive(Clone)]
    struct TierAuthenticator(AuthenticatedLinkUser);

    #[tonic::async_trait]
    impl LinkTokenAuthenticator for TierAuthenticator {
        async fn authenticate_token(
            &self,
            _token: &str,
        ) -> Result<AuthenticatedLinkUser, tonic::Status> {
            Ok(self.0.clone())
        }
    }

    let user_id = Uuid::new_v4();
    let initial = AuthenticatedLinkUser {
        user_id,
        client_id: "testnet".into(),
        expires_at: SystemTime::now() + Duration::from_secs(60),
        tier: Tier::Free,
    };
    let authenticator = TierAuthenticator(AuthenticatedLinkUser {
        user_id,
        client_id: "testnet".into(),
        expires_at: SystemTime::now() + Duration::from_secs(3600),
        tier: Tier::Pro,
    });
    let session = LinkAuthSession::new(initial, authenticator.clone(), None);
    let before = session.tier();
    let user = authenticator
        .authenticate_token("refreshed")
        .await
        .expect("fixture authenticator accepts refreshed token");
    session
        .apply_reauth(user)
        .expect("same-user reauthentication succeeds");
    (before, session.tier())
}

async fn jwks_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind JWKS fixture");
    let address = listener.local_addr().expect("JWKS fixture address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept JWKS request");
        let mut request = Vec::new();
        loop {
            let mut bytes = [0; 1024];
            let read = stream.read(&mut bytes).await.expect("read JWKS request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&bytes[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let body = serde_json::json!({
            "keys": [{
                "kid": JWT_KID,
                "kty": "RSA",
                "alg": "RS256",
                "use": "sig",
                "n": JWK_N,
                "e": "AQAB"
            }]
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write JWKS response");
    });
    (format!("http://{address}"), task)
}
