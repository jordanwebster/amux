import AmuxCore
import AmuxDesign
import SwiftUI

/// What has to be said where the composer will go.
///
/// The three ways a conversation stops taking messages are not the same thing
/// and are not drawn as one. A machine that has gone away can be waited for and
/// asked again; a layer that is catching up will take a message shortly and the
/// screen only has to say so; a run that has ended will never take one, and the
/// screen offers nothing at all — restarting is starting a new agent, and
/// deleting a run at the moment somebody is reading it was never something to
/// put in front of them.
///
/// Which of these is true is read off the core's own typed gate and the core's
/// own refusal. The phone decides nothing about whether a message may be sent;
/// it says what it was told, in the words a person reads.
enum ConversationFootState: Equatable {
    /// The machine that owns this agent is not answering. The feed above is
    /// the last thing that was true and stays readable.
    case unreachable(host: String, since: String?)
    /// The layer will not take a message now. `reason` is the core's own
    /// sentence when it refused one, and this build's sentence for the gate
    /// when nothing has been attempted.
    case refused(headline: String, reason: String)

    /// Nothing to say, which is most conversations.
    init?(gate: SendGate, results: [OpResult], subject: ConversationSubject) {
        // An ended run offers nothing. It has already said what happened, in
        // the feed, where the last thing that happened belongs.
        if subject.ended != nil { return nil }
        if !subject.hostReachable, let host = subject.host {
            self = .unreachable(host: host, since: subject.age)
            return
        }
        guard let sentence = Self.sentence(for: gate) else { return nil }
        // The core's own words whenever the core has spoken. A refusal
        // rewritten on the phone is a second opinion about something only the
        // host knows.
        let refusal = results.last.flatMap { result -> String? in
            guard case .failed(let failure) = result.outcome else { return nil }
            return failure.message
        }
        self = .refused(
            headline: refusal == nil ? "Cannot send" : "Not sent",
            reason: refusal ?? sentence)
    }

    /// What this build says about a gate nobody has tried to send through.
    ///
    /// Only the two gates that pass on their own are worded here. A gate that
    /// is waiting on the reader is the ask panel's to report, one that is
    /// working is the composer's, and one that says the layer is unavailable
    /// has nothing to add to a screen that is already empty.
    private static func sentence(for gate: SendGate) -> String? {
        switch gate {
        case .claudePty(let gate):
            switch gate {
            case .replaying: replaying
            case .sendInFlight: inFlight
            default: nil
            }
        case .codex(let gate):
            switch gate {
            case .replaying: replaying
            case .inputInFlight: inFlight
            default: nil
            }
        case .unavailable: nil
        }
    }

    private static let replaying = "This session is replaying what it missed."
    private static let inFlight = "The last message has not been acknowledged yet."
}

/// The panel in the composer's place.
///
/// It is the same plate the composer will be — a frosted card along the bottom
/// edge — so that when the composer lands the screen does not change shape
/// under a reader, only what is written on it.
struct ConversationFoot: View {
    @Environment(\.design) private var design
    let state: ConversationFootState
    let retry: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 10) {
                mark
                VStack(alignment: .leading, spacing: 2) {
                    Text(headline)
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ink.color)
                    Text(detail)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 4)
            }
            // Asking again is worth offering only where waiting is what is
            // happening. A layer that is catching up needs no button: it is
            // already doing the thing a button would ask for.
            if case .unreachable = state {
                Button(action: retry) {
                    ActionLabel("Retry Now", kind: .outline, fill: true)
                }
                .buttonStyle(.plain)
                .padding(.top, 12)
                .accessibilityLabel("Retry Now")
                .identified("conversation.retry", label: "Retry Now")
            }
        }
        .padding(14)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("conversation.foot", label: headline, value: detail)
    }

    /// A hollow mark for a machine whose state is genuinely unknown, and the
    /// accent on the glyph alone for a refusal — the same rule the transcript
    /// follows, where a denied row is coloured on its mark and nowhere else.
    @ViewBuilder
    private var mark: some View {
        switch state {
        case .unreachable:
            AttentionMark(attention: .unknown, size: 18)
        case .refused:
            Image(systemName: "exclamationmark.circle")
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(design.accent.color)
                .frame(width: 18, height: 18)
        }
    }

    private var headline: String {
        switch state {
        case .unreachable(let host, _): "\(host) is unreachable"
        case .refused(let headline, _): headline
        }
    }

    private var detail: String {
        switch state {
        case .unreachable(_, let since):
            ["Reconnecting", since.map { "last update \($0) ago" }]
                .compactMap { $0 }.joined(separator: " · ")
        case .refused(_, let reason): reason
        }
    }
}

/// A run that stopped for good, at the end of the feed.
///
/// A run that ended is not the same as an agent that is idle, and the exit code
/// is the only thing that says which. Stated plainly, and then nothing: the
/// calls to action that were drawn here were cut, because restarting is
/// starting a new agent and deleting a finished run is not something to offer
/// somebody at the moment they are reading what it did.
struct EndOfRun: View {
    @Environment(\.design) private var design
    let ended: ConversationSubject.Ended
    let age: String?
    let host: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Image(systemName: "stop.circle")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(design.inkMuted.color)
                Text(headline)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                Spacer(minLength: 0)
                if let age {
                    Text(age)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                }
            }
            Explain(sentence)
        }
        .padding(13)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                .fill(design.sunken.color)
        }
        .accessibilityElement(children: .combine)
        .identified("conversation.exited", label: headline, value: sentence)
    }

    /// "Exited · code 1", or just "Exited" where the host never said which.
    /// An absent code is not a zero and is never drawn as one.
    private var headline: String {
        guard let code = ended.code else { return "Exited" }
        return "Exited · code \(code)"
    }

    private var sentence: String {
        guard let host else { return "The process ended on its own." }
        return "The process ended on its own. Nothing is still running on \(host)."
    }
}
