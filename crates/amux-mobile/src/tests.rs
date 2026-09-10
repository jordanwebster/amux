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

/// One credential provider per account, from that account's own configured
/// token, exactly as the bridge builds them.
/// A data root short enough for a profile's socket path.
///
/// Every profile allocates a Unix socket path under its own directory. This
/// phone never listens on one — its profiles run in process — but the check
/// that such a path would fit is compiled into the desktop build these tests
/// run in, and a macOS temporary directory is long enough on its own to fail
/// it before anything has started.
fn test_root() -> tempfile::TempDir {
    #[cfg(unix)]
    let parent = std::path::PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let parent = std::env::temp_dir();
    tempfile::Builder::new()
        .prefix("am")
        .tempdir_in(parent)
        .expect("create a short mobile test root")
}

fn test_credentials(
    requests: mpsc::Sender<TokenRequest>,
) -> impl Fn(&crate::runtime::AccountConfig) -> Arc<dyn CredentialProvider> {
    let next_id = Arc::new(AtomicU64::new(1));
    move |account: &crate::runtime::AccountConfig| {
        Arc::new(Credentials {
            account: account.id.clone(),
            source: account.token.clone(),
            requests: requests.clone(),
            next_id: next_id.clone(),
        }) as Arc<dyn CredentialProvider>
    }
}

/// Where one account's remembered fleet is kept under a phone's cache
/// directory. Each account has a file of its own, so a test that reads the
/// cache has to say whose.
fn cache_path(cache_dir: &std::path::Path, account: &str) -> std::path::PathBuf {
    cache_dir.join("fleet").join(cache::file_name(account))
}

fn config(root: &std::path::Path, url: String, token: Value) -> Value {
    accounts_config(root, url, &[("personal", token)], "personal")
}

