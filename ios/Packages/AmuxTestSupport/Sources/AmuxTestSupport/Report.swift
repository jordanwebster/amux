import AmuxFeatures
import AmuxCore
import AmuxDesign
import SwiftUI

/// The offer to report what is on screen, after the system has taken a
/// screenshot of it.
///
/// It floats over whatever screen the person was on, because that is what they
/// photographed. It is the app's own control and not the system's: iOS hands
/// the app a notification once its screenshot is already taken and saved, and
/// puts its own preview up beside it. So this sits next to that preview rather
/// than under it — inset from the leading edge by more than the preview is
/// wide, since the app is not told where the preview is and cannot ask.
///
/// One tap either way. Taking it opens the report on the frame that was frozen
/// when the offer appeared; anywhere else on the screen puts the offer away
/// and lets go of the capture, so an accidental screenshot costs nothing.
public struct ReportPrompt: View {
    @Environment(\.design) private var design
    private let take: @MainActor () -> Void

    public init(take: @escaping @MainActor () -> Void) {
        self.take = take
    }

    public var body: some View {
        Button(action: take) {
            HStack(spacing: 9) {
                Image(systemName: "ladybug")
                    .font(.system(size: 19, weight: .regular))
                    .foregroundStyle(design.ink.color)
                Text("Report")
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
            }
            .padding(.horizontal, 18)
            .frame(height: 46)
            .frosted(Capsule())
        }
        .buttonStyle(.plain)
        .identified("report.prompt", label: "Report")
    }
}

/// The offer, placed where the system's own screenshot preview is not.
///
/// Written as a modifier so every surface that can be photographed carries it
/// the same way: the shell puts it over the whole app, and a capture of the
/// screenshot state puts it over the one screen being photographed.
public struct ReportOffer: ViewModifier {
    private let offering: Bool
    private let take: @MainActor () -> Void
    private let dismiss: @MainActor () -> Void

    public init(
        offering: Bool,
        take: @escaping @MainActor () -> Void,
        dismiss: @escaping @MainActor () -> Void
    ) {
        self.offering = offering
        self.take = take
        self.dismiss = dismiss
    }

    public func body(content: Content) -> some View {
        content.overlay {
            if offering {
                // One layer, in order: anywhere-else first so the prompt sits
                // on top of it and stays pressable. Two `overlay` calls would
                // not do — the later one is always the higher one, whatever
                // either says about its depth.
                ZStack(alignment: .bottomLeading) {
                    // Anywhere else is no. The catcher exists only while the
                    // offer does, so nothing it does can swallow a press meant
                    // for the screen underneath at any other time.
                    Color.clear
                        .contentShape(Rectangle())
                        .onTapGesture(perform: dismiss)
                        .accessibilityHidden(true)
                    ReportPrompt(take: take)
                        // Clear of the system's preview, which sits in the
                        // corner and is about 100 pt across at its widest
                        // setting. The app is never told the preview's frame,
                        // so the gap is stated here rather than measured.
                        .padding(.leading, 108)
                        // Above the composer and the tab bar, both of which
                        // live against the bottom of whatever screen has them.
                        .padding(.bottom, 122)
                }
                .transition(.opacity)
            }
        }
    }
}

extension View {
    /// Offers to report this screen, when something has just been frozen.
    public func reportOffer(
        _ offering: Bool,
        take: @escaping @MainActor () -> Void,
        dismiss: @escaping @MainActor () -> Void
    ) -> some View {
        modifier(ReportOffer(offering: offering, take: take, dismiss: dismiss))
    }
}


/// What the report screen asked for.
public enum ReportAction: Equatable, Sendable {
    case cancel
    case send
}

/// The report: the frozen frame, whatever has been drawn on it, one note about
/// the whole thing, and Send.
///
/// The frame is a photograph and not a live screen. Nothing on it can be
/// pressed, scrolled or opened, because none of it is there any more — what is
/// drawn is the moment somebody complained about, and a rectangle put on it
/// means the same place to whoever reads the bundle on a Mac.
public struct ReportScreen: View {
    @Environment(\.design) private var design
    private let model: ReportStore
    private let actions: @MainActor (ReportAction) -> Void

    public init(model: ReportStore, actions: @escaping @MainActor (ReportAction) -> Void) {
        self.model = model
        self.actions = actions
    }

