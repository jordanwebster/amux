use std::path::PathBuf;
use std::time::Duration;

use node_test_support::{ProcessAccountFixture, TestAccount, Tier};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("account-fixture: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut state_dir = None;
    let mut relay_port = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--state-dir" => state_dir = args.next().map(PathBuf::from),
            "--relay-port" => {
                relay_port = args
                    .next()
                    .map(|value| value.parse::<u16>())
                    .transpose()
                    .map_err(|error| format!("invalid relay port: {error}"))?
            }
            _ => return Err(format!("unknown argument {arg:?}")),
        }
    }
    let state_dir = state_dir.ok_or("--state-dir is required")?;
    let relay_port = relay_port.ok_or("--relay-port is required")?;
    std::fs::create_dir_all(&state_dir).map_err(|error| error.to_string())?;
    let sub = "11111111-1111-4111-8111-111111111111";
    let fixture = ProcessAccountFixture::start(
        vec![TestAccount {
            sub: sub.into(),
            name: Some("Alice Example".into()),
            email: Some("alice@example.test".into()),
            tier: Tier::Free,
        }],
        "relay",
        relay_port,
    )
    .await;
    let ca = state_dir.join("cloud-routing-ca.pem");
    let cert = state_dir.join("cloud-routing-cert.pem");
    let key = state_dir.join("cloud-routing-key.pem");
    std::fs::copy(fixture.tls_ca(), &ca).map_err(|error| error.to_string())?;
    std::fs::copy(fixture.tls_cert(), &cert).map_err(|error| error.to_string())?;
    std::fs::copy(fixture.tls_key(), &key).map_err(|error| error.to_string())?;
    std::fs::write(
        state_dir.join("fixture.env"),
        format!(
            "CLOUD_URL='{}'\nAMUX_CLOUD_TLS_CA='{}'\nAMUX_TLS_CERT='{}'\nAMUX_TLS_KEY='{}'\n",
            fixture.url(),
            ca.display(),
            cert.display(),
            key.display()
        ),
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(state_dir.join("tier-current"), "free\n").map_err(|error| error.to_string())?;
    std::fs::write(state_dir.join("ready"), "ready\n").map_err(|error| error.to_string())?;

    let mut current = Tier::Free;
    loop {
        if state_dir.join("stop").exists() {
            return Ok(());
        }
        if let Ok(requested) = std::fs::read_to_string(state_dir.join("tier")) {
            let requested = match requested.trim() {
                "free" => Tier::Free,
                "pro" => Tier::Pro,
                other => return Err(format!("unknown requested tier {other:?}")),
            };
            if requested != current {
                fixture.set_tier(sub, requested);
                current = requested;
                let label = if current == Tier::Free { "free" } else { "pro" };
                std::fs::write(state_dir.join("tier-current"), format!("{label}\n"))
                    .map_err(|error| error.to_string())?;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
