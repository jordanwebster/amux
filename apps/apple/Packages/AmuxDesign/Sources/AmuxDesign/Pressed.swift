import SwiftUI

/// What a control does under a thumb.
///
/// SwiftUI's `.plain` style draws a custom label exactly as it is written and
/// adds no pressed state at all, so every row and every control in this app
/// absorbed a touch and showed nothing until the screen changed. A press that
/// goes unacknowledged reads as a dropped one, and the reader presses again.
///
/// Two answers, because the app has two shapes of tappable thing and one
/// answer does not fit both. A full-width row is a piece of a list: shrinking
/// it would pull it away from the rows either side, so it lights instead. A
/// discrete control is an object, and takes a press the way a key does, by
/// giving under it.
public struct AmuxRowButtonStyle: ButtonStyle {
    /// The corner of the thing being lit. Square for a row that runs the full
    /// width of its group, which is most of them; the label's own radius for a
    /// row that is drawn as a rounded plate of its own.
    private let cornerRadius: CGFloat
    /// Whether the press lights the row at all. A row that opens another
    /// screen does not: the screen arriving is the acknowledgement, and a tint
    /// drawn for the frames before it arrives only shows as a grey slab over
    /// a rounded group whose corners it does not follow.
    private let lights: Bool

    public init(cornerRadius: CGFloat = 0, lights: Bool = true) {
        self.cornerRadius = cornerRadius
        self.lights = lights
    }

    public func makeBody(configuration: Configuration) -> some View {
        Lit(configuration: configuration, cornerRadius: cornerRadius, lights: lights)
            // The whole row answers a touch, not only the pixels something is
            // drawn on. Without this the gaps between a row's lines and the
            // space around its text fall through, and a row near its edges
            // takes a hard press to open. Set here so no row can forget it.
            .contentShape(Rectangle())
    }

    /// Environment is read inside a view rather than on the style, because a
    /// `ButtonStyle` is not itself a view: values read on one resolve where the
    /// style was declared rather than where the button is drawn.
    private struct Lit: View {
        @Environment(\.design) private var design
        @Environment(\.isEnabled) private var isEnabled
        let configuration: Configuration
        let cornerRadius: CGFloat
        let lights: Bool

        // The tint exists only while the press does, rather than sitting over
        // every row at zero opacity waiting for one: an acknowledgement is
        // instant, so there is nothing an interpolated value would buy.
        @ViewBuilder
        var body: some View {
            if configuration.isPressed && lights {
                configuration.label
                    // Over the label rather than behind it. Several of these
                    // rows draw their own opaque plate, and a tint behind one
                    // of those is a tint nobody sees.
                    .overlay {
                        // Ink rather than a fixed grey, so one token lightens
                        // a dark row and darkens a light one: a highlight
                        // means "nearer the foreground", not "darker".
                        RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                            .fill(design.ink.color)
                            .opacity(0.07)
                            .allowsHitTesting(false)
                    }
            } else if isEnabled {
                configuration.label
            } else {
                configuration.label.opacity(0.5)
            }
        }
    }
}

public struct AmuxControlButtonStyle: ButtonStyle {
    public init() {}

    public func makeBody(configuration: Configuration) -> some View {
        Giving(configuration: configuration)
    }

    private struct Giving: View {
        @Environment(\.reducesMotion) private var reduceMotion
        @Environment(\.isEnabled) private var isEnabled
        let configuration: Configuration

        // A control at rest is drawn exactly as it was written, with no
        // modifier over it at all. `scaleEffect` resamples what it wraps even
        // at a scale of one: every glyph in the app came back a hair softer
        // and a fraction smaller, on every screen, which the captures caught.
        // So the press is a branch rather than a value, and the cost of that
        // is the transition being instant — which for an acknowledgement is
        // not a cost. Opacity rather than a tint because a control's shape is
        // its own business, and a plate drawn over a circle shows corners it
        // does not have.
        var body: some View {
            if configuration.isPressed {
                if reduceMotion {
                    configuration.label.opacity(0.55)
                } else {
                    configuration.label.scaleEffect(0.96)
                }
            } else if isEnabled {
                configuration.label
            } else {
                configuration.label.opacity(0.5)
            }
        }
    }
}

extension ButtonStyle where Self == AmuxRowButtonStyle {
    /// A row in a list or a menu. Lights under the thumb.
    public static var amuxRow: AmuxRowButtonStyle { AmuxRowButtonStyle() }

    /// A row drawn as a rounded plate of its own, lit to its own corner.
    public static func amuxRow(cornerRadius: CGFloat) -> AmuxRowButtonStyle {
        AmuxRowButtonStyle(cornerRadius: cornerRadius)
    }

    /// A row that opens another screen. Nothing is drawn under the thumb,
    /// because the screen that arrives is the answer to the press.
    public static var amuxPush: AmuxRowButtonStyle { AmuxRowButtonStyle(lights: false) }
}

extension ButtonStyle where Self == AmuxControlButtonStyle {
    /// A discrete control — an icon, a tile, a button with a label on it.
    /// Gives under the thumb.
    public static var amuxControl: AmuxControlButtonStyle { AmuxControlButtonStyle() }
}
