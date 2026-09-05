use std::sync::Mutex;

use amux::testnet::TestNet;

use super::*;

struct Events {
    sender: mpsc::UnboundedSender<Value>,
    captured: Mutex<Vec<Value>>,
    batches: Mutex<Vec<(std::time::Instant, Value)>>,
}
struct Running<'a> {
    handle: *mut Handle,
    _events: &'a Events,
}
impl Drop for Running<'_> {
    fn drop(&mut self) {
        unsafe {
            amux_mobile_stop(self.handle);
        }
    }
}
unsafe extern "C" fn capture(bytes: *const c_char, context: *mut c_void) {
    let context = unsafe { &*context.cast::<Events>() };
    let events: Vec<Value> =
        serde_json::from_str(unsafe { CStr::from_ptr(bytes) }.to_str().unwrap()).unwrap();
    context
        .batches
        .lock()
        .unwrap()
        .push((std::time::Instant::now(), json!(events)));
    for event in events {
        context.captured.lock().unwrap().push(event.clone());
        let _ = context.sender.send(event);
    }
}
fn config(root: &std::path::Path, url: String, token: Value) -> Value {
    json!({
        "data_dir": root.join("data"), "cache_dir":root.join("cache"),
        "log_path":root.join("mobile.log"), "device_name":"phone",
        "relay":{"url":url,"tls":"PlainLoopback","token":token}
    })
}
fn start(config: &Value, events: &Events) -> *mut Handle {
    let config = CString::new(config.to_string()).unwrap();
    unsafe {
        amux_mobile_start(
            config.as_ptr(),
            capture,
            (events as *const Events).cast_mut().cast(),
        )
    }
}
async fn until(
    events: &mut mpsc::UnboundedReceiver<Value>,
    handle: *mut Handle,
    token: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let event = events.recv().await.expect("callback channel closed");
            seen.push(event.clone());
            assert!(event.get("Invariant").is_none(), "{event}");
            if let Some(id) = event["TokenRequest"]["request_id"].as_u64() {
                let reply = CString::new(json!({"token":token}).to_string()).unwrap();
                unsafe {
                    amux_mobile_token_reply(handle, id, reply.as_ptr());
                }
            }
            if predicate(&event) {
                return event;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("mobile event timed out; received {seen:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_lifecycle_connects_reconnects_and_stops_at_the_c_boundary() {
    let net = TestNet::builder()
        .cloud()
        .daemon("host")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let host = net.daemon("host");
    let (_, token) = net.user_credentials("owner");
    let root = tempfile::tempdir().unwrap();
    let static_config = config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        json!({"Static":token}),
    );
    let parsed = serde_json::from_value::<StartConfig>(static_config.clone()).unwrap();
    let (requests, _receive) = mpsc::channel(1);
    let credentials = Arc::new(Credentials {
        source: parsed.relay.token.clone(),
        requests,
        next_id: AtomicU64::new(1),
    });
    let mut seed = MobileRuntime::open(&parsed, credentials).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *seed.relay.borrow_and_update() != RelayConnection::Connected {
            seed.relay.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let admin = host.admin_client().await;
    let pairing = admin.start_qr_pairing().await.unwrap();
    let amux::PairingSecret::QrSecret(secret) = pairing.secret else {
        panic!("QR expected")
    };
    seed.client
        .pair_qr_cloud_peer(host.host_id(), secret)
        .await
        .unwrap();
    let agent = admin
        .create_agent(amux::CreateAgentRequest {
            agent_id: host.host_id(),
            host_id: None,
            name: Some("mobile-lifecycle-agent".into()),
            agent_type: amux::AgentType::TestAgent {
                command: "cat".into(),
            },
            working_dir: root.path().to_owned(),
            terminal_size: None,
            args: vec![],
            parent: None,
            initial_prompt: None,
        })
        .await
        .unwrap();
    seed.client.shutdown().await.unwrap();
    drop(seed);

    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let mut callback_config = static_config;
    callback_config["relay"]["token"] = json!("Callback");
    let running = Running {
        handle: start(&callback_config, &events),
        _events: &events,
    };
    let handle = running.handle;
    assert!(!handle.is_null());
    until(&mut receive, handle, &token, |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;
    let has_agent = |e: &Value| {
        e["Fleet"]["reconciled"] == true
            && e["Fleet"]["agents"].as_array().is_some_and(|agents| {
                agents
                    .iter()
                    .any(|a| a["agent"]["id"] == agent.id.to_string())
            })
    };
    until(&mut receive, handle, &token, has_agent).await;
    net.cloud_offline().await;
    until(&mut receive, handle, &token, |e| {
        e["Connection"]["state"] == "disconnected"
    })
    .await;
    until(&mut receive, handle, &token, |e| {
        e["Fleet"]["reconciled"] == false
    })
    .await;
    net.cloud_online().await;
    until(&mut receive, handle, &token, |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;
    until(&mut receive, handle, &token, has_agent).await;
    drop(running);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let hosts = admin.list_hosts().await.unwrap();
            if hosts.iter().any(|h| h.name == "phone" && !h.online) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("phone relay connection remained live after stop");
    let count = events.captured.lock().unwrap().len();
    net.cloud_offline().await;
    net.cloud_online().await;
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        events.captured.lock().unwrap().len(),
        count,
        "callback after stop"
    );
    let captured = events.captured.lock().unwrap().clone();
    assert!(
        captured
            .iter()
            .filter(|e| e.get("TokenRequest").is_some())
            .count()
            >= 2
    );
    let mut connected = false;
    for event in captured.iter() {
        if let Some(state) = event["Connection"]["state"].as_str() {
            connected = state == "connected";
        }
        if event["Fleet"]["reconciled"] == true {
            assert!(connected, "fleet preceded connection");
        }
    }
    println!("mobile lifecycle C callback batches (routing tokens omitted):");
    for event in captured.iter() {
        println!("{event}");
    }
    drop(admin);
    net.shutdown().await;
}

#[test]
fn mobile_lifecycle_rejects_invalid_endpoints_and_config() {
    let root = tempfile::tempdir().unwrap();
    let (sender, _receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    for url in [
        "http://192.0.2.1:1234",
        "http://localhost:1234",
        "http://127.0.0.1:0",
        "https://127.0.0.1:1234",
    ] {
        assert!(
            start(&config(root.path(), url.into(), json!("Callback")), &events).is_null(),
            "{url}"
        );
    }
    let mut system = config(
        root.path(),
        "http://127.0.0.1:1234".into(),
        json!("Callback"),
    );
    system["relay"]["tls"] = json!("System");
    assert!(start(&system, &events).is_null());
    system["relay"]["url"] = json!("https://relay.example:443");
    assert!(
        serde_json::from_value::<StartConfig>(system)
            .unwrap()
            .endpoint()
            .is_ok()
    );
    unsafe {
        assert!(amux_mobile_start(std::ptr::null(), capture, std::ptr::null_mut()).is_null());
        amux_mobile_stop(std::ptr::null_mut());
    }
}

#[tokio::test]
async fn mobile_lifecycle_stop_cancels_unanswered_token_and_bad_reply() {
    let root = tempfile::tempdir().unwrap();
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let config = config(root.path(), "http://127.0.0.1:9".into(), json!("Callback"));
    let running = Running {
        handle: start(&config, &events),
        _events: &events,
    };
    let handle = running.handle;
    assert!(!handle.is_null());
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = receive.recv().await.unwrap();
            if event.get("TokenRequest").is_some() {
                break event;
            }
        }
    })
    .await
    .unwrap();
    let invalid = CString::new("{broken").unwrap();
    unsafe {
        amux_mobile_token_reply(
            handle,
            request["TokenRequest"]["request_id"].as_u64().unwrap(),
            invalid.as_ptr(),
        );
    }
    let event = until(&mut receive, handle, "unused", |e| {
        e["Connection"]["state"] == "disconnected"
    })
    .await;
    assert!(
        event["Connection"]["reason"]
            .as_str()
            .unwrap()
            .contains("invalid token reply")
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = receive.recv().await.unwrap();
            if event.get("TokenRequest").is_some() {
                break;
            }
        }
    })
    .await
    .unwrap();
    let stopped = std::time::Instant::now();
    drop(running);
    assert!(stopped.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn mobile_projection_c_callback_batches_and_command_errors() {
    let root = tempfile::tempdir().unwrap();
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let mut config = config(root.path(), "http://127.0.0.1:9".into(), json!("Callback"));
    config["frame_interval_ns"] = json!(50_000_000);
    let running = Running {
        handle: start(&config, &events),
        _events: &events,
    };
    assert!(!running.handle.is_null());
    let mut ids = std::collections::BTreeSet::new();
    for index in 0..150 {
        let command = if index % 2 == 0 {
            "{unknown".to_owned()
        } else {
            serde_json::to_string(&Command::Claude(amux_ui::ClaudeCommand::SendPrompt {
                agent: uuid::Uuid::from_u128(1),
                text: "refused".into(),
            }))
            .unwrap()
        };
        let invalid = CString::new(command).unwrap();
        let id = unsafe { amux_mobile_dispatch(running.handle, invalid.as_ptr()) };
        assert!(!id.is_null());
        ids.insert(unsafe { CStr::from_ptr(id) }.to_str().unwrap().to_owned());
        unsafe {
            amux_mobile_free(id);
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let subscribe =
        CString::new(r#"{"command":"subscribe","agent":"00000000-0000-0000-0000-000000000001"}"#)
            .unwrap();
    let id = unsafe { amux_mobile_dispatch(running.handle, subscribe.as_ptr()) };
    unsafe {
        amux_mobile_free(id);
    }
    let mut received = std::collections::BTreeSet::new();
    let mut session = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while received.len() < 150 || !session {
            let event = receive.recv().await.unwrap();
            assert!(event.get("Invariant").is_none(), "{event}");
            if event["OpResult"]["outcome"]["outcome"] == "error" {
                assert!(received.insert(event["OpResult"]["op"].as_str().unwrap().to_owned()));
            }
            session |= event.get("Session").is_some();
        }
    })
    .await
    .unwrap();
    assert_eq!(received, ids);
    drop(running);
    let batches = events.batches.lock().unwrap();
    assert!(batches.len() < 30, "commands were not coalesced");
    assert!(
        batches
            .windows(2)
            .all(|p| p[1].0.duration_since(p[0].0) >= Duration::from_millis(50))
    );
    println!(
        "mobile projection C callback: 150 distinct errors, Session, {} batches at requested 50ms interval",
        batches.len()
    );
}
