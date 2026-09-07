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
    let state: ComposerState
    /// Whose box this is, which is what the empty field says.
    let agent: String
    @Binding var text: String
    let actions: @MainActor (ConversationAction) -> Void
    @FocusState private var writing: Bool
    /// How tall the prose in the field is, measured off a `Text` of it.
    @State private var prose: CGFloat = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let activity = state.activity {
                WorkingLine(activity: activity)
            }
            field
            footer
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .identified("composer", label: placeholder, value: text)
    }

    private var placeholder: String { state.placeholder(agent: agent) }

    private var written: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var field: some View {
        HStack(alignment: .top, spacing: 8) {
            growing
            // Throwing away what you wrote is its own control and shares no
            // gesture with stopping the agent. They are opposite intentions —
            // one is about the message, one is about the turn — and a phone
            // has no modifier key to tell two meanings of one button apart.
            if written {
                Button { text = "" } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 16, weight: .regular))
                        .foregroundStyle(design.inkFaint.color)
                        .frame(width: 28, height: 28)
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear")
                .identified("composer.clear", label: "Clear")
            }
        }
    }

    /// The field, sized by the prose in it rather than by itself.
    ///
    /// A vertical `TextField` measures its own height twice: the box holding
    /// four lines settled two device pixels apart between launches, the whole
    /// plate moved with it, and a still of a layout with two resting places is
    /// a coin toss. `Text` is a pure function of the string, the face and the
    /// width, so the same paragraph is the same height every time. It is
    /// measured hidden underneath and the field is laid into the height it
    /// asks for.
    private var growing: some View {
        ZStack(alignment: .topLeading) {
            Text(text.isEmpty ? placeholder : text)
                .designFont(.body, design)
                .lineLimit(Self.lines)
                .frame(maxWidth: .infinity, alignment: .leading)
                .hidden()
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { prose = $0 }
            TextField(placeholder, text: $text, axis: .vertical)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .lineLimit(1...Self.lines)
                .focused($writing)
                // A blinking caret is a clock, and a baseline cannot
                // photograph one: whichever half of the blink the shutter
                // catches is the picture. See ios/Goldens/BASELINE.md.
                .tint(photographed ? .clear : design.accentColor)
                // The predictive strip above the keyboard rewrites itself as
                // the system thinks about what was typed, which is a second
                // thing on a photographed screen that will not hold still.
                .autocorrectionDisabled()
                .frame(maxWidth: .infinity, alignment: .topLeading)
                .identified("composer.field", label: placeholder, value: text)
        }
        .frame(height: prose > 0 ? prose : nil)
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
            Spacer(minLength: 0)
            Button { actions(.dictate) } label: {
                Image(systemName: "mic")
                    .font(.system(size: 18, weight: .regular))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 44, height: 44)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Dictate")
            .identified("composer.dictate", label: "Dictate")
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
/// A named activity and a number, and then a segment that travels. No track
/// behind it, because a track is the shape of a thing with a known end and
/// nothing here knows when the turn will finish; drawing the trough of a
/// progress bar would promise a proportion the app cannot compute.
private struct WorkingLine: View {
    @Environment(\.design) private var design
    let activity: ComposerActivity

    var body: some View {
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
            MovingSegment()
                .frame(minWidth: 40)
                .padding(.leading, 4)
        }
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
/// Held part-way rather than at either end, because a segment pinned to the
/// left edge reads as a bar that has not started.
private struct MovingSegment: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var travelled = false

    private static let width = 0.42
    private static let resting = 0.3

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