/// The same configuration for a phone signed in to more than one account.
fn accounts_config(
    root: &std::path::Path,
    url: String,
    accounts: &[(&str, Value)],
    active: &str,
) -> Value {
    json!({
        "data_dir": root.join("data"), "cache_dir":root.join("cache"),
        "log_path":root.join("mobile.log"), "device_name":"phone",
        "relay":{"url":url,"tls":"PlainLoopback"},
        "accounts": accounts.iter()
            .map(|(id, token)| json!({"id": id, "token": token}))
            .collect::<Vec<_>>(),
        "active": active
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
/// How many events have been delivered so far, as a place to read from.
fn mark(events: &Events) -> usize {
    events.captured.lock().unwrap().len()
}

/// The most recent event since `from` that matches, waiting for one if none
/// has arrived. Independent projections — the fleet, the trust roster — are
/// emitted in whatever order their facts change, so a reader that waits for
/// one of them in a fixed order can wait forever for something already said.
async fn seen(
    events: &mut mpsc::UnboundedReceiver<Value>,
    captured: &Events,
    from: usize,
    handle: *mut Handle,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let already = captured
        .captured
        .lock()
        .unwrap()
        .iter()
        .skip(from)
        .rev()
        .find(|event| predicate(event))
        .cloned();
    match already {
        Some(event) => event,
        None => until(events, handle, "", predicate).await,
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
    let root = test_root();
    let static_config = config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        json!({"Static":token}),
    );
    let parsed = serde_json::from_value::<StartConfig>(static_config.clone()).unwrap();
    let (requests, _receive) = mpsc::channel(1);
    let credentials = test_credentials(requests);
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
    seed.admin
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
    seed.shutdown().await;

    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let mut callback_config = static_config;
    callback_config["accounts"][0]["token"] = json!("Callback");
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

/// Every write the phone makes about an agent rather than to it, in the exact
/// bytes it puts on the wire.
///
/// The phone spells these by hand — it has no Rust in front of it at dispatch
/// time — so a rename of a field or a tag on this side would be a control that
/// silently stopped working over there. Held here against the real decoder, a
/// drift fails in this crate's own suite instead.
#[test]
fn the_phones_agent_writes_decode_as_the_commands_they_name() {
    let agent = "6d1f2c34-0000-4000-8000-00000000ab01";
    let id: AgentId = agent.parse().unwrap();
    let decoded = |value: Value| match serde_json::from_value::<CommandDto>(value).unwrap() {
        CommandDto::Shared(command) => command,
        CommandDto::Subscription(_) => panic!("an agent write decoded as a subscription"),
        CommandDto::Pairing(_) => panic!("an agent write decoded as a pairing step"),
        CommandDto::Connection(_) => panic!("an agent write decoded as a connection command"),
        CommandDto::Devices(_) => panic!("an agent write decoded as a trust change"),
        CommandDto::Creation(_) => panic!("an agent write decoded as a creation"),
        CommandDto::Accounts(_) => panic!("an agent write decoded as an account change"),
    };

    assert_eq!(
        decoded(json!({"command":"set_model","agent":agent,"model":"gpt-5.6-luna"})),
        Command::SetModel {
            agent: id,
            model: "gpt-5.6-luna".into()
        }
    );
    assert_eq!(
        decoded(json!({"command":"set_effort","agent":agent,"effort":"high"})),
        Command::SetEffort {
            agent: id,
            effort: "high".into()
        }
    );
    assert_eq!(
        decoded(json!({"command":"set_preset","agent":agent,
                       "approval":"never","sandbox":"danger-full-access"})),
        Command::SetPreset {
            agent: id,
            approval: amux_ui::provider::ApprovalPolicy::Never,
            sandbox: amux_ui::provider::SandboxPolicy::DangerFullAccess,
        }
    );
    assert_eq!(
        decoded(
            json!({"command":"claude_sdk","claude_sdk_command":"set_permission_mode",
                       "agent":agent,"mode":"plan"})
        ),
        Command::ClaudeSdk(amux_ui::ClaudeSdkCommand::SetPermissionMode {
            agent: id,
            mode: "plan".into()
        })
    );
    assert_eq!(
        decoded(json!({"command":"rename_agent","agent":agent,"name":"tidy-the-parser"})),
        Command::RenameAgent {
            agent: id,
            name: "tidy-the-parser".into()
        }
    );
    assert_eq!(
        decoded(json!({"command":"delete_agent","agent":agent})),
        Command::DeleteAgent { agent: id }
    );
}

/// A picked photograph and a picked file become one store apiece, carrying the
/// bytes that were picked and an identity computed from them.
///
/// The bytes never travel as JSON, so they are not covered by the write test
/// above; this holds the other half — that what the phone hands the library
/// through the byte pointer is what the command carries, unaltered, and that
/// the artifact is named by its contents rather than by anything the phone
/// chose.
#[test]
fn a_picked_file_becomes_a_store_of_exactly_the_bytes_that_were_picked() {
    let agent = "6d1f2c34-0000-4000-8000-00000000ab01";
    let id: AgentId = agent.parse().unwrap();
    let png = b"\x89PNG\r\n\x1a\n and then some pixels".to_vec();

    let Some(Command::PutAttachment { agent, attachment }) = crate::picked_command(
        &json!({"agent":agent,"kind":"image","name":"reconnect-loop.png","mime":"image/png"})
            .to_string(),
        png.clone(),
    ) else {
        panic!("a picked photograph is a store")
    };
    assert_eq!(agent, id);
    assert_eq!(attachment.kind, amux_ui::ArtifactKind::Image);
    assert_eq!(attachment.name, "reconnect-loop.png");
    assert_eq!(attachment.mime, "image/png");
    assert_eq!(attachment.size, png.len() as u64);
    assert_eq!(attachment.bytes.as_deref(), Some(png.as_slice()));
    // Identity is the contents and nothing else: the same bytes under another
    // name are the same artifact.
    assert_eq!(
        attachment.id,
        amux_ui::DraftAttachment::from_bytes(
            amux_ui::ArtifactKind::File,
            "renamed.bin",
            "application/octet-stream",
            png.clone(),
        )
        .id
    );

    let trace = b"{\"relay\":\"unreachable\"}".to_vec();
    let Some(Command::PutAttachment {
        attachment: file, ..
    }) = crate::picked_command(
        &json!({"agent":agent.to_string(),"kind":"file","name":"relay-trace.json",
                "mime":"application/json"})
        .to_string(),
        trace.clone(),
    )
    else {
        panic!("a picked file is a store")
    };
    assert_eq!(file.kind, amux_ui::ArtifactKind::File);
    assert_eq!(file.bytes.as_deref(), Some(trace.as_slice()));
    assert_ne!(file.id, attachment.id, "two files, two identities");

    // A description the phone could not have written is refused rather than
    // guessed at: an artifact stored under an invented identity would be a
    // token naming something nobody can fetch.
    assert!(crate::picked_command("{\"agent\":\"not-a-uuid\"}", Vec::new()).is_none());
    assert!(
        crate::picked_command(
            &json!({"agent":agent.to_string(),"kind":"image","name":"a.png","mime":"image/png",
                    "id":"sha256:0000"})
            .to_string(),
            Vec::new()
        )
        .is_none(),
        "a client never names the artifact it is storing"
    );
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
    let root = test_root();
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
    let mut override_cloud = config(
        root.path(),
        "http://127.0.0.1:1234".into(),
        json!("Callback"),
    );
    override_cloud["accounts"][0]["service"] = json!("https://elsewhere.example");
    assert!(start(&override_cloud, &events).is_null());
    override_cloud["accounts"][0]
        .as_object_mut()
        .unwrap()
        .remove("service");
    override_cloud["cloud_url"] = json!("https://elsewhere.example");
    assert!(start(&override_cloud, &events).is_null());
    unsafe {
        assert!(amux_mobile_start(std::ptr::null(), capture, std::ptr::null_mut()).is_null());
        amux_mobile_stop(std::ptr::null_mut());
    }
}

#[tokio::test]
async fn mobile_lifecycle_stop_cancels_unanswered_token_and_bad_reply() {
    let root = test_root();
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
    // A credential the app could not answer with is the relay refusing this
    // device, which is what the screen has to be able to say. The reply that
    // could not be read is a diagnostic and goes to the log.
    assert_eq!(event["Connection"]["reason"], json!("rejected"), "{event}");
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

    let root = test_root();
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
    let root = test_root();
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
    let cache: Value = serde_json::from_slice(
        &std::fs::read(cache_path(&root.path().join("cache"), "personal")).unwrap(),
    )
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
    let root = test_root();
    let config = config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        json!({"Static":token}),
    );
    let parsed: StartConfig = serde_json::from_value(config.clone()).unwrap();
    let (requests, _receive) = mpsc::channel(1);
    let mut seed = MobileRuntime::open(&parsed, test_credentials(requests))
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
    seed.admin
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
    seed.shutdown().await;
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
    let cache_path = cache_path(&root.path().join("cache"), "personal");
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
    let root = test_root();
    let path = cache_path(root.path(), "personal");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for bytes in [
        None,
        Some("{"),
        Some(r#"{"TokenRequest":{"request_id":1}}"#),
    ] {
        if let Some(bytes) = bytes {
            std::fs::write(&path, bytes).unwrap();
        }
        let cache = cache::FleetCache::open(root.path(), "personal");
        assert!(
            matches!(cache.initial(), Event::Fleet { agents, reconciled: false, .. } if agents.is_empty())
        );
    }
    let file = root.path().join("not-a-directory");
    std::fs::write(&file, "file").unwrap();
    let mut cache = cache::FleetCache::open(&file, "personal");
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

mod mobile_cache_authoritative_inventory_prunes_offline_deletions_and_unpairing {
    use super::*;

    macro_rules! scenarios {
        ($($scenario:ident),+ $(,)?) => {$(
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn $scenario() {
                check(stringify!($scenario)).await;
            }
        )+};
    }

    scenarios!(delete_one, delete_all, unpair);

    async fn check(scenario: &str) {
        let net = TestNet::builder()
            .cloud()
            .daemon("inventory-host")
            .cloud_only()
            .cloud_user("owner")
            .start()
            .await;
        let host = net.daemon("inventory-host");
        let (_, token) = net.user_credentials("owner");
        let root = test_root();
        let config = config(
            root.path(),
            format!("http://{}", net.relay_addr()),
            json!({"Static": token}),
        );
        let parsed: StartConfig = serde_json::from_value(config.clone()).unwrap();
        let open_runtime = || {
            let (requests, _receive) = mpsc::channel(1);
            MobileRuntime::open(&parsed, test_credentials(requests))
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
        seed.admin
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
        seed.shutdown().await;
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

        let path = cache_path(&root.path().join("cache"), "personal");
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
                let mut offline = open_runtime().await.unwrap();
                offline
                    .admin
                    .unpair(host.host_id(), "Remove this device")
                    .await
                    .unwrap();
                offline.shutdown().await;
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

    let root = test_root();
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
    let start_pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    assert_ne!(
        start_pairing.cloud_url,
        format!("http://{}", net.relay_addr()),
        "the invitation names the account service the machine is on, not the relay \
         it is reachable over. A test where those are the same string cannot tell \
         whether pairing compares the right one, which is how a phone that could \
         never pair by invitation in production went unnoticed."
    );
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

/// Closing a conversation on the phone gives back the stream it opened.
///
/// The phone holds a stream only because somebody opened a conversation:
/// nothing here runs on this device, so the badge policy that keeps a local
/// agent's stream up on a desktop never applies. A conversation the user has
/// closed must therefore not come back after every reconnection for the rest
/// of the session — and the one still open must be undisturbed by its going.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_unsubscribe_releases_the_stream_a_closed_conversation_asked_for() {
    let net = TestNet::builder()
        .cloud()
        .daemon("chat-host")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let host = net.daemon("chat-host");
    let (_, token) = net.user_credentials("owner");
    let root = test_root();
    let config = config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        json!({"Static":token}),
    );
    let parsed: StartConfig = serde_json::from_value(config.clone()).unwrap();
    let (requests, _tokens) = mpsc::channel(1);
    let mut seed = MobileRuntime::open(&parsed, test_credentials(requests))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *seed.relay.borrow_and_update() != RelayConnection::Connected {
            seed.relay.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    let amux::PairingSecret::QrSecret(secret) = pairing.secret else {
        panic!("QR expected")
    };
    seed.admin
        .pair_qr_cloud_peer(host.host_id(), secret)
        .await
        .unwrap();
    // Claude agents rather than echo ones: only an agent whose kind names a
    // structured layer has a session stream to open at all.
    let kept = uuid::Uuid::from_u128(201);
    let closed = uuid::Uuid::from_u128(202);
    for (id, name) in [(kept, "Kept"), (closed, "Closed")] {
        host.register_scripted_claude_agent(id, name, root.path())
            .await;
    }
    seed.shutdown().await;

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
    let dispatch = |command: &str, agent: uuid::Uuid| {
        let json = CString::new(format!(r#"{{"command":"{command}","agent":"{agent}"}}"#)).unwrap();
        let id = unsafe { amux_mobile_dispatch(running.handle, json.as_ptr()) };
        assert!(!id.is_null(), "{command} was not dispatched");
        unsafe { amux_mobile_free(id) };
    };
    // What the model says right now, once it says what was asked of it.
    let settled = async |predicate: &dyn Fn(&Value) -> bool, why: &str| -> Value {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let snapshot = owned_json(unsafe { amux_mobile_snapshot(running.handle) });
                if predicate(&snapshot) {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{why}: {}",
                owned_json(unsafe { amux_mobile_snapshot(running.handle) })
            )
        })
    };
    let open = |snapshot: &Value, agent: uuid::Uuid| -> bool {
        !snapshot["streams"][agent.to_string()].is_null()
    };
    let attached = |snapshot: &Value, agent: uuid::Uuid| -> bool {
        !snapshot["attached"][agent.to_string()].is_null()
    };

    until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["reconciled"] == true
            && e["Fleet"]["agents"]
                .as_array()
                .is_some_and(|a| a.len() == 2)
    })
    .await;
    for agent in [kept, closed] {
        dispatch("subscribe", agent);
    }
    settled(
        &|s| open(s, kept) && open(s, closed) && attached(s, kept) && attached(s, closed),
        "both conversations did not open",
    )
    .await;

    dispatch("unsubscribe", closed);
    until(&mut receive, running.handle, &token, |e| {
        e["OpResult"]["outcome"]["outcome"] == "unsubscribed"
            && e["OpResult"]["outcome"]["agent"] == closed.to_string()
    })
    .await;
    let after = settled(
        &|s| !open(s, closed),
        "the closed conversation kept its stream",
    )
    .await;
    assert!(!attached(&after, closed), "{after}");
    assert!(
        open(&after, kept) && attached(&after, kept),
        "closing one conversation disturbed the other: {after}"
    );

    // Disturb the relay: everything the phone holds is rebuilt from the
    // inventory that follows, which is where a stream nobody asked for any
    // more would come back.
    net.cloud_offline().await;
    until(&mut receive, running.handle, &token, |e| {
        e["Connection"]["state"] == "disconnected"
    })
    .await;
    net.cloud_online().await;
    until(&mut receive, running.handle, &token, |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;
    let recovered = settled(
        &|s| open(s, kept) && s["Fleet"].is_null(),
        "the open conversation did not come back",
    )
    .await;
    assert!(
        !open(&recovered, closed) && !attached(&recovered, closed),
        "a closed conversation came back after the outage: {recovered}"
    );
    // And stays gone while the inventory keeps arriving.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let later = owned_json(unsafe { amux_mobile_snapshot(running.handle) });
    assert!(
        !open(&later, closed) && !attached(&later, closed),
        "a later inventory re-opened a closed conversation: {later}"
    );
    assert!(
        open(&later, kept),
        "the open conversation was lost: {later}"
    );

    let report = owned_json(unsafe { amux_mobile_report_snapshot(running.handle) });
    assert!(
        report["msgs"]["checkpoint"]["attached"][closed.to_string()].is_null(),
        "the recorder checkpoint still holds the closed conversation: {}",
        report["msgs"]["checkpoint"]
    );
    assert!(
        report["msgs"]["msgs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|msg| msg.as_str().is_some_and(
                |line| line.contains("user_detached") && line.contains(&closed.to_string())
            )),
        "the recorder never saw the conversation close"
    );

    // Opening it again is ordinary.
    dispatch("subscribe", closed);
    let reopened = settled(
        &|s| open(s, closed) && attached(s, closed),
        "a conversation closed once could not be opened again",
    )
    .await;
    assert!(open(&reopened, kept), "{reopened}");
    println!(
        "mobile unsubscribe: closed {closed} released its stream and stayed released across a relay outage; {kept} undisturbed"
    );
    drop(running);
    net.shutdown().await;
}

/// Pairing by a six-digit code, in two phases, from the phone's own commands.
///
/// The first phase is the whole point of the design: the code authenticates,
/// the machine says who it is, and nothing is trusted. So this drives it three
/// times — abandoned, refused, and confirmed — and after each of the first two
/// asks the phone's trust store and its fleet whether anything was written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_pairing_by_code_writes_no_trust_until_it_is_confirmed() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let host = net.daemon("workstation");
    let host_id = host.host_id();
    let (_, token) = net.user_credentials("owner");
    let root = test_root();
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
    let dispatch = |command: Value| -> String {
        let json = CString::new(command.to_string()).unwrap();
        let id = unsafe { amux_mobile_dispatch(running.handle, json.as_ptr()) };
        assert!(!id.is_null(), "{command} was not dispatched");
        let op = unsafe { CStr::from_ptr(id) }.to_str().unwrap().to_owned();
        unsafe { amux_mobile_free(id) };
        op
    };
    // The machine this phone has not paired with reaches it as a discovery
    // rather than as a member of the fleet, which is the only way the pairing
    // screens learn a host id to authenticate against.
    let discovered = until(&mut receive, running.handle, &token, |e| {
        e["Discovered"]["hosts"]
            .as_array()
            .is_some_and(|hosts| !hosts.is_empty())
    })
    .await;
    assert_eq!(discovered["Discovered"]["hosts"][0]["name"], "workstation");
    assert_eq!(
        discovered["Discovered"]["hosts"][0]["id"],
        host_id.to_string()
    );
    // Every fleet this phone has been sent so far, and none of them may carry
    // the machine it has only discovered.
    let fleets_carry_no_host = || {
        for event in events.captured.lock().unwrap().iter() {
            if let Some(fleet) = event.get("Fleet") {
                for host in fleet["hosts"].as_array().unwrap() {
                    assert_eq!(
                        host["entry"]["name"], "phone",
                        "a discovered machine entered the fleet before it was paired: {fleet}"
                    );
                }
            }
        }
    };
    fleets_carry_no_host();

    let begin = |pin: &str| -> Value {
        json!({"command": "begin_pair_pin", "host": host_id.to_string(), "pin": pin})
    };
    let answered = async |receive: &mut mpsc::UnboundedReceiver<Value>, op: String| -> Value {
        until(receive, running.handle, &token, |e| {
            e["OpResult"]["op"] == op.as_str()
        })
        .await["OpResult"]["outcome"]
            .clone()
    };
    let paired_peers = async || -> usize {
        host.pairing_admin()
            .await
            .list_peers()
            .await
            .unwrap()
            .iter()
            .filter(|peer| peer.name == "phone")
            .count()
    };

    // One offer on the machine, answered three ways: abandoning releases the
    // attempt rather than spending the offer, so the same code is still the
    // code afterwards.
    let started = host
        .pairing_admin()
        .await
        .start_pin_pairing()
        .await
        .unwrap();
    let amux::PairingSecret::Pin(pin) = started.secret else {
        panic!("PIN pairing returned a QR secret")
    };

    // 1 · A correct code, abandoned. The machine names itself; nothing is written.
    let pending = answered(&mut receive, dispatch(begin(&pin))).await;
    assert_eq!(pending["outcome"], "pairing_pending", "{pending}");
    assert_eq!(pending["name"], "workstation", "{pending}");
    assert!(
        pending["fingerprint"]
            .as_str()
            .is_some_and(|f| !f.is_empty()),
        "a pending peer arrived without the fingerprint to look at: {pending}"
    );
    assert!(pending["expires_at"].is_string(), "{pending}");
    assert_eq!(paired_peers().await, 0, "beginning a pairing wrote trust");
    let handle = pending["pending"].as_str().unwrap().to_owned();
    let abandoned = answered(
        &mut receive,
        dispatch(json!({"command": "abandon", "pending": handle})),
    )
    .await;
    assert_eq!(abandoned["outcome"], "pairing_abandoned", "{abandoned}");
    assert_eq!(paired_peers().await, 0, "abandoning a pairing wrote trust");
    fleets_carry_no_host();
    // The attempt is spent: answering it a second time has nothing to answer.
    let again = answered(
        &mut receive,
        dispatch(json!({"command": "confirm", "pending": handle})),
    )
    .await;
    assert_eq!(again["outcome"], "pairing_lost", "{again}");

    // 2 · A wrong code, in the one shape every wrong code has.
    let refused = answered(&mut receive, dispatch(begin("000000"))).await;
    assert_eq!(
        refused,
        json!({"outcome": "pairing_refused"}),
        "a refusal said more than that it was refused"
    );
    assert_eq!(paired_peers().await, 0);

    // 3 · A correct code, confirmed. Now trust exists on both sides and the
    // machine is a host rather than an offer.
    let pending = answered(&mut receive, dispatch(begin(&pin))).await;
    assert_eq!(pending["outcome"], "pairing_pending", "{pending}");
    let confirmed = answered(
        &mut receive,
        dispatch(json!({"command": "confirm", "pending": pending["pending"]})),
    )
    .await;
    assert_eq!(confirmed["outcome"], "paired", "{confirmed}");
    assert_eq!(confirmed["name"], "workstation", "{confirmed}");
    assert_eq!(paired_peers().await, 1, "confirming wrote no trust");
    let fleet = until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["hosts"].as_array().is_some_and(|hosts| {
            hosts
                .iter()
                .any(|host| host["entry"]["name"] == "workstation")
        })
    })
    .await;
    println!("pending: {pending}, confirmed: {confirmed}, fleet: {fleet}");

    drop(running);
    net.shutdown().await;
}

/// Pairing uses the configured cloud's relay route. The phone defaults to
/// https://amux.sh and cannot reach a host on another cloud.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_pairing_by_link_authenticates_against_the_configured_cloud() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let host = net.daemon("workstation");
    let (_, token) = net.user_credentials("owner");
    let root = test_root();
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
    let dispatch = |command: Value| -> String {
        let json = CString::new(command.to_string()).unwrap();
        let id = unsafe { amux_mobile_dispatch(running.handle, json.as_ptr()) };
        assert!(!id.is_null(), "{command} was not dispatched");
        let op = unsafe { CStr::from_ptr(id) }.to_str().unwrap().to_owned();
        unsafe { amux_mobile_free(id) };
        op
    };
    let answered = async |receive: &mut mpsc::UnboundedReceiver<Value>, op: String| -> Value {
        until(receive, running.handle, &token, |e| {
            e["OpResult"]["op"] == op.as_str()
        })
        .await["OpResult"]["outcome"]
            .clone()
    };
    until(&mut receive, running.handle, &token, |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;

    // Use the invitation the daemon actually issued for its configured cloud.
    let offer = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    let amux::PairingSecret::QrSecret(secret) = &offer.secret else {
        panic!("QR pairing returned a PIN")
    };
    assert_eq!(offer.cloud_url, "https://amux.sh");
    assert_ne!(offer.cloud_url, format!("http://{}", net.relay_addr()));
    let payload = amux::encode_qr_pairing_payload(&offer, secret).unwrap();

    // A second cloud issues a real invitation for a host our relay cannot reach.
    let other_net = TestNet::builder()
        .cloud_url("https://somewhere.else")
        .daemon("other-host")
        .cloud_only()
        .start()
        .await;
    let other_host = other_net.daemon("other-host");
    let other_offer = other_host
        .pairing_admin()
        .await
        .start_qr_pairing()
        .await
        .unwrap();
    let amux::PairingSecret::QrSecret(other_secret) = &other_offer.secret else {
        panic!("QR pairing returned a PIN")
    };
    let elsewhere = amux::encode_qr_pairing_payload(&other_offer, other_secret).unwrap();
    let wrong = answered(
        &mut receive,
        dispatch(json!({"command": "begin_pair_link", "payload": elsewhere})),
    )
    .await;
    assert_eq!(wrong, json!({"outcome": "pairing_refused"}));
    assert!(
        other_host
            .pairing_admin()
            .await
            .list_peers()
            .await
            .unwrap()
            .is_empty()
    );

    // And the invitation for this one authenticates and stops there, with the
    // machine's own name and key to look at and nothing written.
    let pending = answered(
        &mut receive,
        dispatch(json!({"command": "begin_pair_link", "payload": payload})),
    )
    .await;
    assert_eq!(pending["outcome"], "pairing_pending", "{pending}");
    assert_eq!(pending["name"], "workstation", "{pending}");
    assert_eq!(
        host.pairing_admin().await.list_peers().await.unwrap().len(),
        0,
        "authenticating a link wrote trust"
    );
    let confirmed = answered(
        &mut receive,
        dispatch(json!({"command": "confirm", "pending": pending["pending"]})),
    )
    .await;
    assert_eq!(confirmed["outcome"], "paired", "{confirmed}");
    assert_eq!(
        host.pairing_admin().await.list_peers().await.unwrap().len(),
        1,
        "confirming a link wrote no trust"
    );
    println!(
        "configured cloud: {}; relay route: http://{}; other cloud: {wrong}; link pairing: {pending}, {confirmed}",
        offer.cloud_url,
        net.relay_addr()
    );

    drop(running);
    net.shutdown().await;
    other_net.shutdown().await;
}

/// A phone that always has the same token. The relay's own credentials, not an
/// account's.
struct StaticToken(String);
#[async_trait::async_trait]
impl amux::CredentialProvider for StaticToken {
    async fn access_token(&self) -> Result<amux::AccessToken, amux::AuthError> {
        Ok(amux::AccessToken {
            bearer: self.0.clone(),
            expires_at: None,
        })
    }
    fn invalidate(&self, _token: &amux::AccessToken) {}
}

/// Retry Now shortens the wait to the next attempt, and pressing it ten times
/// in a second is one attempt.
///
/// Both halves matter and they pull against each other. Without the first, the
/// control is a lie: it draws a button that does nothing while the connection
/// finishes a four-second sleep. Without the second, the control is a way to
/// turn an unreachable relay into a tight reconnect loop, which is the whole
/// reason the backoff exists.
#[tokio::test]
async fn mobile_retry_now_shortens_one_wait_and_ten_presses_are_one_attempt() {
    // Jump only explicit test delays. Resume time while real socket IO is
    // pending so Tokio cannot auto-advance through unrelated network timers.
    async fn advance(duration: Duration) {
        tokio::time::pause();
        tokio::time::advance(duration).await;
        tokio::time::resume();
    }

    async fn disconnected(relay: &mut tokio::sync::watch::Receiver<amux::RelayConnection>) {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                relay.changed().await.unwrap();
                if matches!(
                    *relay.borrow_and_update(),
                    amux::RelayConnection::Disconnected { .. }
                ) {
                    break;
                }
            }
        })
        .await
        .expect("the relay never reported the failed attempt");
    }
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let (_, token) = net.user_credentials("owner");
    let root = test_root();
    let config: crate::runtime::StartConfig = serde_json::from_value(config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        json!({ "Static": token }),
    ))
    .unwrap();
    let runtime = crate::runtime::MobileRuntime::open(&config, |_| {
        Arc::new(StaticToken(token.clone())) as Arc<dyn CredentialProvider>
    })
    .await
    .unwrap();
    let mut relay = runtime.relay.clone();
    tokio::time::timeout(Duration::from_secs(20), async {
        while *relay.borrow_and_update() != amux::RelayConnection::Connected {
            relay.changed().await.unwrap();
        }
    })
    .await
    .expect("the phone never reached the relay");

    // Take the relay away and let the backoff climb to its four-second cap:
    // 250ms, 500ms, 1s and 2s of waiting, plus four failed dials.
    net.cloud_offline().await;
    disconnected(&mut relay).await;
    for millis in [250, 500, 1000, 2000] {
        advance(Duration::from_millis(millis)).await;
        disconnected(&mut relay).await;
    }
    advance(Duration::from_millis(750)).await;
    let settled = runtime.retry.attempts();

    assert_eq!(
        runtime.retry.shortened(),
        0,
        "a wait was cut short before anything had asked"
    );

    // One press during the four-second wait dials immediately; observing
    // another 900ms must not produce a second attempt.
    runtime.retry.now();
    advance(Duration::from_millis(900)).await;
    let after_one = runtime.retry.attempts();
    assert_eq!(
        after_one,
        settled + 1,
        "one press produced {} attempts, not one",
        after_one - settled
    );
    assert_eq!(
        runtime.retry.shortened(),
        1,
        "the attempt after a press was the backoff coming round, not the press"
    );

    // Ten presses in a second, once the cooldown from the first has passed.
    // Exactly one of them is listened to; the rest are somebody pressing again
    // because nothing looked like it happened.
    advance(Duration::from_millis(1200)).await;
    let before_ten = runtime.retry.attempts();
    for _ in 0..10 {
        runtime.retry.now();
        advance(Duration::from_millis(50)).await;
    }
    advance(Duration::from_millis(500)).await;
    let after_ten = runtime.retry.attempts();
    assert_eq!(
        after_ten,
        before_ten + 1,
        "ten presses produced {} attempts, not one",
        after_ten - before_ten
    );
    assert_eq!(
        runtime.retry.shortened(),
        2,
        "ten presses cut short more than one wait"
    );

    // And the connection is still what recovers: with the relay back, the
    // phone reconnects on its own schedule.
    net.cloud_online().await;
    runtime.retry.now();
    tokio::time::timeout(Duration::from_secs(20), async {
        while *relay.borrow_and_update() != amux::RelayConnection::Connected {
            relay.changed().await.unwrap();
        }
    })
    .await
    .expect("the phone never came back");

    drop(runtime);
    net.shutdown().await;
}

