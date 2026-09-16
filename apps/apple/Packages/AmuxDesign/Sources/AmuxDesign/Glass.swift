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

/// What floats over content, in one of the two finishes this app uses.
///
/// Glass is for small controls: the conversation's pill, the round icon
/// buttons, the changes chip, the tab bar. On something that size the rim and
/// the edge lensing are what say "this floats and can be pressed", and there
/// is too little of it to be read through.
///
/// A panel that carries content — the composer, an ask, a card, a menu, the
/// home's groups of rows — is frosted material with a hairline rim instead.
/// Glass over something that size is a sheet of highlights with words under
/// it; the rim and lensing that read as "control" on a button read as gloss
/// on a card, and the design stops looking like a place to read.
///
/// Either way the ground is washed in behind it. The material samples what is
/// behind it, and over a dense transcript that backdrop stays legible — you
/// could read a mirrored copy of the conversation through the composer, which
/// is a surface pretending to be a mirror. A wash stops the backdrop resolving
/// into words.
///
/// A reader who has asked the system to reduce transparency gets none of it:
/// the surface fills solid and states that it floats with a hairline rim
/// instead of with a sampled backdrop. The rim is what carries the meaning
/// once the blur is gone — without it a solid panel over a solid ground is two
/// flat areas with no edge between them.
public enum Frost: Sendable {
    /// Liquid glass, for small controls.
    case control
    /// Frosted material with a hairline rim, for panels that carry content.
    case panel
}

private struct Frosted<S: Shape>: ViewModifier {
    @Environment(\.design) private var design
    @Environment(\.reducesTransparency) private var reduceTransparency
    let shape: S
    let wash: Double
    let finish: Frost

    func body(content: Content) -> some View {
        if reduceTransparency {
            content
                .background { shape.fill(design.raised.color) }
                .overlay {
                    shape.stroke(design.hairline.color,
                                 lineWidth: design.metrics.hairline)
                }
        } else {
            switch finish {
            case .control:
                content
                    .background { shape.fill(design.ground.color.opacity(wash)) }
                    .glassEffect(.regular, in: shape)
            case .panel:
                content
                    .background {
                        shape.fill(.regularMaterial)
                            .overlay { shape.fill(design.raised.color.opacity(wash)) }
                    }
                    .overlay {
                        shape.stroke(design.hairline.color,
                                     lineWidth: design.metrics.hairline)
                    }
            }
        }
    }
}

extension View {
    /// A floating surface with the ground washed in behind it: frosted
    /// material for a panel, which is most of them, or glass for a control.
    public func frosted<S: Shape>(
        _ shape: S, wash: Double = Glass.wash, as finish: Frost = .panel
    ) -> some View {
        modifier(Frosted(shape: shape, wash: wash, finish: finish))
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
    ///
    /// One number for every screen. Six surfaces used to dim their backdrop by
    /// six slightly different amounts, which nobody chose — it is the kind of
    /// difference that only shows up when two of them open in the same minute.
    public static let scrim: Double = 0.25
}

/// Content pushed back because something has opened over it.
///
/// It is a view of its own rather than a modifier because it is also the way
/// out: everything in this app that opens over the conversation closes by a
/// press anywhere else, and a card with no visible dismissal and no dimmed
/// ground is a trap.
///
/// It fades. A whole screen changing brightness between two frames is the one
/// thing in a set of menus that reads as a fault rather than as a style, and
/// the reason it used to cut — that a fade is a clock, and these screens are
/// photographed — is answered by holding it still in front of a camera rather
/// than by never moving at all. The caller supplies the curve by animating
/// whatever decides the scrim is there.
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
            .transition(.opacity)
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
                Color.clear.frosted(shape, as: .panel)
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