    public var body: some View {
        @Bindable var model = model
        ZStack {
            design.ground.color.ignoresSafeArea()
            VStack(spacing: 0) {
                bar
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        frame
                        guidance("Drag a box around anything wrong. Each box takes a note.")
                        ForEach(Array(model.draft.marks.enumerated()), id: \.offset) { at, mark in
                            markNote(at: at, mark: mark)
                        }
                        note
                        guidance("Includes this screen and the available session and host records.")
                        if case .failed(let why) = model.sending { refusal(why) }
                        if case .sent(let receipt) = model.sending { sent(receipt) }
                    }
                    .padding(.horizontal, 18)
                    .padding(.bottom, 32)
                }
                .scrollDismissesKeyboard(.interactively)
            }
        }
        // A container, so Cancel and Send keep their own names. Without this
        // the screen's identifier is what every control under it reports, and
        // nothing on the page can be reached by the name it declared — by
        // VoiceOver or by anything driving it.
        .accessibilityElement(children: .contain)
        .identified("report.screen", value: state)
    }

    /// Cancel, the title, and the one thing that leaves the phone.
    private var bar: some View {
        HStack {
            Button { actions(.cancel) } label: {
                HStack(spacing: 3) {
                    Image(systemName: "chevron.left")
                        .font(.system(size: 15, weight: .semibold))
                    Text("Cancel")
                        .designFont(.body, design)
                }
                .foregroundStyle(design.accent.color)
                .thumbTarget(y: 13)
            }
            .buttonStyle(.plain)
            .identified("report.cancel", label: "Cancel")
            .reclaimingThumbTarget(y: 13)
            Spacer(minLength: 8)
            Text("Report")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Spacer(minLength: 8)
            Button { actions(.send) } label: {
                Text(sendTitle)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.accent.color.opacity(sending ? 0.4 : 1))
                    .thumbTarget(x: 5, y: 13)
            }
            .buttonStyle(.plain)
            .disabled(sending)
            .identified("report.send", label: sendTitle, enabled: !sending)
            .reclaimingThumbTarget(x: 5, y: 13)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    /// Send, or Retry once the cloud has already turned one down. The same
    /// press either way: nothing about the report changes on a failure, so
    /// offering a second, different action would be a lie about what happens.
    private var sendTitle: String {
        if case .failed = model.sending { return "Retry" }
        return "Send"
    }

    private var sending: Bool { model.sending == .sending }

    /// What the screen is, in one word, for anybody asking it from outside.
    private var state: String {
        switch model.sending {
        case .ready: "ready"
        case .sending: "sending"
        case .failed: "failed"
        case .sent: "sent"
        }
    }

    /// The frozen frame with everything drawn on it, at the shape it was taken
    /// at. The aspect comes from the recorded point size rather than from the
    /// pixels: dividing by the recorded scale is the only thing that puts a
    /// rectangle back where the finger drew it.
    @ViewBuilder private var frame: some View {
        if let capture = model.capture {
            FrozenFrameView(capture: capture, marks: model.draft.marks) { mark in
                model.mark(mark)
            }
            .aspectRatio(capture.frame.width / capture.frame.height, contentMode: .fit)
            .frame(maxWidth: .infinity)
            // Inset a long way. The picture is a whole screen shrunk to fit
            // inside another one, and drawn any wider it fills this screen
            // too — leaving the note and the boxes' own notes below the fold,
            // which is where the person is being asked to write.
            .padding(.horizontal, 85)
        }
    }

    /// One rectangle's own note, under the frame in the order they were drawn.
    private func markNote(at: Int, mark: ReportMark) -> some View {
        @Bindable var model = model
        return HStack(alignment: .top, spacing: 10) {
            Text("\(at + 1)")
                .designFont(.monoSmall, design)
                .foregroundStyle(design.onAccent.color)
                .frame(width: 22, height: 22)
                .background(Circle().fill(design.accent.color))
            TextField(
                "What is wrong here?",
                text: Binding(
                    get: { model.draft.marks.indices.contains(at) ? model.draft.marks[at].note : "" },
                    set: { model.note($0, on: at) }),
                axis: .vertical)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                .textInputAutocapitalization(.sentences)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                // Named in its own right. The card around it is a container
                // and hands nothing down, so without this the field somebody
                // types into has no name at all.
                .identified(
                    "report.mark.\(at).note", label: "What is wrong here?", value: mark.note)
            Button { model.unmark(at) } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(design.inkMuted.color)
                    .frame(width: 22, height: 22)
                    .thumbTarget(x: 12, y: 12)
            }
            .buttonStyle(.plain)
            .identified("report.mark.\(at).remove", label: "Remove box \(at + 1)")
            .reclaimingThumbTarget(x: 12, y: 12)
        }
        .padding(13)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous).fill(design.raised.color))
        // A container for the same reason the screen is one: the field and
        // the cross beside it are what somebody reaches for here.
        .accessibilityElement(children: .contain)
        .identified("report.mark.\(at)", label: "Box \(at + 1)", value: mark.note)
    }

    /// The one note about the whole thing.
    private var note: some View {
        @Bindable var model = model
        return TextField("What went wrong?", text: $model.draft.note, axis: .vertical)
            .designFont(.body, design)
            .foregroundStyle(design.ink.color)
            .textInputAutocapitalization(.sentences)
            .lineLimit(2...)
            // Take the width offered and grow downwards. A vertical field left
            // to size itself horizontally settles a few points wider or
            // narrower depending on what is already in it, and a note that
            // wrapped in a different place on two runs made a capture of this
            // screen disagree with itself.
            .fixedSize(horizontal: false, vertical: true)
            // Stretched before it is padded, not after. Padded first, the
            // field sizes itself to its own text and the stretch happens to
            // the box around it — so the line it wraps on depends on what the
            // field measured itself at, which is not the same on two runs.
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(16)
            .background(
                RoundedRectangle(cornerRadius: 18, style: .continuous).fill(design.raised.color))
            .identified("report.note", label: "What went wrong?", value: model.draft.note)
    }

    /// The two lines that say what to do and what is being sent, set in the
    /// mono face the design uses for anything the app is stating about itself.
    private func guidance(_ text: String) -> some View {
        Text(text)
            .designFont(.monoSmall, design)
            .foregroundStyle(design.inkMuted.color)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The cloud would not take it. What it said, and the draft still here.
    private func refusal(_ why: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("The report was not sent.")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Text(why)
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
            Text("Nothing you wrote has been lost. Retry sends the same report.")
                .designFont(.detail, design)
                .foregroundStyle(design.inkMuted.color)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous).fill(design.raised.color))
        .identified("report.refusal", label: "The report was not sent.", value: why)
    }

    /// It arrived. The receipt is what somebody quotes when they ask about it.
    private func sent(_ receipt: ReportReceipt) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Sent.")
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            Text(receipt.id)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkMuted.color)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous).fill(design.raised.color))
        .identified("report.sent", label: "Sent.", value: receipt.id)
    }
}

