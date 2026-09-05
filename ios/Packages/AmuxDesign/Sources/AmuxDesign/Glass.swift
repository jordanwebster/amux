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
private struct Frosted<S: Shape>: ViewModifier {
    @Environment(\.design) private var design
    let shape: S
    let wash: Double

    func body(content: Content) -> some View {
        content
            .background { shape.fill(design.ground.color.opacity(wash)) }
            .glassEffect(.regular, in: shape)
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
