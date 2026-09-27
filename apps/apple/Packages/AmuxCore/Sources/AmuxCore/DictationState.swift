import Foundation

/// One dictation session over a chat's draft.
///
/// Speech hands over cumulative hypotheses, each one the whole of what was
/// heard so far, corrections included. The session remembers the draft it
/// started from and rebuilds the draft from it with each hypothesis, so a
/// correction replaces the words it corrects rather than appending beside
/// them. It only ever rewrites text it wrote: when the draft is not what the
/// last hypothesis left, the person edited it, and the session ends.
public struct DictationState: Equatable, Sendable {
    public enum Permission: Sendable { case notAsked, allowed, denied }
    public enum Phase: Equatable, Sendable {
        case idle, requestingPermission, starting, listening, denied, unavailable
    }

    public private(set) var phase: Phase = .idle
    private var original: String?
    private var expected: String?

    public init() {}

    public var active: Bool {
        phase == .requestingPermission || phase == .starting || phase == .listening
    }

    public var sentence: String? {
        switch phase {
        case .idle: nil
        case .requestingPermission:
            String(localized: "Allow microphone and speech access to dictate. Your draft stays here.")
        case .starting: String(localized: "Getting ready to listen. Your draft stays here.")
        case .listening: String(localized: "Listening. Tap Stop Dictation when you’re done.")
        case .denied: String(localized: "Dictation needs microphone and speech access. You can allow them in Settings.")
        case .unavailable: String(localized: "Dictation isn’t available right now. You can keep typing.")
        }
    }

    /// No microphone is opened until both permissions are granted and
    /// on-device recognition is there. A denial wins, so Settings stays
    /// findable.
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

    public mutating func began(draft: String) {
        guard phase == .starting else { return }
        original = draft
        expected = draft
        phase = .listening
    }

    /// Rebuilds the draft from where the session started and the whole of
    /// what has been heard. A draft someone edited ends the session instead.
    public mutating func receive(_ heard: String, draft: inout String) {
        guard phase == .listening else { return }
        guard draft == expected, let original else {
            stop()
            return
        }
        let joiner = original.isEmpty || original.last?.isWhitespace == true || heard.isEmpty ? "" : " "
        draft = original + joiner + heard
        expected = draft
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