/// The frozen picture with its rectangles on it, and a drag that draws another.
///
/// The rectangles are held in the frame's own points, not in the points this
/// view happens to be laid out at, so the same report drawn on a phone and
/// read on a Mac describes the same place. The conversion is one scale factor
/// and it happens here, at the one place that knows both sizes.
struct FrozenFrameView: View {
    @Environment(\.design) private var design
    let capture: ReportCapture
    let marks: [ReportMark]
    let drew: @MainActor (ReportMark) -> Void

    /// Where the finger went down and where it is now, in this view's points.
    @State private var from: CGPoint?
    @State private var to: CGPoint?

    var body: some View {
        GeometryReader { geometry in
            let shown = geometry.size
            let scale = shown.width / capture.frame.width
            ZStack(alignment: .topLeading) {
                if let picture = capture.frame.picture {
                    picture
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                } else {
                    Text("The screen could not be photographed.")
                        .designFont(.detail, design)
                        .foregroundStyle(design.inkMuted.color)
                }
                ForEach(Array(marks.enumerated()), id: \.offset) { at, mark in
                    box(mark.rectangle.scaled(by: scale), number: at + 1)
                }
                if let drawing { box(drawing, number: nil) }
            }
            .frame(width: shown.width, height: shown.height)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 4)
                    .onChanged { drag in
                        if from == nil { from = drag.startLocation }
                        to = drag.location
                    }
                    .onEnded { drag in
                        defer {
                            from = nil
                            to = nil
                        }
                        let rectangle = CGRect(from: drag.startLocation, to: drag.location)
                        // A tap that slipped is not a box. Anything under a
                        // few points across marks nothing anybody could point
                        // at, and would arrive in the bundle as a rectangle
                        // with no area.
                        guard rectangle.width > 6, rectangle.height > 6, scale > 0 else { return }
                        drew(ReportMark(rectangle.scaled(by: 1 / scale)))
                    })
        }
        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        // A hairline round it, because a photograph of this app on this app's
        // own ground has no edge of its own: the two grounds are the same
        // colour and the picture would bleed into the screen holding it.
        .overlay {
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .strokeBorder(design.hairline.color, lineWidth: 1)
        }
        .identified("report.frame", value: "\(marks.count) marked")
    }

    /// The rectangle being drawn right now, if one is.
    private var drawing: CGRect? {
        guard let from, let to else { return nil }
        return CGRect(from: from, to: to)
    }

    /// One rectangle: the accent outline, and its number in the same accent so
    /// the note underneath and the box on the picture are visibly the same
    /// thing. A box still being dragged has no number yet.
    private func box(_ rectangle: CGRect, number: Int?) -> some View {
        RoundedRectangle(cornerRadius: 6, style: .continuous)
            .strokeBorder(design.accent.color, lineWidth: 2)
            .frame(width: rectangle.width, height: rectangle.height)
            .overlay(alignment: .bottomLeading) {
                if let number {
                    Text("\(number)")
                        .designFont(.monoSmall, design)
                        .foregroundStyle(design.onAccent.color)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 2)
                        .background(
                            Capsule().fill(design.accent.color))
                        .offset(x: -2, y: 11)
                }
            }
            .offset(x: rectangle.minX, y: rectangle.minY)
    }
}

extension ReportMark {
    /// The rectangle this mark is, in the frame's own points.
    var rectangle: CGRect {
        CGRect(x: x, y: y, width: width, height: height)
    }

    init(_ rectangle: CGRect) {
        self.init(
            x: rectangle.minX, y: rectangle.minY,
            width: rectangle.width, height: rectangle.height)
    }
}

extension CGRect {
    /// The rectangle two points bound, whichever way round they were given.
    init(from: CGPoint, to: CGPoint) {
        self.init(
            x: Swift.min(from.x, to.x), y: Swift.min(from.y, to.y),
            width: abs(to.x - from.x), height: abs(to.y - from.y))
    }

    func scaled(by factor: CGFloat) -> CGRect {
        CGRect(
            x: minX * factor, y: minY * factor,
            width: width * factor, height: height * factor)
    }
}
