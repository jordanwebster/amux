#![cfg(testnet)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Runner(Child);

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn lifecycle(signal: bool) {
    let root = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let mut runner = Runner(
        Command::new(env!("CARGO_BIN_EXE_e2e-runner"))
            .args(["testnet", "serve", "--topology"])
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../e2e-tests/topologies/two-hosts.json"),
            )
            .env("TMPDIR", root.path())
            .env("TMP", root.path())
            .env("TEMP", root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = runner.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        send.send(line).unwrap();
        let mut rest = String::new();
        stdout.read_to_string(&mut rest).unwrap();
        rest
    });
    let line = receive
        .recv_timeout(Duration::from_secs(30))
        .expect("readiness within 30 seconds");
    assert!(start.elapsed() < Duration::from_secs(30));
    let ready: serde_json::Value = serde_json::from_str(&line).expect("one readiness JSON line");
    let relay: SocketAddr = ready["relay"].as_str().unwrap().parse().unwrap();
    let control: SocketAddr = ready["control"].as_str().unwrap().parse().unwrap();
    assert!(relay.ip().is_loopback() && control.ip().is_loopback());
    assert_eq!(ready["users"].as_array().unwrap().len(), 3);
    for user in ready["users"].as_array().unwrap() {
        assert!(!user["token"].as_str().unwrap().is_empty());
        uuid::Uuid::parse_str(user["user_id"].as_str().unwrap()).unwrap();
    }
    assert_eq!(ready["daemons"].as_array().unwrap().len(), 2);
    for daemon in ready["daemons"].as_array().unwrap() {
        uuid::Uuid::parse_str(daemon["host_id"].as_str().unwrap()).unwrap();
        assert_eq!(daemon["fingerprint"].as_str().unwrap().len(), 64);
    }
    assert_eq!(ready["agents"][0]["name"], "helper");
    let relay_stream = TcpStream::connect(relay).unwrap();
    // An idle control client must not prevent another client from stopping the runner.
    let idle = TcpStream::connect(control).unwrap();
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_some());
    if signal {
        #[cfg(unix)]
        assert!(
            Command::new("kill")
                .args(["-TERM", &runner.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
    } else {
        let mut stream = TcpStream::connect(control).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream.write_all(b"{invalid}\n\"Shutdown\"\n").unwrap();
        let mut reader = BufReader::new(stream);
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&reply)
                .unwrap()
                .get("Error")
                .is_some()
        );
        reply.clear();
        reader.read_line(&mut reply).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&reply)
                .unwrap()
                .get("Ack")
                .is_some()
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = runner.0.try_wait().unwrap() {
            assert!(status.success(), "runner exited with {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "runner did not exit after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        reader.join().unwrap().is_empty(),
        "stdout contains only readiness"
    );
    drop((relay_stream, idle));
    assert!(TcpStream::connect(relay).is_err());
    assert!(TcpStream::connect(control).is_err());
    let _relay = TcpListener::bind(relay).expect("relay listener released");
    let _control = TcpListener::bind(control).expect("control listener released");
    assert!(
        std::fs::read_dir(root.path()).unwrap().next().is_none(),
        "runner removes all temporary state"
    );
}

#[test]
fn testnet_serve_shutdown_releases_process_sockets_and_temporary_state() {
    lifecycle(false);
}

#[cfg(unix)]
#[test]
fn testnet_serve_sigterm_releases_process_sockets_and_temporary_state() {
    lifecycle(true);
}
