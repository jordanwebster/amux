//! Chapter 8 — Live daemon diagnostics.
//!
//! The report-facing dump is checked across a real direct route and session,
//! so its counts cannot quietly fall back to placeholders while the ordinary
//! protocol remains healthy.

use amux::testnet::{TestNet, Via};

/// A live dump explains both halves of a remote session: the calling daemon's
/// route, link, and tunnel, and the hosting daemon's output subscription,
/// retained buffer, provider process state, and unanswered asks.
#[tokio::test]
async fn debug_dump_reports_live_routing_and_session_state() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .paired("laptop", "desktop", Via::Tcp)
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    desktop.spawn_echo_agent("worker").await;
    laptop.sees_agent_on(&desktop, "worker").await;
    let mut session = laptop.attach(&desktop, "worker").await;
    session.send("diagnostic-tail").await;
    session.expect_output("diagnostic-tail").await;

    let laptop_dump = laptop.debug_dump(true).await;
    for field in ["hosts", "routes", "links", "tunnels"] {
        let entries = laptop_dump[field]
            .as_array()
            .unwrap_or_else(|| panic!("laptop dump field {field} is not an array"));
        assert!(!entries.is_empty(), "laptop dump field {field} is empty");
    }

    let desktop_dump = desktop.debug_dump(true).await;
    let agents = desktop_dump["users"][0]["agents"]
        .as_array()
        .expect("desktop verbose dump has an agents array");
    let worker = agents
        .iter()
        .find(|agent| agent["name"] == "worker")
        .unwrap_or_else(|| panic!("desktop dump contains the attached worker: {agents:#?}"));
    let session_debug = &worker["session"]["session"];
    assert!(
        session_debug["subscriber_count"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "desktop session has a live subscriber: {session_debug}"
    );
    assert!(session_debug["epoch"].is_u64());
    assert!(session_debug["buffer"].is_object());
    let head = session_debug["buffer"]["head_seq"]
        .as_u64()
        .expect("buffer head is a sequence number");
    let tail = session_debug["buffer"]["tail_seq"]
        .as_u64()
        .expect("buffer tail is a sequence number");
    assert!(tail > head, "echoed bytes move the retained buffer tail");
    assert!(session_debug["buffer"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(session_debug["backend"]["state"], "running");
    assert!(session_debug["obligations"].is_array());

    let evidence = serde_json::json!({
        "laptop": laptop_dump,
        "desktop": desktop_dump,
    });
    println!(
        "AMUX_DEBUG_DUMP_BEGIN\n{}\nAMUX_DEBUG_DUMP_END",
        serde_json::to_string_pretty(&evidence).expect("debug evidence serializes")
    );
}