/// The press reaches the connection through the same door every other command
/// goes through, and is answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_retry_now_reaches_the_connection_through_the_bridge() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let (_, token) = net.user_credentials("owner");
    let root = test_root();
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
    until(&mut receive, running.handle, &token, |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;

    net.cloud_offline().await;
    until(&mut receive, running.handle, &token, |e| {
        e["Connection"]["state"] == "disconnected"
    })
    .await;

    let command = CString::new(r#"{"command":"retry_now"}"#).unwrap();
    let id = unsafe { amux_mobile_dispatch(running.handle, command.as_ptr()) };
    assert!(!id.is_null(), "retry_now was not dispatched");
    let op = unsafe { CStr::from_ptr(id) }.to_str().unwrap().to_owned();
    unsafe { amux_mobile_free(id) };

    let answered = until(&mut receive, running.handle, &token, |e| {
        e["OpResult"]["op"] == op.as_str()
    })
    .await;
    assert_eq!(
        answered["OpResult"]["outcome"],
        json!({"outcome": "retry_requested"}),
        "{answered}"
    );

    net.cloud_online().await;
    until(&mut receive, running.handle, &token, |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;

    drop(running);
    net.shutdown().await;
}

/// Revoking a machine ends the access it already had.
///
/// The claim under test is "immediately", not "eventually": a conversation
/// that is open when the key is withdrawn holds a live stream to that machine,
/// and the withdrawal is worth nothing if that stream survives it. So this
/// opens one, revokes, and then asks the runtime what it is still holding.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_revoking_a_machine_closes_the_stream_its_conversation_held() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let host = net.daemon("workstation");
    let (_, token) = net.user_credentials("owner");
    let root = test_root();
    let config = config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        json!({"Static":token}),
    );
    let parsed: StartConfig = serde_json::from_value(config.clone()).unwrap();
    let (requests, _tokens) = mpsc::channel(1);
    let mut seed = MobileRuntime::open(&parsed, test_credentials(requests))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *seed.relay.borrow_and_update() != RelayConnection::Connected {
            seed.relay.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    let amux::PairingSecret::QrSecret(secret) = pairing.secret else {
        panic!("QR expected")
    };
    seed.admin
        .pair_qr_cloud_peer(host.host_id(), secret)
        .await
        .unwrap();
    // A Claude agent, because only an agent whose kind names a structured
    // layer has a session stream for a revocation to close.
    let agent = uuid::Uuid::from_u128(401);
    host.register_scripted_claude_agent(agent, "fix-login", root.path())
        .await;
    seed.shutdown().await;

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
    let dispatch = |json: String| {
        let json = CString::new(json).unwrap();
        let id = unsafe { amux_mobile_dispatch(running.handle, json.as_ptr()) };
        assert!(!id.is_null(), "nothing was dispatched");
        unsafe { amux_mobile_free(id) };
    };
    let settled = async |predicate: &dyn Fn(&Value) -> bool, why: &str| -> Value {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let snapshot = owned_json(unsafe { amux_mobile_snapshot(running.handle) });
                if predicate(&snapshot) {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{why}: {}",
                owned_json(unsafe { amux_mobile_snapshot(running.handle) })
            )
        })
    };
    let open = |snapshot: &Value| -> bool { !snapshot["streams"][agent.to_string()].is_null() };

    // What the screen reads before anybody decides anything: this phone's own
    // identity, and the one machine holding a key to it.
    let roster = until(&mut receive, running.handle, &token, |e| {
        e["Devices"]["devices"]
            .as_array()
            .is_some_and(|devices| devices.len() == 1)
    })
    .await;
    assert_eq!(roster["Devices"]["devices"][0]["name"], "workstation");
    assert_eq!(
        roster["Devices"]["devices"][0]["host"],
        host.host_id().to_string()
    );
    assert!(
        roster["Devices"]["devices"][0]["fingerprint"]
            .as_str()
            .is_some_and(|print| print.len() >= 32),
        "a paired machine arrived without a fingerprint to compare: {roster}"
    );
    assert!(
        roster["Devices"]["identity"]["fingerprint"]
            .as_str()
            .is_some_and(|print| !print.is_empty()),
        "this phone did not say what key it is known by: {roster}"
    );

    until(&mut receive, running.handle, &token, |e| {
        e["Fleet"]["reconciled"] == true
            && e["Fleet"]["agents"]
                .as_array()
                .is_some_and(|a| a.len() == 1)
    })
    .await;
    dispatch(format!(r#"{{"command":"subscribe","agent":"{agent}"}}"#));
    settled(&open, "the conversation never opened a stream").await;

    dispatch(format!(
        r#"{{"command":"revoke","host":"{}"}}"#,
        host.host_id()
    ));
    let answered = until(&mut receive, running.handle, &token, |e| {
        e["OpResult"]["outcome"]["outcome"] == "revoked"
    })
    .await;
    assert_eq!(answered["OpResult"]["outcome"]["name"], "workstation");

    // The access that existed before the revocation is what has to end.
    let after = settled(
        &|s| !open(s),
        "the revoked machine's conversation kept its stream",
    )
    .await;
    println!("after revocation: {after}");
    // And the trust store says so, which is what the screen redraws from.
    let emptied = until(&mut receive, running.handle, &token, |e| {
        e["Devices"]["devices"]
            .as_array()
            .is_some_and(|devices| devices.is_empty())
    })
    .await;
    println!("roster: {roster}, after revoking: {emptied}");

    // A second tap on a row that is already gone withdraws nothing and says so.
    dispatch(format!(
        r#"{{"command":"revoke","host":"{}"}}"#,
        host.host_id()
    ));
    let again = until(&mut receive, running.handle, &token, |e| {
        e["OpResult"]["outcome"]["outcome"] == "revoke_refused"
    })
    .await;
    println!("revoking twice: {again}");

    drop(running);
    net.shutdown().await;
}

/// A Claude agent started from the phone is an SDK session, said out loud.
///
/// The driver is not a preference this device happens to hold: it is the whole
/// difference between a conversation the app can drive and a terminal it
/// cannot. So it is a required field with no default on the way in, and this
/// reads the command the runtime would hand the client to prove the word
/// survives the boundary rather than being filled in downstream.
#[test]
fn mobile_creating_a_claude_agent_names_the_sdk_driver() {
    let json = serde_json::json!({
        "command": "create_agent",
        "host": "2d3a0659-4f51-440a-a224-54240cfea36b",
        "directory": "/Users/ada/src/amux",
        "name": "fix-login",
        "agent": {"provider": "claude", "driver": "sdk"},
    });
    let command = match serde_json::from_value::<CommandDto>(json).unwrap() {
        CommandDto::Creation(command) => creation(command).expect("a create is a shared command"),
        other => panic!(
            "a create decoded as something else: {:?}",
            serde_json::to_value(other)
        ),
    };
    let Command::CreateAgent {
        host,
        name,
        agent_type,
        working_dir,
    } = command
    else {
        panic!("a create did not become a CreateAgent")
    };
    assert_eq!(
        agent_type,
        amux::AgentType::Claude {
            driver: amux::ClaudeDriver::Sdk
        },
        "the phone started a Claude agent on a driver it did not name"
    );
    assert_eq!(name, "fix-login");
    assert_eq!(working_dir, std::path::Path::new("/Users/ada/src/amux"));
    assert_eq!(
        host,
        Some("2d3a0659-4f51-440a-a224-54240cfea36b".parse().unwrap())
    );

    // Codex carries its model where the type has somewhere to put one, and
    // nothing else is assumed on its behalf.
    let codex = serde_json::json!({
        "command": "create_agent",
        "host": "2d3a0659-4f51-440a-a224-54240cfea36b",
        "directory": "/Users/ada/src/amux",
        "name": "spec-suite",
        "agent": {"provider": "codex", "model": "gpt-5.2"},
    });
    let CommandDto::Creation(command) = serde_json::from_value::<CommandDto>(codex).unwrap() else {
        panic!("a create decoded as something else")
    };
    let Some(Command::CreateAgent { agent_type, .. }) = creation(command) else {
        panic!("a create did not become a CreateAgent")
    };
    assert_eq!(
        agent_type,
        amux::AgentType::Codex {
            model: Some("gpt-5.2".into()),
            approval_policy: None,
            sandbox_policy: None,
            resume_thread_id: None,
        }
    );
}

/// A create that does not say which driver is refused, not defaulted.
///
/// This is the failure worth a test of its own: a default would make a PTY
/// session by omission, and a PTY session looks like every other agent until
/// somebody asks it to do something only the SDK can do. It is refused where
/// the JSON is read, so nothing downstream ever has to decide.
#[test]
fn mobile_a_create_without_a_driver_is_refused_at_the_boundary() {
    for silent in [
        serde_json::json!({"provider": "claude"}),
        serde_json::json!({"provider": "claude", "driver": null}),
    ] {
        let json = serde_json::json!({
            "command": "create_agent",
            "host": "2d3a0659-4f51-440a-a224-54240cfea36b",
            "directory": "/Users/ada/src/amux",
            "name": "fix-login",
            "agent": silent,
        });
        let refused = serde_json::from_value::<CommandDto>(json.clone());
        assert!(
            refused.is_err(),
            "a Claude create with no driver was accepted: {json}"
        );
    }

    // And a request that names a driver the runtime does not have is refused
    // in the same place rather than becoming the other one.
    let nonsense = serde_json::json!({
        "command": "create_agent",
        "host": "2d3a0659-4f51-440a-a224-54240cfea36b",
        "directory": "/Users/ada/src/amux",
        "name": "fix-login",
        "agent": {"provider": "claude", "driver": "carrier-pigeon"},
    });
    assert!(serde_json::from_value::<CommandDto>(nonsense).is_err());
}

/// A relay nothing is listening on is a refused connection, and the phone is
/// told which kind of failure that is rather than being handed the transport's
/// own error to print. The screen writes the sentence; this fixes the word it
/// writes it from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_a_refused_relay_reports_itself_unreachable() {
    // A port that was bound and released: connecting to it is refused at once
    // rather than hanging, so the failure under test is the dial itself.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    let root = test_root();
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
                format!("http://{address}"),
                json!({"Static":"token"}),
            ),
            &events,
        ),
        _events: &events,
    };
    assert!(!running.handle.is_null());
    let offline = until(&mut receive, running.handle, "token", |event| {
        event["Connection"]["state"] == "disconnected"
    })
    .await;
    assert_eq!(
        offline["Connection"]["reason"],
        json!("unreachable"),
        "{offline}"
    );
}

