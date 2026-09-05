#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use amux::{Config, Server};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Read the real command's stdout, then cancel through the same admin API a
/// second terminal uses. The printed deadline belongs to the generated PIN.
#[tokio::test]
async fn pairing_cli_prints_pin_expiry() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("config.yaml");
    let config = Config {
        host_name: "pairing-output".into(),
        socket_path: root.path().join("amux.sock"),
        state_path: root.path().join("state.yaml"),
        data_dir: root.path().join("data"),
        enable_cloud_mode: Some(false),
        prevent_idle_sleep: Some(false),
        path: Some(config_path.clone()),
        ..Config::default()
    };
    std::fs::write(&config_path, serde_yaml::to_string(&config).unwrap()).unwrap();
    let server_config = config.clone();
    let server = tokio::spawn(async move {
        Server::builder().config(server_config).run().await.unwrap();
    });
    let client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(client) = Server::builder()
                .config(config.clone())
                .daemon()
                .open()
                .await
            {
                break client;
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
    client.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
