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

/// The report itself, on the frame that was frozen.
///
/// What is here is the frozen picture and a way out of it. The rectangles,
/// their notes, the note about the whole thing and Send are the next piece of
/// this screen and are not drawn yet; what this establishes is the one thing
/// the flow rests on — that the picture on this screen is the picture that was
/// on screen when the report started, not a picture of the report.
public struct ReportScreen: View {
    @Environment(\.design) private var design
    private let capture: ReportCapture
    private let close: @MainActor () -> Void

    public init(capture: ReportCapture, close: @escaping @MainActor () -> Void) {
        self.capture = capture
        self.close = close
    }

    public var body: some View {
        ZStack {
            design.ground.color.ignoresSafeArea()
            VStack(spacing: 16) {
                HStack {
                    Text("Report a Problem")
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.ink.color)
                    Spacer(minLength: 8)
                    Button(action: close) { GlassIcon(glyph: "xmark") }
                        .buttonStyle(.plain)
                        .identified("report.close", label: "Close")
                }
                frame
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 18)
            .padding(.top, 12)
        }
        .identified("report.screen", value: capture.route ?? "unknown")
    }

    /// The frozen frame, drawn at the shape it was taken at.
    ///
    /// The recorded point size decides the aspect, not the bytes: the picture
    /// is the phone's pixels and dividing by the recorded scale is the only
    /// thing that puts a rectangle drawn on it back where the person drew it.
    @ViewBuilder private var frame: some View {
        if let image = capture.frame.picture {
            image
                .resizable()
                .aspectRatio(capture.frame.width / capture.frame.height, contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                .identified(
                    "report.frame",
                    value: "\(Int(capture.frame.width))x\(Int(capture.frame.height))")
        } else {
            Text("The screen could not be photographed.")
                .designFont(.body, design)
                .foregroundStyle(design.inkMuted.color)
                .identified("report.frame", value: "unreadable")
        }
    }
}

