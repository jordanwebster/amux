import Foundation

/// A recognition session edits only the text it inserted. A keyboard edit or
/// caret move ends that session before a revised hypothesis can replace it.
public struct DictationState: Equatable, Sendable {
    public enum Permission: Sendable { case notAsked, allowed, denied }
    public enum Phase: Equatable, Sendable {
        case idle, requestingPermission, starting, listening, denied, unavailable
    }

    public private(set) var phase: Phase = .idle
    private var original: MessageDraft?
    private var expected: MessageDraft?

    public init() {}

    public var active: Bool {
        phase == .requestingPermission || phase == .starting || phase == .listening
    }

    public var sentence: String? {
        switch phase {
        case .idle: nil
        case .requestingPermission: "Allow microphone and speech access to dictate. Your draft stays here."
        case .starting: "Getting ready to listen. Your draft stays here."
        case .listening: "Listening. Tap Stop Dictation when you’re done."
        case .denied: "Dictation needs microphone and speech access. You can allow them in Settings."
        case .unavailable: "Dictation isn’t available right now. You can keep typing."
        }
    }

    /// No microphone is opened until both permissions and local recognition
    /// are available. A denial takes precedence so Settings remains findable.
    public mutating func prepare(speech: Permission, microphone: Permission, available: Bool) {
        if speech == .denied || microphone == .denied {
            phase = .denied
        } else if !available {
            phase = .unavailable
        } else if speech == .notAsked || microphone == .notAsked {
            phase = .requestingPermission
        } else {
            phase = .starting
        }
    }

    public mutating func began(draft: MessageDraft) {
        guard phase == .starting else { return }
        original = draft
        expected = draft
        phase = .listening
    }

    /// Speech supplies cumulative hypotheses, including corrections. Rebuild
    /// only from the unchanged draft at the start, never append the same words
    /// twice or discard text and attachment tokens on either side of the caret.
    public mutating func receive(_ text: String, draft: inout MessageDraft) {
        guard phase == .listening else { return }
        guard draft == expected, var replacement = original else {
            stop()
            return
        }
        replacement.insert(text: text)
        draft = replacement
        expected = replacement
    }

    public mutating func stop() {
        phase = .idle
        original = nil
        expected = nil
    }

    public mutating func failed() {
        stop()
        phase = .unavailable
    }
}
