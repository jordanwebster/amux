import SwiftUI

/// The app's backdrop.
public struct Ground: View {
    @Environment(\.design) private var design

    public init() {}

    public var body: some View {
        Group {
            if design.surfaces.graduated {
                LinearGradient(
                    colors: [design.ground.color, design.sunken.color],
                    startPoint: .top, endPoint: .bottom)
            } else {
                design.ground.color
            }
        }
        .ignoresSafeArea()
    }
}

/// Glass, over a wash of the ground.
///
/// The material samples what is behind it, and over a dense transcript that
/// backdrop stays legible — you could read a mirrored copy of the conversation
/// through the composer, which is a surface pretending to be a mirror. A wash
/// of the ground underneath stops the backdrop resolving into words while
/// leaving the rim and the edge lensing, which are the part of the material
/// that says "this floats". Everything that floats over content uses this;
/// nothing uses bare glass.
///
/// A reader who has asked the system to reduce transparency gets none of it:
/// the surface fills solid and states that it floats with a hairline rim
/// instead of with a sampled backdrop. The rim is what carries the meaning
/// once the lensing is gone — without it a solid panel over a solid ground is
/// two flat areas with no edge between them.
private struct Frosted<S: Shape>: ViewModifier {
    @Environment(\.design) private var design
    @Environment(\.reducesTransparency) private var reduceTransparency
    let shape: S
    let wash: Double

    func body(content: Content) -> some View {
        if reduceTransparency {
            content
                .background { shape.fill(design.raised.color) }
                .overlay {
                    shape.stroke(design.hairline.color,
                                 lineWidth: design.metrics.hairline)
                }
        } else {
            content
                .background { shape.fill(design.ground.color.opacity(wash)) }
                .glassEffect(.regular, in: shape)
        }
    }
}

extension View {
    /// Glass with the ground washed in behind it.
    public func frosted<S: Shape>(_ shape: S, wash: Double = Glass.wash) -> some View {
        modifier(Frosted(shape: shape, wash: wash))
    }
}

public enum Glass {
    /// How much ground is washed in under the material by default. Raised for
    /// a surface that opens over the whole screen, where more of the backdrop
    /// would otherwise show through.
    public static let wash: Double = 0.78
    public static let openWash: Double = 0.88
    /// How far back the content goes when something opens over it.
    ///
    /// Black rather than a colour resolved per appearance, and the same amount
    /// in both: on a light ground it takes the content back a quarter of the
    /// way and on a dark one it does almost nothing, which is right, because a
    /// dark screen already reads the floating surface as nearer.
    public static let scrim: Double = 0.25
}

/// Content pushed back because something has opened over it.
///
/// It is a view of its own rather than a modifier because it is also the way
/// out: everything in this app that opens over the conversation closes by a
/// press anywhere else, and a card with no visible dismissal and no dimmed
/// ground is a trap. Nothing about it moves — it is drawn on screens that are
/// photographed, and a fade is a clock.
public struct Scrim: View {
    private let dismiss: () -> Void

    public init(dismiss: @escaping () -> Void) {
        self.dismiss = dismiss
    }

    public var body: some View {
        Color.black
            .opacity(Glass.scrim)
            .ignoresSafeArea()
            .contentShape(Rectangle())
            .onTapGesture(perform: dismiss)
            .accessibilityLabel("Close")
            .accessibilityAddTraits(.isButton)
    }
}

/// A raised surface. How it separates from the ground is a decision about the
/// design as a whole, not a per-screen one, so it lives here.
public struct Surface<Content: View>: View {
    @Environment(\.design) private var design
    private let radius: CGFloat?
    private let prominence: Design.Prominence
    private let always: Bool
    private let content: Content

    /// - Parameters:
    ///   - prominence: subject surfaces may be glass even where the design's
    ///     default is a rule; configuration never is.
    ///   - always: some things are containers whatever the design thinks — a
    ///     banner, a card that has to look like one — and are drawn even under
    ///     a rule separation.
    public init(
        radius: CGFloat? = nil,
        prominence: Design.Prominence = .plain,
        always: Bool = false,
        @ViewBuilder content: () -> Content
    ) {
        self.radius = radius
        self.prominence = prominence
        self.always = always
        self.content = content()
    }

    public var body: some View {
        let corner = radius ?? design.metrics.cardRadius
        content.background {
            let shape = RoundedRectangle(cornerRadius: corner, style: .continuous)
            switch prominence == .subject ? .glass : design.surfaces.separation {
            case .glass:
                Color.clear.frosted(shape)
            case .card:
                card(shape)
            case .rule:
                if always { card(shape) }
            }
        }
    }

    @ViewBuilder
    private func card(_ shape: RoundedRectangle) -> some View {
        shape.fill(design.raised.color)
            .overlay(shape.strokeBorder(design.hairline.color,
                                        lineWidth: design.metrics.hairline))
    }
}
