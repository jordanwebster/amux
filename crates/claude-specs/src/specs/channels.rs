//! Specifications for how a message reaches headless Claude and how the
//! sender recognises it again: the id a stdin message carries, and the
//! messaging socket another program can post through.

use serde_json::Value;

use super::{HAIKU, SessionSetup, SpecDef, SpecSession, Turn};
use crate::driver::sdk::{Message, PermissionMode, SettingsConfig};
use crate::expect;

/// The id the specification gives its prompt. Any UUID works; a recognisable
/// one keeps the recording readable.
const PROMPT_UUID: &str = "11111111-1111-4111-8111-111111111111";

/// The command a hook runs to post a message through the session's messaging
/// socket. `claude-probe` installs it on the capture's `PATH`.
pub const PEER_MESSAGE_HELPER: &str = "spec-peer-message";

pub(super) static CLIENT_ID: SpecDef = SpecDef {
    name: "probes/client_id",
    fixture: "client_id",
    setup: client_id_setup,
    run: |session| Box::pin(client_id(session)),
};

fn client_id_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly ALPHA and nothing else.");
    setup.prompt_uuid = Some(PROMPT_UUID.to_owned());
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
        .options
        .extra_args
        .insert("replay-user-messages".to_owned(), None);
    setup
}

/// A stdin message's `uuid` is the sender's handle on it: Claude hands it back
/// on the message's reflection and on every step of its command lifecycle, so
/// the sender matches what Claude took by id rather than by arrival order.
async fn client_id(session: &mut SpecSession) {
    let mut turn = session.turn().await;
    expect!(turn.succeeded(), "the prompt is answered");
    turn.messages.extend(session.drain().await.messages);
    let frames = raw(&turn);

    let reflections = frames
        .iter()
        .filter(|frame| frame["type"] == "user" && frame["isReplay"] == true)
        .collect::<Vec<_>>();
    expect!(
        reflections.len() == 1 && reflections[0]["uuid"] == PROMPT_UUID,
        "with --replay-user-messages the prompt comes back once, carrying the \
         uuid the sender gave it: {reflections:?}"
    );

    let lifecycle = lifecycle_states(&frames, PROMPT_UUID);
    expect!(
        lifecycle == ["queued", "started", "completed"],
        "the prompt's command lifecycle is queued, started, then completed, \
         each frame naming the sender's uuid: {lifecycle:?}"
    );
}

pub(super) static SIDE_CHANNEL: SpecDef = SpecDef {
    name: "probes/side_channel",
    fixture: "side_channel",
    setup: side_channel_setup,
    run: |session| Box::pin(side_channel(session)),
};

fn side_channel_setup() -> SessionSetup {
    let mut setup = SessionSetup::conversation(HAIKU, "Reply with exactly ALPHA and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    // Socket paths are limited in length, so this one is short. The path only
    // matters live, where the harness creates its private directory; replay
    // starts no process.
    let socket = format!("/tmp/claude-spec-{}/messaging.sock", std::process::id());
    setup
        .options
        .extra_args
        .insert("messaging-socket-path".to_owned(), Some(socket));
    setup
        .options
        .extra_args
        .insert("replay-user-messages".to_owned(), None);
    setup.options.settings = Some(SettingsConfig::Inline(serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{PEER_MESSAGE_HELPER} 'Reply with exactly the word BRAVO.'"),
                }],
            }],
        },
    })));
    setup
}

/// Headless Claude honours `--messaging-socket-path`: it listens there, hands
/// the socket and its token to hooks, and runs what another program posts as
/// a peer message. Claude picks that message's id, not the poster.
async fn side_channel(session: &mut SpecSession) {
    session
        .advance_until(|message| peer_reflection(message).is_some())
        .await;
    let mut turn = session.turn().await;
    turn.messages.extend(session.drain().await.messages);
    let frames = raw(&turn);

    let peers = frames
        .iter()
        .filter(|frame| {
            frame["type"] == "user"
                && frame["isReplay"] == true
                && frame["origin"]["kind"] == "peer"
        })
        .collect::<Vec<_>>();
    expect!(
        peers.len() == 1,
        "the posted message comes back once, marked as coming from a peer: {peers:?}"
    );
    let peer = peers[0];
    expect!(
        peer["message"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("Reply with exactly the word BRAVO.")),
        "the model sees the posted text, inside Claude's own framing: {:?}",
        peer["message"]["content"]
    );
    let id = peer["uuid"].as_str().unwrap_or_default();
    expect!(
        uuid::Uuid::parse_str(id).is_ok(),
        "Claude gives the posted message an id of its own: {id:?}"
    );
    let lifecycle = lifecycle_states(&frames, id);
    expect!(
        lifecycle.ends_with(&["completed".to_owned()]) && lifecycle.contains(&"started".to_owned()),
        "the posted message runs through Claude's command lifecycle under that \
         id: {lifecycle:?}"
    );
}

fn peer_reflection(message: &Message) -> Option<Value> {
    let Message::UserReplay(_) = message else {
        return None;
    };
    let frame = serde_json::to_value(message).ok()?;
    (frame["origin"]["kind"] == "peer").then_some(frame)
}

/// Every frame in the order it arrived, as Claude wrote it.
fn raw(turn: &Turn) -> Vec<Value> {
    turn.messages()
        .iter()
        .map(|message| serde_json::to_value(message).expect("a parsed frame serialises"))
        .collect()
}

fn lifecycle_states(frames: &[Value], id: &str) -> Vec<String> {
    frames
        .iter()
        .filter(|frame| frame["type"] == "command_lifecycle" && frame["command_uuid"] == id)
        .filter_map(|frame| frame["state"].as_str().map(str::to_owned))
        .collect()
}
