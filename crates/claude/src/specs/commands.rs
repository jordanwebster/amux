//! Specifications for the things a session does that are not answering.
//!
//! A slash command sent as a prompt is not a question for the model; it is an
//! instruction to Claude Code, and what comes back is a change to the session
//! itself. These are the cheapest way to reach behaviour that otherwise needs a
//! session to run long enough to hit it by accident.

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::expect;
use crate::sdk::{CompactTrigger, PermissionMode};

pub(super) static COMPACTED: SpecDef = SpecDef {
    name: "commands/compacted",
    fixture: "compacted",
    setup: compacted_setup,
    run: |session| Box::pin(compacted(session)),
};

fn compacted_setup() -> SessionSetup {
    // Compaction needs something to compact, so this opens a conversation and
    // fills it before asking. A single prompt would have nothing to reclaim and
    // `/compact` would simply return.
    let mut setup = SessionSetup::conversation(
        HAIKU,
        "Write three paragraphs about the history of rope-making.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// A conversation can be compacted, and the session says what that cost.
///
/// This is the one event in the protocol that silently changes what the model
/// can see. A caller that kept its own copy of the conversation and never
/// noticed the boundary would drift out of step with the session it is showing
/// — which is why the boundary is reported at all, and why it carries the
/// token counts either side rather than just announcing that something
/// happened.
async fn compacted(session: &mut SpecSession) {
    let first = session.turn().await;
    expect!(first.succeeded(), "the conversation starts normally");

    session
        .say("Now three more paragraphs about knots.")
        .await
        .expect("a second turn can be sent");
    let second = session.turn().await;
    expect!(second.succeeded(), "and continues");

    session
        .say("/compact")
        .await
        .expect("a slash command is sent as a prompt like any other message");
    let compaction = session.turn().await;

    let boundaries = compaction.compactions();
    expect!(
        boundaries.len() == 1,
        "compacting reports one boundary, so a caller can tell exactly where \
         the conversation it was shown stopped being the conversation the model \
         has: {} reported",
        boundaries.len()
    );
    let boundary = boundaries[0];
    expect!(
        boundary.compact_metadata.trigger == CompactTrigger::Manual,
        "and names this compaction as the one that was asked for, rather than \
         one the session decided on: {:?}",
        boundary.compact_metadata.trigger
    );
    expect!(
        boundary.compact_metadata.pre_tokens > 0,
        "the boundary accounts for what was there before it: {}",
        boundary.compact_metadata.pre_tokens
    );
    expect!(
        boundary
            .compact_metadata
            .post_tokens
            .is_some_and(|after| after < boundary.compact_metadata.pre_tokens),
        "and for what is left, which is less - a compaction that reclaimed \
         nothing would be a boundary a caller should not have been given: \
         {:?} from {}",
        boundary.compact_metadata.post_tokens,
        boundary.compact_metadata.pre_tokens
    );
    expect!(
        compaction.replayed() > 0,
        "the conversation the model keeps is re-stated as replayed history, \
         which is how a caller re-syncs to what actually survived"
    );
}

pub(super) static CLEARED: SpecDef = SpecDef {
    name: "commands/cleared",
    fixture: "cleared",
    setup: cleared_setup,
    run: |session| Box::pin(cleared(session)),
};

fn cleared_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "/clear");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup
}

/// Clearing a conversation starts a new one inside the same session.
///
/// The distinction matters to anything holding a transcript: the session id is
/// unchanged, so a caller keying on that alone would append the new
/// conversation to the old one. The reset names the conversation that replaces
/// it.
async fn cleared(session: &mut SpecSession) {
    let session_id = session.session_id().to_owned();
    let turn = session.turn().await;

    expect!(
        turn.succeeded(),
        "clearing completes like any other instruction"
    );
    expect!(
        turn.saw("conversation_reset"),
        "and is reported as a reset rather than as an answer, so a caller is \
         told the conversation it was following has ended"
    );
    expect!(
        turn.text().is_empty(),
        "the model is not asked anything, so it says nothing: {:?}",
        turn.text()
    );
    expect!(
        session.session_id() == session_id,
        "the session outlives the conversation it was holding: {} against \
         {session_id}",
        session.session_id()
    );
}
