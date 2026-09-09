import AmuxCore
import AmuxDesign
import SwiftUI

/// The box a message is written in.
///
/// A field with a footer under it rather than a field with controls inside it.
/// A message to an agent is often a paragraph — the whole reason this app has
/// a phone client is that people write to agents from places they cannot open
/// a terminal — so the field is allowed to grow, and the things you can do to
/// a message sit on a row of their own underneath, where they do not move as
/// the text does.
///
/// Return inserts a newline. Sending is the button, and only the button: a
/// keyboard whose return key sends is a keyboard that cannot write a second
/// paragraph, and on a phone there is no modifier to escape it with.
struct ComposerBox: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    @Environment(\.dynamicTypeSize) private var typeSize
    let state: ComposerState
    /// Whose box this is, which is what the empty field says.
    let agent: String
    /// What the layer reports about how this agent thinks, which is what the
    /// footer chip names and what its sheet is built from.
    let provider: ProviderFacts
    @Binding var draft: MessageDraft
    var dictation = DictationState()
    let actions: @MainActor (ConversationAction) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let activity = state.activity {
                WorkingLine(activity: activity)
            }
            field
            if let sentence = dictation.sentence {
                VStack(alignment: .leading, spacing: 4) {
                    Text(sentence)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkMuted.color)
                        .identified("composer.dictation", label: sentence)
                    if dictation.phase == .denied {
                        Button("Open Settings") { actions(.dictationSettings) }
                            .designFont(.caption, design)
                            .foregroundStyle(design.ink.color)
                            .frame(minHeight: 44)
                            .identified("composer.dictation.settings", label: "Open Settings")
                    }
                }
            }
            footer
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("composer", label: placeholder, value: spoken)
    }

    private var placeholder: String { state.placeholder(agent: agent) }

    private var written: Bool { !draft.isEmpty }

    /// What the field says, with each token read as the words on it rather
    /// than as the character it occupies. It is what VoiceOver speaks and what
    /// a driver reads back, and neither can see a chip.
    private var spoken: String {
        var said = ""
        for character in draft.body {
            if let token = draft.tokens[character] {
                said += "[\(token.label)]"
            } else {
                said.append(character)
            }
        }
        return said
    }

    private var field: some View {
        HStack(alignment: .top, spacing: 8) {
            growing
            // Throwing away what you wrote is its own control and shares no
            // gesture with stopping the agent. They are opposite intentions —
            // one is about the message, one is about the turn — and a phone
            // has no modifier key to tell two meanings of one button apart.
            if written {
                Button { draft.clear() } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 16, weight: .regular))
                        .foregroundStyle(design.inkFaint.color)
                        .frame(width: 28, height: 28)
                        .thumbTarget(x: 9, y: 9)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear")
                .identified("composer.clear", label: "Clear")
                .reclaimingThumbTarget(x: 9, y: 9)
            }
        }
    }

    /// The field, and the placeholder behind it.
    ///
    /// A `UITextView` has no placeholder of its own, and the one the app draws
    /// is a piece of the design rather than a piece of the field: it is the
    /// same words in the same face as the ink that will replace them.
    private var growing: some View {
        ZStack(alignment: .topLeading) {
            if draft.body.isEmpty {
                Text(placeholder)
                    .designFont(.body, design)
                    .foregroundStyle(design.inkFaint.color)
                    .allowsHitTesting(false)
            }
            TokenTextField(
                draft: $draft, design: design, photographed: photographed, lines: Self.lines,
                typeSize: typeSize)
                .frame(maxWidth: .infinity, alignment: .topLeading)
                .identified("composer.field", label: placeholder, value: spoken)
        }
    }

    /// How far the box grows before the field scrolls inside it. Eight lines
    /// is most of a phone's screen; past that, what is being written is worth
    /// scrolling rather than worth burying the conversation under.
    private static let lines = 8

    private var footer: some View {
        HStack(spacing: 6) {
            Button { actions(.attach) } label: {
                Image(systemName: "plus")
                    .font(.system(size: 19, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 34, height: 34)
                    .background { Circle().fill(design.sunken.color) }
                    .frame(width: 44, height: 44)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Attach")
            .identified("composer.attach", label: "Attach")
            // Between the plus and the microphone, which is where the design
            // puts it: it is a standing fact about the message you are about
            // to send rather than an action on it.
            ModelChip(provider: provider) { actions(.openSettings) }
            Spacer(minLength: 0)
            Button { actions(.dictate) } label: {
                Image(systemName: dictation.active ? "stop.circle" : "mic")
                    .font(.system(size: 18, weight: .regular))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 44, height: 44)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(dictation.active ? "Stop Dictation" : "Dictate")
            .identified("composer.dictate", label: dictation.active ? "Stop Dictation" : "Dictate")
            primary
        }
    }

    /// One round button, and which of the two things it is depends on whether
    /// there is anything to say.
    ///
    /// With something written it sends — or holds, while a turn is running,
    /// which is what the field already said it would do. With nothing written
    /// and a turn running it stops the turn, because that is the only thing
    /// left for it to do and stopping is the reason a person reaches for the
    /// bottom of the screen mid-turn. With nothing written and nothing running
    /// it is drawn quiet and refuses, rather than disappearing: a control that
    /// comes and goes under the thumb is a control you cannot aim at.
    @ViewBuilder
    private var primary: some View {
        if state.busy && !written {
            Button { actions(.interrupt) } label: {
                RoundButton(glyph: "stop.fill", filled: true)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Stop")
            .identified("composer.interrupt", label: "Stop")
        } else {
            Button { actions(.send) } label: {
                RoundButton(glyph: "arrow.up", filled: written)
            }
            .buttonStyle(.plain)
            .disabled(!written)
            .accessibilityLabel(state.busy ? "Queue" : "Send")
            .identified(
                "composer.send", label: state.busy ? "Queue" : "Send", enabled: written)
        }
    }
}

/// The one round control at the composer's trailing edge.
private struct RoundButton: View {
    @Environment(\.design) private var design
    let glyph: String
    /// Filled in ink once it will do something. Ink and not the accent: the
    /// accent is this app's one word for "something is waiting for you", and a
    /// send button is not that.
    let filled: Bool

    var body: some View {
        Image(systemName: glyph)
            .font(.system(size: 15, weight: .semibold))
            .foregroundStyle(filled ? design.ground.color : design.inkFaint.color)
            .frame(width: 34, height: 34)
            .background {
                if filled {
                    Circle().fill(design.ink.color)
                } else {
                    Circle().strokeBorder(design.hairline.color, lineWidth: 1)
                }
            }
            .frame(width: 44, height: 44)
            .contentShape(Circle())
    }
}

/// What the agent is doing, above the field it will be answered in.
///
/// A named activity and a number, and under them a segment that travels. No
/// track behind it, because a track is the shape of a thing with a known end
/// and nothing here knows when the turn will finish; drawing the trough of a
/// progress bar would promise a proportion the app cannot compute.
///
/// The segment gets a row of its own, the full width of the card, rather than
/// the space left over beside the words. Two things come of that: it reads as
/// movement, which a short stub starting wherever the text happens to end does
/// not, and it draws the line the eye follows from the activity down into the
/// field underneath. Beside the label it read as a rule somebody had left in
/// the header.
private struct WorkingLine: View {
    @Environment(\.design) private var design
    let activity: ComposerActivity

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 8) {
                Text(activity.name)
                    .designFont(.body, design)
                    .foregroundStyle(design.inkMuted.color)
                    .lineLimit(1)
                if let elapsed = activity.elapsed {
                    Text(elapsed)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                }
            }
            MovingSegment()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .identified(
            "composer.working", label: activity.name, value: activity.elapsed ?? "")
    }
}

/// A short line that travels across the space it is given.
///
/// Under Reduce Motion, and in front of a camera, it holds at one position:
/// the capture keeps the last of many photographs when no run of them agree,
/// so anything sweeping on a timer of its own makes a baseline a coin toss.
/// Held at the middle of its travel rather than at either end, because a
/// segment pinned to the left edge reads as a bar that has not started.
private struct MovingSegment: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reduceMotion
    @State private var travelled = false

    private static let width = 0.42
    private static let resting = 0.5

    var body: some View {
        GeometryReader { frame in
            let travel = frame.size.width * (1 - Self.width)
            Capsule()
                .fill(design.inkFaint.color)
                .frame(width: frame.size.width * Self.width, height: 2)
                .offset(x: still ? travel * Self.resting : (travelled ? travel : 0))
                .animation(
                    still ? nil : .easeInOut(duration: 1.1).repeatForever(autoreverses: true),
                    value: travelled)
        }
        .frame(height: 2)
        .allowsHitTesting(false)
        .onAppear { travelled = true }
    }

    private var still: Bool { photographed || reduceMotion }
}
