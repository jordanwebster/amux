import SwiftUI

/// The app's visual language, as values.
///
/// One rule outranks the rest: colour means attention. An agent with nothing
/// to say is drawn achromatically, so the eye lands on the one that does
/// without anyone hunting for a badge — which is why there is a single
/// chromatic accent and every other token is a step on the neutral ramp.
public struct Design: Sendable, Equatable {
    public let name: String

    // Ground and surfaces, all distances along `Neutral`.
    public let ground: Ramp
    public let raised: Ramp
    public let sunken: Ramp
    public let hairline: Ramp

    // Ink. Neither pole is absolute: pure black on white is heavy and pure
    // white on black glares, so both back off a step.
    public let ink: Ramp
    public let inkMuted: Ramp
    public let inkFaint: Ramp

    /// The one chromatic voice, spent only on something that is waiting.
    public let accent: Ramp
    public let onAccent: Ramp

    /// What a change added and what it took away.
    ///
    /// The only colours in the design that are not the accent or a step on the
    /// neutral ramp, and the exception is deliberate: green and red are a
    /// convention about what changed, while the accent is this app's one word
    /// for "something is waiting for you". A diff is not that, so it must not
    /// borrow the word. Both are muted well below a signal green and red so
    /// that a page of them still reads as text.
    public let added: Ramp
    public let removed: Ramp

    public let faces: Faces
    public let metrics: Metrics
    public let type: Typography
    public let surfaces: Surfaces

    public init(
        name: String, ground: Ramp, raised: Ramp, sunken: Ramp, hairline: Ramp, ink: Ramp,
        inkMuted: Ramp, inkFaint: Ramp, accent: Ramp, onAccent: Ramp, added: Ramp,
        removed: Ramp, faces: Faces, metrics: Metrics, type: Typography, surfaces: Surfaces
    ) {
        self.name = name
        self.ground = ground
        self.raised = raised
        self.sunken = sunken
        self.hairline = hairline
        self.ink = ink
        self.inkMuted = inkMuted
        self.inkFaint = inkFaint
        self.accent = accent
        self.onAccent = onAccent
        self.added = added
        self.removed = removed
        self.faces = faces
        self.metrics = metrics
        self.type = type
        self.surfaces = surfaces
    }

    public struct Metrics: Sendable, Equatable {
        public let cardRadius: CGFloat
        public let controlRadius: CGFloat
        /// The radius of anything that floats free of an edge; a detached
        /// surface is rounder than one that meets a border.
        public let floatRadius: CGFloat
        public let rowPadding: CGFloat
        public let gutter: CGFloat
        public let rowGap: CGFloat
        public let feedGap: CGFloat
        public let hairline: CGFloat
    }

    /// How a raised thing separates itself from what is behind it.
    public enum Separation: String, Sendable {
        /// A rule between rows, and no container at all.
        case rule
        /// A filled surface with a hairline border.
        case card
        /// Glass, and nothing else — no border, no shadow.
        case glass
    }

    public struct Surfaces: Sendable, Equatable {
        public let separation: Separation
        /// Whether the ground is flat or graduated.
        public let graduated: Bool
    }

    /// Where a surface may be glass. The agents are what the app is about and
    /// glass makes them read as objects; a host row is configuration, and
    /// configuration wants to be quiet.
    public enum Prominence: Sendable { case subject, plain }

    public struct Typography: Sendable, Equatable {
        /// Agent and host names. Mono reads as "an identifier you might type",
        /// which is what they are.
        public let identifierIsMono: Bool
        public let titleWeight: Font.Weight
        /// How big a screen's own name is.
        public let titleSize: CGFloat
        public let bodySize: CGFloat
        public let tightTracking: CGFloat
    }

    /// The app's one skin: type-led, rules instead of containers, and mono
    /// kept for the things that are literally identifiers.
    public static let app = Design(
        name: "Studio",
        ground: Neutral.pair(light: 2, dark: 12),
        raised: Neutral.pair(light: 0, dark: 10),
        // Light recesses by going darker; dark cannot, because its ground is
        // already near-black, so on dark a well is a step up.
        sunken: Neutral.pair(light: 3, dark: 11),
        // Borders need a touch more separation on dark than on light to read
        // at the same strength: three steps against two.
        hairline: Neutral.pair(light: 4, dark: 9),
        ink: Neutral.pair(light: 13, dark: 1),
        inkMuted: Neutral.pair(light: 8, dark: 6),
        inkFaint: Neutral.pair(light: 6, dark: 7),
        // Petrol: cool enough to stay calm, far enough round the wheel from
        // the indigo every AI product wears to read as instrument, not brand.
        accent: Ramp(0x0E5F6E, 0x4FBACD),
        onAccent: Ramp(0xFFFFFF, 0x06080B),
        // Read off the approved drawings rather than picked afresh, so the
        // arithmetic on a chip and the body of a diff are the same green and
        // the same red the design was signed off in.
        added: Ramp(0x458353, 0x84C490),
        removed: Ramp(0x9B3C40, 0xD78583),
        faces: .instrument,
        metrics: Metrics(
            cardRadius: 16, controlRadius: 12, floatRadius: 26,
            rowPadding: 14, gutter: 18, rowGap: 1, feedGap: 16,
            hairline: 0.5),
        type: Typography(
            identifierIsMono: true, titleWeight: .semibold,
            titleSize: 26, bodySize: 16, tightTracking: -0.4),
        surfaces: Surfaces(separation: .rule, graduated: false))

    /// Every colour token in the order a catalogue or a pinned table reads
    /// them, so no token can be added without showing up in both.
    public var colours: [(name: String, ramp: Ramp)] {
        [
            ("ground", ground), ("raised", raised), ("sunken", sunken),
            ("hairline", hairline), ("ink", ink), ("inkMuted", inkMuted),
            ("inkFaint", inkFaint), ("accent", accent), ("onAccent", onAccent),
            ("added", added), ("removed", removed),
        ]
    }

    /// The accent as a plain `Color` for `.tint`, which cannot resolve a Ramp.
    public var accentColor: Color { accent.color }
}

private struct DesignKey: EnvironmentKey {
    static let defaultValue = Design.app
}

extension EnvironmentValues {
    public var design: Design {
        get { self[DesignKey.self] }
        set { self[DesignKey.self] = newValue }
    }
}

/// Whether the screen is being photographed for a baseline rather than looked
/// at by a person.
///
/// A photograph of a screen has to be of the screen and not of the moment it
/// was taken in, so anything that runs on a timer of its own draws its resting
/// state while this is set. The text caret is the one that matters: it blinks
/// about once a second, on nobody's schedule but its own, and a picture caught
/// mid-blink differs from the one before it by a bar of accent that means
/// nothing.
private struct PhotographedKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    public var photographed: Bool {
        get { self[PhotographedKey.self] }
        set { self[PhotographedKey.self] = newValue }
    }
}
