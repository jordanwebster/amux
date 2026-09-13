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
        VStack(alignment: .leading, spacing: 0) {
            if let activity = state.activity {
                VStack(alignment: .leading, spacing: 0) {
                    WorkingLine(activity: activity)
                        .padding(.horizontal, 14)
                        .padding(.top, 11)
                        .padding(.bottom, 9)
                    MovingSegment()
                }
                // Grown into and settled out of, rather than appearing and
                // vanishing: the box getting taller *is* the turn starting,
                // and a jump there is the composer moving under a thumb that
                // is about to write in it.
                .transition(.opacity.combined(with: .move(edge: .bottom)))
            }
            field
                .padding(.horizontal, 14)
                .padding(.top, 13)
                .padding(.bottom, 4)
            if let sentence = dictation.sentence {
                VStack(alignment: .leading, spacing: 4) {
                    Text(sentence)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkMuted.color)
                        .identified("composer.dictation", label: sentence)
                    if dictation.phase == .denied {
                        Button { actions(.dictationSettings) } label: {
                            Text("Open Settings")
                                .designFont(.caption, design)
                                .foregroundStyle(design.ink.color)
                                .frame(minWidth: 44, minHeight: 44)
                                .contentShape(Rectangle())
                        }
                        .identified("composer.dictation.settings", label: "Open Settings")
                    }
                }
                .padding(.horizontal, 14)
                .padding(.top, 4)
            }
            footer
                .padding(.horizontal, 9)
                .padding(.bottom, 9)
                .padding(.top, 3)
        }
        .moving(value: state.activity != nil)
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
                .buttonStyle(.amuxControl)
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
        HStack(spacing: 9) {
            Button { actions(.attach) } label: {
                Image(systemName: "plus")
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 30, height: 30)
                    .background { Circle().fill(design.sunken.color.opacity(0.8)) }
                    .thumbTarget(x: 7, y: 7)
                    .contentShape(Circle())
            }
            .buttonStyle(.amuxControl)
            .accessibilityLabel("Attach")
            .identified("composer.attach", label: "Attach")
            .reclaimingThumbTarget(x: 7, y: 7)
            // Between the plus and the microphone, which is where the design
            // puts it: it is a standing fact about the message you are about
            // to send rather than an action on it.
            ModelChip(provider: provider) { actions(.openSettings) }
            Spacer(minLength: 0)
            Button { actions(.dictate) } label: {
                Image(systemName: dictation.active ? "stop.circle" : "mic")
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 30, height: 30)
                    .thumbTarget(x: 7, y: 7)
                    .contentShape(Circle())
            }
            .buttonStyle(.amuxControl)
            .accessibilityLabel(dictation.active ? "Stop Dictation" : "Dictate")
            .identified("composer.dictate", label: dictation.active ? "Stop Dictation" : "Dictate")
            .reclaimingThumbTarget(x: 7, y: 7)
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
            .buttonStyle(.amuxControl)
            .accessibilityLabel("Stop")
            .identified("composer.interrupt", label: "Stop")
        } else {
            Button { actions(.send) } label: {
                RoundButton(glyph: "arrow.up", filled: written)
            }
            .buttonStyle(.amuxControl)
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
            .font(.system(size: glyph == "stop.fill" ? 12 : 14, weight: .bold))
            .foregroundStyle(filled ? design.ground.color : design.inkFaint.color)
            .frame(width: 30, height: 30)
            .background {
                if filled {
                    Circle().fill(design.ink.color)
                } else {
                    Circle().fill(design.sunken.color.opacity(0.8))
                }
            }
            .thumbTarget(x: 7, y: 7)
            .contentShape(Circle())
            .reclaimingThumbTarget(x: 7, y: 7)
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
        HStack(spacing: 7) {
            Text(activity.name)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
                .lineLimit(1)
                .truncationMode(.middle)
            if let elapsed = activity.elapsed {
                Text(elapsed)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .identified(
            "composer.working", label: activity.name, value: activity.elapsed ?? "")
    }
}

/// A short line that stretches and recoils across the space it is given.
///
/// Two ends, not one block. Each crosses the same stretch of the row; they
/// differ only in when they set off. The leading end leaves first and the
/// trailing end follows a beat later, so the line pulls long as it departs and
/// gathers up as it arrives. Set the lag to zero and the rigid shuttle this
/// used to be comes back.
///
/// Why elastic at all: a fixed-width block sliding to and fro reads as a
/// shuttle on a track, and a track is the shape of a thing with a known end.
/// The rule above it already went trackless for that reason. A length that
/// will not hold still finishes the thought — nothing here is being measured.
///
/// Expressed as keyframes rather than as a formula sampled off a clock, so
/// SwiftUI owns the timing: it stops when the view leaves the hierarchy, and
/// the phase lag is two keyframe durations rather than arithmetic anybody has
/// to reason about.
///
/// Under Reduce Motion, and in front of a camera, it holds at one position:
/// the capture keeps the last of many photographs when no run of them agree,
/// so anything moving on a clock of its own makes a baseline a coin toss. Held
/// at the middle of its crossing, which is both where the line is at its
/// longest and where it already held before it could stretch — so the still
/// frame is unchanged.
private struct MovingSegment: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reduceMotion

    /// Where the two ends are, as fractions of the row's width.
    private struct Ends {
        var left = Self.leftNear
        var right = Self.rightNear

        static let leftNear = 0.025
        static let leftFar = 0.87
        static let rightNear = 0.13
        static let rightFar = 0.975
    }

    /// How much of a crossing each end waits out before it sets off. The gap
    /// between the two is the whole effect.
    private static let lead = 0.04
    private static let follow = 0.22
    private static let cross = 0.74

    /// Where a held frame sits: the middle of a crossing, at the length the
    /// line is longest. A segment pinned to an edge reads as a bar that has
    /// not started, and a hairline reads as nothing at all.
    private static let heldLeft = 0.29
    private static let heldRight = 0.71

    var body: some View {
        GeometryReader { frame in
            let width = frame.size.width
            if still {
                line(width, Self.heldLeft, Self.heldRight)
            } else {
                KeyframeAnimator(initialValue: Ends(), repeating: true) { ends in
                    line(width, ends.left, ends.right)
                } keyframes: { _ in
                    // Outbound the right end leads and on the way back the
                    // left one does, which is why these are the same shape
                    // with their waits swapped rather than one reversed.
                    KeyframeTrack(\.left) {
                        hold(Ends.leftNear, Self.follow)
                        travel(to: Ends.leftFar)
                        hold(Ends.leftFar, 2 * Self.lead)
                        travel(to: Ends.leftNear)
                        hold(Ends.leftNear, Self.follow)
                    }
                    KeyframeTrack(\.right) {
                        hold(Ends.rightNear, Self.lead)
                        travel(to: Ends.rightFar)
                        hold(Ends.rightFar, 2 * Self.follow)
                        travel(to: Ends.rightNear)
                        hold(Ends.rightNear, Self.lead)
                    }
                }
            }
        }
        .frame(height: 1)
        .allowsHitTesting(false)
    }

    private func line(_ width: CGFloat, _ left: Double, _ right: Double) -> some View {
        Capsule()
            .fill(design.inkFaint.color)
            .frame(width: width * (right - left), height: 1)
            .offset(x: width * left)
    }

    /// Both tracks run two crossings and come back to where they began, so the
    /// loop closes without anybody counting legs.
    private func hold(_ value: Double, _ share: Double) -> LinearKeyframe<Double> {
        LinearKeyframe(value, duration: share * Motion.breath)
    }

    private func travel(to value: Double) -> LinearKeyframe<Double> {
        LinearKeyframe(value, duration: Self.cross * Motion.breath, timingCurve: .easeInOut)
    }

    private var still: Bool { photographed || reduceMotion }
}