/// Put away, the phone holds no connection at all: the link is severed at
/// once, nothing dials while it is away, and the machine it was watching sees
/// it leave rather than holding a socket nobody is reading. Coming back dials
/// immediately and the fleet is confirmed again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_going_away_releases_the_link_and_coming_back_reconciles() {
    let net = TestNet::builder()
        .cloud()
        .daemon("host")
        .cloud_only()
        .cloud_user("owner")
        .start()
        .await;
    let host = net.daemon("host");
    let (_, token) = net.user_credentials("owner");
    let root = test_root();
    let static_config = config(
        root.path(),
        format!("http://{}", net.relay_addr()),
        json!({"Static":token}),
    );
    let parsed = serde_json::from_value::<StartConfig>(static_config.clone()).unwrap();
    let (requests, _receive) = mpsc::channel(1);
    let credentials = test_credentials(requests);
    // Pairing first, through a seed runtime, so what the phone reconciles
    // afterwards is a machine that trusts it.
    let mut seed = MobileRuntime::open(&parsed, credentials).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *seed.relay.borrow_and_update() != RelayConnection::Connected {
            seed.relay.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    let amux::PairingSecret::QrSecret(secret) = pairing.secret else {
        panic!("QR expected")
    };
    seed.admin
        .pair_qr_cloud_peer(host.host_id(), secret)
        .await
        .unwrap();
    seed.shutdown().await;

    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(&static_config, &events),
        _events: &events,
    };
    let handle = running.handle;
    assert!(!handle.is_null());
    until(&mut receive, handle, &token, |event| {
        event["Fleet"]["reconciled"] == true
    })
    .await;

    // Away. The connection says which kind of offline this is, and the
    // machine stops seeing the phone.
    unsafe { amux_mobile_set_active(handle, false) };
    let away = until(&mut receive, handle, &token, |event| {
        event["Connection"]["state"] == "disconnected"
    })
    .await;
    assert_eq!(away["Connection"]["reason"], json!("suspended"), "{away}");
    let admin = host.admin_client().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let hosts = admin.list_hosts().await.unwrap();
            if !hosts
                .iter()
                .any(|entry| entry.name == "phone" && entry.online)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the machine still saw the phone after it was put away");

    // And nothing dials while it is away: a suspended phone is not a client
    // with a network problem, so there is nothing for a backoff to retry.
    let idle = until(&mut receive, handle, &token, |event| {
        event["Fleet"]["reconciled"] == false
    })
    .await;
    assert_eq!(idle["Fleet"]["reconciled"], json!(false), "{idle}");

    // Back. The dial happens at once and the fleet is confirmed again.
    unsafe { amux_mobile_set_active(handle, true) };
    until(&mut receive, handle, &token, |event| {
        event["Connection"]["state"] == "connected"
    })
    .await;
    until(&mut receive, handle, &token, |event| {
        event["Fleet"]["reconciled"] == true
    })
    .await;
}

