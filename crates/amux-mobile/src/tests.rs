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
    let pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    let amux::PairingSecret::QrSecret(secret) = pairing.secret else {
        panic!("QR expected")
    };
    seed.embedded
        .admin()
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
    seed.embedded.shutdown().await;

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
fn build_marker_names_the_debug_tools_library() {
    // The suffix is how an application binary is told apart from one that
    // linked the shipping library: it is a string literal the shipping build
    // does not contain at all. Tests always run with the driving tools on.
    let build = unsafe { std::ffi::CStr::from_ptr(crate::amux_mobile_build()) }
        .to_str()
        .unwrap();
    let version = unsafe { std::ffi::CStr::from_ptr(crate::amux_mobile_version()) }
        .to_str()
        .unwrap();
    assert_eq!(build, format!("{version}+debug-tools"));
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
    struct GatedEvents<'a> {
        events: &'a Events,
        gate: Mutex<()>,
    }
    unsafe extern "C" fn gated_capture(bytes: *const c_char, context: *mut c_void) {
        let context = unsafe { &*context.cast::<GatedEvents<'_>>() };
        unsafe { capture(bytes, (context.events as *const Events).cast_mut().cast()) };
        drop(context.gate.lock().unwrap());
    }

    let root = tempfile::tempdir().unwrap();
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let mut config = config(root.path(), "http://127.0.0.1:9".into(), json!("Callback"));
    config["frame_interval_ns"] = json!(50_000_000);
    let gated = GatedEvents {
        events: &events,
        gate: Mutex::new(()),
    };
    let running;
    let mut ids = std::collections::BTreeSet::new();
    {
        // Hold the initial callback until the entire burst is queued. Sleeping
        // between sends makes the batch count depend on OS timer resolution.
        // The guard drops before Running even if a dispatch assertion fails.
        let _gate = gated.gate.lock().unwrap();
        let config = CString::new(config.to_string()).unwrap();
        running = Running {
            handle: unsafe {
                amux_mobile_start(
                    config.as_ptr(),
                    gated_capture,
                    (&gated as *const GatedEvents<'_>).cast_mut().cast(),
                )
            },
            _events: &events,
        };
        assert!(!running.handle.is_null());
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
        }
        let subscribe = CString::new(
            r#"{"command":"subscribe","agent":"00000000-0000-0000-0000-000000000001"}"#,
        )
        .unwrap();
        let id = unsafe { amux_mobile_dispatch(running.handle, subscribe.as_ptr()) };
        unsafe {
            amux_mobile_free(id);
        }
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
    assert_eq!(
        batches
            .iter()
            .filter(|(_, batch)| batch
                .as_array()
                .unwrap()
                .iter()
                .any(|e| ids.contains(e["OpResult"]["op"].as_str().unwrap_or(""))))
            .count(),
        1,
        "queued command results were not coalesced"
    );
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

fn owned_json(pointer: *mut c_char) -> Value {
    assert!(!pointer.is_null(), "snapshot unavailable");
    let value = serde_json::from_str(unsafe { CStr::from_ptr(pointer) }.to_str().unwrap()).unwrap();
    unsafe { amux_mobile_free(pointer) };
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_unpaired_relay_hosts_are_discovered_without_entering_the_fleet() {
    let net = TestNet::builder()
        .cloud()
        .daemon("owners-host")
        .cloud_only()
        .cloud_user("owner")
        .daemon("other-accounts-host")
        .cloud_only()
        .cloud_user("other")
        .start()
        .await;
    let host_id = net.daemon("owners-host").host_id().to_string();
    let other_id = net.daemon("other-accounts-host").host_id().to_string();
    let (_, token) = net.user_credentials("owner");
    let root = tempfile::tempdir().unwrap();
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(
            &config(
                root.path(),
                format!("http://{}", net.relay_addr()),
                json!({"Static": token}),
            ),
            &events,
        ),
        _events: &events,
    };
    assert!(!running.handle.is_null());
    for reconnect in [false, true] {
        if reconnect {
            net.cloud_offline().await;
            until(&mut receive, running.handle, &token, |e| {
                e["Connection"]["state"] == "disconnected"
            })
            .await;
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let snapshot = owned_json(unsafe { amux_mobile_snapshot(running.handle) });
                    if snapshot["hosts"][&host_id]["entry"]["online"] != true {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("unpaired host stayed online after relay disconnect");
            net.cloud_online().await;
        }
        until(&mut receive, running.handle, &token, |e| {
            e["Connection"]["state"] == "connected"
        })
        .await;
        let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot = owned_json(unsafe { amux_mobile_snapshot(running.handle) });
                assert!(snapshot["hosts"][&other_id].is_null(), "{snapshot}");
                if snapshot["hosts"][&host_id]["entry"]["online"] == true {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "unpaired relay host missing: {}",
                owned_json(unsafe { amux_mobile_snapshot(running.handle) })
            )
        });
        assert_eq!(snapshot["hosts"][&host_id]["entry"]["name"], "owners-host");
        assert_eq!(
            snapshot["hosts"][&host_id]["entry"]["trust_status"],
            json!(amux::HostTrustStatus::UntrustedButOnline)
        );
        println!("Unpaired mobile C snapshot (reconnected={reconnect}): {snapshot}");
    }
    drop(running);
    for event in events.captured.lock().unwrap().iter() {
        if let Some(fleet) = event.get("Fleet") {
            for host in fleet["hosts"].as_array().unwrap() {
                assert_eq!(host["entry"]["name"], "phone", "{fleet}");
            }
            assert_eq!(fleet["agents"], json!([]), "{fleet}");
        }
    }
    let cache: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("cache/fleet.json")).unwrap())
            .unwrap();
    for host in cache["Fleet"]["hosts"].as_array().unwrap() {
        assert_eq!(host["entry"]["name"], "phone", "{cache}");
    }
    net.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_cache_offline_restart_reconciles_in_place_and_exports_report() {
    let net = TestNet::builder()
        .cloud()
        .daemon("cache-host")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let host = net.daemon("cache-host");
    let (_, token) = net.user_credentials("owner");
    let root = tempfile::tempdir().unwrap();
    let config = config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        json!({"Static":token}),
    );
    let parsed: StartConfig = serde_json::from_value(config.clone()).unwrap();
    let (requests, _receive) = mpsc::channel(1);
    let mut seed = MobileRuntime::open(
        &parsed,
        Arc::new(Credentials {
            source: parsed.relay.token.clone(),
            requests,
            next_id: AtomicU64::new(1),
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *seed.relay.borrow_and_update() != RelayConnection::Connected {
            seed.relay.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let admin = host.admin_client().await;
    let pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    let amux::PairingSecret::QrSecret(secret) = pairing.secret else {
        panic!("QR expected")
    };
    seed.embedded
        .admin()
        .pair_qr_cloud_peer(host.host_id(), secret)
        .await
        .unwrap();
    for (id, name) in [(101, "First"), (102, "Second")] {
        admin
            .create_agent(amux::CreateAgentRequest {
                agent_id: uuid::Uuid::from_u128(id),
                host_id: None,
                name: Some(name.into()),
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
    }
    seed.embedded.shutdown().await;
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(&config, &events),
        _events: &events,
    };
    assert!(!running.handle.is_null());
    until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["reconciled"] == true
            && e["Fleet"]["agents"]
                .as_array()
                .is_some_and(|a| a.len() == 2)
    })
    .await;
    admin
        .rename_agent(uuid::Uuid::from_u128(101), "Renamed".into())
        .await
        .unwrap();
    let changed = until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["agents"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a["display_name"] == "Renamed"))
    })
    .await;
    let cache_path = root.path().join("cache/fleet.json");
    let disk: Value = serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert_eq!(
        disk, changed,
        "fleet change was not persisted before callback"
    );
    net.cloud_offline().await;
    let offline = until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["reconciled"] == false
    })
    .await;
    assert_eq!(offline["Fleet"]["agents"].as_array().unwrap().len(), 2);
    drop(running);

    // A displayed order need not match the reducer's UUID map order.
    let mut cached: Value = serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    cached["Fleet"]["agents"].as_array_mut().unwrap().reverse();
    std::fs::write(&cache_path, cached.to_string()).unwrap();
    let ids = |e: &Value| {
        e["Fleet"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["agent"]["id"].clone())
            .collect::<Vec<_>>()
    };
    let expected = ids(&cached);
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(&config, &events),
        _events: &events,
    };
    assert!(!running.handle.is_null());
    let first = tokio::time::timeout(Duration::from_secs(5), receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first["Fleet"]["reconciled"], false);
    assert_eq!(ids(&first), expected);
    until(&mut receive, running.handle, &token, |e| {
        e["Connection"]["state"] == "disconnected"
    })
    .await;
    net.cloud_online().await;
    let reconciled = until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["reconciled"] == true
    })
    .await;
    assert_eq!(ids(&reconciled), expected);
    for event in events
        .captured
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.get("Fleet").is_some())
    {
        assert_eq!(
            ids(event),
            expected,
            "cold sync removed or reordered cached rows"
        );
    }
    let report = owned_json(unsafe { amux_mobile_report_snapshot(running.handle) });
    assert_eq!(
        report["msgs"]["format_version"],
        amux_ui::MSGS_SCHEMA_VERSION
    );
    assert!(
        report["msgs"]["msgs"]
            .as_array()
            .is_some_and(|msgs| !msgs.is_empty())
    );
    assert!(report["daemon_absent_reason"].is_null());
    let dump: Value = serde_json::from_str(report["daemon"].as_str().unwrap()).unwrap();
    assert!(dump.is_object());
    let replay_path = root.path().join("msgs.jsonl");
    let mut lines = vec![json!({"format_version": report["msgs"]["format_version"], "checkpoint": report["msgs"]["checkpoint"]}).to_string()];
    lines.extend(
        report["msgs"]["msgs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|msg| msg.as_str().unwrap().to_owned()),
    );
    std::fs::write(&replay_path, lines.join("\n") + "\n").unwrap();
    let replay = amux_ui::replay_msgs(&replay_path).unwrap();
    // The same file the app hands back through the replay call, which is what
    // a Mac-side replay of a report bundle reads: the recorded model folded
    // again and projected as the events a live connection would have sent.
    let projected = owned_json(unsafe {
        amux_mobile_replay_report(
            CString::new(replay_path.to_str().unwrap())
                .unwrap()
                .as_ptr(),
        )
    });
    let events = projected["events"].as_array().expect("replayed events");
    let fleet = events
        .iter()
        .find(|event| event.get("Fleet").is_some())
        .expect("a replayed batch carries the fleet it recorded");
    // The same agents. Not the same order: the display order lives in the
    // cache the phone kept, and a replay has only the recording.
    let (mut replayed_ids, mut recorded_ids) = (ids(fleet), expected.clone());
    replayed_ids.sort_by_key(ToString::to_string);
    recorded_ids.sort_by_key(ToString::to_string);
    assert_eq!(
        replayed_ids, recorded_ids,
        "replay rebuilt a different fleet"
    );
    assert!(projected["error"].is_null());
    // A file that is not a recording is refused with a reason rather than
    // replayed as an empty screen.
    let missing = owned_json(unsafe {
        amux_mobile_replay_report(CString::new("/nonexistent/msgs.jsonl").unwrap().as_ptr())
    });
    assert!(missing["error"].is_string(), "{missing}");
    assert!(unsafe { amux_mobile_replay_report(std::ptr::null()) }.is_null());
    let snapshot: amux_ui::Model =
        serde_json::from_value(owned_json(unsafe { amux_mobile_snapshot(running.handle) }))
            .unwrap();
    assert_eq!(
        replay.agents().map(|a| a.agent.clone()).collect::<Vec<_>>(),
        snapshot
            .agents()
            .map(|a| a.agent.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(replay.agent_count(), 2);
    println!("Cold-start first C callback: {first}");
    println!("Reconciled C callback: {reconciled}");
    println!("Report C snapshot: {report}");
    drop(running);
    drop(admin);
    net.shutdown().await;
}

#[test]
fn mobile_cache_missing_corrupt_and_unwritable_are_nonfatal() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fleet.json");
    for bytes in [
        None,
        Some("{"),
        Some(r#"{"TokenRequest":{"request_id":1}}"#),
    ] {
        if let Some(bytes) = bytes {
            std::fs::write(&path, bytes).unwrap();
        }
        let cache = cache::FleetCache::open(root.path());
        assert!(
            matches!(cache.initial(), Event::Fleet { agents, reconciled: false, .. } if agents.is_empty())
        );
    }
    let file = root.path().join("not-a-directory");
    std::fs::write(&file, "file").unwrap();
    let mut cache = cache::FleetCache::open(&file);
    assert!(
        cache
            .update(&mut cache.initial(), &amux_ui::Model::default())
            .is_err()
    );
    unsafe {
        assert!(amux_mobile_snapshot(std::ptr::null_mut()).is_null());
        assert!(amux_mobile_report_snapshot(std::ptr::null_mut()).is_null());
        amux_mobile_free(std::ptr::null_mut());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_cache_authoritative_inventory_prunes_offline_deletions_and_unpairing() {
    for scenario in ["delete_one", "delete_all", "unpair"] {
        let net = TestNet::builder()
            .cloud()
            .daemon("inventory-host")
            .cloud_only()
            .cloud_user("owner")
            .start()
            .await;
        let host = net.daemon("inventory-host");
        let (_, token) = net.user_credentials("owner");
        let root = tempfile::tempdir().unwrap();
        let config = config(
            root.path(),
            format!("http://{}", net.relay_addr()),
            json!({"Static": token}),
        );
        let parsed: StartConfig = serde_json::from_value(config.clone()).unwrap();
        let open_runtime = || {
            let (requests, _receive) = mpsc::channel(1);
            MobileRuntime::open(
                &parsed,
                Arc::new(Credentials {
                    source: parsed.relay.token.clone(),
                    requests,
                    next_id: AtomicU64::new(1),
                }),
            )
        };
        let mut seed = open_runtime().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while *seed.relay.borrow_and_update() != RelayConnection::Connected {
                seed.relay.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let admin = host.admin_client().await;
        let pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
        let amux::PairingSecret::QrSecret(secret) = pairing.secret else {
            panic!("QR expected")
        };
        seed.embedded
            .admin()
            .pair_qr_cloud_peer(host.host_id(), secret)
            .await
            .unwrap();
        for (id, name) in [(201, "First"), (202, "Deleted"), (203, "Last")] {
            admin
                .create_agent(amux::CreateAgentRequest {
                    agent_id: uuid::Uuid::from_u128(id),
                    host_id: None,
                    name: Some(name.into()),
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
        }
        seed.embedded.shutdown().await;
        let (sender, mut receive) = mpsc::unbounded_channel();
        let events = Events {
            sender,
            captured: Mutex::new(vec![]),
            batches: Mutex::new(vec![]),
        };
        let running = Running {
            handle: start(&config, &events),
            _events: &events,
        };
        assert!(!running.handle.is_null());
        until(&mut receive, running.handle, &token, |e| {
            e["Fleet"]["reconciled"] == true
                && e["Fleet"]["agents"]
                    .as_array()
                    .is_some_and(|a| a.len() == 3)
        })
        .await;
        net.cloud_offline().await;
        until(&mut receive, running.handle, &token, |e| {
            e["Fleet"]["reconciled"] == false
        })
        .await;
        drop(running);

        let path = root.path().join("cache/fleet.json");
        let mut cached: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        cached["Fleet"]["agents"].as_array_mut().unwrap().reverse();
        std::fs::write(&path, cached.to_string()).unwrap();
        let ids = |event: &Value| {
            event["Fleet"]["agents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["agent"]["id"].clone())
                .collect::<Vec<_>>()
        };
        let original = ids(&cached);
        assert_eq!(original.len(), 3);
        match scenario {
            "delete_one" => admin
                .delete_agent(uuid::Uuid::from_u128(202))
                .await
                .unwrap(),
            "delete_all" => {
                for id in [201, 202, 203] {
                    admin.delete_agent(uuid::Uuid::from_u128(id)).await.unwrap();
                }
            }
            "unpair" => {
                let offline = open_runtime().await.unwrap();
                offline
                    .embedded
                    .admin()
                    .unpair(host.host_id(), "Remove this device")
                    .await
                    .unwrap();
                offline.embedded.shutdown().await;
            }
            _ => unreachable!(),
        }
        let expected = if scenario == "delete_one" {
            vec![
                json!(uuid::Uuid::from_u128(203)),
                json!(uuid::Uuid::from_u128(201)),
            ]
        } else {
            vec![]
        };
        let (sender, mut receive) = mpsc::unbounded_channel();
        let events = Events {
            sender,
            captured: Mutex::new(vec![]),
            batches: Mutex::new(vec![]),
        };
        let running = Running {
            handle: start(&config, &events),
            _events: &events,
        };
        assert!(!running.handle.is_null());
        let first = tokio::time::timeout(Duration::from_secs(5), receive.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first["Fleet"]["reconciled"], false);
        assert_eq!(ids(&first), original);
        until(&mut receive, running.handle, &token, |e| {
            e["Connection"]["state"] == "disconnected"
        })
        .await;
        // Local synchronization must finish while no remote inventory can arrive.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let model: amux_ui::Model = serde_json::from_value(owned_json(unsafe {
                    amux_mobile_snapshot(running.handle)
                }))
                .unwrap();
                assert_eq!(model.agent_count(), 0, "cached rows entered the reducer");
                if model.is_synchronized() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        if scenario == "unpair" {
            let is_removed = |e: &Value| {
                e["Fleet"]["agents"].as_array().is_some_and(Vec::is_empty)
                    && e["Fleet"]["hosts"].as_array().is_some_and(|hosts| {
                        hosts
                            .iter()
                            .all(|h| h["entry"]["id"] != json!(host.host_id()))
                    })
            };
            let captured = events
                .captured
                .lock()
                .unwrap()
                .iter()
                .find(|e| is_removed(e))
                .cloned();
            let removed = match captured {
                Some(event) => event,
                None => until(&mut receive, running.handle, &token, is_removed).await,
            };
            println!("Unpaired host pruned while relay offline: {removed}");
        } else {
            for e in events
                .captured
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.get("Fleet").is_some())
            {
                assert_eq!(
                    ids(e),
                    original,
                    "local completion pruned remote cached rows"
                );
                assert_eq!(e["Fleet"]["reconciled"], false);
            }
        }
        net.cloud_online().await;
        let reconciled = until(&mut receive, running.handle, &token, |e| {
            e["Fleet"]["reconciled"] == true && ids(e) == expected
        })
        .await;
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            ids(&disk),
            expected,
            "pruning was not persisted before callback"
        );
        for e in events
            .captured
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.get("Fleet").is_some())
        {
            let displayed = ids(e);
            assert_eq!(
                displayed,
                original
                    .iter()
                    .filter(|id| displayed.contains(id))
                    .cloned()
                    .collect::<Vec<_>>(),
                "survivors reordered"
            );
            assert!(
                expected.iter().all(|id| displayed.contains(id)),
                "survivor disappeared"
            );
        }
        println!("{scenario} cold-start C callback: {first}");
        println!("{scenario} authoritative C callback: {reconciled}");
        if scenario == "delete_one" {
            admin
                .delete_agent(uuid::Uuid::from_u128(203))
                .await
                .unwrap();
            let deleted_live = until(&mut receive, running.handle, &token, |e| {
                e["Fleet"]["reconciled"] == true
                    && ids(e) == vec![json!(uuid::Uuid::from_u128(201))]
            })
            .await;
            println!("Confirmed live deletion C callback: {deleted_live}");
        }

        drop(running);
        drop(admin);
        net.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_pairing_over_the_relay_admits_the_hosts_agents_to_the_fleet() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let host = net.daemon("workstation");
    let (_, token) = net.user_credentials("owner");
    let admin = host.admin_client().await;
    admin
        .create_agent(amux::CreateAgentRequest {
            agent_id: uuid::Uuid::from_u128(301),
            host_id: None,
            name: Some("fix-login".into()),
            agent_type: amux::AgentType::TestAgent {
                command: "cat".into(),
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: vec![],
            parent: None,
            initial_prompt: None,
        })
        .await
        .unwrap();

    let root = tempfile::tempdir().unwrap();
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(
            &config(
                root.path(),
                format!("http://{}", net.relay_addr()),
                json!({ "Static": token }),
            ),
            &events,
        ),
        _events: &events,
    };
    assert!(!running.handle.is_null());
    let connected = until(&mut receive, running.handle, &token, |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;
    let empty = until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["reconciled"] == true
    })
    .await;
    assert_eq!(
        empty["Fleet"]["agents"].as_array().unwrap().len(),
        0,
        "an unpaired phone was given the host's agents: {empty}"
    );

    // What the phone reads off a screen the host is showing.
    let mut start_pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    start_pairing.cloud_url = format!("http://{}", net.relay_addr());
    let amux::PairingSecret::QrSecret(secret) = &start_pairing.secret else {
        panic!("QR pairing returned a PIN")
    };
    let payload =
        CString::new(amux::encode_qr_pairing_payload(&start_pairing, secret).unwrap()).unwrap();
    let paired = owned_json(unsafe { amux_mobile_pair_qr(running.handle, payload.as_ptr()) });
    assert_eq!(
        paired["host"], "workstation",
        "pairing did not name the host it trusted: {paired}"
    );

    let fleet = until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["agents"]
            .as_array()
            .is_some_and(|agents| agents.len() == 1)
    })
    .await;
    assert_eq!(fleet["Fleet"]["agents"][0]["agent"]["name"], "fix-login");
    println!("connected: {connected}");
    println!("paired: {paired}, fleet: {fleet}");

    // A payload nothing on the other side is offering is refused, not obeyed.
    let nonsense = CString::new("not a pairing payload").unwrap();
    let refused = owned_json(unsafe { amux_mobile_pair_qr(running.handle, nonsense.as_ptr()) });
    assert!(
        refused["error"].is_string(),
        "an unreadable payload was accepted: {refused}"
    );

    drop(running);
    drop(admin);
    net.shutdown().await;
}
