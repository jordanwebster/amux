import AmuxCore
import AmuxFeatures
import Foundation

/// What a conversation has open over itself and where it is being read.
///
/// Both are view state, and both belong to the screen that owns them: a card
/// somebody opened is about the message they are writing, and the entry they
/// scrolled back to is about what they are reading. Neither is in any message
/// the runtime carries, so neither can be worked out again from a recording of
/// one — and a report of a conversation without them replays an empty screen
/// resting at the tail of the transcript, which is not the screen anybody was
/// complaining about.
///
/// So this is the way out of the screen and the way back in. Nothing in the
/// shipping app makes one: it exists so a build with the reporting tools in it
/// can keep the recording, and it is closures and plain values rather than a
/// recorder this package knows about, for the same reason ``Router/arrived``
/// is.
@MainActor
public final class ConversationRecording {
    /// Told what one conversation has open over itself, named the way the
    /// screen that owns it names it, or nothing when it has closed everything.
    public var opened: (@MainActor (AgentId, String?) -> Void)?
    /// Told where the reader of one transcript has come to rest, once they
    /// have taken it off its tail.
    public var read: (@MainActor (AgentId, TranscriptResting) -> Void)?
    /// Told when a finished turn's offer of its changes is set aside, or
    /// offered again.
    public var asided: (@MainActor (AgentId, Bool) -> Void)?
    /// What each conversation is to be built already showing. Empty in the
    /// app; written by a replay before the page it names is built, which is
    /// the only moment a screen's own state can be decided from outside it.
    public var showing: [AgentId: String] = [:]
    /// Where each transcript is to be built already resting.
    public var reading: [AgentId: TranscriptResting] = [:]
    /// Which conversations are to be built with a finished turn's offer
    /// already set aside.
    public var aside: Set<AgentId> = []

    /// What the drawer is called where a recording names what is open. It is
    /// not one of the conversation's own overlays — it is the fleet borrowing
    /// the screen — so it is named beside them rather than among them.
    public static let drawer = "drawer"

    public init() {}
}