/// One agent on a machine, so an account has something to show.
async fn seed_agent(host: &amux::testnet::Daemon, id: u128, name: &str) -> amux::Client {
    let admin = host.admin_client().await;
    admin
        .create_agent(amux::CreateAgentRequest {
            agent_id: uuid::Uuid::from_u128(id),
            host_id: None,
            name: Some(name.into()),
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
    admin
}

/// Pair the account currently on screen with a machine, the way a person
/// pointing a phone at a screen does.
async fn pair_with(handle: *mut Handle, host: &amux::testnet::Daemon) -> Value {
    let start_pairing = host.pairing_admin().await.start_qr_pairing().await.unwrap();
    let amux::PairingSecret::QrSecret(secret) = &start_pairing.secret else {
        panic!("QR pairing returned a PIN")
    };
    let payload =
        CString::new(amux::encode_qr_pairing_payload(&start_pairing, secret).unwrap()).unwrap();
    owned_json(unsafe { amux_mobile_pair_qr(handle, payload.as_ptr()) })
}

/// Put another account on screen and wait for the bridge to say it is there.
async fn select_account(
    events: &mut mpsc::UnboundedReceiver<Value>,
    handle: *mut Handle,
    account: &str,
) -> Value {
    let json = CString::new(format!(r#"{{"command":"select","account":"{account}"}}"#)).unwrap();
    let reply = unsafe { amux_mobile_dispatch(handle, json.as_ptr()) };
    assert!(!reply.is_null(), "a select was not dispatched");
    let op = unsafe { CString::from_raw(reply) }
        .into_string()
        .expect("an operation id is text");
    let done = until(events, handle, "", |e| {
        e["OpResult"]["op"] == json!(op) && e["OpResult"]["outcome"]["outcome"] == "selected"
    })
    .await;
    assert_eq!(done["OpResult"]["outcome"]["account"], json!(account));
    done
}

fn agent_names(fleet: &Value) -> Vec<String> {
    fleet["Fleet"]["agents"]
        .as_array()
        .map(|agents| {
            agents
                .iter()
                .map(|agent| agent["agent"]["name"].as_str().unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Two accounts on one phone are two devices, not one device with two names.
///
/// A machine admits a device by its key, so an account's pairings are that
/// account's alone: what This Phone reports, and which machines are trusted,
/// changes completely when the account being read changes — and switching back
/// finds the first account exactly as it was left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_profiles_give_each_account_its_own_device_identity_and_trust() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("personal")
        .daemon("laptop")
        .cloud_only()
        .cloud_user("work")
        .start()
        .await;
    let workstation = net.daemon("workstation");
    let laptop = net.daemon("laptop");
    let (_, personal_token) = net.user_credentials("personal");
    let (_, work_token) = net.user_credentials("work");
    let _workstation_admin = seed_agent(&workstation, 401, "fix-login").await;
    let _laptop_admin = seed_agent(&laptop, 402, "write-docs").await;

    let root = test_root();
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(
            &accounts_config(
                root.path(),
                format!("http://{}", net.relay_addr()),
                &[
                    ("personal", json!({ "Static": personal_token })),
                    ("work", json!({ "Static": work_token })),
                ],
                "personal",
            ),
            &events,
        ),
        _events: &events,
    };
    let handle = running.handle;
    assert!(!handle.is_null());

    until(&mut receive, handle, "", |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;
    // An unpaired account reaches nothing: the machines are there, and this
    // device is not admitted to any of them.
    let empty = until(&mut receive, handle, "", |e| {
        e["Fleet"]["reconciled"] == true
    })
    .await;
    assert_eq!(agent_names(&empty), Vec::<String>::new(), "{empty}");

    let personal_from = mark(&events);
    let paired = pair_with(handle, &workstation).await;
    assert_eq!(paired["host"], "workstation", "{paired}");
    let personal_fleet = seen(&mut receive, &events, personal_from, handle, |e| {
        agent_names(e) == ["fix-login"]
    })
    .await;
    let personal_devices = seen(&mut receive, &events, personal_from, handle, |e| {
        e["Devices"]["devices"]
            .as_array()
            .is_some_and(|devices| devices.len() == 1)
    })
    .await;
    assert_eq!(
        personal_devices["Devices"]["devices"][0]["name"],
        "workstation"
    );
    let personal_identity = personal_devices["Devices"]["identity"].clone();

    let work_from = mark(&events);
    select_account(&mut receive, handle, "work").await;
    // The work account has never paired with anything, so it is a device with
    // an empty trust store — not the personal account's device under another
    // name, and not the personal account's machines.
    let work_devices = seen(&mut receive, &events, work_from, handle, |e| {
        e["Devices"]["devices"]
            .as_array()
            .is_some_and(|devices| devices.is_empty())
    })
    .await;
    let work_identity = work_devices["Devices"]["identity"].clone();
    assert_ne!(
        work_identity["host"], personal_identity["host"],
        "both accounts presented one device: {work_identity}"
    );
    assert_ne!(
        work_identity["fingerprint"],
        personal_identity["fingerprint"]
    );

    let laptop_from = mark(&events);
    let paired = pair_with(handle, &laptop).await;
    assert_eq!(paired["host"], "laptop", "{paired}");
    let work_fleet = seen(&mut receive, &events, laptop_from, handle, |e| {
        agent_names(e) == ["write-docs"]
    })
    .await;

    let back_from = mark(&events);
    select_account(&mut receive, handle, "personal").await;
    let back = seen(&mut receive, &events, back_from, handle, |e| {
        e["Devices"]["identity"] == personal_identity
    })
    .await;
    assert_eq!(
        back["Devices"]["devices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|device| device["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["workstation"],
        "the personal account was given the work account's machine: {back}"
    );
    let again = seen(&mut receive, &events, back_from, handle, |e| {
        agent_names(e) == ["fix-login"]
    })
    .await;
    println!("personal: {personal_fleet}\nwork: {work_fleet}\nback: {again}");
    // Nothing either account showed ever reached the other one.
    for event in events.captured.lock().unwrap().iter() {
        let names = agent_names(event);
        assert!(
            !(names.contains(&"fix-login".to_string())
                && names.contains(&"write-docs".to_string())),
            "one fleet carried both accounts' agents: {event}"
        );
    }

    drop(running);
    let profile_files = |root: &std::path::Path| {
        let mut files = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(root.join("data/installation/profiles")).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            for file in ["data/device.key", "data/trust.json", "data/host_id"] {
                let path = entry.path().join(file);
                files.insert((entry.file_name(), file), std::fs::read(path).unwrap());
            }
        }
        files
    };
    let saved = profile_files(root.path());
    assert_eq!(saved.len(), 6, "both profile namespaces must survive");
    let cached = ["personal", "work"]
        .map(|account| std::fs::read(cache_path(&root.path().join("cache"), account)).unwrap());
    let moved = test_root();
    std::fs::remove_dir(moved.path()).unwrap();
    std::fs::rename(root.path(), moved.path()).unwrap();
    for (index, account) in ["personal", "work"].into_iter().enumerate() {
        assert_eq!(
            std::fs::read(cache_path(&moved.path().join("cache"), account)).unwrap(),
            cached[index]
        );
    }
    while receive.try_recv().is_ok() {}
    let reopened_from = mark(&events);
    let reopened = Running {
        handle: start(
            &accounts_config(
                moved.path(),
                format!("http://{}", net.relay_addr()),
                &[
                    ("personal", json!({ "Static": personal_token })),
                    ("work", json!({ "Static": work_token })),
                ],
                "personal",
            ),
            &events,
        ),
        _events: &events,
    };
    assert!(!reopened.handle.is_null());
    for (account, identity, host, agent) in [
        ("personal", personal_identity, "workstation", "fix-login"),
        ("work", work_identity, "laptop", "write-docs"),
    ] {
        let from = if account == "personal" {
            reopened_from
        } else {
            mark(&events)
        };
        if account == "work" {
            select_account(&mut receive, reopened.handle, account).await;
        }
        let devices = seen(&mut receive, &events, from, reopened.handle, |event| {
            event["Devices"]["identity"] == identity
        })
        .await;
        let trusted = devices["Devices"]["devices"].as_array().unwrap();
        assert_eq!(trusted.len(), 1, "{devices}");
        assert_eq!(trusted[0]["name"], host);
        seen(&mut receive, &events, from, reopened.handle, |event| {
            agent_names(event) == [agent] && event["Fleet"]["reconciled"] == true
        })
        .await;
    }
    assert_eq!(profile_files(moved.path()), saved);
    drop(reopened);
    net.shutdown().await;
}

/// Switching accounts is not reconnecting: a result already in flight for the
/// account being left is refused, not folded into the account moved to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_profiles_switching_drops_every_late_result_from_the_previous_account() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("personal")
        .start()
        .await;
    let (_, personal_token) = net.user_credentials("personal");
    let (_, work_token) = net.user_credentials("work");

    let root = test_root();
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(
            &accounts_config(
                root.path(),
                format!("http://{}", net.relay_addr()),
                &[
                    ("personal", json!({ "Static": personal_token })),
                    ("work", json!({ "Static": work_token })),
                ],
                "personal",
            ),
            &events,
        ),
        _events: &events,
    };
    let handle = running.handle;
    assert!(!handle.is_null());
    until(&mut receive, handle, "", |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;

    let unknown = owned_json(unsafe { amux_mobile_late_results(handle) });
    assert_eq!(unknown["dropped"], 0, "{unknown}");

    select_account(&mut receive, handle, "work").await;
    let reported = owned_json(unsafe { amux_mobile_late_from_previous(handle) });
    assert_eq!(
        reported["reported"], true,
        "the account left had no edge to report on: {reported}"
    );
    let dropped = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ledger = owned_json(unsafe { amux_mobile_late_results(handle) });
            if ledger["dropped"].as_u64().unwrap_or(0) > 0 {
                return ledger;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a result from the previous account was neither folded nor refused");
    assert_eq!(dropped["kinds"], json!(["Inventory"]), "{dropped}");
    println!("refused after the switch: {dropped}");

    // And it never reached a screen: no fleet this phone drew ever carried it.
    for event in events.captured.lock().unwrap().iter() {
        assert!(
            !agent_names(event).contains(&"from-the-account-you-left".to_string()),
            "a late result was folded into the account switched to: {event}"
        );
    }

    drop(running);
    net.shutdown().await;
}

fn host_names(fleet: &Value) -> Vec<String> {
    fleet["Fleet"]["hosts"]
        .as_array()
        .map(|hosts| {
            hosts
                .iter()
                .map(|host| host["entry"]["name"].as_str().unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// What a phone remembers is an account's, not the phone's.
///
/// The remembered fleet is what puts rows on screen before a machine has
/// answered, and every one of those rows is a machine and an agent that
/// belongs to one account. Switching therefore opens the account moved to:
/// the frames drawn while the new account is still reaching its machines show
/// what it remembered, never what the account just left did, and the files on
/// disk are one account each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_profiles_the_remembered_fleet_belongs_to_the_account_that_saw_it() {
    let net = TestNet::builder()
        .cloud()
        .daemon("workstation")
        .cloud_only()
        .cloud_user("personal")
        .daemon("laptop")
        .cloud_only()
        .cloud_user("work")
        .start()
        .await;
    let workstation = net.daemon("workstation");
    let laptop = net.daemon("laptop");
    let (_, personal_token) = net.user_credentials("personal");
    let (_, work_token) = net.user_credentials("work");
    let _workstation_admin = seed_agent(&workstation, 411, "fix-login").await;
    let _laptop_admin = seed_agent(&laptop, 412, "write-docs").await;

    let root = test_root();
    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(
            &accounts_config(
                root.path(),
                format!("http://{}", net.relay_addr()),
                &[
                    ("personal", json!({ "Static": personal_token })),
                    ("work", json!({ "Static": work_token })),
                ],
                "personal",
            ),
            &events,
        ),
        _events: &events,
    };
    let handle = running.handle;
    assert!(!handle.is_null());
    until(&mut receive, handle, "", |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;

    // Each account pairs with a machine of its own and remembers what it saw.
    let from = mark(&events);
    pair_with(handle, &workstation).await;
    seen(&mut receive, &events, from, handle, |e| {
        agent_names(e) == ["fix-login"] && host_names(e) == ["workstation"]
    })
    .await;

    let from = mark(&events);
    select_account(&mut receive, handle, "work").await;
    pair_with(handle, &laptop).await;
    seen(&mut receive, &events, from, handle, |e| {
        agent_names(e) == ["write-docs"] && host_names(e) == ["laptop"]
    })
    .await;
    // Nothing the personal account remembered was drawn under the work
    // account, in any frame — including the ones before the work account had
    // reached its own machine.
    let leaked = |from: usize, agent: &str, host: &str| {
        for event in events.captured.lock().unwrap().iter().skip(from) {
            assert!(
                !agent_names(event).iter().any(|name| name == agent)
                    && !host_names(event).iter().any(|name| name == host),
                "a fleet drawn after the switch carried the account left behind: {event}"
            );
        }
    };
    leaked(from, "fix-login", "workstation");

    // And switching back is the same in the other direction.
    let from = mark(&events);
    select_account(&mut receive, handle, "personal").await;
    let back = seen(&mut receive, &events, from, handle, |e| {
        agent_names(e) == ["fix-login"] && host_names(e) == ["workstation"]
    })
    .await;
    leaked(from, "write-docs", "laptop");
    println!("the account switched back to: {back}");

    drop(running);

    // Two accounts, two files, neither of them a mixture.
    let cache_dir = root.path().join("cache");
    let personal: Value =
        serde_json::from_slice(&std::fs::read(cache_path(&cache_dir, "personal")).unwrap())
            .unwrap();
    let work: Value =
        serde_json::from_slice(&std::fs::read(cache_path(&cache_dir, "work")).unwrap()).unwrap();
    assert_eq!(agent_names(&personal), ["fix-login"], "{personal}");
    assert_eq!(host_names(&personal), ["workstation"], "{personal}");
    assert_eq!(agent_names(&work), ["write-docs"], "{work}");
    assert_eq!(host_names(&work), ["laptop"], "{work}");
    println!("remembered by personal: {personal}\nremembered by work: {work}");

    net.shutdown().await;
}

/// What the switcher says about an account nobody is looking at is a count of
/// real work, read from that account's own live subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_profiles_report_what_is_waiting_on_the_account_that_is_not_on_screen() {
    use amux::testnet::script::{Reaction, Script, ScriptAsk, Step, Trigger};

    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .cloud_user("work")
        .start()
        .await;
    let laptop = net.daemon("laptop");
    let (_, personal_token) = net.user_credentials("personal");
    let (_, work_token) = net.user_credentials("work");
    let root = test_root();
    let (agent, provider) = laptop
        .spawn_scripted_agent(
            "deploy",
            root.path(),
            Script {
                reactions: vec![Reaction {
                    on: Trigger::Any,
                    play: vec![],
                }],
                ..Script::default()
            },
            None,
        )
        .await
        .unwrap();

    let (sender, mut receive) = mpsc::unbounded_channel();
    let events = Events {
        sender,
        captured: Mutex::new(vec![]),
        batches: Mutex::new(vec![]),
    };
    let running = Running {
        handle: start(
            &accounts_config(
                root.path(),
                format!("http://{}", net.relay_addr()),
                &[
                    ("personal", json!({ "Static": personal_token })),
                    ("work", json!({ "Static": work_token })),
                ],
                "work",
            ),
            &events,
        ),
        _events: &events,
    };
    let handle = running.handle;
    assert!(!handle.is_null());
    until(&mut receive, handle, "", |e| {
        e["Connection"]["state"] == "connected"
    })
    .await;
    let paired = pair_with(handle, &laptop).await;
    assert_eq!(paired["host"], "laptop", "{paired}");

    // The agent asks for something, so there is something to be waiting for.
    provider
        .play(vec![
            Step::Prompt {
                text: "deploy the service".into(),
            },
            Step::Ask(ScriptAsk::Permission {
                tool: "Bash".into(),
                invocation: json!({"command": "./deploy.sh"}),
                scoped_directories: vec![],
            }),
        ])
        .await
        .unwrap();
    // Nothing runs on a phone, so what an agent needs is known only where
    // somebody is listening. On screen that is a conversation being open.
    let subscribe = CString::new(format!(
        r#"{{"command":"subscribe","agent":"{}"}}"#,
        agent.id
    ))
    .unwrap();
    let reply = unsafe { amux_mobile_dispatch(handle, subscribe.as_ptr()) };
    assert!(!reply.is_null());
    unsafe { amux_mobile_free(reply) };
    let waiting_on_screen = until(&mut receive, handle, "", |e| {
        e["Fleet"]["agents"].as_array().is_some_and(|agents| {
            agents
                .iter()
                .any(|agent| agent["attention"]["attention"] == "needs_you")
        })
    })
    .await;
    println!("on screen: {waiting_on_screen}");

    // Now nobody is looking at that account. What it has waiting is still
    // read, and it is the same number.
    select_account(&mut receive, handle, "personal").await;
    let attention = until(&mut receive, handle, "", |e| {
        e["Attention"]["account"] == "work" && e["Attention"]["waiting"] == json!(1)
    })
    .await;
    println!("off screen: {attention}");

    drop(running);
    net.shutdown().await;
}
