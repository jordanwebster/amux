#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use amux::installation::FrontDoorClient;
use amux::{Installation, InstallationConfig, OperationId};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Read the real command's stdout, then cancel through the same admin API a
/// second terminal uses. The printed deadline belongs to the generated PIN.
#[tokio::test]
async fn pairing_cli_prints_pin_expiry() {
    let root = tempfile::Builder::new()
        .prefix("pair-")
        .tempdir_in("/tmp")
        .unwrap();
    let installation_path = root.path().join("installation.yaml");
    let config = InstallationConfig {
        root: root.path().to_owned(),
        host_name: "pairing-output".into(),
        front_door_socket: root.path().join("amux.sock"),
        keymaps_dir: root.path().join("keymaps"),
        prevent_idle_sleep: Some(false),
        path: Some(installation_path.clone()),
        ..InstallationConfig::default()
    };
    std::fs::write(&installation_path, serde_yaml::to_string(&config).unwrap()).unwrap();
    let installation = Installation::from_config(config.clone()).await.unwrap();
    let profile = installation.create(OperationId::new(), None).await.unwrap();
    let config_path = root
        .path()
        .join("profiles")
        .join(profile.record.id.to_string())
        .join("config.yaml");
    let client = installation.admin(profile.record.id).await.unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(installation.serve(async move {
        let _ = stopped.await;
    }));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if FrontDoorClient::connect(&config.front_door_socket)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_amux"))
        .arg("--config")
        .arg(config_path)
        .arg("pair")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(command.stdout.take().unwrap()).lines();
    let output = tokio::time::timeout(Duration::from_secs(30), async {
        let mut output = String::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            output.push_str(&line);
            output.push('\n');
            if line == "Pairing mode active for 5 minutes." {
                break;
            }
        }
        output
    })
    .await
    .unwrap();
    let pin = output
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Pairing PIN: ")
        .unwrap();
    assert_eq!(pin.len(), 6);
    assert!(pin.bytes().all(|b| b.is_ascii_digit()));
    assert!(output.contains("PIN expires in 5 minutes.\n"));
    print!("{output}");
    client.cancel_pairing().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), command.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
